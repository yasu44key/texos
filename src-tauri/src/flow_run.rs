#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ============================================================================
// フローレーン（構造の実行）— §4.4 共有2レーンの境界
// ----------------------------------------------------------------------------
// ここから下（FlowReport / FlowPayload / NodeRun / execute_node / eval_filter /
// run_flow / build_cmdline と、フィルタ／ランチャーライブラリ群）は「部分グラフの
// 実行と spec の取り回し」= フローレーン。実身/仮身/origin/CAS の「中身の共有」=
// データレーンとはコードパスを分けて考える（混ぜると食い違う、設計メモ §4.4）。
//
// 将来の整理メモ:
//  - この一群は flow_run.rs として lib.rs から切り出せる（v1.5 のファイル分割候補）。
//  - filter_library / launcher_library は現状「単一カード」だけを共有する。設計メモ §2 の
//    「部分グラフ＝モジュール」へ一般化する余地を残す（execute_node が部分グラフの
//    1ノード実行になっているのはその布石）。
// ============================================================================

#[derive(Serialize)]
pub struct FlowReport {
    /// 新しく生まれた結果カードの vid（front は緑シグナル＋UNDO登録に使う）
    pub created: Vec<String>,
    /// 固定名上書きで中身を差し替えた既存カードの vid（緑シグナルのみ・UNDO対象外）
    pub updated: Vec<String>,
    /// 生成されたファイル名（ダイアログ表示用）
    pub outputs: Vec<String>,
    /// 型不一致などでスキップした入力（ラベル＋理由）
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    /// シェルカード統合（2026-07-06）: window:"hidden" のシェル実行結果
    /// （stdout/stderr 時系列マージ＋終了コード）。front は結果パネルで表示する。
    /// シェル以外のフロー・ウィンドウあり実行では None。
    pub shell_capture: Option<ShellCapture>,
}

/// window:"hidden" シェル実行の捕捉結果（結果パネル用）。
#[derive(Serialize)]
pub struct ShellCapture {
    /// プロセス終了コード（シグナル等で取れなければ None）。
    pub exit_code: Option<i32>,
    /// stdout+stderr（2>&1 マージ・推定デコード済み）。長大出力は切り詰める。
    pub output: String,
    /// 出力を切り詰めたか（パネルに「以下省略」を出す）。
    pub truncated: bool,
}

/// パイプラインの中間結果。中間ファイル・中間カードは作らず、メモリで次段へ渡す
/// （外部コマンドの一時ファイルは flow.rs 側で即削除）。
pub(crate) struct FlowPayload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) ext: String,
    pub(crate) mime: String,
    /// 連鎖の起点データのファイル名語幹（最終出力の命名に使う）
    pub(crate) stem: String,
}

/// §4.1 リファクタリング: ノード1回分の実行結果。`outputs` は生成物（builtin/ai は1個、
/// 外部は1個、ジェネレータは1個）。`exit_code`/`stderr` は外部プロセスを完走させたときの
/// 情報で、現状 eval_filter は使わない — 将来の exit code 分岐（§4.3「成功時/失敗時」の
/// flow 矢印）でここを見るための土台アンカー。
pub(crate) struct NodeRun {
    outputs: Vec<(Vec<u8>, String, String)>, // (bytes, ext, mime)
    #[allow(dead_code)] // §4.3 で消費予定
    exit_code: Option<i32>,
    #[allow(dead_code)] // §4.3 で消費予定
    stderr: String,
}

