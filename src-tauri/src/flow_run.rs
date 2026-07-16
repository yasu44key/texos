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
/// v1.5.3 WS-P1: メモ化（ジョブスコープ）で複数下流に配るため Clone。
#[derive(Clone)]
pub(crate) struct FlowPayload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) ext: String,
    pub(crate) mime: String,
    /// 連鎖の起点データのファイル名語幹（最終出力の命名に使う）
    pub(crate) stem: String,
    /// v1.7 P1: 取得メタ（web_fetch の {source_url, fetched_at, etag, …} JSON）。
    /// 終端でカード化されるとき virtual_bodies.web_meta に保存される（P2 更新マークの材料）。
    /// 中間ノードを経ると付かない（下流の出力は取得物そのものではないため）。
    pub(crate) meta: Option<String>,
}

/// §4.1 リファクタリング: ノード1回分の実行結果。`outputs` は生成物（builtin/ai は1個、
/// 外部は1個、ジェネレータは1個）。`exit_code`/`stderr` は外部プロセスを完走させたときの
/// 情報で、現状 eval_filter は使わない — 将来の exit code 分岐（§4.3「成功時/失敗時」の
/// flow 矢印）でここを見るための土台アンカー。
pub(crate) struct NodeRun {
    // (bytes, ext, mime, stem_override, meta)。stem_override は v1.6 {out*} 複数出力のとき
    // プログラムが付けたファイル名の語幹（＝カード名になる）。None なら呼び出し側の
    // 従来命名（入力 stem / prefix / "output"）。meta は v1.7 web_fetch の取得メタ JSON
    // （FlowPayload::meta へそのまま流れる。web_fetch 以外は None）。
    outputs: Vec<(Vec<u8>, String, String, Option<String>, Option<String>)>,
    #[allow(dead_code)] // §4.3 で消費予定
    exit_code: Option<i32>,
    #[allow(dead_code)] // §4.3 で消費予定
    stderr: String,
}

/// §4.1 実行ディスパッチの一本道: 「ノード種別 → 実行 → 結果（payload + exit code + stderr）」。
/// 種別判定（flow::NodeKind）に応じて内蔵フィルタ / AI / 外部プロセスへ振り分ける。
/// 入力の収集・型チェック・命名（stem）は呼び出し側（eval_filter）の責務で、ここは
/// 「与えられた入力でこの spec を1回実行する」ことだけに専念する（データ/フローの関心を分離）。
/// v1.5.2 WS-C: cancel/stdin_slot 個別渡しを Job 渡しへ（t2i_active の点灯もここで行う）。
///
/// inputs は (bytes, ext, mime) の並び:
/// - Generator … 空（入力を取らない）
/// - Builtin / Ai … 必ず1個（v1.5.17: 集約系 builtin＝text_concat/diff/paste は N 個）
/// - External … ファンアウト時1個 / 集約時 N 個
pub(crate) fn execute_node(
    root: &Path,
    spec: &flow::FilterSpec,
    kind: flow::NodeKind,
    inputs: &[(&[u8], &str, &str)],
    job: &Job,
) -> Result<NodeRun, String> {
    use flow::NodeKind;
    let cancel = &job.cancel;
    let stdin_slot = &job.control;
    let plain = |bytes: Vec<u8>, ext: String, mime: String| NodeRun {
        outputs: vec![(bytes, ext, mime, None, None)],
        exit_code: None,
        stderr: String::new(),
    };
    match kind {
        NodeKind::Builtin => {
            // builtin 名は node_kind が Builtin と判定した時点で Some（"ai" 以外）。
            let b = spec.builtin.as_deref().unwrap_or("");
            // v1.5.17 P2: 集約系 builtin は全入力を1回で受ける（eval_filter が集約経路で
            // N 個渡してくる。並びは port 昇順 → 接続順）。
            if flow::builtin_is_aggregate(b) {
                let ins: Vec<&[u8]> = inputs.iter().map(|i| i.0).collect();
                let (bytes, ext, mime) = flow::run_builtin_agg(spec, b, &ins)?;
                Ok(plain(bytes, ext, mime))
            }
            // v1.5.15 L2 OSアダプタ: speak は fire-and-forget（出力カードなし）。
            // {out} を書かない自己完結コマンドと同じ契約 = 成功しても outputs は空。
            else if b == "speak" {
                flow::run_speak(inputs[0].0)?;
                Ok(NodeRun {
                    outputs: Vec::new(),
                    exit_code: None,
                    stderr: String::new(),
                })
            }
            // v1.7 P1: web_fetch はネットワークを使う別経路（AI と同系・同期 blocking）。
            // 入力なし（パラメータ欄 URL）も許すので inputs は Option で渡す。
            // 語幹（ページタイトル）とメタ JSON を返す拡張契約のため run_builtin に乗せない。
            else if b == "web_fetch" {
                let r = web::run_web_fetch(spec, inputs.first().map(|i| i.0))?;
                // AI と同じ「中断＝完走後に破棄」の契約（HTTP は途中で切れない）。
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("中断されました".into());
                }
                Ok(NodeRun {
                    outputs: vec![(r.bytes, r.ext, r.mime, r.stem, Some(r.meta))],
                    exit_code: None,
                    stderr: String::new(),
                })
            } else {
                let (bytes, ext, mime) = flow::run_builtin(spec, b, inputs[0].0)?;
                Ok(plain(bytes, ext, mime))
            }
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
            // v1.5.2 WS-C: 生成 HTTP 中だけ t2i_active を点灯 — break_flow はこれを見て
            // デーモンへ /cancel を送る（並行時に他ジョブの生成を誤って止めない）。
            job.t2i_active.store(true, std::sync::atomic::Ordering::Relaxed);
            let run = flow::run_t2i(spec, prompt, init);
            job.t2i_active.store(false, std::sync::atomic::Ordering::Relaxed);
            let (bytes, ext, mime) = run?;
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
            } else if o.multi.is_some() {
                // v1.6 {out*}: 複数出力モード。成否ポリシーは単一出力と同じ（非0→エラー）。
                // 各ファイルは自分の拡張子で ext/MIME を決め、ファイル名の語幹がカード名になる
                // （ChillChordMaker の「jazz_01_1_Dm7.wav」のような意味を持つ名前を活かす）。
                if !o.success() {
                    return Err(o.into_bytes().unwrap_err());
                }
                let multi = o.multi.unwrap_or_default();
                if multi.is_empty() {
                    return Err(
                        "コマンドは成功しましたが {out*} フォルダに出力がありません".into()
                    );
                }
                multi
                    .into_iter()
                    .map(|(fname, bytes)| {
                        let p = std::path::Path::new(&fname);
                        let ext = p
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_ascii_lowercase())
                            .unwrap_or_else(|| out_ext.clone());
                        let mime = mime_for_ext(&ext)
                            .unwrap_or("application/octet-stream")
                            .to_string();
                        let stem = p
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("output")
                            .to_string();
                        (bytes, ext, mime, Some(stem), None)
                    })
                    .collect()
            } else {
                vec![(o.into_bytes()?, out_ext, out_mime, None, None)]
            };
            Ok(NodeRun {
                outputs,
                exit_code,
                stderr,
            })
        }
        // セレクタは eval_filter 側で完結する（first-match・実行しない）。ここへ来るのは
        // 呼び出しバグなので防御的にエラー。
        NodeKind::Select => Err("セレクタは実行ノードではありません".into()),
        NodeKind::Invalid => Err("フィルタ定義に builtin も cmd もありません".into()),
    }
}

/// ノードの必要資源を導出して permit を取得する（凍結 §5・§6。WS-C）。
/// 取得順序は device→ai→heavy に固定（§12.D）。permit はノード実行中だけ保持し、
/// 返り値の Vec が drop されると解放される。排他資源（device/ai）は即拒否＋案内、
/// heavy は FIFO 待ち（待機中も job.cancel が効く・待ち始めに job:progress を1回出す）。
fn acquire_node_permits(
    state: &AppState,
    app: &tauri::AppHandle,
    job: &Job,
    spec: &flow::FilterSpec,
    kind: flow::NodeKind,
) -> Result<Vec<Permit>, String> {
    use flow::NodeKind;
    let mut permits = Vec::new();
    // 1) device（録音ジェネレータ）: dshow デバイス名で排他。
    if kind == NodeKind::Generator && spec.record {
        let name = flow::dshow_audio_device(spec.cmd.as_deref().unwrap_or(""))
            .unwrap_or_else(|| "既定デバイス".to_string());
        let sem = state.resources.device_sem(&name);
        permits.push(sem.try_acquire().ok_or_else(|| {
            format!(
                "録音デバイス「{name}」は使用中です。実行中の録音を「停止」してからもう一度実行してください"
            )
        })?);
    }
    // 2) ai（AI/T2i）: 正規化エンドポイント単位（既定許可数1）。
    let endpoint = match kind {
        NodeKind::Ai => Some(flow::effective_ai_endpoint(spec)),
        NodeKind::T2i => Some(format!("http://127.0.0.1:{}", flow::T2I_PORT)),
        _ => None,
    };
    if let Some(ep) = endpoint {
        let key = normalize_endpoint(&ep);
        let sem = state.resources.ai_sem(&key);
        permits.push(sem.try_acquire().ok_or_else(|| {
            format!(
                "AIエンドポイント（{key}）は使用中です。実行中のAI処理・画像生成の完了を待つか中断してください"
            )
        })?);
    }
    // 3) heavy（外部プロセス）: 上限まで並行・超過は順番待ち。録音ジェネレータは
    //    デバイス排他で守られる長時間ジョブなので heavy 枠を消費しない。
    if kind == NodeKind::External || (kind == NodeKind::Generator && !spec.record) {
        let p = state.resources.heavy.acquire(&job.cancel, || {
            emit_job_progress(app, job, Some("順番待ち…"), None);
        })?;
        permits.push(p);
    }
    Ok(permits)
}