/// §4.1 実行ディスパッチの一本道: 「ノード種別 → 実行 → 結果（payload + exit code + stderr）」。
/// 種別判定（flow::NodeKind）に応じて内蔵フィルタ / AI / 外部プロセスへ振り分ける。
/// 入力の収集・型チェック・命名（stem）は呼び出し側（eval_filter）の責務で、ここは
/// 「与えられた入力でこの spec を1回実行する」ことだけに専念する（データ/フローの関心を分離）。
///
/// inputs は (bytes, ext, mime) の並び:
/// - Generator … 空（入力を取らない）
/// - Builtin / Ai … 必ず1個
/// - External … ファンアウト時1個 / 集約時 N 個
pub(crate) fn execute_node(
    root: &Path,
    spec: &flow::FilterSpec,
    kind: flow::NodeKind,
    inputs: &[(&[u8], &str, &str)],
    cancel: &std::sync::atomic::AtomicBool,
    stdin_slot: &flow::StdinSlot,
) -> Result<NodeRun, String> {
    use flow::NodeKind;
    let plain = |bytes: Vec<u8>, ext: String, mime: String| NodeRun {
        outputs: vec![(bytes, ext, mime)],
        exit_code: None,
        stderr: String::new(),
    };
    match kind {
        NodeKind::Builtin => {
            // builtin 名は node_kind が Builtin と判定した時点で Some（"ai" 以外）。
            let b = spec.builtin.as_deref().unwrap_or("");
            let (bytes, ext, mime) = flow::run_builtin(spec, b, inputs[0].0)?;
            Ok(plain(bytes, ext, mime))
        }
        NodeKind::Ai => {
            // v1.4.0 AIフィルタは builtin だがネットワークを使う別経路（同期blocking）。
            let (bytes, ext, mime) = flow::run_ai(spec, inputs[0].0)?;
            // v1.5.1 WS-B: 実行中に中断された場合は応答を破棄する（HTTP は途中で切れないため
            // 完走後にここで判定。「中断＝出力は破棄」の既存契約に合わせる）。
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("中断されました".into());
            }
            Ok(plain(bytes, ext, mime))
        }
        NodeKind::T2i => {
            // v1.5 Text-To-Image: 入力を種別で振り分け＝テキスト→プロンプト / 画像→元画像(img2img)。
            // 集約ノードなので inputs に複数（プロンプト＋元画像）が来る。AI 同様ネットワーク別経路。
            let mut prompt: Option<&[u8]> = None;
            let mut init: Option<&[u8]> = None;
            for (bytes, _ext, mime) in inputs {
                if mime.starts_with("image/") {
                    if init.is_none() {
                        init = Some(*bytes);
                    }
                } else if prompt.is_none() {
                    prompt = Some(*bytes); // テキスト等はプロンプト扱い（最初の1本）
                }
            }
            // v1.4.17 インラインパネルの追加プロンプトを合成: 接続テキストの後ろに ", " で
            // 連結（SD は前方トークンほど効くため接続入力＝主・パネル＝追加）。入力が無ければ
            // パネル単独で生成できる。両方無いときだけエラー。
            let combined: Vec<u8>;
            let prompt: &[u8] = match (prompt, spec.own_prompt()) {
                (Some(p), Some(x)) => {
                    combined = [p, b", ", x.as_bytes()].concat();
                    &combined
                }
                (Some(p), None) => p,
                (None, Some(x)) => x.as_bytes(),
                (None, None) => {
                    return Err(
                        "プロンプトがありません。テキストカードを接続するか、パネル（ダブルクリック）に入力してください".into(),
                    )
                }
            };
            let (bytes, ext, mime) = flow::run_t2i(spec, prompt, init)?;
            // v1.5.1 WS-B t2i 中断の実効化: 中断（break_flow/cancel_all → デーモン /cancel）
            // されると diffusers は次ステップ境界で denoise を抜け**部分画像を返して完走**する。
            // そのまま進むと中途半端な画像カードが生まれるので、ここで破棄してエラー化する
            //（デーモンは既に解放済み＝次の生成を受けられる）。
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("中断されました".into());
            }
            Ok(plain(bytes, ext, mime))
        }
        NodeKind::Generator | NodeKind::External => {
            // 出力拡張子: output_ext 指定 > 最初の入力の拡張子 > "bin"
            //（ジェネレータは入力が無いので "bin"、外部は入力拡張子に追従）。
            let out_ext = spec
                .output_ext
                .clone()
                .unwrap_or_else(|| inputs.first().map(|i| i.1.to_string()).unwrap_or_else(|| "bin".into()));
            let out_mime = mime_for_ext(&out_ext)
                .unwrap_or("application/octet-stream")
                .to_string();
            let cmdline = build_cmdline(root, spec)?;
            let ins: Vec<(&[u8], &str)> = inputs.iter().map(|(b, e, _)| (*b, *e)).collect();
            // v1.4.9: record モードのジェネレータ（録音）だけ stdin を開いて 'q' で停止可能に。
            let slot = if kind == NodeKind::Generator && spec.record {
                Some(stdin_slot)
            } else {
                None
            };
            let o = flow::run_external_full(
                &cmdline,
                &ins,
                &out_ext,
                spec.timeout_secs,
                cancel,
                Some(root),
                slot,
            )?;
            let exit_code = o.exit_code;
            let stderr = o.stderr.clone();
            // {out} を書かない自己完結コマンド（カメラ起動・読み上げ等の fire-and-forget）は
            // 取り込む出力が無い。成功＝完了とみなし出力カードを作らない。非0終了は従来どおり
            // エラーにする（stderr 付きの文面を再利用）。{out} を書くコマンドは従来のまま
            // 「出力なし→エラー」で挙動不変。
            let outputs = if spec.is_self_contained() {
                if !o.success() {
                    return Err(o.into_bytes().unwrap_err());
                }
                Vec::new()
            } else {
                vec![(o.into_bytes()?, out_ext, out_mime)]
            };
            Ok(NodeRun {
                outputs,
                exit_code,
                stderr,
            })
        }
        NodeKind::Invalid => Err("フィルタ定義に builtin も cmd もありません".into()),
    }
}

/// フィルタを再帰評価する（pull型連鎖 — 実行モデル3）。
/// 入力にフィルタカードが繋がっていれば先にそれを評価し、結果をこのフィルタに通す。
/// `visited` は評価中パスのスタックで、循環を検出したらエラー。
pub(crate) fn eval_filter(
    state: &AppState,
    root: &Path,
    fid: &str,
    visited: &mut std::collections::HashSet<String>,
    report: &mut FlowReport,
    cancel: &std::sync::atomic::AtomicBool,
    stdin_slot: &flow::StdinSlot,
) -> Result<Vec<FlowPayload>, String> {
    if !visited.insert(fid.to_string()) {
        return Err("フローに循環があります".into());
    }
    // 短ロック: フィルタ spec を読む（実行はこのあとロック外で）。
    let spec_json: Option<String> = with_db(state, |p| {
        p.conn
            .query_row(
                "SELECT filter_ref FROM virtual_bodies
                 WHERE id = ?1 AND kind = 'filter' AND deleted_at IS NULL",
                rusqlite::params![fid],
                |r| r.get(0),
            )
            .map_err(|_| "フィルタカードが見つかりません".to_string())
    })?;
    let spec = flow::parse_spec(spec_json.as_deref().unwrap_or("{}"))?;
    let accept = spec.accept_mime.clone().unwrap_or_else(|| "*/*".into());

    // ジェネレータ型: 入力を取らず1回実行してファイルを生む（入力矢印は無視）。
    // §4.1: 実行は execute_node に一任し、ここは命名（stem＝prefix or "output"）だけ担う。
    if spec.node_kind() == flow::NodeKind::Generator {
        let run = execute_node(root, &spec, flow::NodeKind::Generator, &[], cancel, stdin_slot);
        visited.remove(fid);
        return run.map(|r| {
            let stem = spec
                .prefix
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "output".into());
            r.outputs
                .into_iter()
                .map(|(bytes, ext, mime)| FlowPayload {
                    bytes,
                    ext,
                    mime,
                    stem: stem.clone(),
                })
                .collect()
        });
    }

    // 入力: このフィルタへ入る flow 矢印（生きているカードのみ・接続順）。
    // §4.3 拡張点アンカー: 将来 relations.kind='flow' に「成功時/失敗時/常に」の制御属性を
    // 持たせる（オートメーション設計メモ §1.2）。そのとき execute_node が返す NodeRun.exit_code
    // を見て分岐し、矢印の制御属性でこの収集をフィルタする。今はモデルを汚さず収集は無条件。
    // 短ロック: このフィルタへ入る flow 矢印（接続順）を取得。
    let inputs: Vec<(String, String, String, String, Option<String>, Option<String>)> =
        with_db(state, |p| {
            let mut stmt = p
                .conn
                .prepare(
                    "SELECT v.id, v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext), v.kind
                     FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                     JOIN real_bodies r ON r.id = v.real_id
                     WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                     ORDER BY rel.created_at",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![fid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        })?;

    let mut produced = Vec::new();
    // フェーズ1: 全入力のペイロードを接続順に収集（{inN} の番号は矢印を張った順）
    let mut collected: Vec<FlowPayload> = Vec::new();
    for (vid, real_id, vlabel, mime, ext, kind) in &inputs {
        // 入力ペイロードを集める: 上流フィルタ＝再帰評価／データカード＝窓の今を読む
        // 中断後は新しいプロセスを起こさない（複数入力の連鎖を断つ）
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            report.errors.push("中断されました".into());
            break;
        }
        let payloads: Vec<FlowPayload> = if kind.as_deref() == Some("filter") {
            // 再帰評価（呼び出し先が自分で短ロックを取る。ここではロックを保持しない）。
            match eval_filter(state, root, vid, visited, report, cancel, stdin_slot) {
                Ok(v) => v,
                Err(e) => {
                    report.errors.push(format!("{vlabel}: {e}"));
                    continue;
                }
            }
        } else if real_id.starts_with("drop:") {
            report
                .skipped
                .push(format!("{vlabel}（まだファイルが投入されていません）"));
            continue;
        } else if kind.as_deref() == Some("note") {
            // v1.4.17 カード統一＝定数カード: 付箋は本文をそのまま text/plain として供給する
            //（実体・CAS 非依存の読み出し専用テキスト。付箋→AI/t2i/grep がそのまま繋がる）。
            let text = match with_db(state, |p| {
                p.conn
                    .query_row(
                        "SELECT note_text FROM virtual_bodies WHERE id = ?1",
                        rusqlite::params![vid],
                        |r| Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default()),
                    )
                    .map_err(|e| e.to_string())
            }) {
                Ok(t) => t,
                Err(e) => {
                    report.errors.push(format!("{vlabel}: {e}"));
                    continue;
                }
            };
            let stem = if vlabel.trim().is_empty() {
                "コメント".to_string()
            } else {
                vlabel.clone()
            };
            vec![FlowPayload {
                bytes: text.into_bytes(),
                ext: "txt".into(),
                mime: "text/plain".into(),
                stem,
            }]
        } else {
            // 短ロック: 窓の今を解決（snapshot 更新＋ビューパス）。ファイル読みはロック外。
            let path = match with_db(state, |p| {
                refresh_origin_snapshot(p, vid);
                Ok(resolve_view_path(p, vid, real_id))
            }) {
                Ok(path) => path,
                Err(e) => {
                    report.errors.push(format!("{vlabel}: {e}"));
                    continue;
                }
            };
            match std::fs::read(&path) {
                Ok(d) => {
                    let stem = std::path::Path::new(vlabel)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("output")
                        .to_string();
                    vec![FlowPayload {
                        bytes: d,
                        ext: ext.clone().unwrap_or_else(|| "bin".into()),
                        mime: mime.clone(),
                        stem,
                    }]
                }
                Err(e) => {
                    report.errors.push(format!("{vlabel}: 読み込み失敗 ({e})"));
                    continue;
                }
            }
        };
        collected.extend(payloads);
    }

    // フェーズ2: 実行。種別ごとの分岐は execute_node へ一本化（§4.1）。ここに残るのは
    // 「型チェック（受け入れMIME・想定拡張子）」と「集約/ファンアウトの入力の束ね方」だけ。
    // 集約か個別かは CMD の書き方で決まる（{in*} か {in2} 以降があれば全入力を1回 = 集約:
    // gcc分割ソース・画像合成 / {in}/{in1} のみなら入力ごとに実行 = 従来のファンアウト）。
    let kind = spec.node_kind();
    // T2i は集約ノード（プロンプト＋元画像など全入力を集めて1回実行＝1枚生成）。
    if spec.is_aggregate() || kind == flow::NodeKind::T2i {
        // 集約実行は引数の位置・本数が意味を持つ → 合わない入力はスキップではなくエラー
        let mut bad = Vec::new();
        for pl in &collected {
            if !flow::mime_matches(&accept, &pl.mime) {
                bad.push(format!("{}（{} は受け入れ対象外）", pl.stem, pl.mime));
            } else if let Some(exts) = &spec.in_exts {
                let ok = exts.is_empty()
                    || exts
                        .iter()
                        .any(|e| e.trim().trim_start_matches('.').eq_ignore_ascii_case(&pl.ext));
                if !ok {
                    bad.push(format!("{}（.{} は想定拡張子外）", pl.stem, pl.ext));
                }
            }
        }
        if !bad.is_empty() {
            report.errors.extend(bad);
        } else if !collected.is_empty()
            // v1.4.17: t2i はパネルの追加プロンプトだけでも生成できる（入力ゼロで実行可）。
            || (kind == flow::NodeKind::T2i && spec.own_prompt().is_some())
        {
            let ins: Vec<(&[u8], &str, &str)> = collected
                .iter()
                .map(|pl| (pl.bytes.as_slice(), pl.ext.as_str(), pl.mime.as_str()))
                .collect();
            // 入力ゼロ（t2i パネル単独生成）でも落ちないよう stem はフォールバック付き。
            let stem0 = collected
                .first()
                .map(|pl| pl.stem.clone())
                .unwrap_or_else(|| "output".to_string());
            match execute_node(root, &spec, kind, &ins, cancel, stdin_slot) {
                Ok(run) => {
                    for (bytes, ext, mime) in run.outputs {
                        produced.push(FlowPayload { bytes, ext, mime, stem: stem0.clone() });
                    }
                }
                Err(e) => report.errors.push(format!("{stem0}: {e}")),
            }
        }
    } else {
        for pl in collected {
            if !flow::mime_matches(&accept, &pl.mime) {
                report
                    .skipped
                    .push(format!("{}（{} は受け入れ対象外）", pl.stem, pl.mime));
                continue;
            }
            // 想定拡張子のチェック（新形式 IN欄。空なら制限なし）
            if let Some(exts) = &spec.in_exts {
                let ok = exts.is_empty()
                    || exts
                        .iter()
                        .any(|e| e.trim().trim_start_matches('.').eq_ignore_ascii_case(&pl.ext));
                if !ok {
                    report
                        .skipped
                        .push(format!("{}（.{} は想定拡張子外）", pl.stem, pl.ext));
                    continue;
                }
            }
            let ins = [(pl.bytes.as_slice(), pl.ext.as_str(), pl.mime.as_str())];
            match execute_node(root, &spec, kind, &ins, cancel, stdin_slot) {
                Ok(run) => {
                    for (bytes, ext, mime) in run.outputs {
                        produced.push(FlowPayload {
                            bytes,
                            ext,
                            mime,
                            stem: pl.stem.clone(),
                        });
                    }
                }
                Err(e) => report.errors.push(format!("{}: {e}", pl.stem)),
            }
        }
    }
    visited.remove(fid);
    Ok(produced)
}