/// v1.7 P2: 取得系譜（URLカード→取得実身・relations kind='fetched'）。
/// 終端の出力に取得メタ（web_fetch の {source_url, …}）が付いていたら、終端フィルタへ
/// 直結している URLカードのうち url_ref が source_url と一致するものから、この実身へ
/// 系譜を張り替える（最新のみ表示 §7-3・実処理は web::record_fetch_lineage）。
/// 中間ノードを経た出力はメタが付かないので、ここには来ない（取得物そのものだけが系譜を持つ）。
fn link_fetch_source(p: &Project, fid: &str, meta: &str, target: &str) {
    let Some(src) = serde_json::from_str::<serde_json::Value>(meta)
        .ok()
        .and_then(|v| v.get("source_url").and_then(|s| s.as_str()).map(str::to_string))
    else {
        return;
    };
    let Ok(mut stmt) = p.conn.prepare(
        "SELECT v.id FROM relations rel
         JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
         WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
           AND TRIM(v.url_ref) = TRIM(?2)",
    ) else {
        return;
    };
    let url_cards: Vec<String> = stmt
        .query_map(rusqlite::params![fid, src], |r| r.get(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    for url_card in url_cards {
        crate::web::record_fetch_lineage(&p.conn, &url_card, target);
    }
}

/// job:progress の note 用にカード名を引く（best-effort・パネルの「いま何が走っているか」）。
fn node_note(p: &Project, fid: &str) -> String {
    p.conn
        .query_row(
            "SELECT label FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![fid],
            |r| r.get::<_, String>(0),
        )
        .map(|l| format!("実行中: {l}"))
        .unwrap_or_else(|_| "実行中…".to_string())
}

// --- v1.5.3 WS-P1 オペレーション層 Phase1（オペ層設計 §5.4.2）------------------------
// eval_filter の結果を「payload の列」から一級の評価結果 EvalOut へ。when 条件（成功時/
// 失敗時）・no-data・メモ化・セレクタがこの上に載る。relations.attrs（NULL=従来）が
// エッジの意味論を決める＝§4.3 拡張点アンカーの成立形。

/// ノード1回の評価結果（ジョブ内で揮発・メモ化で共有）。
#[derive(Clone)]
pub(crate) struct EvalOut {
    pub(crate) payloads: Vec<FlowPayload>,
    /// no-data 一級（オペ層 §5.1-4）: 枝が取られなかった／材料が揃わなかった。
    /// エラーでも空ファイルでもない第三の結果。伝搬型ノードはこれを NULL 伝搬する。
    pub(crate) nodata: bool,
    /// このノードの実行がエラーなしで完走したか（when="success"/"failure" が読む）。
    pub(crate) ok: bool,
    /// 外部ノードの exit code（完走できた場合のみ。内蔵/AI/データカードは None）。
    pub(crate) exit: Option<i32>,
    /// このノードで起きた実行エラー。**report へは直接流さない** — 消費側エッジが
    /// when=always なら従来どおり report.errors へ（後方互換）、when 付きなら
    /// 経路選択の情報として黙って分岐する（失敗の格下げ・§5.4.2）。
    pub(crate) errors: Vec<String>,
    /// 失敗時の入力スナップショット（carry:"input" のリトライ材料。失敗時のみ保持）。
    pub(crate) failed_inputs: Vec<FlowPayload>,
}

impl EvalOut {
    pub(crate) fn data(payloads: Vec<FlowPayload>, exit: Option<i32>) -> Self {
        EvalOut { payloads, nodata: false, ok: true, exit, errors: vec![], failed_inputs: vec![] }
    }
    fn nodata_out() -> Self {
        EvalOut { payloads: vec![], nodata: true, ok: true, exit: None, errors: vec![], failed_inputs: vec![] }
    }
    fn failed(errors: Vec<String>, exit: Option<i32>, failed_inputs: Vec<FlowPayload>) -> Self {
        EvalOut { payloads: vec![], nodata: false, ok: false, exit, errors, failed_inputs }
    }
}

/// ジョブスコープのメモ表（§5.1-5）: 複数下流が同じカードを pull しても実行は1回。
/// 副作用のある上流（録音・AI 呼び出し）の二重実行を防ぐ。run_flow / チャット1ターンごと。
pub(crate) type EvalMemo = std::collections::HashMap<String, EvalOut>;

/// 構造化実行記録 last_run の保存（§6.1: {at, exit, ok, nodata, skipped, message}）。
/// message = このノードで起きたエラー文言（一級の語彙 — メタ認知カードの入力になる）。
fn write_last_run(p: &Project, fid: &str, out: &EvalOut, skipped: u32) {
    let v = serde_json::json!({
        "at": db::now_ms(),
        "exit": out.exit,
        "ok": out.ok,
        "nodata": out.nodata,
        "skipped": skipped,
        "message": if out.errors.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(out.errors.join("\n"))
        },
    });
    let _ = p.conn.execute(
        "UPDATE virtual_bodies SET last_run = ?1 WHERE id = ?2",
        rusqlite::params![v.to_string(), fid],
    );
}

/// 評価の終端処理: visited を抜け・last_run を保存・メモ表へ登録して返す。
fn finish_eval(
    p: &Project,
    fid: &str,
    visited: &mut std::collections::HashSet<String>,
    memo: &mut EvalMemo,
    skipped: u32,
    out: EvalOut,
) -> Result<EvalOut, String> {
    visited.remove(fid);
    write_last_run(p, fid, &out, skipped);
    memo.insert(fid.to_string(), out.clone());
    Ok(out)
}

/// エッジ1本の収集結果（フェーズ1）。nodata=枝が取られなかった（NULL伝搬の種）。
/// failure=失敗時矢印（when:"failure"）で取られた枝（v1.5.7 セレクタのエラー時メッセージ用）。
pub(crate) struct EdgeOut {
    pub(crate) payloads: Vec<FlowPayload>,
    pub(crate) nodata: bool,
    pub(crate) failure: bool,
}

/// when/carry をエッジに適用する（§5.4.2）。上流の no-data は when に関わらず no-data
/// （NULL伝搬）。always エッジだけが上流エラーを report へ流す＝失敗の格下げの実装点。
pub(crate) fn apply_edge(
    up: &EvalOut,
    attrs: &flow::EdgeAttrs,
    vlabel: &str,
    report: &mut FlowReport,
) -> EdgeOut {
    if up.nodata {
        return EdgeOut { payloads: vec![], nodata: true, failure: false };
    }
    let when = attrs.when.as_deref().unwrap_or("always");
    let taken = match when {
        "success" => up.ok,
        "failure" => !up.ok,
        _ => {
            // always（従来）: 上流エラーはフロー全体のエラーとして伝搬（後方互換）。
            for e in &up.errors {
                report.errors.push(e.clone());
            }
            true
        }
    };
    if !taken {
        return EdgeOut { payloads: vec![], nodata: true, failure: false };
    }
    // 運ぶもの: 成功枝の既定=上流出力／失敗枝の既定=エラーメッセージ（stderr・§6.1 一級）。
    let carry = attrs
        .carry
        .as_deref()
        .unwrap_or(if when == "failure" { "stderr" } else { "output" });
    let payloads = match carry {
        "none" => vec![], // 信号のみ（エッジ属性版ゲート）。no-data ではない＝枝は取られた
        "stderr" => {
            let msg = if up.errors.is_empty() {
                "（エラーメッセージなし）".to_string()
            } else {
                up.errors.join("\n")
            };
            vec![FlowPayload {
                bytes: msg.into_bytes(),
                ext: "txt".into(),
                mime: "text/plain".into(),
                stem: format!("{vlabel}のエラー"),
                meta: None,
            }]
        }
        "input" => up.failed_inputs.clone(),
        _ => up.payloads.clone(),
    };
    EdgeOut { payloads, nodata: false, failure: when == "failure" }
}

/// フィルタを再帰評価する（pull型連鎖 — 実行モデル3）。
/// 入力にフィルタカードが繋がっていれば先にそれを評価し、結果をこのフィルタに通す。
/// `visited` は評価中パスのスタックで、循環を検出したらエラー。
/// v1.5.2 WS-C: DB 接触は**ジョブ専用接続**（p）で行う（state.current 非依存）。
/// v1.5.3 WS-P1: 返り値を EvalOut に（when/no-data/セレクタ/メモ化はこの関数が担う）。
/// v1.6 「新規カードを登録」の本体。spec.path のフォルダ（空=プロジェクト直下）を走査し、
/// 未登録ファイルをカードとして登録する。プロジェクト内=実身／外部=窓仮身
/// （import_file_into の in_project 判定）。新カードは本カードの下にグリッドで並ぶ。
/// - 既知判定は committed_path と origin_path の**両方**（フロー出力の実身も窓仮身も重複しない）
/// - 一時・隠しファイル（先頭 ~ / . ・拡張子 tmp/part/crdownload/bak）はスキップ
/// - 判定は登録の直前に行い、並行発火の競合窓を最小化する
fn run_import_new(
    p: &Project,
    fid: &str,
    spec: &flow::FilterSpec,
    report: &mut FlowReport,
) -> Result<usize, String> {
    let dir: PathBuf = match spec.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => p.root.clone(),
    };
    if !dir.is_dir() {
        return Err(format!("フォルダがありません: {}", dir.display()));
    }
    // 新カードの並べ先（本カードの位置を起点に下へグリッド）
    let (fx, fy): (f64, f64) = p
        .conn
        .query_row(
            "SELECT pos_x, pos_y FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![fid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    let texos = texos_dir(&p.root);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|q| q.is_file())
        .collect();
    files.sort(); // 登録順を安定に（ファイル名順 = {out*} と同じ順序規約）

    let mut count = 0usize;
    for q in files {
        if path_inside(&q, &texos) {
            continue; // .texos 配下は対象外（フラット運用の原則）
        }
        let name = match q.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('~') || name.starts_with('.') {
            continue; // 隠し・Office ロックファイル等
        }
        let ext = q
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(ext.as_str(), "tmp" | "part" | "crdownload" | "bak") {
            continue; // 書き込み途中・一時ファイルの流儀
        }
        let path_str = q.to_string_lossy().to_string();
        // 既知判定（登録直前）: 実身（committed_path）も窓仮身（origin_path）も重複させない
        let known: bool = p
            .conn
            .query_row(
                "SELECT 1 FROM virtual_bodies
                 WHERE (committed_path = ?1 OR origin_path = ?1) AND deleted_at IS NULL",
                rusqlite::params![path_str],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .is_some();
        if known {
            continue;
        }
        let cx = fx + (count % 5) as f64 * 170.0;
        let cy = fy + 150.0 + (count / 5) as f64 * 140.0;
        match objects::import_file_into(p, path_str, String::new(), cx, cy) {
            Ok(_vid) => {
                report.outputs.push(format!("{name}（登録）"));
                count += 1;
            }
            Err(e) => report.errors.push(format!("{name}: {e}")),
        }
    }
    Ok(count)
}

pub(crate) fn eval_filter(
    state: &AppState,
    app: &tauri::AppHandle,
    p: &Project,
    fid: &str,
    visited: &mut std::collections::HashSet<String>,
    memo: &mut EvalMemo,
    report: &mut FlowReport,
    job: &Job,
) -> Result<EvalOut, String> {
    // メモ化（§5.1-5）: 同一ジョブ内の再 pull は実行せず前回結果を配る。
    if let Some(hit) = memo.get(fid) {
        return Ok(hit.clone());
    }
    let root: &Path = &p.root;
    let cancel = &job.cancel;
    if !visited.insert(fid.to_string()) {
        return Err("フローに循環があります".into());
    }
    // v1.5.6 WS-M M1: IN/OUT バーチャルカード（kind='op'・op:"in"/"out"）も評価対象
    // （素通しノード＝入力をそのまま返す）。トリガ/タイマ/ウォッチャは req レーンなので
    // ここには来ない（来ても Invalid 定義エラーで安全に落ちる）。
    let spec_json: Option<String> = p
        .conn
        .query_row(
            "SELECT filter_ref FROM virtual_bodies
             WHERE id = ?1 AND kind IN ('filter','op') AND deleted_at IS NULL",
            rusqlite::params![fid],
            |r| r.get(0),
        )
        .map_err(|_| "フィルタカードが見つかりません".to_string())?;
    let passthrough = ops::is_io_op_spec(spec_json.as_deref().unwrap_or("{}"));
    let spec = if passthrough {
        flow::parse_spec("{}")? // 素通しノードに FilterSpec の意味は無い（既定値で安全に）
    } else {
        flow::parse_spec(spec_json.as_deref().unwrap_or("{}"))?
    };
    let accept = spec.accept_mime.clone().unwrap_or_else(|| "*/*".into());

    // ジェネレータ型: 入力を取らず1回実行してファイルを生む（入力矢印は無視）。
    // §4.1: 実行は execute_node に一任し、ここは命名（stem＝prefix or "output"）だけ担う。
    if spec.node_kind() == flow::NodeKind::Generator {
        let run = acquire_node_permits(state, app, job, &spec, flow::NodeKind::Generator)
            .and_then(|_permits| {
                emit_job_progress(app, job, Some(&node_note(p, fid)), None);
                execute_node(root, &spec, flow::NodeKind::Generator, &[], job)
            });
        let out = match run {
            Ok(r) => {
                let stem = spec
                    .prefix
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "output".into());
                let exit = r.exit_code;
                EvalOut::data(
                    r.outputs
                        .into_iter()
                        .map(|(bytes, ext, mime, stem_o, meta)| FlowPayload {
                            bytes,
                            ext,
                            mime,
                            stem: stem_o.unwrap_or_else(|| stem.clone()),
                            meta,
                        })
                        .collect(),
                    exit,
                )
            }
            Err(e) => EvalOut::failed(vec![e], None, vec![]),
        };
        return finish_eval(p, fid, visited, memo, 0, out);
    }

    // v1.6 「新規カードを登録」（builtin:"import_new"）: 入力を取らずフォルダを走査して
    // 未登録ファイルをカード登録する（セレクタと同じ「eval 側で完結」の流儀 — 登録は
    // プロジェクト DB への作用なので execute_node のバイト変換契約に乗せない）。
    // ウォッチャ→本カードの req 矢印で「随時登録」パターンが成立する。
    if spec.builtin.as_deref() == Some("import_new") {
        emit_job_progress(app, job, Some(&node_note(p, fid)), None);
        let out = match run_import_new(p, fid, &spec, report) {
            Ok(_n) => EvalOut::data(vec![], None),
            Err(e) => EvalOut::failed(vec![e], None, vec![]),
        };
        return finish_eval(p, fid, visited, memo, 0, out);
    }

    // 入力: このフィルタへ入る flow 矢印（生きているカードのみ）。
    // v1.5.3 WS-P1（§4.3 拡張点アンカーの成立形）: rel.attrs {when, carry, port} を読み、
    // 並びは port 昇順 → created_at（attrs=NULL は従来どおり接続順）。{inN} の番号・
    // セレクタの first-match 順はこの並びに従う。
    #[allow(clippy::type_complexity)]
    let mut inputs: Vec<(String, String, String, String, Option<String>, Option<String>, Option<String>, flow::EdgeAttrs)> = {
        let mut stmt = p
            .conn
            .prepare(
                "SELECT v.id, v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext), v.kind, v.url_ref, rel.attrs
                 FROM relations rel
                 JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                 JOIN real_bodies r ON r.id = v.real_id
                 WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                 ORDER BY rel.created_at",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![fid], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(vid, rid, l, m, e, k, u, attrs): (String, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>)| {
                (vid, rid, l, m, e, k, u, flow::parse_edge_attrs(attrs.as_deref()))
            })
            .collect()
    };
    // 安定ソート: port 指定なしは末尾（created_at 順を保つ＝完全後方互換）。
    inputs.sort_by_key(|(.., a)| a.port.unwrap_or(i64::MAX));

    let mut produced = Vec::new();
    // フェーズ1: エッジごとに上流を評価し when/carry を適用（EdgeOut の列で保持＝
    // セレクタの first-match と no-data の NULL 伝搬はエッジ単位の情報が要る）。
    let mut edges: Vec<EdgeOut> = Vec::new();
    for (vid, real_id, vlabel, mime, ext, kind, url_ref, attrs) in &inputs {
        // 中断後は新しいプロセスを起こさない（複数入力の連鎖を断つ）
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            report.errors.push("中断されました".into());
            break;
        }
        // 上流の評価結果（EvalOut）を得る: フィルタ＝再帰評価（メモ化）／データカード＝窓の今
        // v1.5.6: 上流が IN/OUT バーチャルカード（kind='op'）なら再帰評価＝透過。
        // req レーンの op（トリガ等）は flow 矢印を持たないのでここには現れない。
        let up: EvalOut = if matches!(kind.as_deref(), Some("filter") | Some("op")) {
            match eval_filter(state, app, p, vid, visited, memo, report, job) {
                Ok(v) => v,
                // 構造エラー（循環・カード不明）も「失敗」として一級化 — when=failure で拾える。
                Err(e) => EvalOut::failed(vec![format!("{vlabel}: {e}")], None, vec![]),
            }
        } else if real_id.starts_with("drop:") {
            // ドロップポイント未投入は従来どおり「スキップ」（no-data にはしない＝後方互換。
            // 伝搬型ノードを止めたければ将来 no-data 化を検討 — 申し送り参照）。
            report
                .skipped
                .push(format!("{vlabel}（まだファイルが投入されていません）"));
            continue;
        } else if let Some(u) = url_ref.as_deref() {
            // v1.7 P2（URLカードの仮身昇格・実行契約）: kind='url' の入力は url_ref 文字列を
            // text/plain として渡す。web_fetch の入力（1行目=URL）にそのまま乗るほか、
            // テキスト系フィルタにも普通に繋がる。stem はラベルをファイル名安全に整形
            // （ラベルが URL そのままでも壊れない）。
            let stem = crate::web::sanitize_stem(vlabel).unwrap_or_else(|| "URL".into());
            EvalOut::data(
                vec![FlowPayload {
                    bytes: u.as_bytes().to_vec(),
                    ext: "txt".into(),
                    mime: "text/plain".into(),
                    stem,
                    meta: None,
                }],
                None,
            )
        } else if kind.as_deref() == Some("note") {
            // v1.4.17 カード統一＝定数カード: 付箋は本文をそのまま text/plain として供給する
            //（実体・CAS 非依存の読み出し専用テキスト。付箋→AI/t2i/grep がそのまま繋がる）。
            match p
                .conn
                .query_row(
                    "SELECT note_text FROM virtual_bodies WHERE id = ?1",
                    rusqlite::params![vid],
                    |r| Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default()),
                )
                .map_err(|e| e.to_string())
            {
                Ok(text) => {
                    let stem = if vlabel.trim().is_empty() {
                        "コメント".to_string()
                    } else {
                        vlabel.clone()
                    };
                    EvalOut::data(
                        vec![FlowPayload {
                            bytes: text.into_bytes(),
                            ext: "txt".into(),
                            mime: "text/plain".into(),
                            stem,
                            meta: None,
                        }],
                        None,
                    )
                }
                Err(e) => EvalOut::failed(vec![format!("{vlabel}: {e}")], None, vec![]),
            }
        } else {
            // 窓の今を解決（snapshot 更新＋ビューパス）— ジョブ専用接続なのでロック不要。
            let path = {
                refresh_origin_snapshot(p, vid);
                resolve_view_path(p, vid, real_id)
            };
            match std::fs::read(&path) {
                Ok(d) => {
                    let stem = std::path::Path::new(vlabel)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("output")
                        .to_string();
                    EvalOut::data(
                        vec![FlowPayload {
                            bytes: d,
                            ext: ext.clone().unwrap_or_else(|| "bin".into()),
                            mime: mime.clone(),
                            stem,
                            meta: None,
                        }],
                        None,
                    )
                }
                Err(e) => {
                    EvalOut::failed(vec![format!("{vlabel}: 読み込み失敗 ({e})")], None, vec![])
                }
            }
        };
        edges.push(apply_edge(&up, attrs, vlabel, report));
    }

    // セレクタ（§5.4.1）: 実行せず「最初に有データを返した枝」を採用。no-data 吸収を内包し、
    // 全滅なら自分が no-data（エラーではない — 下流やレポートが no-data として扱う）。
    // v1.5.7 セレクタ設定: 採用後に (1) 失敗枝ならエラー時メッセージへ差し替え（fail_text）、
    // (2) 出力名（prefix）で stem/拡張子を宣言名に差し替え、してから下流へ流す。
    if spec.node_kind() == flow::NodeKind::Select {
        let out = match edges.iter().find(|e| !e.nodata && !e.payloads.is_empty()) {
            Some(e) => {
                let mut pls = e.payloads.clone();
                if e.failure {
                    if let Some(msg) =
                        spec.fail_text.as_deref().map(str::trim).filter(|s| !s.is_empty())
                    {
                        let stem = pls
                            .first()
                            .map(|pl| pl.stem.clone())
                            .unwrap_or_else(|| "エラー".into());
                        pls = vec![FlowPayload {
                            bytes: msg.as_bytes().to_vec(),
                            ext: "txt".into(),
                            mime: "text/plain".into(),
                            stem,
                            meta: None,
                        }];
                    }
                }
                if let Some(name) =
                    spec.prefix.as_deref().map(str::trim).filter(|s| !s.is_empty())
                {
                    let (stem, ext) = flow::split_output_name(name);
                    for pl in &mut pls {
                        pl.stem = stem.to_string();
                        if let Some(x) = ext {
                            pl.ext = x.to_string();
                        }
                    }
                }
                EvalOut::data(pls, None)
            }
            None => EvalOut::nodata_out(),
        };
        return finish_eval(p, fid, visited, memo, 0, out);
    }

    // no-data の NULL 伝搬（§5.1-4）: 伝搬型（既定）は入力エッジに1つでも no-data が
    // あれば自分も no-data（実行しない）。吸収型（nodata:"absorb"）は欠けを落として続行。
    if !spec.absorbs_nodata() && edges.iter().any(|e| e.nodata) {
        return finish_eval(p, fid, visited, memo, 0, EvalOut::nodata_out());
    }
    let all_nodata = !edges.is_empty() && edges.iter().all(|e| e.nodata);
    if all_nodata {
        // 吸収型でも材料が全滅なら no-data（実行するものがない）。
        return finish_eval(p, fid, visited, memo, 0, EvalOut::nodata_out());
    }
    let collected: Vec<FlowPayload> = edges
        .into_iter()
        .filter(|e| !e.nodata)
        .flat_map(|e| e.payloads)
        .collect();

    // v1.5.6 WS-M M1（§9.4）: IN/OUT バーチャルカードは素通し＝入力をそのまま返す。
    // 型チェックも実行も無し。入力が無ければ no-data（未接続の口＝材料なし）。
    // v1.5.7: シェルカードも連鎖評価では素通し（実行ノードではない — ウィンドウを開くのは
    // run_flow の終端分岐だけ）。これで run_flow がシェル終端から eval_filter(自ID) を呼ぶと
    // 上流の採用ペイロードがそのまま返り、トリガ発行→シェルで上流全体が pull で動く。
    if passthrough || spec.is_shell() {
        let out = if collected.is_empty() {
            EvalOut::nodata_out()
        } else {
            EvalOut::data(collected, None)
        };
        return finish_eval(p, fid, visited, memo, 0, out);
    }

    // フェーズ2: 実行。種別ごとの分岐は execute_node へ一本化（§4.1）。ここに残るのは
    // 「型チェック（受け入れMIME・想定拡張子）」と「集約/ファンアウトの入力の束ね方」だけ。
    // 集約か個別かは CMD の書き方で決まる（{in*} か {in2} 以降があれば全入力を1回 = 集約:
    // gcc分割ソース・画像合成 / {in}/{in1} のみなら入力ごとに実行 = 従来のファンアウト）。
    // v1.5.3 WS-P1: 実行エラーは report へ直接流さず errs に持ち上げる（消費側エッジが
    // when=always のときだけ report へ＝失敗の格下げ）。型スキップは従来どおり report.skipped。
    let kind = spec.node_kind();
    let mut errs: Vec<String> = Vec::new();
    let mut skips: u32 = 0;
    let mut last_exit: Option<i32> = None;
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
            errs.extend(bad);
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
            let run = acquire_node_permits(state, app, job, &spec, kind).and_then(|_permits| {
                emit_job_progress(app, job, Some(&node_note(p, fid)), None);
                execute_node(root, &spec, kind, &ins, job)
            });
            match run {
                Ok(run) => {
                    last_exit = run.exit_code;
                    for (bytes, ext, mime, stem_o, meta) in run.outputs {
                        let stem = stem_o.unwrap_or_else(|| stem0.clone());
                        produced.push(FlowPayload { bytes, ext, mime, stem, meta });
                    }
                }
                Err(e) => errs.push(format!("{stem0}: {e}")),
            }
        }
    } else {
        // v1.7 P1 web_fetch: 入力接続が無くてもパラメータ欄の URL だけで実行できる
        // （t2i のパネル単独生成と同型）。入力があるときは従来のファンアウト＝1入力1URL。
        if collected.is_empty() && spec.builtin.as_deref() == Some("web_fetch") {
            let run = acquire_node_permits(state, app, job, &spec, kind).and_then(|_permits| {
                emit_job_progress(app, job, Some(&node_note(p, fid)), None);
                execute_node(root, &spec, kind, &[], job)
            });
            match run {
                Ok(run) => {
                    last_exit = run.exit_code;
                    for (bytes, ext, mime, stem_o, meta) in run.outputs {
                        produced.push(FlowPayload {
                            bytes,
                            ext,
                            mime,
                            stem: stem_o.unwrap_or_else(|| "web".into()),
                            meta,
                        });
                    }
                }
                // URL 未設定の案内（run_web_fetch のエラー文言）もここを通って表示される。
                Err(e) => errs.push(e),
            }
        }
        for pl in &collected {
            if !flow::mime_matches(&accept, &pl.mime) {
                report
                    .skipped
                    .push(format!("{}（{} は受け入れ対象外）", pl.stem, pl.mime));
                skips += 1;
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
                    skips += 1;
                    continue;
                }
            }
            let ins = [(pl.bytes.as_slice(), pl.ext.as_str(), pl.mime.as_str())];
            let run = acquire_node_permits(state, app, job, &spec, kind).and_then(|_permits| {
                emit_job_progress(app, job, Some(&node_note(p, fid)), None);
                execute_node(root, &spec, kind, &ins, job)
            });
            match run {
                Ok(run) => {
                    last_exit = run.exit_code;
                    for (bytes, ext, mime, stem_o, meta) in run.outputs {
                        produced.push(FlowPayload {
                            bytes,
                            ext,
                            mime,
                            stem: stem_o.unwrap_or_else(|| pl.stem.clone()),
                            meta,
                        });
                    }
                }
                Err(e) => errs.push(format!("{}: {e}", pl.stem)),
            }
        }
    }
    let out = if errs.is_empty() {
        EvalOut::data(produced, last_exit)
    } else {
        // 失敗: 入力スナップショットを carry:"input" 用に保持（リトライ材料）。
        // 部分成功（ファンアウトの一部だけ失敗）も ok=false 扱い — 成功分 payloads は
        // 残す（always 枝は従来どおり両方見える。§5.4.2 の when は全体成否で読む）。
        EvalOut {
            payloads: produced,
            nodata: false,
            ok: false,
            exit: last_exit,
            errors: errs,
            failed_inputs: collected,
        }
    };
    finish_eval(p, fid, visited, memo, skips, out)
}