/// フローを実行（手動トリガー — 確定方針4）。
/// このフィルタを終端としてパイプラインを pull 評価し（上流のフィルタ連鎖は
/// メモリ上で通過）、最終結果だけをプロジェクト直下のコミット済み実身として
/// カード化（緑シグナルで誕生）。フィルタ→結果の flow 矢印を張り、
/// 実行ログはフィルタカードの memo に記録する。
// (async) でスレッドプール実行にする — 同期コマンドはメインスレッドで走るため、
// 外部コマンドがストールするとイベントループごと固まり中断ボタンが届かない。
#[tauri::command(async)]
pub(crate) fn run_flow(
    id: String,
    app: tauri::AppHandle,
    state: State<AppState>,
) -> Result<FlowReport, String> {
    // v1.5.1 WS-B 多重起動ガードの台帳版（v1.4.10 録音ハング修正の継承）: 台帳に Flow ジョブが
    // 居れば state.current をロックする**前**に即エラーで弾く（ここで待つと録音中フローの裏で
    // 積み上がる）。完了同期返しは維持＝この関数は従来どおり FlowReport を返す。
    let job = state.jobs.register_flow_exclusive("フロー実行")?;
    let _guard = JobGuard::new(&state.jobs, job.id);
    let result = run_flow_inner(&id, &app, &state, &job);
    // job:done / job:failed（WS-C 全規格の先行最小3種の後ろ2つ。progress は WS-C）。
    // report 内のエラーは「完了した実行の報告」なので done 扱い＝Err はコマンド自体の失敗。
    let _ = app.emit(
        if result.is_ok() { "job:done" } else { "job:failed" },
        serde_json::json!({ "job_id": job.id }),
    );
    result
}

fn run_flow_inner(
    id: &str,
    app: &tauri::AppHandle,
    state: &State<AppState>,
    job: &Job,
) -> Result<FlowReport, String> {
    let id = id.to_string();
    // 短ロック: 終端フィルタの位置・ラベル・spec と root を取得（実行中はロックを手放す）。
    let (fx, fy, flabel, fref, root): (f64, f64, String, Option<String>, PathBuf) =
        with_db(state.inner(), |p| {
            let (fx, fy, flabel, fref): (f64, f64, String, Option<String>) = p
                .conn
                .query_row(
                    "SELECT pos_x, pos_y, label, filter_ref FROM virtual_bodies
                     WHERE id = ?1 AND kind = 'filter' AND deleted_at IS NULL",
                    rusqlite::params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .map_err(|_| "フィルタカードが見つかりません".to_string())?;
            Ok((fx, fy, flabel, fref, p.root.clone()))
        })?;
    // 終端フィルタの設定が出力先と命名を決める（prefix連番 / 出力フォルダ）
    let term_spec = flow::parse_spec(fref.as_deref().unwrap_or("{}"))?;

    // v1.5.1 WS-B: ラベル・起動元プロジェクト（記録のみ）を確定し job:started を通知。
    // フロントはこの job_id を保持して break_flow / flow_send_stdin に渡す。
    *job.label.lock().unwrap() = flabel.clone();
    *job.project_root.lock().unwrap() = root.to_string_lossy().into_owned();
    let _ = app.emit(
        "job:started",
        serde_json::json!({ "job_id": job.id, "kind": "flow", "label": flabel }),
    );

    // v1.5 Text-To-Image: 終端が t2i カードなら生成デーモンを先に起こしておく（▶実行を
    // 「ただ押せば動く」に）。best-effort — 起動できなければ run_t2i が分かりやすく案内する。
    if term_spec.node_kind() == flow::NodeKind::T2i {
        let _ = t2i_ensure_server(&state.app_data, state.inner());
    }

    // v1.3.11 対話シェル: cmd/PowerShell で一行目（cmd）を実行。
    // 接続された最初の入力の生きた実ファイルを {in} に展開する。§4.2: 種別判定は is_shell() に集約。
    // シェルカード統合（2026-07-06）: window="keep"（既定・開いたまま）/"close"（実行して閉じる）は
    // detached ウィンドウ、"hidden" はウィンドウなしで捕捉して結果パネルへ返す。
    if term_spec.is_shell() {
        let shell = term_spec.shell.as_deref().unwrap_or("").to_string();
        // 短ロック: 接続された最初の入力の生きた実ファイルを解決（起動はロック外）。
        let input: Option<PathBuf> = with_db(state.inner(), |p| {
            Ok(p
                .conn
                .query_row(
                    "SELECT v.id, v.real_id, v.committed_path FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                     WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                     ORDER BY rel.created_at LIMIT 1",
                    rusqlite::params![id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| e.to_string())?
                .map(|(vid, real_id, committed)| {
                    committed
                        .map(PathBuf::from)
                        .filter(|c| c.is_file())
                        .unwrap_or_else(|| resolve_view_path(p, &vid, &real_id))
                }))
        })?;
        let firstline = term_spec.cmd.clone().unwrap_or_default();
        let firstline = match &input {
            Some(pb) => firstline.replace("{in}", &format!("\"{}\"", pb.display())),
            None => firstline.replace("{in}", ""),
        };
        let window = term_spec.window.as_deref().unwrap_or("keep");
        let capture = if window == "hidden" {
            // ウィンドウなし実行: 中断フラグはジョブ別（毎回新品）なのでリセット不要。
            let out = run_shell_captured(
                &shell,
                &firstline,
                &root,
                term_spec.timeout_secs,
                &job.cancel,
            )?;
            // 出力はシェルウィンドウ相当のテキスト。エンコーディングは UTF-8 → 推定
            // （cmd 内蔵コマンドは CP932 を吐く）。パネル表示用に 64K 字で切り詰める。
            let bytes = out.output.unwrap_or_default();
            let (text, _enc) = notes::decode_text_bytes(&bytes);
            const MAX_CHARS: usize = 65536;
            let truncated = text.chars().count() > MAX_CHARS;
            let text = if truncated {
                text.chars().take(MAX_CHARS).collect()
            } else {
                text
            };
            Some(ShellCapture {
                exit_code: out.exit_code,
                output: text,
                truncated,
            })
        } else {
            launch_shell_window(&shell, &firstline, &root, window != "close")?;
            None
        };
        return Ok(FlowReport {
            created: vec![],
            updated: vec![],
            outputs: vec![],
            skipped: vec![],
            errors: vec![],
            shell_capture: capture,
        });
    }

    let prefix = term_spec
        .prefix
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut seq = term_spec.seq.unwrap_or(0);
    let out_dir = term_spec
        .out_dir
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (out_root, external_out) = match &out_dir {
        Some(d) => {
            let pb = PathBuf::from(d);
            if !pb.is_dir() {
                return Err(format!("出力フォルダがありません: {d}"));
            }
            (pb, true)
        }
        None => (root.clone(), false),
    };

    let mut report = FlowReport {
        created: vec![],
        updated: vec![],
        outputs: vec![],
        skipped: vec![],
        errors: vec![],
        shell_capture: None,
    };
    let mut visited = std::collections::HashSet::new();
    // 評価フェーズ: eval_filter が DB 接触ごとに短ロックを取り、実行（外部/AI/録音）は
    // ロック外で走る。録音/AI 中も他のコマンドが state.current を取れる＝UI が固まらない。
    // 中断フラグ・stdin 制御はこのジョブのもの（ジョブ別＝毎回新品なのでリセット不要）。
    let payloads = eval_filter(
        state.inner(),
        &root,
        &id,
        &mut visited,
        &mut report,
        &job.cancel,
        &job.control,
    )?;
    if payloads.is_empty() && report.skipped.is_empty() && report.errors.is_empty() {
        // 自己完結コマンド（{out} を書かないカメラ起動・読み上げ等）は出力ゼロで正常終了する
        // のが正しい。それ以外で空なら入力の繋ぎ忘れとして案内する。
        if !term_spec.is_self_contained() {
            return Err("入力がありません。データカードからフィルタへ矢印を引いてください".into());
        }
    }

    // 出力ファイル名に使えるラベル（Windowsの禁止文字を逃がす）
    let safe_flabel: String = flabel
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(24)
        .collect();

    // マテリアライズ（DB と fs のみ・長時間プロセスなし → 1回の短ロックでまとめて書く）。
    with_db(state.inner(), |p| {
    let mut out_i = 0usize;
    for pl in &payloads {
        // 結果の置き場と名前: prefix＋連番→「prefix連番.ext」（空き番号まで進める）、
        // prefixのみ（連番空欄）→「prefix.ext」固定名で毎回上書き（コンパイラ等）、
        // prefixなし→従来の「入力名_フィルタ名.ext」。out_dir指定なら外部フォルダへ。
        let dest = match (&prefix, term_spec.seq) {
            (Some(px), Some(_)) => loop {
                let cand = out_root.join(format!("{px}{seq:03}.{}", pl.ext));
                seq += 1;
                if !cand.exists() {
                    break cand;
                }
            },
            (Some(px), None) => out_root.join(format!("{px}.{}", pl.ext)),
            (None, _) => {
                unique_file_path(&out_root, &format!("{}_{safe_flabel}.{}", pl.stem, pl.ext))
            }
        };
        if let Err(e) = std::fs::write(&dest, &pl.bytes) {
            report.errors.push(format!("{}: 書き出し失敗 ({e})", pl.stem));
            continue;
        }
        let hash = match cas::store_bytes(&p.cas_root, &pl.bytes) {
            Ok(h) => h,
            Err(e) => {
                report.errors.push(format!("{}: CAS保存失敗 ({e})", pl.stem));
                continue;
            }
        };
        if let Err(e) =
            db::insert_real_body(&p.conn, &hash, &pl.mime, pl.bytes.len() as u64, Some(&pl.ext))
        {
            report.errors.push(format!("{}: {e}", pl.stem));
            continue;
        }
        let out_label = dest
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string();
        let dest_str = dest.to_string_lossy().into_owned();

        // 同じパスを指す生きたカードが既にあれば（固定名上書きの再実行）、
        // 新カードは作らず中身を差し替える — 位置と矢印は維持、緑シグナルで再誕生。
        let existing: Option<String> = p
            .conn
            .query_row(
                "SELECT id FROM virtual_bodies
                 WHERE deleted_at IS NULL AND (committed_path = ?1 OR origin_path = ?1)",
                rusqlite::params![dest_str],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        if let Some(vid) = existing {
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET real_id = ?1, committed_mtime = ?2 WHERE id = ?3",
                rusqlite::params![
                    hash,
                    if external_out { None } else { file_mtime_ms(&dest) },
                    vid
                ],
            );
            // フィルタ→結果の flow 矢印が無ければ張る（重複矢印は作らない）
            let has_rel: bool = p
                .conn
                .query_row(
                    "SELECT 1 FROM relations
                     WHERE from_id = ?1 AND to_id = ?2 AND kind = 'flow' AND deleted_at IS NULL",
                    rusqlite::params![id, vid],
                    |_| Ok(true),
                )
                .optional()
                .unwrap_or(None)
                .unwrap_or(false);
            if !has_rel {
                let _ = p.conn.execute(
                    "INSERT INTO relations (from_id, to_id, kind, created_at)
                     VALUES (?1, ?2, 'flow', ?3)",
                    rusqlite::params![id, vid, db::now_ms()],
                );
            }
            report.updated.push(vid);
            report.outputs.push(format!("{out_label}（上書き）"));
            continue;
        }

        let new_vid = uuid();
        let nx = fx + 190.0;
        let ny = fy + (out_i as f64) * 170.0;
        // 出力フォルダ指定時は外部ファイルへの窓（origin_path の仮身）、
        // 指定なしはプロジェクト直下のコミット済み実身（従来どおり）
        let (committed_path, committed_mtime, origin_path) = if external_out {
            (None, None, Some(dest_str))
        } else {
            (Some(dest_str), file_mtime_ms(&dest), None)
        };
        if let Err(e) = p.conn.execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at,
                 committed_path, committed_mtime, origin_path, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'file')",
            rusqlite::params![
                new_vid,
                hash,
                out_label,
                nx,
                ny,
                db::now_ms(),
                committed_path,
                committed_mtime,
                origin_path
            ],
        ) {
            report.errors.push(format!("{}: {e}", pl.stem));
            continue;
        }
        // フィルタ → 結果 の flow 矢印（パイプラインの連鎖が目に見える）
        let _ = p.conn.execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at)
             VALUES (?1, ?2, 'flow', ?3)",
            rusqlite::params![id, new_vid, db::now_ms()],
        );
        report.created.push(new_vid);
        report.outputs.push(out_label);
        out_i += 1;
    }

    // 連番の永続化: 使った分だけ進めてカードに書き戻す
    // （未知フィールドを保持するため serde_json::Value 経由で seq だけ更新）
    if prefix.is_some() && term_spec.seq.unwrap_or(0) != seq {
        if let Ok(mut v) =
            serde_json::from_str::<serde_json::Value>(fref.as_deref().unwrap_or("{}"))
        {
            v["seq"] = seq.into();
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2",
                rusqlite::params![v.to_string(), id],
            );
        }
    }

    // 実行ログを memo に記録（最後の実行が常に見える — 確定方針4）。
    // v1.3.9: 先頭の ▶ は実行ボタンと紛らわしいので「✓ 前回」に変更（これは結果表示）。
    let mut log = format!("✓ 前回: 出力{}", report.outputs.len());
    if !report.skipped.is_empty() {
        log.push_str(&format!("・スキップ{}", report.skipped.len()));
    }
    if !report.errors.is_empty() {
        log.push_str(&format!("・エラー{}", report.errors.len()));
    }
    let _ = p.conn.execute(
        "UPDATE virtual_bodies SET memo = ?1 WHERE id = ?2",
        rusqlite::params![log, id],
    );
    Ok(())
    })?;

    Ok(report)
}