#[cfg(test)]
mod ws_p1_tests {
    use super::*;

    fn payload(stem: &str, bytes: &[u8]) -> FlowPayload {
        FlowPayload {
            bytes: bytes.to_vec(),
            ext: "txt".into(),
            mime: "text/plain".into(),
            stem: stem.into(),
            meta: None,
        }
    }
    fn empty_report() -> FlowReport {
        FlowReport {
            created: vec![],
            updated: vec![],
            outputs: vec![],
            skipped: vec![],
            errors: vec![],
            shell_capture: None,
        }
    }
    fn attrs(json: &str) -> flow::EdgeAttrs {
        flow::parse_edge_attrs(Some(json))
    }

    /// v1.6 「新規カードを登録」: 未登録ファイルだけ登録・一時/隠しファイルはスキップ・
    /// 2回目は重複ゼロ（committed_path 照合）・登録カードはプロジェクト内=実身。
    #[test]
    fn import_new_registers_and_dedups() {
        let root = std::env::temp_dir().join(format!("texos_import_new_{}", db::now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let p = project::open_project_at(&root).unwrap();
        // 本カード（位置の起点になるだけ）
        p.conn
            .execute(
                "INSERT INTO real_bodies (id, mime_type, size, created_at)
                 VALUES ('filter:sentinel', 'application/vnd.texos.filter', 0, 0)",
                [],
            )
            .unwrap();
        p.conn
            .execute(
                "INSERT INTO virtual_bodies
                    (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
                 VALUES ('f1', 'filter:sentinel', '登録', 100.0, 100.0, 0, 'filter',
                         '{\"builtin\":\"import_new\"}')",
                [],
            )
            .unwrap();
        // 対象2つ＋スキップ対象3つ
        std::fs::write(root.join("a.txt"), b"A").unwrap();
        std::fs::write(root.join("b.wav"), b"RIFFxxxxWAVE").unwrap();
        std::fs::write(root.join("c.tmp"), b"partial").unwrap();
        std::fs::write(root.join("~lock.docx"), b"lock").unwrap();
        std::fs::write(root.join(".hidden"), b"dot").unwrap();

        let spec = flow::parse_spec(r#"{"builtin":"import_new"}"#).unwrap();
        let mut report = empty_report();
        let n = run_import_new(&p, "f1", &spec, &mut report).unwrap();
        assert_eq!(n, 2, "a.txt と b.wav のみ登録のはず: {:?}", report.outputs);
        // プロジェクト内 → committed_path 付きの実身
        let committed: i64 = p
            .conn
            .query_row(
                "SELECT COUNT(*) FROM virtual_bodies
                 WHERE committed_path IS NOT NULL AND kind IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(committed, 2);
        // 2回目は既知スキップで重複ゼロ
        let mut report2 = empty_report();
        let n2 = run_import_new(&p, "f1", &spec, &mut report2).unwrap();
        assert_eq!(n2, 0, "2回目に重複登録: {:?}", report2.outputs);
        // 存在しないフォルダ指定はエラー
        let bad = flow::parse_spec(r#"{"builtin":"import_new","path":"Q:\\no_such_dir_xyz"}"#)
            .unwrap();
        assert!(run_import_new(&p, "f1", &bad, &mut empty_report()).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// always（既定）: 上流エラーは report へ伝搬し、payload はそのまま通る（後方互換）。
    #[test]
    fn always_edge_propagates_errors() {
        let mut up = EvalOut::data(vec![payload("a", b"x")], Some(0));
        up.ok = false;
        up.errors = vec!["a: 失敗".into()];
        let mut report = empty_report();
        let e = apply_edge(&up, &flow::EdgeAttrs::default(), "a", &mut report);
        assert_eq!(report.errors, vec!["a: 失敗".to_string()]);
        assert!(!e.nodata);
        assert_eq!(e.payloads.len(), 1);
    }

    /// success: 成功時は上流出力を運び、失敗は no-data（report にエラーを流さない＝格下げ）。
    #[test]
    fn success_edge_downgrades_failure() {
        let ok = EvalOut::data(vec![payload("a", b"x")], Some(0));
        let bad = EvalOut::failed(vec!["a: 失敗".into()], Some(1), vec![]);
        let mut report = empty_report();
        let a = attrs(r#"{"when":"success"}"#);
        let e1 = apply_edge(&ok, &a, "a", &mut report);
        assert!(!e1.nodata);
        assert!(!e1.failure, "成功枝は失敗枝フラグを立てない");
        assert_eq!(e1.payloads.len(), 1);
        let e2 = apply_edge(&bad, &a, "a", &mut report);
        assert!(e2.nodata, "失敗は枝が取られない＝no-data");
        assert!(report.errors.is_empty(), "条件付き矢印はエラーを格下げする");
    }

    /// failure: 既定はエラーメッセージ（stderr）を text/plain で運ぶ。成功時は no-data。
    #[test]
    fn failure_edge_carries_stderr_by_default() {
        let bad = EvalOut::failed(vec!["a: exit 1\nstderr本文".into()], Some(1), vec![]);
        let ok = EvalOut::data(vec![payload("a", b"x")], Some(0));
        let mut report = empty_report();
        let a = attrs(r#"{"when":"failure"}"#);
        let e1 = apply_edge(&bad, &a, "a", &mut report);
        assert!(!e1.nodata);
        assert!(e1.failure, "失敗枝フラグ（v1.5.7 セレクタのエラー時メッセージが見る）");
        assert_eq!(e1.payloads.len(), 1);
        assert_eq!(e1.payloads[0].mime, "text/plain");
        assert!(String::from_utf8_lossy(&e1.payloads[0].bytes).contains("stderr本文"));
        let e2 = apply_edge(&ok, &a, "a", &mut report);
        assert!(e2.nodata, "成功時は失敗枝が取られない");
        assert!(report.errors.is_empty());
    }

    /// carry: "input"=失敗時の入力を横流し／"none"=データなしで枝だけ通す（ゲート）。
    #[test]
    fn failure_edge_carry_variants() {
        let bad = EvalOut::failed(
            vec!["a: 失敗".into()],
            Some(2),
            vec![payload("in1", b"retry-me")],
        );
        let mut report = empty_report();
        let e_in = apply_edge(&bad, &attrs(r#"{"when":"failure","carry":"input"}"#), "a", &mut report);
        assert_eq!(e_in.payloads.len(), 1);
        assert_eq!(e_in.payloads[0].bytes, b"retry-me");
        let e_none = apply_edge(&bad, &attrs(r#"{"when":"failure","carry":"none"}"#), "a", &mut report);
        assert!(!e_none.nodata, "枝は取られている（信号のみ）");
        assert!(e_none.payloads.is_empty());
    }

    /// 上流 no-data は when に関わらず no-data（NULL伝搬・§5.1-4）。
    #[test]
    fn nodata_null_propagates_through_any_when() {
        let nd = EvalOut::nodata_out();
        let mut report = empty_report();
        for a in [
            flow::EdgeAttrs::default(),
            attrs(r#"{"when":"success"}"#),
            attrs(r#"{"when":"failure"}"#),
        ] {
            let e = apply_edge(&nd, &a, "a", &mut report);
            assert!(e.nodata);
        }
        assert!(report.errors.is_empty());
    }

    /// last_run の構造化保存（§6.1）: at/exit/ok/nodata/skipped/message が JSON で入る。
    #[test]
    fn last_run_is_persisted_as_json() {
        let dir = std::env::temp_dir().join(format!("texos_p1_test_{}", db::now_ms()));
        let cas_root = dir.join(".texos").join("cas");
        std::fs::create_dir_all(&cas_root).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        let p = Project {
            conn,
            root: dir.clone(),
            cas_root,
        };
        let h = cas::store_bytes(&p.cas_root, b"spec").unwrap();
        db::insert_real_body(&p.conn, &h, "application/json", 4, None).unwrap();
        db::insert_virtual_body(&p.conn, "f1", &h, "フィルタ", 0.0, 0.0, None, None).unwrap();
        let out = EvalOut::failed(vec!["exit 1: 詳細".into()], Some(1), vec![]);
        write_last_run(&p, "f1", &out, 2);
        let s: String = p
            .conn
            .query_row(
                "SELECT last_run FROM virtual_bodies WHERE id='f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["exit"], serde_json::json!(1));
        assert_eq!(v["nodata"], serde_json::json!(false));
        assert_eq!(v["skipped"], serde_json::json!(2));
        assert!(v["message"].as_str().unwrap().contains("詳細"));
        assert!(v["at"].as_i64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
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
) -> Result<FlowReport, String> {
    run_flow_for(&id, &app)
}

/// run_flow の実体（v1.5.4 WS-P2: 代理発行の入口）。トリガ/タイマ/ウォッチャ（ops.rs）と
/// スケジューラは AppHandle しか持たないスレッドから呼ぶため、State はここで引く。
/// ジョブ台帳への登録・job:started/done/failed の emit まで含む＝呼び出し元はスレッドに
/// 乗せるだけでパネル表示と Stop-all が効く。
pub(crate) fn run_flow_for(id: &str, app: &tauri::AppHandle) -> Result<FlowReport, String> {
    use tauri::Manager;
    let state: State<AppState> = app.state();
    let id = id.to_string();
    // v1.5.2 WS-C 並行解禁: 短ロックで終端カードとルートを読み、**同一終端カード**の Flow
    // だけを拒否して登録（v1.4.10 再入エラーの一般化）。別フローとは並行し、資源の
    // 取り合いはセマフォ（acquire_node_permits）が調停する。完了同期返しは維持。
    // v1.5.6 WS-M M1（§9.3）: OUT バーチャルカード（op:"out"）も終端になれる＝▶で
    // さかのぼり pull 実行・結果カードが OUT の横に生まれる（受容体の最小形）。
    // それ以外の op（トリガ/タイマ/ウォッチャ/IN）は run_flow の終端にしない。
    let (fx, fy, flabel, fref, root): (f64, f64, String, Option<String>, PathBuf) =
        with_db(state.inner(), |p| {
            let (fx, fy, flabel, fref): (f64, f64, String, Option<String>) = p
                .conn
                .query_row(
                    "SELECT pos_x, pos_y, label, filter_ref FROM virtual_bodies
                     WHERE id = ?1 AND kind IN ('filter','op') AND deleted_at IS NULL",
                    rusqlite::params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .map_err(|_| "フィルタカードが見つかりません".to_string())?;
            if let Some(spec) = fref.as_deref().filter(|s| ops::is_op_spec(s)) {
                let is_out = serde_json::from_str::<serde_json::Value>(spec)
                    .ok()
                    .and_then(|v| v.get("op").and_then(|o| o.as_str().map(String::from)))
                    .as_deref()
                    == Some("out");
                if !is_out {
                    return Err(
                        "このカードは実行の終端にできません（OUT カードまたはフィルタカードで実行してください）"
                            .into(),
                    );
                }
            }
            Ok((fx, fy, flabel, fref, p.root.clone()))
        })?;
    let job = state
        .jobs
        .register_flow(&id, &flabel, &root.to_string_lossy())?;
    let _guard = JobGuard::new(&state.jobs, job.id);
    // card 付き（パネルが録音対応/停止コマンドを spec から引く）。
    let _ = app.emit(
        "job:started",
        serde_json::json!({ "job_id": job.id, "kind": "flow", "label": flabel, "card": id }),
    );
    let result = run_flow_inner(&id, fx, fy, &flabel, fref.as_deref(), &root, app, &state, &job);
    // report 内のエラーは「完了した実行の報告」なので done 扱い＝Err はコマンド自体の失敗。
    let _ = app.emit(
        if result.is_ok() { "job:done" } else { "job:failed" },
        serde_json::json!({ "job_id": job.id }),
    );
    result
}

#[allow(clippy::too_many_arguments)]
fn run_flow_inner(
    id: &str,
    fx: f64,
    fy: f64,
    flabel: &str,
    fref: Option<&str>,
    root: &Path,
    app: &tauri::AppHandle,
    state: &State<AppState>,
    job: &Job,
) -> Result<FlowReport, String> {
    let id = id.to_string();
    let flabel = flabel.to_string();
    // 終端フィルタの設定が出力先と命名を決める（prefix連番 / 出力フォルダ）
    let term_spec = flow::parse_spec(fref.unwrap_or("{}"))?;

    // v1.5.2 WS-C: ジョブ専用の DB 接続（起動元プロジェクト束縛）。以後の評価・materialize は
    // すべてこの接続で行う＝実行中にプロジェクトを切り替えてもこのジョブは起動元へ書き続ける。
    let jp = open_project_at(root)?;

    // v1.5 Text-To-Image: 終端が t2i カードなら生成デーモンを先に起こしておく（▶実行を
    // 「ただ押せば動く」に）。best-effort — 起動できなければ run_t2i が分かりやすく案内する。
    if term_spec.node_kind() == flow::NodeKind::T2i {
        let _ = t2i_ensure_server(&state.app_data, state.inner());
    }

    // v1.3.11 対話シェル: cmd/PowerShell で一行目（cmd）を実行。
    // シェルカード統合（2026-07-06）: window="keep"（既定・開いたまま）/"close"（実行して閉じる）は
    // detached ウィンドウ、"hidden" はウィンドウなしで捕捉して結果パネルへ返す。
    // v1.5.7: {in} の解決を2段に。
    //  ①直結の実ファイルカード（生きたファイルを持つ kind='file'）＝従来動作。ライブパスを
    //    そのまま渡す（`notepad {in}` で本物を編集できる意味論を保つ）。従来はフィルタ/op/付箋
    //    カードも拾って存在しない CAS パスに展開されるバグがあった（kind 条件で解消）。
    //  ②無ければ上流を pull 評価（eval_filter はシェルを素通し扱い）し、採用ペイロードを
    //    一時ファイルへ実体化して渡す — トリガ発行→シェルで上流全体が動く（要求駆動の入口）。
    if term_spec.is_shell() {
        let shell = term_spec.shell.as_deref().unwrap_or("").to_string();
        let mut report = FlowReport {
            created: vec![],
            updated: vec![],
            outputs: vec![],
            skipped: vec![],
            errors: vec![],
            shell_capture: None,
        };
        let firstline = term_spec.cmd.clone().unwrap_or_default();
        let firstline = if firstline.contains("{in}") {
            // ① 直結の実ファイルカード（ジョブ専用接続）。ファイルが実在するときだけ採用。
            let direct: Option<PathBuf> = {
                let p = &jp;
                p.conn
                    .query_row(
                        "SELECT v.id, v.real_id, v.committed_path FROM relations rel
                         JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                         WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                           AND v.kind = 'file'
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
                    .and_then(|(vid, real_id, committed)| {
                        committed
                            .map(PathBuf::from)
                            .filter(|c| c.is_file())
                            .or_else(|| {
                                let pb = resolve_view_path(p, &vid, &real_id);
                                pb.is_file().then_some(pb)
                            })
                    })
            };
            // ② 上流の pull 評価 → 一時実体化（{stem}.{ext} 名＝exe やバッチが素直に動く）。
            let resolved: Option<PathBuf> = match direct {
                Some(p) => Some(p),
                None => {
                    let mut visited = std::collections::HashSet::new();
                    let mut memo = EvalMemo::new();
                    let out = eval_filter(
                        state.inner(),
                        app,
                        &jp,
                        &id,
                        &mut visited,
                        &mut memo,
                        &mut report,
                        job,
                    )?;
                    report.errors.extend(out.errors.clone());
                    if out.nodata {
                        report.skipped.push(
                            "no-data: 条件を満たす枝がありませんでした（材料なし＝エラーではありません）"
                                .into(),
                        );
                    }
                    match out.payloads.first() {
                        Some(pl) => {
                            let dir = crate::os_integration::temp_view_dir()
                                .join("shell_in")
                                .join(uuid());
                            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                            let stem: String = pl
                                .stem
                                .chars()
                                .map(|c| match c {
                                    '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                                    c if c.is_control() => '_',
                                    c => c,
                                })
                                .take(48)
                                .collect();
                            let stem = if stem.trim().is_empty() { "input".to_string() } else { stem };
                            let path = dir.join(format!("{stem}.{}", pl.ext));
                            std::fs::write(&path, &pl.bytes).map_err(|e| e.to_string())?;
                            Some(path)
                        }
                        None => None,
                    }
                }
            };
            // 材料が無くエラーだけなら窓を開かず報告（コンパイル失敗が黙って素通りしない）。
            if resolved.is_none() && !report.errors.is_empty() {
                return Ok(report);
            }
            match &resolved {
                Some(pb) => firstline.replace("{in}", &format!("\"{}\"", pb.display())),
                None => firstline.replace("{in}", ""),
            }
        } else {
            firstline
        };
        let window = term_spec.window.as_deref().unwrap_or("keep");
        let capture = if window == "hidden" {
            // ウィンドウなし実行: 中断フラグはジョブ別（毎回新品）なのでリセット不要。
            let out = run_shell_captured(
                &shell,
                &firstline,
                root,
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
            launch_shell_window(&shell, &firstline, root, window != "close")?;
            None
        };
        report.shell_capture = capture;
        return Ok(report);
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
        None => (root.to_path_buf(), false),
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
    let mut memo = EvalMemo::new();
    // 評価フェーズ: DB 接触はジョブ専用接続（jp）＝ UI の state.current とは独立。
    // 実行（外部/AI/録音）中も UI・他フローが固まらない。資源の取り合いはセマフォが調停。
    // 中断フラグ・stdin 制御はこのジョブのもの（ジョブ別＝毎回新品なのでリセット不要）。
    let out = eval_filter(
        state.inner(),
        app,
        &jp,
        &id,
        &mut visited,
        &mut memo,
        &mut report,
        job,
    )?;
    // v1.5.3 WS-P1: 終端ノード自身の失敗は常に報告（エッジによる格下げは中間ノードの話）。
    report.errors.extend(out.errors.clone());
    if out.nodata {
        report
            .skipped
            .push("no-data: 条件を満たす枝がありませんでした（材料なし＝エラーではありません）".into());
    }
    let payloads = out.payloads;
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

    // マテリアライズ（DB と fs のみ・長時間プロセスなし）。ジョブ専用接続で起動元へ書く。
    // v1.5.2 WS-C（凍結 §11.4）: プロジェクト単位の採番ロックで、並行フローの固定名照合と
    // prefix 連番（seq）の採番・書き戻しを直列化する（同じ空き番号を同時に掴む競合の防止）。
    let write_lock = project_write_lock(state.inner(), root);
    let _write_guard = write_lock.lock().unwrap();
    {
    let p = &jp;
    let mut out_i = 0usize;
    for pl in &payloads {
        // v1.7 P2 実機FB（2026-07-17）: 取得メタ付き出力（web_fetch）は、内容が前回と
        // 同一なら実身を増やさない（計画 §1-3「内容が同一なら CAS により同一ハッシュ＝
        // 実身は増えない」の着地）。このフィルタが過去に作った生きた出力カードに同じ
        // ハッシュがあればそれを再利用し、取得日時/ETag だけ最新化する（ファイルも書かない）。
        if pl.meta.is_some() {
            let hash = cas::hash_bytes(&pl.bytes);
            let prev: Option<String> = p
                .conn
                .query_row(
                    "SELECT v.id FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.to_id AND v.deleted_at IS NULL
                     WHERE rel.from_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                       AND v.real_id = ?2 LIMIT 1",
                    rusqlite::params![id, hash],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            if let Some(vid) = prev {
                let _ = p.conn.execute(
                    "UPDATE virtual_bodies SET web_meta = ?1 WHERE id = ?2",
                    rusqlite::params![pl.meta, vid],
                );
                if let Some(m) = &pl.meta {
                    link_fetch_source(p, &id, m, &vid);
                }
                report.updated.push(vid);
                report.outputs.push(format!("{}（変化なし・実身は増やしません）", pl.stem));
                continue;
            }
        }
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
            // v1.5.7: 拡張子込みで書かれた固定名（"fibo.exe" 等）に .ext を重ねない。
            (Some(px), None) => {
                let dot_ext = format!(".{}", pl.ext);
                if px.to_ascii_lowercase().ends_with(&dot_ext.to_ascii_lowercase()) {
                    out_root.join(px)
                } else {
                    out_root.join(format!("{px}{dot_ext}"))
                }
            }
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
            // v1.7 P1: 再取得の上書きは取得メタも更新（None は既存値を保持）。
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET real_id = ?1, committed_mtime = ?2,
                        web_meta = COALESCE(?3, web_meta) WHERE id = ?4",
                rusqlite::params![
                    hash,
                    if external_out { None } else { file_mtime_ms(&dest) },
                    pl.meta,
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
            // v1.7 P2: 取得メタ付き出力（web_fetch）は URLカード→取得実身の系譜を張り替える。
            if let Some(m) = &pl.meta {
                link_fetch_source(p, &id, m, &vid);
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
                 committed_path, committed_mtime, origin_path, web_meta, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'file')",
            rusqlite::params![
                new_vid,
                hash,
                out_label,
                nx,
                ny,
                db::now_ms(),
                committed_path,
                committed_mtime,
                origin_path,
                pl.meta
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
        // v1.7 P2: 取得メタ付き出力（web_fetch）は URLカード→取得実身の系譜を張る。
        if let Some(m) = &pl.meta {
            link_fetch_source(p, &id, m, &new_vid);
        }
        report.created.push(new_vid);
        report.outputs.push(out_label);
        out_i += 1;
    }

    // 連番の永続化: 使った分だけ進めてカードに書き戻す
    // （未知フィールドを保持するため serde_json::Value 経由で seq だけ更新）
    if prefix.is_some() && term_spec.seq.unwrap_or(0) != seq {
        if let Ok(mut v) =
            serde_json::from_str::<serde_json::Value>(fref.unwrap_or("{}"))
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
    }

    Ok(report)
}

/// ジョブの中断（UIの中断ボタン・v1.5.1 WS-B で job_id 引数化）。run_flow が外部コマンドで
/// ストールしていてもこのコマンドは DB の Mutex を取らないため即座に通り、監視ループがツリー
/// ごと kill する。ジョブが既に終わっていれば黙って何もしない（連打・競合は無害）。
/// v1.5.2 WS-C: Flow に限らず任意のジョブ（Chat/Setup）の cancel を立てる汎用中断口。
/// デーモンへの /cancel は**そのジョブが t2i 生成中のときだけ**送る（並行時に他ジョブの
/// 生成を誤って止めない）。
#[tauri::command(async)]
pub(crate) fn break_flow(job_id: u64, state: State<AppState>) {
    if let Some(job) = state.jobs.get(job_id) {
        job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        if job.t2i_active.load(std::sync::atomic::Ordering::Relaxed) {
            flow::t2i_post_cancel();
        }
    }
}

/// v1.5.1 WS-B: 実行中ジョブの一覧（WS-C 背景ジョブパネルの復元用）。
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