/// フロー実行の中断（UIの中断ボタン・v1.5.1 WS-B で job_id 引数化）。run_flow が外部コマンドで
/// ストールしていてもこのコマンドは DB の Mutex を取らないため即座に通り、監視ループがツリー
/// ごと kill する。ジョブが既に終わっていれば黙って何もしない（連打・競合は無害）。
#[tauri::command(async)]
pub(crate) fn break_flow(job_id: u64, state: State<AppState>) {
    if let Some(job) = state.jobs.get(job_id) {
        job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // 画像生成中ならデーモン側の生成も止める（GPU を握り続けないように）。best-effort。
    flow::t2i_post_cancel();
}

/// v1.5.1 WS-B: 実行中ジョブの一覧（WS-C 背景ジョブパネルの土台。現 UI は未使用）。
#[tauri::command]
pub(crate) fn list_jobs(state: State<AppState>) -> Vec<JobInfo> {
    state.jobs.list()
}

/// v1.5.1 WS-B: 中央 kill スイッチ（凍結仕様 §12.B・自律安全設計 §6.3 の「最後の物理停止」）。
/// 全ジョブの cancel を立てる。子プロセスは各実行系の監視ループが taskkill /T でツリーごと
/// 止め（既存流用）、画像生成はデーモンへ /cancel を送って次ステップ境界で止める。
#[tauri::command(async)]
pub(crate) fn cancel_all(state: State<AppState>) {
    state.jobs.cancel_all();
    flow::t2i_post_cancel();
}

