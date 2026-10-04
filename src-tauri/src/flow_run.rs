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
    card: &str,
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
            }
            // v1.7 P3: web_feed も同系のネットワーク別経路。1入力→N出力＝ビルトイン
            // 複数出力の解禁実例（標準フィルタ充実検討 §10 で据え置いた件の実例がこれ。
            // NodeRun.outputs が受け皿なので実行系の拡張はこの分岐だけで済む）。
            // Web検索 P2: web_search（検索語 → SearXNG → 結果一覧 Markdown 1枚）。
            // 入力なし（パラメータ欄の検索語）も許す。結果0件は outputs 空 → eval_filter が
            // no-data にする（下流を止める）。取得メタは付けない（更新確認の対象ではない）。
            else if b == "web_search" {
                let r = web::run_web_search(spec, inputs.first().map(|i| i.0), Some(cancel))?;
                Ok(NodeRun {
                    outputs: r
                        .into_iter()
                        .map(|r| (r.bytes, r.ext, r.mime, r.stem, None))
                        .collect(),
                    exit_code: None,
                    stderr: String::new(),
                })
            }
            else if b == "web_feed" {
                let rs = web::run_web_feed(spec, inputs.first().map(|i| i.0))?;
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("中断されました".into());
                }
                Ok(NodeRun {
                    outputs: rs
                        .into_iter()
                        .map(|r| (r.bytes, r.ext, r.mime, r.stem, Some(r.meta)))
                        .collect(),
                    exit_code: None,
                    stderr: String::new(),
                })
            }
            // v1.7 P5(b): to_url（URLカード化）— テキスト全文を走査して URL を全部
            // ペイロード化（1入力→N出力）。終端ならマテリアライザが URL カードとして
            // 着地させ、途中なら text/plain の URL として下流へ流れる（web_fetch 連鎖可）。
            // タイトル取得（既定）はネットワークに出るため cancel を渡す（URL 境界で中断）。
            else if b == "to_url" {
                let rs = web::run_to_url(spec, inputs[0].0, Some(cancel))?;
                Ok(NodeRun {
                    outputs: rs
                        .into_iter()
                        .map(|r| (r.bytes, r.ext, r.mime, r.stem, Some(r.meta)))
                        .collect(),
                    exit_code: None,
                    stderr: String::new(),
                })
            }
            // v1.7 P4: web_capture はヘッドレスブラウザの外部起動（L2 OSアダプタ）。
            // 外部プロセスなので中断は kill が効く＝cancel を中へ渡す。
            else if b == "web_capture" {
                let r = web::run_web_capture(spec, inputs.first().map(|i| i.0), Some(cancel))?;
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
            // v1.8.9 実行条件（digest・能力・実際に送ったパラメータ）を成否に関わらず記録。
            let mut rec = None;
            let res = flow::run_ai_rec(spec, inputs[0].0, &mut rec);
            if let Some(r) = rec {
                job.trace.ai(card, r);
            }
            let (bytes, ext, mime) = res?;
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
        NodeKind::T23d => {
            // v2.0 t23d: 入力から最初の画像を採る（画像実身のみ・テキストピン無し＝設計 §4。
            // 「文→(ai)→画→(t2i)→(t23d)→3D」のカード連結が正道）。集約ノード扱いで全入力が来る。
            let mut image: Option<&[u8]> = None;
            for (bytes, _ext, mime) in inputs {
                if mime.starts_with("image/") && image.is_none() {
                    image = Some(*bytes);
                }
            }
            let image = image.ok_or(
                "入力画像がありません。画像カード（t2i の出力など）を接続してください",
            )?;
            // t2i と同じ契約: 生成 HTTP 中だけ点灯 — break_flow はこれを見てデーモンへ
            // /cancel を送る（並行時に他ジョブの生成を誤って止めない）。
            job.t23d_active.store(true, std::sync::atomic::Ordering::Relaxed);
            let run = flow::run_t23d(spec, image);
            job.t23d_active.store(false, std::sync::atomic::Ordering::Relaxed);
            let (bytes, ext, mime) = run?;
            // 中断時は部分出力を破棄してエラー化（t2i と同じ既存契約。t23d デーモンは例外
            // 方式なので通常こちらへは来ないが、cancel 直後の完走を拾う保険）。
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
        NodeKind::T23d => Some(format!("http://127.0.0.1:{}", flow::T23D_PORT)),
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
    // 2.5) gpu（v1.7.2 t23d）: t2i / t23d は単一 VRAM を取り合うため生成を直列化する
    //（生成カード群企画 §3 の GPU 資源・粒度は「生成中だけロック」。texture 込みの t23d は
    // 16GB 常駐なので、SDXL 等との同時実行は共倒れになる）。超過は ai と同じ即拒否。
    if matches!(kind, NodeKind::T2i | NodeKind::T23d) {
        permits.push(state.resources.gpu.try_acquire().ok_or_else(|| {
            "GPU は使用中です。実行中の画像生成・3D生成の完了を待つか中断してください"
                .to_string()
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
    let Ok(v) = serde_json::from_str::<serde_json::Value>(meta) else {
        return;
    };
    // v1.7 P4: キャプチャ（png）は取得系譜・「更新を確認」の対象外 — 更新確認の比較基準は
    // 本文変換（md）のハッシュで、png とは比較が成立しないため（系譜を張ると URL カードの
    // ベースラインが png になり「常に更新あり」になってしまう）。
    if v.get("capture").and_then(|c| c.as_bool()).unwrap_or(false) {
        return;
    }
    let Some(src) = v.get("source_url").and_then(|s| s.as_str()).map(str::to_string) else {
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

/// v1.7 P5(a): 終端フィルタ（fid）から矢印が引かれた URL カード＝「リンクの受け皿」。
/// to_url の出力矢印も URL カードを指すため、呼び出し側は to_url を対象外にすること。
fn url_sink_ids(conn: &Connection, fid: &str) -> Vec<String> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT v.id FROM relations rel
         JOIN virtual_bodies v ON v.id = rel.to_id AND v.deleted_at IS NULL
         WHERE rel.from_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
           AND v.kind = 'url'
         ORDER BY rel.created_at",
    ) else {
        return Vec::new();
    };
    stmt.query_map(rusqlite::params![fid], |r| r.get(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// v1.7 P5(a): 受け皿 URL カードへ URL を流し込む。返り値は表示用ラベル。
/// - ラベルが旧 URL のままなら新 URL に追従（タイトルを付けたカードは名前を保つ）。
/// - URL が変わったら旧取得系譜（fetched）と更新マークは無効なので畳む。
fn update_url_sink(conn: &Connection, sink: &str, url: &str) -> Result<String, String> {
    let old: Option<String> = conn
        .query_row(
            "SELECT url_ref FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![sink],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    conn.execute(
        "UPDATE virtual_bodies
         SET label = CASE WHEN label = COALESCE(url_ref, '') THEN ?1 ELSE label END,
             url_ref = ?1
         WHERE id = ?2",
        rusqlite::params![url, sink],
    )
    .map_err(|e| e.to_string())?;
    if old.as_deref() != Some(url) {
        let _ = conn.execute(
            "UPDATE relations SET deleted_at = ?1
             WHERE from_id = ?2 AND kind = 'fetched' AND deleted_at IS NULL",
            rusqlite::params![db::now_ms(), sink],
        );
        let _ = conn.execute(
            "UPDATE virtual_bodies SET web_meta = NULL WHERE id = ?1",
            rusqlite::params![sink],
        );
    }
    conn.query_row(
        "SELECT label FROM virtual_bodies WHERE id = ?1",
        rusqlite::params![sink],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

/// v1.7 P5(b): このフィルタ（fid）が過去に出した URL カードに url があるか（既出台帳）。
/// guid（P3）と同じ原則: **削除済みも既出に数える**＝消したリンクは再実行で蘇らない。
fn url_output_seen(conn: &Connection, fid: &str, url: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM relations rel
         JOIN virtual_bodies v ON v.id = rel.to_id
         WHERE rel.from_id = ?1 AND rel.kind = 'flow' AND v.url_ref = ?2 LIMIT 1",
        rusqlite::params![fid, url],
        |_| Ok(true),
    )
    .optional()
    .unwrap_or(None)
    .unwrap_or(false)
}

/// v1.7 P3: このフィルタ（fid）の出力カードが持つ web_meta guid の集合（既読台帳）。
/// web_feed の差分 pull がここと照合する — 台帳は別テーブルでなく出力カードそのもの。
/// **削除済みカードの guid も既読に数える**（deleted_at を見ない）: 読んで消した記事が
/// 再実行のたびに蘇らないため。全部読み直したいときはフィルタカードを作り直す（台帳が空になる）。
fn feed_seen_guids(conn: &Connection, fid: &str) -> std::collections::HashSet<String> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT v.web_meta FROM relations rel
         JOIN virtual_bodies v ON v.id = rel.to_id
         WHERE rel.from_id = ?1 AND rel.kind = 'flow' AND v.web_meta IS NOT NULL",
    ) else {
        return Default::default();
    };
    stmt.query_map(rusqlite::params![fid], |r| r.get::<_, String>(0))
        .map(|rows| {
            rows.flatten()
                .filter_map(|m| {
                    serde_json::from_str::<serde_json::Value>(&m)
                        .ok()
                        .and_then(|v| v.get("guid").and_then(|g| g.as_str()).map(str::to_string))
                })
                .filter(|g| !g.is_empty())
                .collect()
        })
        .unwrap_or_default()
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
    pub(crate) fn nodata_out() -> Self {
        EvalOut { payloads: vec![], nodata: true, ok: true, exit: None, errors: vec![], failed_inputs: vec![] }
    }
    pub(crate) fn failed(errors: Vec<String>, exit: Option<i32>, failed_inputs: Vec<FlowPayload>) -> Self {
        EvalOut { payloads: vec![], nodata: false, ok: false, exit, errors, failed_inputs }
    }
}

/// ジョブスコープのメモ表（§5.1-5）: 複数下流が同じカードを pull しても実行は1回。
/// 副作用のある上流（録音・AI 呼び出し）の二重実行を防ぐ。run_flow / チャット1ターンごと。
pub(crate) type EvalMemo = std::collections::HashMap<String, EvalOut>;

/// v1.8 ④反復カード（`builtin:"loop"`）の実行文脈。ジョブ内で持ち回る揮発情報で、
/// **反復の外では空**＝従来の評価に一切影響しない。
///
/// 計画 §4 案B（DAG を保つ）の要: グラフに循環を作らずに「前周の結果」を上流へ届けるため、
/// 値をエッジではなくここへ置く。戻り矢印（`role:"previous"`）は評価で遡らず、この表を引く。
#[derive(Default)]
pub(crate) struct LoopCtx {
    /// 反復カード id → 前周の出力。`role:"previous"` の矢印がここから材料を取る。
    prev: std::collections::HashMap<String, Vec<FlowPayload>>,
    /// 入れ子の周回番号（外側から順・1始まり）。反復の外では空。
    /// 記録カード（tap）はこれを名前の接尾辞にするので、各周の途中経過が上書きで消えない。
    rounds: Vec<u32>,
}

impl LoopCtx {
    /// 記録カードなどが使う周回の接尾辞（`_2` / 入れ子なら `_2_3`）。反復の外では空文字。
    fn round_suffix(&self) -> String {
        if self.rounds.is_empty() {
            return String::new();
        }
        let mut s = String::new();
        for r in &self.rounds {
            s.push('_');
            s.push_str(&r.to_string());
        }
        s
    }
}

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

/// v1.8.7 カードの色でのお知らせ: 評価そのものは成功したが、その後の段取り（キャンバスへの
/// 着地など）で失敗した／フロー全体が起動前に止まった、を last_run に書き足す。
/// `level` = "warn"（評価は成功・後段で失敗）| "error"（実行できなかった）。
/// 次の実行で write_last_run が丸ごと上書きする＝直せば自然に消える。
pub(crate) fn mark_last_run(p: &Project, fid: &str, level: &str, message: &str) {
    let cur: Option<String> = p
        .conn
        .query_row(
            "SELECT last_run FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![fid],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten();
    let mut v = cur
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({ "at": db::now_ms() }));
    if level == "error" {
        v["ok"] = serde_json::Value::Bool(false);
        v["at"] = db::now_ms().into();
        v["message"] = serde_json::Value::String(message.to_string());
    } else {
        let prev = v.get("warning").and_then(|w| w.as_str()).unwrap_or("").to_string();
        let joined = if prev.is_empty() { message.to_string() } else { format!("{prev}\n{message}") };
        v["warning"] = serde_json::Value::String(joined);
    }
    let _ = p.conn.execute(
        "UPDATE virtual_bodies SET last_run = ?1 WHERE id = ?2",
        rusqlite::params![v.to_string(), fid],
    );
}

/// 評価の終端処理: visited を抜け・last_run を保存・メモ表へ登録して返す。
fn finish_eval(
    p: &Project,
    job: &Job,
    fid: &str,
    visited: &mut std::collections::HashSet<String>,
    memo: &mut EvalMemo,
    skipped: u32,
    out: EvalOut,
) -> Result<EvalOut, String> {
    visited.remove(fid);
    write_last_run(p, fid, &out, skipped);
    job.trace.end(fid, &out, skipped);
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
    // v1.8.3: 空欄＝プロジェクト直下／相対＝プロジェクト基準（可搬性・util §3）。
    let dir = resolve_project_path(&p.root, spec.path.as_deref());
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
                rusqlite::params![store_path(&p.root, &path_str)],
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

/// v1.8 記録カード（builtin:"tap"）の書き出し。
///
/// フローの途中を通ったペイロードを、**固定名で上書き**しながらプロジェクトへ残し、
/// 実身カードとして面に出す。名前は `prefix`（無ければカードのラベル）。2本目以降の
/// 入力は `_2`, `_3` を付ける。出力先は `out_dir` 指定があればそこ、無ければ直下。
///
/// 2回目以降は同じ実ファイルを指す生きたカードを探して**中身だけ差し替える**
/// （毎回 import すると同じ実体に2枚目のカードができてしまう＝v1.3.9 の根幹原則違反）。
///
/// v1.8 ④: `round_suffix` は反復カードの中にいるときの周回番号（`_2` / 入れ子なら `_2_3`）。
/// 固定名で上書きする設計のままだと**反復の各周が次の周に消される**ので、周ごとに別名にする。
/// 反復の外では空文字＝従来どおりの固定名。
fn write_tap_record(
    p: &Project,
    fid: &str,
    spec: &flow::FilterSpec,
    payloads: &[FlowPayload],
    round_suffix: &str,
) -> Result<Vec<String>, String> {
    let (label, fx, fy): (String, f64, f64) = p
        .conn
        .query_row(
            "SELECT label, pos_x, pos_y FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![fid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    let base = spec
        .prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&label);
    let base = crate::web::sanitize_stem(base).unwrap_or_else(|| "記録".into());
    let base = format!("{base}{round_suffix}");
    // v1.8.3: 空欄＝プロジェクト直下／相対＝プロジェクト基準（可搬性・util §3）。
    let dir = resolve_project_path(&p.root, spec.out_dir.as_deref());
    if !dir.is_dir() {
        return Err(format!("記録の出力先フォルダがありません: {}", dir.display()));
    }

    let mut names = Vec::new();
    for (i, pl) in payloads.iter().enumerate() {
        let stem = if i == 0 { base.clone() } else { format!("{base}_{}", i + 1) };
        let name = format!("{stem}.{}", pl.ext);
        let path = dir.join(&name);
        std::fs::write(&path, &pl.bytes).map_err(|e| format!("{name}: 書き出し失敗 ({e})"))?;
        let hash = cas::store_bytes(&p.cas_root, &pl.bytes).map_err(|e| e.to_string())?;
        let ext_l = pl.ext.to_ascii_lowercase();
        let mime = mime_for_ext(&ext_l).unwrap_or(pl.mime.as_str());
        db::insert_real_body(&p.conn, &hash, mime, pl.bytes.len() as u64, Some(&ext_l))
            .map_err(|e| e.to_string())?;

        let path_str = path.to_string_lossy().to_string();
        let existing: Option<String> = p
            .conn
            .query_row(
                "SELECT id FROM virtual_bodies
                 WHERE committed_path = ?1 AND deleted_at IS NULL
                 ORDER BY created_at LIMIT 1",
                rusqlite::params![store_path(&p.root, &path_str)],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match existing {
            Some(vid) => {
                p.conn
                    .execute(
                        "UPDATE virtual_bodies
                         SET real_id = ?1, committed_mtime = ?2, ext = ?3, mime_type = ?4
                         WHERE id = ?5",
                        rusqlite::params![hash, file_mtime_ms(&path), ext_l, mime, vid],
                    )
                    .map_err(|e| e.to_string())?;
            }
            None => {
                // 初回だけカードを作る。記録カードのすぐ下に並べる。
                let cx = fx + (i as f64) * 170.0;
                let cy = fy + 150.0;
                objects::import_file_into(p, path_str, String::new(), cx, cy)?;
            }
        }
        names.push(name);
    }
    Ok(names)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_filter(
    state: &AppState,
    app: &tauri::AppHandle,
    p: &Project,
    fid: &str,
    visited: &mut std::collections::HashSet<String>,
    memo: &mut EvalMemo,
    report: &mut FlowReport,
    job: &Job,
    ctx: &mut LoopCtx,
) -> Result<EvalOut, String> {
    // メモ化（§5.1-5）: 同一ジョブ内の再 pull は実行せず前回結果を配る。
    if let Some(hit) = memo.get(fid) {
        // v1.8.7 トレース: 同じ実行内で2回目以降に引かれた（再実行はしていない）。
        job.trace.reused(fid, &node_label(p, fid));
        return Ok(hit.clone());
    }
    let root: &Path = &p.root;
    // 中断フラグは入力収集（collect_input_edges）が見る。v1.8 ④の切り出しでこちらは不要に。
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

    // v1.8.7 トレース: 評価開始（終了は finish_eval が必ず記録する）。
    job.trace.begin(fid, &node_label(p, fid), &trace_kind_name(&spec, passthrough), &ctx.round_suffix());
    if !passthrough
        && !spec.is_shell()
        && matches!(spec.node_kind(), flow::NodeKind::External | flow::NodeKind::Generator)
    {
        match build_cmdline(root, &spec) {
            Ok(c) => job.trace.note(fid, "コマンド", &c),
            Err(e) => job.trace.note(fid, "コマンド（解決失敗）", &e),
        }
    }

    // ジェネレータ型: 入力を取らず1回実行してファイルを生む（入力矢印は無視）。
    // §4.1: 実行は execute_node に一任し、ここは命名（stem＝prefix or "output"）だけ担う。
    if spec.node_kind() == flow::NodeKind::Generator {
        let run = acquire_node_permits(state, app, job, &spec, flow::NodeKind::Generator)
            .and_then(|_permits| {
                emit_job_progress(app, job, Some(&node_note(p, fid)), None);
                execute_node(root, &spec, flow::NodeKind::Generator, &[], job, fid)
            });
        let out = match run {
            Ok(r) => {
                job.trace.note_detail(fid, "stderr", &r.stderr);
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
        let warn = emit_mid_outputs(state, p, job, fid, &spec, spec_json.as_deref(), &out, report);
        let r = finish_eval(p, job, fid, visited, memo, 0, out);
        if let Some(w) = warn {
            mark_last_run(p, fid, "warn", &w);
        }
        return r;
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
        return finish_eval(p, job, fid, visited, memo, 0, out);
    }

    // v1.8 ④反復カード（builtin:"loop"）: 上流サブグラフを条件が満たされるまで引き直す。
    // **入力の収集より手前**に置くのが要: 収集は反復カードが周回番号つきで自分で呼ぶ。
    // ここで先に1回収集してしまうと、その周は番号が付かず、上流の記録カード（tap）が
    // 1周目を番号なしの名前で書いてしまう。
    if spec.builtin.as_deref() == Some("loop") {
        let out = run_loop_node(state, app, p, fid, &spec, visited, memo, report, job, ctx)?;
        return finish_eval(p, job, fid, visited, memo, 0, out);
    }

    // v1.8 ③ スイッチ（実機FB 2026-08-08）: 「答えが入ったら上流を動かさない」。
    // **収集より手前**でなければ意味がない — 収集した時点で上流の AI はもう走っている。
    // 人がもう判断した後に材料を引き直すと、原案が別物に書き換わってしまう。
    if spec.builtin.as_deref() == Some("ask") && spec.skip_upstream.as_deref() == Some("on") {
        if let Some(note) = peek_answer_note(p, fid) {
            let answer = flow::answer_text(&note);
            if !answer.is_empty() {
                let out = ask_answer_out(&spec, &node_label(p, fid), answer);
                return finish_eval(p, job, fid, visited, memo, 0, out);
            }
        }
    }

    // フェーズ1: 入力エッジの収集（v1.8 ④で切り出し — 反復カードが同じ収集を何周も呼ぶ）。
    let CollectedInputs { edges, role_system, role_answer } =
        collect_input_edges(state, app, p, fid, visited, memo, report, job, ctx)?;

    let mut produced = Vec::new();

    // セレクタ（§5.4.1）: 実行せず「最初に有データを返した枝」を採用。no-data 吸収を内包し、
    // 全滅なら自分が no-data（エラーではない — 下流やレポートが no-data として扱う）。
    // v1.5.7 セレクタ設定: 採用後に (1) 失敗枝ならエラー時メッセージへ差し替え（fail_text）、
    // (2) 出力名（prefix）で stem/拡張子を宣言名に差し替え、してから下流へ流す。
    if spec.node_kind() == flow::NodeKind::Select {
        // v1.8.7 トレース: 各枝の状態と採用した枝を残す（セレクタは「どれが選ばれたか」が要）。
        {
            let all: Vec<FlowPayload> =
                edges.iter().filter(|e| !e.nodata).flat_map(|e| e.payloads.clone()).collect();
            job.trace.inputs(fid, &all);
            let states: Vec<String> = edges
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let st = if e.nodata {
                        "no-data"
                    } else if e.payloads.is_empty() {
                        "空"
                    } else if e.failure {
                        "失敗枝"
                    } else {
                        "有データ"
                    };
                    format!("枝{}={st}", i + 1)
                })
                .collect();
            job.trace.note(fid, "枝の状態", &states.join(" / "));
            let adopted = edges
                .iter()
                .position(|e| !e.nodata && !e.payloads.is_empty())
                .map(|i| format!("枝{}", i + 1))
                .unwrap_or_else(|| "なし（全枝 no-data → 自分も no-data）".into());
            job.trace.note(fid, "採用した枝", &adopted);
        }
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
        return finish_eval(p, job, fid, visited, memo, 0, out);
    }

    // no-data の NULL 伝搬（§5.1-4）: 伝搬型（既定）は入力エッジに1つでも no-data が
    // あれば自分も no-data（実行しない）。吸収型（nodata:"absorb"）は欠けを落として続行。
    if !spec.absorbs_nodata() && edges.iter().any(|e| e.nodata) {
        return finish_eval(p, job, fid, visited, memo, 0, EvalOut::nodata_out());
    }
    let all_nodata = !edges.is_empty() && edges.iter().all(|e| e.nodata);
    if all_nodata {
        // 吸収型でも材料が全滅なら no-data（実行するものがない）。
        return finish_eval(p, job, fid, visited, memo, 0, EvalOut::nodata_out());
    }
    let collected: Vec<FlowPayload> = edges
        .into_iter()
        .filter(|e| !e.nodata)
        .flat_map(|e| e.payloads)
        .collect();
    // v1.8.7 トレース: このノードが実際に受け取った入力。
    job.trace.inputs(fid, &collected);

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
        return finish_eval(p, job, fid, visited, memo, 0, out);
    }

    // v1.8 役割ポート: system 役割の矢印が来ていたら、このノードの spec を差し替える。
    // spec を作り替えるだけなので execute_node 以下（run_ai）は無改造で乗る。
    let spec = match role_system {
        Some(sys) => {
            if spec.node_kind() == flow::NodeKind::Ai {
                job.trace.note_detail(fid, "役割（システムプロンプト）", &sys);
                let mut s = spec.clone();
                s.system = Some(sys);
                s
            } else {
                report
                    .skipped
                    .push("役割（システムプロンプト）の矢印は AI カードにだけ効きます".into());
                spec
            }
        }
        None => spec,
    };

    // v1.8 ③問い合わせカード（builtin:"ask"）: 途中で人に聞く。答えが無ければ no-data で
    // 止まり、答えがあればそれを下流へ流す。tap と同じ「eval 側で完結」の流儀。
    if spec.builtin.as_deref() == Some("ask") {
        let out = run_ask_node(p, fid, &spec, role_answer.as_deref(), &collected, report);
        return finish_eval(p, job, fid, visited, memo, 0, out);
    }

    // v1.8 記録カード（builtin:"tap"）: 入力をそのまま下流へ通しつつ、通った中身を
    // プロジェクトへ書き出してカードにする。中間結果は本来メモリ上で消えるので、
    // 「1周流した後もグラフが読める」ようにするための素通しノード。
    // import_new と同じ流儀で eval 側で完結させる（DB への作用は execute_node の
    // バイト変換契約に乗せない）。集約/ファンアウトの手前で返すので入力は何本でもよい。
    if spec.builtin.as_deref() == Some("tap") {
        let out = if collected.is_empty() {
            EvalOut::nodata_out()
        } else {
            match write_tap_record(p, fid, &spec, &collected, &ctx.round_suffix()) {
                Ok(names) => {
                    for n in names {
                        report.outputs.push(format!("{n}（記録）"));
                    }
                }
                Err(e) => report.errors.push(e),
            }
            EvalOut::data(collected, None)
        };
        return finish_eval(p, job, fid, visited, memo, 0, out);
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
    // T2i / T23d は集約ノード（プロンプト＋元画像など全入力を集めて1回実行＝1つ生成）。
    if spec.is_aggregate() || kind == flow::NodeKind::T2i || kind == flow::NodeKind::T23d {
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
                execute_node(root, &spec, kind, &ins, job, fid)
            });
            match run {
                Ok(run) => {
                    last_exit = run.exit_code;
                    job.trace.note_detail(fid, "stderr", &run.stderr);
                    for (bytes, ext, mime, stem_o, meta) in run.outputs {
                        let stem = stem_o.unwrap_or_else(|| stem0.clone());
                        produced.push(FlowPayload { bytes, ext, mime, stem, meta });
                    }
                }
                Err(e) => errs.push(format!("{stem0}: {e}")),
            }
        }
    } else {
        // v1.7 P1/P3/P4 web_*: 入力接続が無くてもパラメータ欄の URL だけで実行できる
        // （t2i のパネル単独生成と同型）。入力があるときは従来のファンアウト＝1入力1URL。
        if collected.is_empty()
            && matches!(
                spec.builtin.as_deref(),
                Some("web_fetch") | Some("web_feed") | Some("web_capture") | Some("web_search")
            )
        {
            let run = acquire_node_permits(state, app, job, &spec, kind).and_then(|_permits| {
                emit_job_progress(app, job, Some(&node_note(p, fid)), None);
                execute_node(root, &spec, kind, &[], job, fid)
            });
            match run {
                Ok(run) => {
                    last_exit = run.exit_code;
                    job.trace.note_detail(fid, "stderr", &run.stderr);
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
                execute_node(root, &spec, kind, &ins, job, fid)
            });
            match run {
                Ok(run) => {
                    last_exit = run.exit_code;
                    job.trace.note_detail(fid, "stderr", &run.stderr);
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
    // Web検索 P2: 検索結果が1件も無い＝材料が無い → no-data（下流の AI を空で走らせない）。
    let search_empty =
        errs.is_empty() && produced.is_empty() && spec.builtin.as_deref() == Some("web_search");
    if search_empty {
        report.skipped.push(format!(
            "{}（Web検索の結果が0件でした — 検索語や条件を見直してください）",
            node_note(p, fid)
        ));
    }
    let out = if search_empty {
        EvalOut::nodata_out()
    } else if errs.is_empty() {
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
    let warn = emit_mid_outputs(state, p, job, fid, &spec, spec_json.as_deref(), &out, report);
    let r = finish_eval(p, job, fid, visited, memo, skips, out);
    if let Some(w) = warn {
        mark_last_run(p, fid, "warn", &w);
    }
    r
}

/// v1.8.7 途中出力スイッチ（`emit_mid:"on"`）の本体: このカードがパイプラインの**途中**で
/// 評価されて成功したら、出力を終端と同じ規則でキャンバスへ着地させる。
/// - 終端カード自身は run_flow_inner が着地させるので対象外（二重に出さない）
/// - フロー以外のジョブ（AI チャットの文脈評価など）では出さない（副作用を持ち込まない）
/// - 失敗・no-data・出力なしは出さない（出すものがない）
#[allow(clippy::too_many_arguments)]
fn emit_mid_outputs(
    state: &AppState,
    p: &Project,
    job: &Job,
    fid: &str,
    spec: &flow::FilterSpec,
    fref: Option<&str>,
    out: &EvalOut,
    report: &mut FlowReport,
) -> Option<String> {
    if spec.emit_mid.as_deref() != Some("on") {
        return None;
    }
    if job.kind != JobKind::Flow || job.card.as_deref() == Some(fid) {
        return None;
    }
    if !out.ok || out.nodata || out.payloads.is_empty() {
        return None;
    }
    let Ok((fx, fy, label)) = p.conn.query_row(
        "SELECT pos_x, pos_y, label FROM virtual_bodies WHERE id = ?1",
        rusqlite::params![fid],
        |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?)),
    ) else {
        return None;
    };
    let before = report.outputs.len();
    match materialize_payloads(
        state, p, &p.root, fid, fx, fy, &label, fref, spec, out.payloads.clone(), true, report,
    ) {
        Ok(()) => {
            let names = report.outputs[before..].join(", ");
            job.trace.note(fid, "キャンバスへ出力（途中出力スイッチ）", &names);
            None
        }
        Err(e) => {
            job.trace.note(fid, "キャンバスへ出力（失敗）", &e);
            report.errors.push(format!("{label}（途中出力）: {e}"));
            Some(format!("途中出力をキャンバスに出せませんでした: {e}"))
        }
    }
}


/// フェーズ1の収集結果。通常入力の列と、役割ポートが運んできたもの。
pub(crate) struct CollectedInputs {
    edges: Vec<EdgeOut>,
    /// `role:"system"` が運んできたテキスト（AI カードのシステムプロンプト）。
    role_system: Option<String>,
    /// v1.8 ③ `role:"answer"` が運んできたテキスト（人の答え）。
    /// **None = 答え箱の付箋がそもそも繋がっていない**／`Some("")` = 繋がっているが未記入。
    role_answer: Option<String>,
}

/// フェーズ1（入力の収集）: このノードへ入る flow 矢印を順に評価し、when/carry を適用した
/// `EdgeOut` の列と、役割ポート（`role:"system"` / `role:"answer"`）が運んだテキストを返す。
///
/// v1.8 ④で eval_filter の本体から切り出した。**反復カードが同じ収集を何周も呼ぶ**ため
/// （周ごとにメモを消して引き直す＝計画 §4 案B）、1か所に閉じている必要がある。
/// 切り出しにあたって挙動は変えていない。
#[allow(clippy::too_many_arguments)]
fn collect_input_edges(
    state: &AppState,
    app: &tauri::AppHandle,
    p: &Project,
    fid: &str,
    visited: &mut std::collections::HashSet<String>,
    memo: &mut EvalMemo,
    report: &mut FlowReport,
    job: &Job,
    ctx: &mut LoopCtx,
) -> Result<CollectedInputs, String> {
    let cancel = &job.cancel;
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

    // エッジごとに上流を評価し when/carry を適用（EdgeOut の列で保持＝セレクタの
    // first-match と no-data の NULL 伝搬はエッジ単位の情報が要る）。
    let mut edges: Vec<EdgeOut> = Vec::new();
    // v1.8 役割ポート: role="system" の矢印が運んできたテキスト（AI のシステムプロンプト）。
    let mut role_system: Option<String> = None;
    // v1.8 ③: role="answer" の矢印（答え箱の付箋）が運んできたテキスト。
    let mut role_answer: Option<String> = None;
    for (vid, real_id, vlabel, mime, ext, kind, url_ref, attrs) in &inputs {
        // 中断後は新しいプロセスを起こさない（複数入力の連鎖を断つ）
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            report.errors.push("中断されました".into());
            break;
        }
        // v1.8 ④反復カードの戻り矢印（role:"previous"）: **上流を評価しない**のがこの分岐の
        // 全部。値は反復カードが文脈へ置いた前周の出力から取る。ここで遡らないから、
        // キャンバスに輪が描いてあっても評価は DAG のまま（循環判定にも pull にも触らない）。
        // 1周目は前周が無いので空を運ぶ — no-data にすると伝搬型の下流が丸ごと止まり、
        // 1周目が実行されずに反復そのものが始まらなくなる。
        if attrs.role.as_deref() == Some("previous") {
            let payloads = ctx.prev.get(vid).cloned().unwrap_or_default();
            edges.push(EdgeOut { payloads, nodata: false, failure: false });
            continue;
        }
        // 上流の評価結果（EvalOut）を得る: フィルタ＝再帰評価（メモ化）／データカード＝窓の今
        // v1.5.6: 上流が IN/OUT バーチャルカード（kind='op'）なら再帰評価＝透過。
        // req レーンの op（トリガ等）は flow 矢印を持たないのでここには現れない。
        let up: EvalOut = if matches!(kind.as_deref(), Some("filter") | Some("op")) {
            match eval_filter(state, app, p, vid, visited, memo, report, job, ctx) {
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
        // v1.8 役割ポート: role 付きの矢印は「入力」ではなく「設定」を運ぶ。通常入力の列
        // （edges）に入れないので、AI カードのファンアウト（入力ごとに1回実行）を起こさない。
        // when/carry は適用しない — 役割は条件分岐する対象ではなく直結の配線だから。
        if let Some(role) = attrs.role.as_deref() {
            let joined = || {
                up.payloads
                    .iter()
                    .map(|pl| String::from_utf8_lossy(&pl.bytes).into_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            match role {
                "system" => {
                    let text = joined();
                    if !text.trim().is_empty() {
                        role_system = Some(text);
                    }
                }
                // v1.8 ③: 空でも Some を入れる — 「繋がっているが未記入」と「そもそも
                // 繋がっていない」は扱いが違う（後者にだけ答え箱の付箋を作ってやる）。
                "answer" => role_answer = Some(joined()),
                _ => {}
            }
            continue;
        }
        edges.push(apply_edge(&up, attrs, vlabel, report));
    }
    Ok(CollectedInputs { edges, role_system, role_answer })
}

/// v1.8 ④反復カード（`builtin:"loop"`）の実行（計画 §4 案B — DAG を保つ反復）。
///
/// 上流サブグラフを最大 `max_rounds` 周まで**引き直す**。周と周のあいだにやることは2つだけ:
/// ①上流のメモを消す（消さないと同じ結果が配られて2周目が実行されない）
/// ②前周の出力を文脈へ置く（戻り矢印 `role:"previous"` がそこから材料を取る）。
///
/// 反復は「1枚のカードの内部実装」に閉じており、グラフは DAG のまま＝可視性・再現性を
/// 崩さない。停止判定も**このカードの内部制御**であって「グラフの分岐」ではないので、
/// V2.0課題 §8-1（AI は判定器・分岐は push 側・pull は分岐と相性が悪い）と衝突しない。
#[allow(clippy::too_many_arguments)]
fn run_loop_node(
    state: &AppState,
    app: &tauri::AppHandle,
    p: &Project,
    fid: &str,
    spec: &flow::FilterSpec,
    visited: &mut std::collections::HashSet<String>,
    memo: &mut EvalMemo,
    report: &mut FlowReport,
    job: &Job,
    ctx: &mut LoopCtx,
) -> Result<EvalOut, String> {
    let label = node_label(p, fid);
    // ヒューズ（§8-3「反復は max_fires 相当必須」）: 未指定=3。0 や巨大値は丸める。
    let max = spec.max_rounds.unwrap_or(3).clamp(1, flow::LOOP_MAX_ROUNDS);
    // 引き直しの範囲＝この反復カードの上流サブグラフ。戻り矢印は辿らない（辿ると
    // 反復カード自身が「自分の上流」に見えてしまう）。
    let upstream = upstream_ids(p, fid);

    let mut cur: Vec<FlowPayload> = Vec::new();
    let mut done = 0u32;
    let mut reason = "上限まで回りました";
    for round in 1..=max {
        if job.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            reason = "中断されました";
            break;
        }
        if round > 1 {
            // 前周の結果を戻り矢印へ渡してから、上流のメモを捨てて引き直す。
            ctx.prev.insert(fid.to_string(), cur.clone());
            for id in &upstream {
                memo.remove(id);
            }
        }
        emit_job_progress(app, job, Some(&format!("{label}: {round}周目")), None);
        ctx.rounds.push(round);
        let collected = collect_input_edges(state, app, p, fid, visited, memo, report, job, ctx)
            .map(|CollectedInputs { edges, .. }| {
                // 材料が全滅した周は「そこで打ち切り」。1周目なら反復そのものが no-data。
                if edges.is_empty() || edges.iter().all(|e| e.nodata) {
                    None
                } else {
                    Some(
                        edges
                            .into_iter()
                            .filter(|e| !e.nodata)
                            .flat_map(|e| e.payloads)
                            .collect::<Vec<_>>(),
                    )
                }
            });
        ctx.rounds.pop();
        let collected = match collected? {
            Some(v) => v,
            None => {
                if round == 1 {
                    ctx.prev.remove(fid);
                    return Ok(EvalOut::nodata_out());
                }
                reason = "材料が尽きました";
                break;
            }
        };
        cur = collected;
        done = round;
        // 停止判定（§8-2 の判定2段のうち①決定的ルール）。判定対象はその周の出力テキスト全体。
        match loop_should_stop(spec, &cur) {
            Ok(true) => {
                reason = "停止条件を満たしました";
                break;
            }
            Ok(false) => {}
            Err(e) => {
                // 判定器が壊れている（正規表現が不正など）。黙って回し続けない。
                ctx.prev.remove(fid);
                return Ok(EvalOut::failed(vec![format!("{label}: {e}")], None, vec![]));
            }
        }
    }
    ctx.prev.remove(fid);
    report.outputs.push(format!("{label}: {done}周（{reason}）"));
    Ok(EvalOut::data(cur, None))
}

/// 反復の停止判定。`until` 未指定なら回数だけで止める（＝常に false）。
fn loop_should_stop(spec: &flow::FilterSpec, payloads: &[FlowPayload]) -> Result<bool, String> {
    let Some(mode) = spec.until.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    let pattern = spec.pattern.as_deref().unwrap_or("");
    // 語・正規表現を要るモードでパターンが空だと、`contains("")` も `regex("")` も
    // **常に真**＝1周で止まる。設定の書き落としが「なぜか繰り返さない」に化けるので、
    // 判定する前にエラーにする（反復カードの last_run とレポートに理由が出る）。
    if pattern.is_empty()
        && matches!(mode, "contains" | "not_contains" | "regex" | "not_regex")
    {
        return Err(format!("停止条件（{mode}）に語・正規表現が設定されていません"));
    }
    let text = payloads
        .iter()
        .map(|pl| String::from_utf8_lossy(&pl.bytes).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    flow::judge_text(mode, pattern, &text)
}

/// 反復カードの上流サブグラフ（周ごとにメモを捨てる範囲）を後ろ向きに集める。
/// 戻り矢印（`role:"previous"`）は辿らない＝ここも DAG として歩く。
fn upstream_ids(p: &Project, fid: &str) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack = vec![fid.to_string()];
    let mut out = Vec::new();
    while let Some(cur) = stack.pop() {
        let Ok(mut stmt) = p.conn.prepare(
            "SELECT rel.from_id, rel.attrs FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
             WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL",
        ) else {
            break;
        };
        let ins: Vec<(String, Option<String>)> = stmt
            .query_map(rusqlite::params![cur], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        for (from, attrs) in ins {
            if flow::parse_edge_attrs(attrs.as_deref()).role.as_deref() == Some("previous") {
                continue;
            }
            if seen.insert(from.clone()) {
                out.push(from.clone());
                stack.push(from);
            }
        }
    }
    out
}

/// 答え箱の付箋に書き写す材料の上限（行数・文字数）。付箋として読める量に留める。
const ANSWER_QUOTE_LINES: usize = 40;
const ANSWER_QUOTE_CHARS: usize = 1200;

/// 答え箱の付箋の「ここから下は人のもの」を示す目印。**この行より上だけ**を実行のたびに
/// 入れ替える（下には人が書いた案内や答えが残る）。文言を変えると過去の付箋で目印が
/// 見つからなくなり、自動生成分が二重に積まれるので変えないこと。
const ANSWER_NOTE_MARK: &str = "── ここから下に答えを書いてください";

/// 材料の引用に添える案内。目印の直後に置かれる自動生成分なので、入れ直すときは
/// 人の書き足しと区別してここで捨てる（新しいものが改めて入る）。
const ANSWER_NOTE_GUIDE: &str = "上の引用は、聞かれたときに届いていた材料の控えです";

/// 答え箱の付箋の本文を組む（質問＋そのとき届いていた材料＋目印＋案内）。
///
/// 本文は**すべて `>` の引用**にする。素の行を書くと `flow::answer_text` がそれを
/// 「人の答え」として読んでしまい、誰も答えていないのに下流が動き出す。
fn answer_note_text(question: &str, collected: &[FlowPayload]) -> String {
    let quote = |s: &str| -> String {
        s.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n")
    };
    let mut text = quote(question);
    let mut guide = String::new();
    // 届いていた材料（テキストのものだけ）を引用で添える。付箋として読める長さに切る
    // ——全文は記録カードや実身で読めるので、ここは「何の話か思い出せる」量でよい。
    let material = collected
        .iter()
        .filter(|pl| pl.mime.starts_with("text/"))
        .map(|pl| String::from_utf8_lossy(&pl.bytes).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let material = material.trim();
    if !material.is_empty() {
        let mut shown: String = material.lines().take(ANSWER_QUOTE_LINES).collect::<Vec<_>>().join("\n");
        let trimmed = shown.chars().count() > ANSWER_QUOTE_CHARS
            || material.lines().count() > ANSWER_QUOTE_LINES;
        if shown.chars().count() > ANSWER_QUOTE_CHARS {
            shown = shown.chars().take(ANSWER_QUOTE_CHARS).collect();
        }
        text.push_str("\n>\n");
        text.push_str(&quote(&shown));
        if trimmed {
            text.push_str("\n> …（以下略）");
        }
        guide.push_str(&format!("（{ANSWER_NOTE_GUIDE}）"));
    }
    // 使い方の注釈もこの付箋に置く。実機フィードバック（2026-08-08）＝「答えはここに書く」
    // 「自分で用意した付箋を答えにしてもよい」が、面のどこにも書いていなかった。
    // **引用行に混ぜる**のがこの設計の効くところ: `>` 行は答えとして読まないので、
    // 案内を書いても「答えたこと」にならず、書かれたまま残って次に見る人にも読める。
    text.push_str(&format!(
        "\n>\n> {ANSWER_NOTE_MARK}（「>」で始まる行は答えになりません）"
    ));
    if !guide.is_empty() {
        text.push_str(&format!("\n> {guide}"));
    }
    text
}

/// v1.8.4: **既にある**答え箱の付箋へ、いま届いている材料を入れ直す。
///
/// 呼ばれるのは「答えがまだ空」のときだけ（`run_ask_node`）。目印 `ANSWER_NOTE_MARK` の
/// 行より下は人の領分なのでそのまま残し、上（質問と材料の引用）だけを差し替える。
/// 目印が無い付箋——エージェントや人が自分で置いた答え箱——は全体が人の書いたものなので、
/// **消さずに新しい引用を上へ載せる**。次からは目印が挟まっているので二重にはならない。
fn refresh_answer_note(
    p: &Project,
    fid: &str,
    question: &str,
    collected: &[FlowPayload],
) -> Result<Option<String>, String> {
    // 材料が1つも無いなら書き換える意味がない（空の引用で人の付箋を荒らさない）。
    if !collected.iter().any(|pl| pl.mime.starts_with("text/")) {
        return Ok(None);
    }
    // 答え箱の特定は peek_answer_note と同じ流儀（attrs を読んで role を見る）。
    let mut stmt = p
        .conn
        .prepare(
            "SELECT v.id, v.note_text, rel.attrs FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
             WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
               AND v.kind = 'note'
             ORDER BY rel.created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map(rusqlite::params![fid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    let found = rows.into_iter().find(|(_, _, attrs)| {
        flow::parse_edge_attrs(attrs.as_deref()).role.as_deref() == Some("answer")
    });
    let Some((vid, old, _)) = found else {
        return Ok(None); // 付箋以外（実身など）が答えに繋がっている＝触らない
    };
    let old = old.unwrap_or_default();
    // 自動生成分は [質問・材料・目印・材料の案内] の4部で、**その後ろが人の領分**。
    // 目印が無い付箋は丸ごと人の書いたものなので、消さずに下へ送る（次からは目印が挟まる）。
    let mine = match old.lines().position(|l| l.contains(ANSWER_NOTE_MARK)) {
        Some(i) => old
            .lines()
            .skip(i + 1)
            .filter(|l| !l.contains(ANSWER_NOTE_GUIDE))
            .collect::<Vec<_>>()
            .join("\n"),
        None if old.trim().is_empty() => String::new(),
        None => old.trim_end().to_string(),
    };
    let fresh = answer_note_text(question, collected);
    let text = if mine.trim().is_empty() {
        fresh
    } else {
        format!("{fresh}\n{mine}")
    };
    if text == old {
        return Ok(None); // 変化なし（毎回 UPDATE して面をちらつかせない）
    }
    p.conn
        .execute(
            "UPDATE virtual_bodies SET label = ?1, note_text = ?2 WHERE id = ?3",
            rusqlite::params![crate::notes::note_label(&text), text, vid],
        )
        .map_err(|e| e.to_string())?;
    Ok(Some(vid))
}

/// v1.8 ③問い合わせカード（`builtin:"ask"`）— 途中で人に聞く（計画 §3 案B）。
///
/// **ドロップポイントの「問い版」**。あちらが人のファイル投入待ち、こちらが人の言葉待ち。
/// 答えは**答え箱の付箋**（`role:"answer"` の矢印で繋がった付箋）に置く。付箋にしたのは、
/// 答えが面から読めて・その場で直せて・プロジェクトに残るから（水泡に帰さない設計と整合）。
///
/// - 答えあり → その文字列を text/plain で下流へ。
/// - 答えなし → **no-data**（エラーではない）。付箋がまだ無ければ**作って矢印まで引く**ので、
///   人は現れた付箋に書いて ▶ を押し直すだけでよい。
///
/// 案C（止めて待つ＝実行中のジョブへ答えを流し込む）へ進むときも、答えの置き場はここで
/// できた付箋のままでよい。永続化を先に済ませてあるのがこの順序の狙い（計画 §3）。
fn run_ask_node(
    p: &Project,
    fid: &str,
    spec: &flow::FilterSpec,
    role_answer: Option<&str>,
    collected: &[FlowPayload],
    report: &mut FlowReport,
) -> EvalOut {
    let label = node_label(p, fid);
    let question = spec
        .question
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(label.as_str())
        .to_string();

    // v1.8.4: **材料の矢印は繋がっているのに中身が空**＝まだ聞く段階ではない。
    // 2パスのフロー（①原案を作る ②人が判断して仕上げる）で、②を先に押したときに
    // ここで静かに止まる。答えが先に書いてあっても止める —— 何も無いものに対する
    // 「採用」を下流へ流すと、下流の AI は**承認された中身を知らないまま**書き始める
    // （実機で踏んだ: 人が読んでいない文書に判子を押した形になった・2026-08-27）。
    //
    // 入力の矢印が1本も無い場合は従来どおり許す（質問だけのカードは正当・§3-12）。
    let material_empty = !collected.is_empty()
        && collected.iter().all(|pl| {
            if pl.mime.starts_with("text/") {
                String::from_utf8_lossy(&pl.bytes).trim().is_empty()
            } else {
                pl.bytes.is_empty()
            }
        });
    if material_empty {
        report.skipped.push(format!(
            "{label}（材料がまだありません — 先に上流を実行してから、もう一度どうぞ）"
        ));
        return EvalOut::nodata_out();
    }

    // 答え箱の中身から、人が書いた分だけを取り出す（`>` 行＝質問は答えではない）。
    let answer = role_answer.map(flow::answer_text).unwrap_or_default();
    if !answer.is_empty() {
        return ask_answer_out(spec, &label, answer);
    }

    // 未回答。付箋がまだ無ければ用意する（無いまま「答えてください」と言われても困る）。
    // v1.8.4: 付箋が**既にある**ときも材料を入れ直す。エージェントや人があらかじめ
    // 答え箱を置いておく配線（規約 §3-12 が勧める形）だと従来ここへ来ず、材料の引用が
    // 一度も入らなかった —— 人は判断のたびに別のカードを開きに行く羽目になっていた
    // （2026-08-27 実機）。答えが空なら付箋の中身は自動生成分と案内だけなので、
    // 入れ替えても人の書いたものは消えない。
    if role_answer.is_none() {
        match create_answer_note(p, fid, &question, collected) {
            Ok(()) => report.outputs.push(format!("{label}: 答えの付箋を用意しました")),
            Err(e) => report.errors.push(format!("{label}: 答えの付箋を作れません（{e}）")),
        }
    } else {
        match refresh_answer_note(p, fid, &question, collected) {
            // 中身が変わった付箋には緑シグナルを付ける（固定名上書きのカードと同じ流儀）。
            Ok(Some(vid)) => report.updated.push(vid),
            Ok(None) => {}
            Err(e) => report.errors.push(format!("{label}: 答えの付箋を更新できません（{e}）")),
        }
    }
    report
        .skipped
        .push(format!("{label}（答え待ち: {question} — 付箋に答えを書いて、もう一度実行してください）"));
    EvalOut::nodata_out()
}

/// 人の答え1本を下流へ流す EvalOut（名前は `prefix` かカード名）。
fn ask_answer_out(spec: &flow::FilterSpec, label: &str, answer: String) -> EvalOut {
    let stem = spec
        .prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(label)
        .to_string();
    EvalOut::data(
        vec![FlowPayload {
            bytes: answer.into_bytes(),
            ext: "txt".into(),
            mime: "text/plain".into(),
            stem,
            meta: None,
        }],
        None,
    )
}

/// v1.8 ③ スイッチ `skip_upstream:"on"` 用: **上流を1枚も評価せずに**答えだけ覗く。
///
/// 答え箱に使うのは付箋（`role:"answer"`）なので、`note_text` を直接読めば評価は要らない。
/// 付箋以外が繋がっているときは None を返して通常の評価に落ちる（中身が評価しないと
/// 決まらないため）。**評価しないことがこの関数の目的**なので、ここで手を抜いてはいけない。
fn peek_answer_note(p: &Project, fid: &str) -> Option<String> {
    let mut stmt = p
        .conn
        .prepare(
            "SELECT v.note_text, rel.attrs FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
             WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
               AND v.kind = 'note'
             ORDER BY rel.created_at",
        )
        .ok()?;
    let rows: Vec<(Option<String>, Option<String>)> = stmt
        .query_map(rusqlite::params![fid], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    rows.into_iter()
        .find(|(_, attrs)| {
            flow::parse_edge_attrs(attrs.as_deref()).role.as_deref() == Some("answer")
        })
        .map(|(text, _)| text.unwrap_or_default())
}

/// 答え箱の付箋を問い合わせカードの横に作り、`role:"answer"` の矢印で繋ぐ。
///
/// 本文は**すべて `>` の引用**（質問と、そのとき届いていた材料）で、**答え欄は空のまま**。
/// 案内文を素で書くと、それがそのまま「答え」として下流へ流れてしまうため。
///
/// 材料まで書き写すのは、**人が何を見て答えたのかを残す**ため。AI の案について聞くとき、
/// 引用が無いと付箋には答えだけが残り、後から読んでも何の話か分からない。
/// 作るのは1回だけで、以後この付箋には触らない（人が書いた答えを消さない）。
fn create_answer_note(
    p: &Project,
    fid: &str,
    question: &str,
    collected: &[FlowPayload],
) -> Result<(), String> {
    let (fx, fy): (f64, f64) = p
        .conn
        .query_row(
            "SELECT pos_x, pos_y FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![fid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let text = answer_note_text(question, collected);
    let id = crate::util::uuid();
    let style = serde_json::json!({ "width": 300.0, "font_size": 13.0 }).to_string();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, note_text, style_ref)
             VALUES (?1, 'note:sentinel', ?2, ?3, ?4, ?5, 'note', ?6, ?7)",
            rusqlite::params![
                id,
                crate::notes::note_label(&text),
                fx - 340.0,
                fy,
                db::now_ms(),
                text,
                style
            ],
        )
        .map_err(|e| e.to_string())?;
    p.conn
        .execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at, attrs)
             VALUES (?1, ?2, 'flow', ?3, '{\"role\":\"answer\"}')",
            rusqlite::params![id, fid, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// カードのラベル（進捗・レポートの文言用。見つからなければ id）。
fn node_label(p: &Project, fid: &str) -> String {
    p.conn
        .query_row(
            "SELECT label FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![fid],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_else(|_| fid.to_string())
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

    /// v1.7 P3: 既読台帳（feed_seen_guids）— このフィルタの出力カードの guid を集める。
    /// 削除済みカードの guid も既読に数える（読んで消した記事が再実行で蘇らない）。
    /// 他のフィルタの出力・guid 無しメタ（web_fetch）は混ざらない。
    #[test]
    fn feed_seen_guids_ledger() {
        let dir = std::env::temp_dir().join(format!("texos_feed_guids_{}", db::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = db::open(&dir.join("texos.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO real_bodies (id, mime_type, size, created_at)
               VALUES ('h1', 'text/markdown', 1, 1);
             INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, web_meta, kind)
               VALUES
               ('e1', 'h1', 'A.md', 0, 0, 1, '{\"guid\":\"g-a\",\"source_url\":\"https://x/a\"}', 'file'),
               ('e2', 'h1', 'B.md', 0, 0, 2, '{\"guid\":\"g-b\"}', 'file'),
               ('e3', 'h1', 'C.md', 0, 0, 3, '{\"source_url\":\"https://x/c\"}', 'file'),
               ('e4', 'h1', 'D.md', 0, 0, 4, '{\"guid\":\"g-other\"}', 'file');
             -- e2 は削除済みカード（既読のまま数える）
             UPDATE virtual_bodies SET deleted_at = 9 WHERE id = 'e2';
             INSERT INTO relations (from_id, to_id, kind, created_at) VALUES
               ('feed1', 'e1', 'flow', 1),
               ('feed1', 'e2', 'flow', 2),
               ('feed1', 'e3', 'flow', 3),
               ('feed2', 'e4', 'flow', 4);",
        )
        .unwrap();
        let seen = feed_seen_guids(&conn, "feed1");
        assert!(seen.contains("g-a"));
        assert!(seen.contains("g-b"), "削除済みカードの guid も既読");
        assert!(!seen.contains("g-other"), "他フィルタの出力は混ざらない");
        assert_eq!(seen.len(), 2, "guid 無しメタは数えない: {seen:?}");
        assert!(feed_seen_guids(&conn, "no_such").is_empty());
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1.7 P5: リンクの受け皿（url_sink_ids / update_url_sink）と既出台帳（url_output_seen）。
    /// - 受け皿: フィルタ→URLカードの矢印列挙・url_ref 更新・ラベル追従（旧URL名のときだけ）・
    ///   URL変更時に旧系譜と更新マークを畳む。
    /// - 既出台帳: このフィルタが出した URL は削除済みでも既出（消したリンクは蘇らない）。
    #[test]
    fn url_sink_and_seen_ledger() {
        let dir = std::env::temp_dir().join(format!("texos_url_sink_{}", db::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = db::open(&dir.join("texos.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO real_bodies (id, mime_type, size, created_at) VALUES
               ('url:sentinel', 'application/vnd.texos.url', 0, 1),
               ('filter:sentinel', 'application/vnd.texos.filter', 0, 1);
             INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, url_ref, kind) VALUES
               ('u1', 'url:sentinel', 'https://old.jp/', 0, 0, 1, 'https://old.jp/', 'url'),
               ('u2', 'url:sentinel', '定点ページ', 0, 0, 1, 'https://old.jp/', 'url'),
               ('u3', 'url:sentinel', 'A', 0, 0, 1, 'https://ex.jp/a', 'url');
             INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind) VALUES
               ('f1', 'filter:sentinel', '置換', 0, 0, 1, 'filter'),
               ('t1', 'filter:sentinel', 'URL化', 0, 0, 1, 'filter');
             INSERT INTO relations (from_id, to_id, kind, created_at) VALUES
               ('f1', 'u1', 'flow', 1),
               ('f1', 'u2', 'flow', 2),
               ('u1', 'f0_dummy', 'fetched', 3),
               ('t1', 'u3', 'flow', 4);
             UPDATE virtual_bodies SET web_meta = '{\"update\":true}' WHERE id = 'u1';",
        )
        .unwrap();
        // 受け皿の列挙（矢印の張られた URL カードだけ・接続順）
        assert_eq!(url_sink_ids(&conn, "f1"), vec!["u1".to_string(), "u2".to_string()]);
        assert!(url_sink_ids(&conn, "no_such").is_empty());
        // 更新: u1 はラベル=旧URL → 追従。u2 はタイトル付き → 名前を保つ。
        let l1 = update_url_sink(&conn, "u1", "https://new.jp/x").unwrap();
        assert_eq!(l1, "https://new.jp/x");
        let l2 = update_url_sink(&conn, "u2", "https://new.jp/x").unwrap();
        assert_eq!(l2, "定点ページ");
        let (url1, meta1): (String, Option<String>) = conn
            .query_row(
                "SELECT url_ref, web_meta FROM virtual_bodies WHERE id = 'u1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(url1, "https://new.jp/x");
        assert!(meta1.is_none(), "URL変更で更新マークは畳む");
        let live_fetched: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relations
                 WHERE from_id = 'u1' AND kind = 'fetched' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live_fetched, 0, "URL変更で旧系譜は畳む");
        // 既出台帳: t1 が出した https://ex.jp/a は既出。削除しても既出のまま。
        assert!(url_output_seen(&conn, "t1", "https://ex.jp/a"));
        assert!(!url_output_seen(&conn, "t1", "https://ex.jp/b"));
        assert!(!url_output_seen(&conn, "f1", "https://ex.jp/a"), "フィルタ単位の台帳");
        conn.execute("UPDATE virtual_bodies SET deleted_at = 9 WHERE id = 'u3'", []).unwrap();
        assert!(url_output_seen(&conn, "t1", "https://ex.jp/a"), "削除済みも既出");
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1.7 P4: link_fetch_source は capture:true メタ（web_capture）では系譜を張らない
    /// （更新確認のベースラインが png になり「常に更新あり」になる事故の防止）。
    /// 通常メタ（web_fetch）は従来どおり張る。
    #[test]
    fn capture_meta_skips_lineage() {
        let root = std::env::temp_dir().join(format!("texos_cap_lineage_{}", db::now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let p = project::open_project_at(&root).unwrap();
        p.conn
            .execute_batch(
                "INSERT INTO real_bodies (id, mime_type, size, created_at) VALUES
                   ('url:sentinel', 'application/vnd.texos.url', 0, 1),
                   ('h1', 'image/png', 1, 1);
                 INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, url_ref, kind)
                   VALUES ('u1', 'url:sentinel', 'Example', 0, 0, 1, 'https://ex.jp/', 'url');
                 INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind) VALUES
                   ('f1', 'h1', 'キャプチャ', 0, 0, 1, 'filter'),
                   ('o1', 'h1', 'shot.png', 0, 0, 2, 'file');
                 INSERT INTO relations (from_id, to_id, kind, created_at)
                   VALUES ('u1', 'f1', 'flow', 1);",
            )
            .unwrap();
        let fetched_count = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM relations WHERE kind = 'fetched' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        // capture メタ → 張らない
        link_fetch_source(&p, "f1", r#"{"source_url":"https://ex.jp/","capture":true}"#, "o1");
        assert_eq!(fetched_count(&p.conn), 0, "capture は系譜対象外");
        // 通常メタ（web_fetch）→ 張る
        link_fetch_source(&p, "f1", r#"{"source_url":"https://ex.jp/"}"#, "o1");
        assert_eq!(fetched_count(&p.conn), 1);
        drop(p);
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
    // v1.8.7 実行トレース: ログの書き出し名（日時_ラベル_job）をここで確定する。
    job.trace.begin_run(&root, &flabel, job.id);
    // card 付き（パネルが録音対応/停止コマンドを spec から引く）。
    let _ = app.emit(
        "job:started",
        serde_json::json!({ "job_id": job.id, "kind": "flow", "label": flabel, "card": id }),
    );
    let result = run_flow_inner(&id, fx, fy, &flabel, fref.as_deref(), &root, app, &state, &job);
    // v1.8.7 カードの色でのお知らせ: 実行そのものが起動前後に止まった（出力フォルダがない・
    // 入力が繋がっていない等）ときは、終端カードを「エラー」にする。トリガ等から起動した実行は
    // ダイアログが出ないので、これが無いとユーザーのミスが黙って消える。
    if let Err(e) = &result {
        if let Ok(p) = open_project_at(&root) {
            mark_last_run(&p, &id, "error", e);
        }
    }
    // v1.8.7 実行トレース: ノード単位の記録を .texos/logs へ書き出し、各カードの last_trace を更新。
    // best-effort（書けなくてもフローの結果には影響させない）。front は trace:written で知る。
    {
        let (report, fatal) = match &result {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e.as_str())),
        };
        let sum = TraceSummary { job_id: job.id, label: &flabel, card: &id, report, fatal };
        if let Some(path) = write_trace_log(&root, &job.trace, &sum) {
            let _ = app.emit(
                "trace:written",
                serde_json::json!({ "job_id": job.id, "card": id, "path": path.to_string_lossy() }),
            );
        }
    }
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
    // v2.0 t23d: 同じく終端が t23d ならデーモンを先に起こす（best-effort）。
    if term_spec.node_kind() == flow::NodeKind::T23d {
        let _ = t23d_ensure_server(&state.app_data, state.inner());
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
                            .map(|c| PathBuf::from(load_path(&p.root, &c)))
                            .filter(|c| c.is_file())
                            .or_else(|| {
                                let pb = resolve_view_path(p, &vid, &real_id);
                                pb.is_file().then_some(pb)
                            })
                    })
            };
            // ② 上流の pull 評価 → 一時実体化（{stem}.{ext} 名＝exe やバッチが素直に動く）。
            let resolved: Option<PathBuf> = match direct {
                Some(p) => {
                    job.trace.note(&id, "入力（直結の実ファイル・上流は評価せず）", &p.display().to_string());
                    Some(p)
                }
                None => {
                    let mut visited = std::collections::HashSet::new();
                    let mut memo = EvalMemo::new();
                    let mut ctx = LoopCtx::default();
                    let out = eval_filter(
                        state.inner(),
                        app,
                        &jp,
                        &id,
                        &mut visited,
                        &mut memo,
                        &mut report,
                        job,
                        &mut ctx,
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
        // v1.8.7 トレース: シェル終端で実際に投げたコマンド行と窓の扱い。
        // ウィンドウあり実行ではプログラムの出力は TexoS に戻らない（窓の中にだけ出る）。
        job.trace.note(&id, "シェル", &shell);
        job.trace.note(&id, "実行コマンド", &firstline);
        job.trace.note(
            &id,
            "ウィンドウ",
            match window {
                "hidden" => "なし（出力を捕捉して結果パネルへ）",
                "close" => "開いて実行後に閉じる（出力は記録されません）",
                _ => "開いたまま（出力はウィンドウ内のみ・記録されません）",
            },
        );
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

    // v1.8.7: 早期の出力フォルダ検査（実行してから「置き場がない」で落ちないように）。
    if let Some(d) = term_spec.out_dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let pb = resolve_project_path(root, Some(d));
        if !pb.is_dir() {
            return Err(format!("出力フォルダがありません: {}", pb.display()));
        }
    }

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
    let mut ctx = LoopCtx::default();
    let out = eval_filter(
        state.inner(),
        app,
        &jp,
        &id,
        &mut visited,
        &mut memo,
        &mut report,
        job,
        &mut ctx,
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

    // マテリアライズ（v1.8.7 で materialize_payloads へ切り出し。途中出力スイッチと共用）。
    let before_err = report.errors.len();
    materialize_payloads(
        state.inner(), &jp, root, &id, fx, fy, &flabel, fref, &term_spec, payloads, false, &mut report,
    )?;
    // v1.8.7: 着地（書き出し・CAS）で起きた失敗は終端カードの色で知らせる。
    if report.errors.len() > before_err {
        mark_last_run(&jp, &id, "warn", &report.errors[before_err..].join("\n"));
    }
    {
    let p = &jp;
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

/// 結果の着地（マテリアライズ）: ペイロードをファイル＋CAS＋カードとしてキャンバスへ出す。
/// 命名・置き場所はカードの設定（prefix／seq／out_dir）に従う＝単体▶実行の終端と同じ規則。
/// v1.8.7: 終端（run_flow_inner）と「途中の出力もキャンバスに出す」スイッチ（eval_filter）の
/// 両方がここを通る。規則を一箇所にしか置かない＝両者がズレようがない。
/// `below` = 新しいカードをカードの下に並べる（途中出力。右は次段のカードと重なるため）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_payloads(
    state: &AppState,
    p: &Project,
    root: &Path,
    id: &str,
    fx: f64,
    fy: f64,
    flabel: &str,
    fref: Option<&str>,
    term_spec: &flow::FilterSpec,
    payloads: Vec<FlowPayload>,
    below: bool,
    report: &mut FlowReport,
) -> Result<(), String> {
    let id = id.to_string();
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
        // v1.8.3: 相対パスはプロジェクト基準で解く（従来はプロセスの CWD 基準＝実質壊れていた）。
        Some(d) => {
            let pb = resolve_project_path(root, Some(d));
            if !pb.is_dir() {
                // 相対指定のときは解決後のフルパスで知らせる（どこを見に行ったかが分かる）。
                return Err(format!("出力フォルダがありません: {}", pb.display()));
            }
            (pb, true)
        }
        None => (root.to_path_buf(), false),
    };


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
    let write_lock = project_write_lock(state, root);
    let _write_guard = write_lock.lock().unwrap();
    let mut out_i = 0usize;
    // v1.7 P5(a) リンクの受け皿: 終端フィルタから URL カードへ矢印が引いてあれば、
    // ペイロードのテキストから**最初の URL** を拾ってそのカードの url_ref を更新し、
    // 新しいカードは作らない（固定名上書きと同型の「定点リンク」。開くのは従来どおり手動）。
    // to_url は自分の出力矢印が URL カードを指すので対象外。
    let url_sinks = if term_spec.builtin.as_deref() == Some("to_url") {
        Vec::new()
    } else {
        url_sink_ids(&p.conn, &id)
    };
    let payloads: Vec<FlowPayload> = if url_sinks.is_empty() || payloads.is_empty() {
        payloads
    } else {
        let first_url = payloads
            .iter()
            .find_map(|pl| web::scan_urls(&String::from_utf8_lossy(&pl.bytes)).into_iter().next());
        match first_url {
            Some(u) => {
                for sid in &url_sinks {
                    match update_url_sink(&p.conn, sid, &u) {
                        Ok(label) => {
                            report.updated.push(sid.clone());
                            report.outputs.push(format!("リンク「{label}」→ {u}"));
                        }
                        Err(e) => report.errors.push(e),
                    }
                }
            }
            None => report.errors.push(
                "リンクカードへ流せる URL がありません（テキストに http:// / https:// を含めてください）"
                    .into(),
            ),
        }
        Vec::new() // 受け皿が引き取った＝通常のカード化はしない
    };
    // v1.7 P3 既読管理（差分 pull・計画 §3 P3）: guid 付きメタ（web_feed のエントリ）は、
    // このフィルタが過去に出力した guid と照合し、取り込み済みならスキップ＝再実行は
    // 新着だけをカード化する。台帳は最初に必要になったとき一度だけ読む。
    let mut seen_guids: Option<std::collections::HashSet<String>> = None;
    for pl in &payloads {
        // v1.7 P5(b) URLカード化（to_url）: url_card マーカー付きペイロードはファイルでなく
        // **URL カード**として着地する（実ファイル・CAS なし・url:sentinel 実身を共用）。
        // このフィルタが過去に出した URL（削除済み含む・guid と同じ原則）は増やさない。
        if let Some(m) = &pl.meta {
            let is_url_card = serde_json::from_str::<serde_json::Value>(m)
                .ok()
                .and_then(|v| v.get("url_card").and_then(|b| b.as_bool()))
                .unwrap_or(false);
            if is_url_card {
                let url = String::from_utf8_lossy(&pl.bytes).trim().to_string();
                if url_output_seen(&p.conn, &id, &url) {
                    report.outputs.push(format!("{url}（既存リンク・増やしません）"));
                    continue;
                }
                let _ = p.conn.execute(
                    "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
                     VALUES ('url:sentinel', ?1, 0, ?2)",
                    rusqlite::params![crate::project::URL_MIME, db::now_ms()],
                );
                let label = if pl.stem.trim().is_empty() || pl.stem == "output" {
                    url.clone()
                } else {
                    pl.stem.clone()
                };
                let new_vid = uuid();
                if let Err(e) = p.conn.execute(
                    "INSERT INTO virtual_bodies
                        (id, real_id, label, pos_x, pos_y, created_at, url_ref, kind)
                     VALUES (?1, 'url:sentinel', ?2, ?3, ?4, ?5, ?6, 'url')",
                    rusqlite::params![
                        new_vid,
                        label,
                        if below { fx + (out_i as f64) * 170.0 } else { fx + 190.0 },
                        if below { fy + 150.0 } else { fy + (out_i as f64) * 170.0 },
                        db::now_ms(),
                        url
                    ],
                ) {
                    report.errors.push(format!("{label}: {e}"));
                    continue;
                }
                let _ = p.conn.execute(
                    "INSERT INTO relations (from_id, to_id, kind, created_at)
                     VALUES (?1, ?2, 'flow', ?3)",
                    rusqlite::params![id, new_vid, db::now_ms()],
                );
                report.created.push(new_vid);
                report.outputs.push(format!("{label}（リンク）"));
                out_i += 1;
                continue;
            }
        }
        // v1.7 P3: guid の既読チェック（内容ハッシュより先。概要の微修正で guid が同じ
        // まま内容が変わっても「新着」扱いにはしない — 新着のみ出力の原則）。
        if let Some(m) = &pl.meta {
            let guid = serde_json::from_str::<serde_json::Value>(m)
                .ok()
                .and_then(|v| v.get("guid").and_then(|g| g.as_str()).map(str::to_string))
                .filter(|g| !g.is_empty());
            if let Some(g) = guid {
                let seen = seen_guids.get_or_insert_with(|| feed_seen_guids(&p.conn, &id));
                // insert が false = 既読（同一実行内の重複 guid もここで畳まれる）。
                if !seen.insert(g) {
                    report.outputs.push(format!("{}（既読・実身は増やしません）", pl.stem));
                    continue;
                }
            }
        }
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
                rusqlite::params![store_path(&p.root, &dest_str)],
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
        let (nx, ny) = if below {
            (fx + (out_i as f64) * 170.0, fy + 150.0)
        } else {
            (fx + 190.0, fy + (out_i as f64) * 170.0)
        };
        // 出力フォルダ指定時は外部ファイルへの窓（origin_path の仮身）、
        // 指定なしはプロジェクト直下のコミット済み実身（従来どおり）
        let dest_db = store_path(&p.root, &dest_str);
        let (committed_path, committed_mtime, origin_path) = if external_out {
            (None, None, Some(dest_db))
        } else {
            (Some(dest_db), file_mtime_ms(&dest), None)
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
    Ok(())
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
        if job.t23d_active.load(std::sync::atomic::Ordering::Relaxed) {
            flow::t23d_post_cancel();
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
    flow::t23d_post_cancel();
}


#[cfg(test)]
mod tap_tests {
    use super::*;

    fn payload(text: &str) -> FlowPayload {
        FlowPayload {
            bytes: text.as_bytes().to_vec(),
            ext: "md".into(),
            mime: "text/markdown".into(),
            stem: "x".into(),
            meta: None,
        }
    }

    /// v1.8 記録カード: 1回目はカードを作り、2回目は**同じカードの中身を差し替える**。
    /// 2回目に import してしまうと、同じ実ファイルを指すカードが2枚できて
    /// 「同一プロジェクト内に同じ実体を2つの実身として登録しない」（v1.3.9 根幹原則）を破る。
    #[test]
    fn tap_writes_once_then_overwrites_same_card() {
        let dir = std::env::temp_dir().join(format!("texos_tap_test_{}", db::now_ms()));
        let cas_root = dir.join(".texos").join("cas");
        std::fs::create_dir_all(&cas_root).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        let p = Project { conn, root: dir.clone(), cas_root: cas_root.clone() };
        p.conn
            .execute(
                "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
                 VALUES ('filter:sentinel','application/vnd.texos.filter',0,?1)",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
                 VALUES ('tap1','filter:sentinel','記録',10.0,20.0,?1,'filter','{\"builtin\":\"tap\",\"prefix\":\"途中\"}')",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        let spec = flow::parse_spec(r#"{"builtin":"tap","prefix":"途中"}"#).unwrap();

        let names = write_tap_record(&p, "tap1", &spec, &[payload("一回目")], "").unwrap();
        assert_eq!(names, vec!["途中.md".to_string()]);
        let path = dir.join("途中.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "一回目");
        // v1.9.1: DB の保存形（root 基準の相対）で照合する。
        let path_str = store_path(&p.root, &path.to_string_lossy());
        let count = |p: &Project| -> i64 {
            p.conn
                .query_row(
                    "SELECT COUNT(*) FROM virtual_bodies
                     WHERE committed_path = ?1 AND deleted_at IS NULL",
                    rusqlite::params![path_str],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count(&p), 1, "1回目でカードが1枚できる");
        let first_real: String = p
            .conn
            .query_row(
                "SELECT real_id FROM virtual_bodies WHERE committed_path = ?1",
                rusqlite::params![path_str],
                |r| r.get(0),
            )
            .unwrap();

        // 2回目: 中身が変わってもカードは増えず、実身だけ差し替わる。
        write_tap_record(&p, "tap1", &spec, &[payload("二回目")], "").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "二回目");
        assert_eq!(count(&p), 1, "2回目でカードが増えてはいけない");
        let second_real: String = p
            .conn
            .query_row(
                "SELECT real_id FROM virtual_bodies WHERE committed_path = ?1",
                rusqlite::params![path_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(first_real, second_real, "内容が変われば実身も変わる（CAS）");
        assert!(cas::object_path(&cas_root, &second_real).exists(), "CAS に載っている");

        // 入力が複数のときは _2, _3 を付けて全部残す（素通し＝ファンアウトしない）。
        let names = write_tap_record(&p, "tap1", &spec, &[payload("A"), payload("B")], "").unwrap();
        assert_eq!(names, vec!["途中.md".to_string(), "途中_2.md".to_string()]);
        assert_eq!(std::fs::read_to_string(dir.join("途中_2.md")).unwrap(), "B");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1.8 ④: 反復の中の記録カードは周ごとに別名で残る（固定名のままだと各周が消える）。
    /// 反復の外（接尾辞なし）では従来の固定名を1文字も変えない。
    #[test]
    fn tap_names_are_per_round_inside_a_loop() {
        let dir = std::env::temp_dir().join(format!("texos_tap_round_{}", db::now_ms()));
        let cas_root = dir.join(".texos").join("cas");
        std::fs::create_dir_all(&cas_root).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        let p = Project { conn, root: dir.clone(), cas_root };
        p.conn
            .execute(
                "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
                 VALUES ('filter:sentinel','application/vnd.texos.filter',0,?1)",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
                 VALUES ('tap1','filter:sentinel','記録',10.0,20.0,?1,'filter','{\"builtin\":\"tap\",\"prefix\":\"途中\"}')",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        let spec = flow::parse_spec(r#"{"builtin":"tap","prefix":"途中"}"#).unwrap();

        let r1 = write_tap_record(&p, "tap1", &spec, &[payload("1周目")], "_1").unwrap();
        let r2 = write_tap_record(&p, "tap1", &spec, &[payload("2周目")], "_2").unwrap();
        assert_eq!(r1, vec!["途中_1.md".to_string()]);
        assert_eq!(r2, vec!["途中_2.md".to_string()]);
        assert_eq!(std::fs::read_to_string(dir.join("途中_1.md")).unwrap(), "1周目");
        assert_eq!(std::fs::read_to_string(dir.join("途中_2.md")).unwrap(), "2周目");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// v1.8 ③問い合わせカード。人が答えるところは実機の仕事だが、**答えの有無で止まる／進む**と
/// **答え箱を用意する**は DB だけで確かめられる。
#[cfg(test)]
mod ask_tests {
    use super::*;

    fn project(tag: &str) -> (Project, PathBuf) {
        let dir = std::env::temp_dir().join(format!("texos_ask_{tag}_{}", db::now_ms()));
        std::fs::create_dir_all(dir.join(".texos")).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES ('filter:sentinel','application/vnd.texos.filter',0,?1)",
            rusqlite::params![db::now_ms()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
             VALUES ('ask1','filter:sentinel','確認',500.0,300.0,?1,'filter',
                     '{\"builtin\":\"ask\",\"question\":\"この案でよいですか？\"}')",
            rusqlite::params![db::now_ms()],
        )
        .unwrap();
        let p = Project { conn, root: dir.clone(), cas_root: dir.join(".texos").join("cas") };
        (p, dir)
    }

    fn report() -> FlowReport {
        FlowReport {
            created: vec![],
            updated: vec![],
            outputs: vec![],
            skipped: vec![],
            errors: vec![],
            shell_capture: None,
        }
    }

    /// 未回答＝no-data（エラーではない）。付箋が無ければ用意して矢印まで引く
    /// ——「答えてください」と言われても書く場所が無い、を起こさないため。
    #[test]
    fn unanswered_stops_the_flow_and_prepares_the_answer_note() {
        let (p, dir) = project("new");
        let spec = flow::parse_spec(
            r#"{"builtin":"ask","question":"この案でよいですか？"}"#,
        )
        .unwrap();
        let mut r = report();

        let out = run_ask_node(&p, "ask1", &spec, None, &[], &mut r);
        assert!(out.nodata, "未回答は no-data");
        assert!(out.ok, "エラーではない");
        assert!(r.skipped.iter().any(|s| s.contains("答え待ち")), "{:?}", r.skipped);

        // 付箋が1枚でき、role:"answer" の矢印で問い合わせカードに繋がっている。
        let (note_id, text): (String, String) = p
            .conn
            .query_row(
                "SELECT id, note_text FROM virtual_bodies WHERE kind = 'note' AND deleted_at IS NULL",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(text.starts_with("> この案でよいですか？"), "質問は引用で書く: {text}");
        assert!(
            text.contains("ここから下に答えを書いてください"),
            "使い方の注釈も付箋に置く（面のどこにも書いていなかった・実機FB）: {text}"
        );
        assert!(text.lines().all(|l| l.starts_with('>')), "案内も引用行にする: {text}");
        assert_eq!(
            flow::answer_text(&text),
            "",
            "用意した付箋をそのまま読んでも「未回答」でなければならない"
        );
        let attrs: String = p
            .conn
            .query_row(
                "SELECT attrs FROM relations WHERE from_id = ?1 AND to_id = 'ask1'",
                rusqlite::params![note_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(attrs.contains("answer"), "{attrs}");

        // 付箋が既にあるとき（＝繋がっているが未記入）は作り足さない。
        let mut r2 = report();
        run_ask_node(&p, "ask1", &spec, Some(&text), &[], &mut r2);
        let notes: i64 = p
            .conn
            .query_row("SELECT COUNT(*) FROM virtual_bodies WHERE kind = 'note'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(notes, 1, "答え箱を毎回増やしてはいけない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 届いていた材料も引用で書き写す（人が何を見て答えたのかを残すため）。
    /// **書き写した材料が「答え」に化けないこと**がこのテストの本題 —— 引用を1行でも
    /// 素で書いてしまうと、誰も答えていないのにフローが先へ進む。
    #[test]
    fn the_answer_note_quotes_the_material_without_becoming_an_answer() {
        let (p, dir) = project("material");
        let spec = flow::parse_spec(r#"{"builtin":"ask","question":"この案でよいですか？"}"#).unwrap();
        let mut r = report();
        let material = FlowPayload {
            bytes: "案:\nバックアップは自動で取ります。".as_bytes().to_vec(),
            ext: "md".into(),
            mime: "text/markdown".into(),
            stem: "案".into(),
            meta: None,
        };

        run_ask_node(&p, "ask1", &spec, None, std::slice::from_ref(&material), &mut r);
        let text: String = p
            .conn
            .query_row(
                "SELECT note_text FROM virtual_bodies WHERE kind = 'note'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(text.contains("> 案:"), "材料が引用で残る: {text}");
        assert!(text.contains("> バックアップは自動で取ります。"), "{text}");
        assert!(text.lines().all(|l| l.starts_with('>')), "素の行があってはいけない: {text}");
        assert_eq!(flow::answer_text(&text), "", "作った直後は必ず未回答");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1.8.4（実機FB 2026-08-27）: **答え箱が先に置いてある**配線でも材料が入ること。
    ///
    /// 規約 §3-12 は「付箋は自分で用意して先に繋いでおける（そのほうが面から読める）」と
    /// 勧めていて、エージェントが組んだフローは実際そうする。ところが従来は
    /// 「付箋が無いとき」だけ材料を書き写していたので、**この配線では引用が一度も入らず**、
    /// 人は判断のたびに別のカードを開きに行く羽目になっていた。
    ///
    /// 併せて確かめること: 人が書いた案内を消さないこと、2回流しても二重に積まないこと。
    #[test]
    fn an_existing_answer_note_gets_the_material_too() {
        let (p, dir) = project("refresh");
        let 人の案内 = "> 「採用」または「やり直し」を1行で書く";
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, note_text)
                 VALUES ('note1','note:sentinel','答え',0.0,0.0,?1,'note',?2)",
                rusqlite::params![db::now_ms(), 人の案内],
            )
            .unwrap();
        p.conn
            .execute(
                "INSERT INTO relations (from_id, to_id, kind, created_at, attrs)
                 VALUES ('note1','ask1','flow',?1,'{\"role\":\"answer\"}')",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        let spec = flow::parse_spec(r#"{"builtin":"ask","question":"この案でよいですか？"}"#).unwrap();
        let material = FlowPayload {
            bytes: "案:\nバックアップは自動で取ります。".as_bytes().to_vec(),
            ext: "md".into(),
            mime: "text/markdown".into(),
            stem: "案".into(),
            meta: None,
        };
        let read = |p: &Project| -> String {
            p.conn
                .query_row("SELECT note_text FROM virtual_bodies WHERE id='note1'", [], |r| r.get(0))
                .unwrap()
        };

        let mut r = report();
        let out = run_ask_node(&p, "ask1", &spec, Some(人の案内), std::slice::from_ref(&material), &mut r);
        assert!(out.nodata, "まだ誰も答えていないので no-data のまま");
        let text = read(&p);
        assert!(text.contains("> バックアップは自動で取ります。"), "材料が入る: {text}");
        assert!(text.contains(人の案内), "人が書いた案内は消さない: {text}");
        assert!(text.lines().all(|l| l.starts_with('>')), "素の行があってはいけない: {text}");
        assert_eq!(flow::answer_text(&text), "", "入れ直しても未回答のまま");

        // 2回目: 目印より上だけが入れ替わるので、案内も目印も1つずつのまま。
        let mut r2 = report();
        run_ask_node(&p, "ask1", &spec, Some(&text), std::slice::from_ref(&material), &mut r2);
        let again = read(&p);
        assert_eq!(again, text, "同じ材料なら中身は動かない");
        assert_eq!(
            again.matches(ANSWER_NOTE_MARK).count(),
            1,
            "目印が二重に積まれてはいけない: {again}"
        );
        assert_eq!(again.matches(人の案内).count(), 1, "案内も二重にしない: {again}");

        // 人が答えを書いたら、もう触らない（材料の入れ直しで答えを流さない）。
        let 答え入り = format!("{again}\n採用");
        let mut r3 = report();
        let out3 = run_ask_node(&p, "ask1", &spec, Some(&答え入り), std::slice::from_ref(&material), &mut r3);
        assert!(!out3.nodata, "答えがあるので下流へ流れる");
        assert_eq!(read(&p), again, "答えが入った付箋は書き換えない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1.8.4（実機FB 2026-08-27）: **材料が空なら、答えが書いてあっても止まる。**
    ///
    /// 2パスのフロー（①原案を作る ②人が判断して仕上げる）で ② を先に押すと、従来は
    /// 「採用」の3文字だけが下流へ流れ、下流の AI は**承認された中身を知らないまま**
    /// それらしい文書を書いた —— 人は自分が読んでいないものに判子を押した形になる。
    /// 空の実身を間に挟む配線（先に置いて記録カードが埋める）が成立するかどうかは、
    /// ここが止まるかどうかで決まる。
    #[test]
    fn empty_material_stops_even_when_the_answer_is_written() {
        let (p, dir) = project("empty");
        let spec = flow::parse_spec(r#"{"builtin":"ask","question":"この案でよいですか？"}"#).unwrap();
        let empty = FlowPayload {
            bytes: "\n  \n".as_bytes().to_vec(), // まだ埋まっていない実身
            ext: "md".into(),
            mime: "text/markdown".into(),
            stem: "企画案".into(),
            meta: None,
        };

        let mut r = report();
        let out = run_ask_node(&p, "ask1", &spec, Some("採用"), std::slice::from_ref(&empty), &mut r);
        assert!(out.nodata, "材料が空なら答えがあっても流さない");
        assert!(
            r.skipped.iter().any(|s| s.contains("材料がまだありません")),
            "何が足りないかを言う: {:?}",
            r.skipped
        );

        // 材料が入れば、同じ答えでそのまま通る。
        let filled = FlowPayload {
            bytes: "企画案: 320ml ボディに温度センサー".as_bytes().to_vec(),
            ..empty.clone()
        };
        let mut r2 = report();
        let out2 = run_ask_node(&p, "ask1", &spec, Some("採用"), std::slice::from_ref(&filled), &mut r2);
        assert!(!out2.nodata, "材料が揃えば通る");
        assert_eq!(
            String::from_utf8_lossy(&out2.payloads[0].bytes),
            "採用",
            "出力は答えの文字列だけ（従来どおり）"
        );

        // 入力の矢印が1本も無いカードは従来どおり許す（質問だけのカードは正当・§3-12）。
        let mut r3 = report();
        let out3 = run_ask_node(&p, "ask1", &spec, Some("採用"), &[], &mut r3);
        assert!(!out3.nodata, "入力0は従来どおり通す");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// スイッチ `skip_upstream:"on"`（実機FB）: 答えが入っていれば**上流を1枚も評価せずに**
    /// 答えだけを返せること。ここが評価に落ちると、2パス目に原案の AI が走り直して
    /// 人が読んだ案と別物になる。答えが空のあいだは通常どおり（＝上流を見る）。
    #[test]
    fn the_switch_reads_the_answer_without_touching_upstream() {
        let (p, dir) = project("switch");
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, note_text)
                 VALUES ('note1','note:sentinel','答え',0.0,0.0,?1,'note',?2)",
                rusqlite::params![db::now_ms(), "> 問い\n> ── ここから下に\n"],
            )
            .unwrap();
        p.conn
            .execute(
                "INSERT INTO relations (from_id, to_id, kind, created_at, attrs)
                 VALUES ('note1','ask1','flow',?1,'{\"role\":\"answer\"}')",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();

        // 未回答のあいだは「答えなし」— 呼び出し側は通常の評価へ落ちる。
        let note = peek_answer_note(&p, "ask1").expect("答え箱は見つかる");
        assert_eq!(flow::answer_text(&note), "", "引用行だけ＝未回答");

        // 人が書いたら、上流に触らずここで答えが取れる。
        p.conn
            .execute(
                "UPDATE virtual_bodies SET note_text = ?1 WHERE id = 'note1'",
                rusqlite::params!["> 問い\n> ── ここから下に\n方針: 上位2割だけ片づける。"],
            )
            .unwrap();
        let note = peek_answer_note(&p, "ask1").unwrap();
        assert_eq!(flow::answer_text(&note), "方針: 上位2割だけ片づける。");

        // 役割なしの矢印（ふつうの材料）は答え箱ではない。
        p.conn
            .execute("UPDATE relations SET attrs = NULL WHERE from_id = 'note1'", [])
            .unwrap();
        assert!(peek_answer_note(&p, "ask1").is_none(), "材料の矢印を答えと取り違えない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回答済み＝答えの文字列を text/plain で下流へ。質問の引用行は混ぜない。
    #[test]
    fn answered_passes_the_human_text_downstream() {
        let (p, dir) = project("answered");
        let spec =
            flow::parse_spec(r#"{"builtin":"ask","question":"この案でよいですか？","prefix":"判断"}"#)
                .unwrap();
        let mut r = report();

        let out = run_ask_node(
            &p,
            "ask1",
            &spec,
            Some("> この案でよいですか？\n\nはい。ただし価格は据え置きで。"),
            &[],
            &mut r,
        );
        assert!(!out.nodata);
        assert_eq!(out.payloads.len(), 1);
        assert_eq!(
            String::from_utf8_lossy(&out.payloads[0].bytes),
            "はい。ただし価格は据え置きで。"
        );
        assert_eq!(out.payloads[0].stem, "判断");
        assert_eq!(out.payloads[0].mime, "text/plain");
        // 答えたのだから、付箋を作り足すことはない。
        let notes: i64 = p
            .conn
            .query_row("SELECT COUNT(*) FROM virtual_bodies WHERE kind = 'note'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(notes, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// v1.8 ④反復カード。実行そのもの（AI を何周も回す）は実機の仕事だが、**反復を成り立たせて
/// いる約束**＝停止判定・引き直す範囲・周回番号は純関数／DB だけで確かめられる。
#[cfg(test)]
mod loop_tests {
    use super::*;

    fn payload(text: &str) -> FlowPayload {
        FlowPayload {
            bytes: text.as_bytes().to_vec(),
            ext: "md".into(),
            mime: "text/markdown".into(),
            stem: "x".into(),
            meta: None,
        }
    }

    #[test]
    fn round_suffix_is_empty_outside_a_loop_and_nests() {
        let mut ctx = LoopCtx::default();
        assert_eq!(ctx.round_suffix(), "", "反復の外では従来どおり固定名");
        ctx.rounds.push(2);
        assert_eq!(ctx.round_suffix(), "_2");
        ctx.rounds.push(3);
        assert_eq!(ctx.round_suffix(), "_2_3", "入れ子は外側から順に並べる");
    }

    #[test]
    fn stop_condition_reads_the_round_output() {
        let none = flow::parse_spec(r#"{"builtin":"loop"}"#).unwrap();
        assert_eq!(
            loop_should_stop(&none, &[payload("なんでも")]),
            Ok(false),
            "until 未指定＝回数だけで止める"
        );
        let ok = flow::parse_spec(r#"{"builtin":"loop","until":"contains","pattern":"合格"}"#).unwrap();
        assert_eq!(loop_should_stop(&ok, &[payload("判定: 合格")]), Ok(true));
        assert_eq!(loop_should_stop(&ok, &[payload("判定: 不足")]), Ok(false));
        // 複数入力は連結して判定する（束カードを挟まなくても意味が変わらない）。
        assert_eq!(loop_should_stop(&ok, &[payload("不足"), payload("合格")]), Ok(true));
        // 判定器が壊れているときは黙って回し続けない（止まらない反復になるため）。
        let bad = flow::parse_spec(r#"{"builtin":"loop","until":"regex","pattern":"("}"#).unwrap();
        assert!(loop_should_stop(&bad, &[payload("x")]).is_err());
        // 語の書き落とし: contains("") も regex("") も常に真＝1周で止まる。設定ダイアログで
        // 停止条件だけ選んでパターンを空にできるので、静かに「繰り返さない」に化ける前に落とす。
        for m in ["contains", "not_contains", "regex", "not_regex"] {
            let s = flow::parse_spec(&format!(r#"{{"builtin":"loop","until":"{m}"}}"#)).unwrap();
            assert!(loop_should_stop(&s, &[payload("x")]).is_err(), "{m}");
        }
        // 「空」系はパターンを見ないので、空欄のままで正しく動く。
        let e = flow::parse_spec(r#"{"builtin":"loop","until":"empty"}"#).unwrap();
        assert_eq!(loop_should_stop(&e, &[payload("  ")]), Ok(true));
    }

    /// 引き直す範囲＝上流サブグラフ。戻り矢印（role:"previous"）を辿ってはいけない
    /// （辿ると反復カード自身が「自分の上流」に見え、無関係なノードのメモまで捨てる）。
    #[test]
    fn upstream_ids_walks_back_but_never_through_the_feedback_edge() {
        let dir = std::env::temp_dir().join(format!("texos_loop_up_{}", db::now_ms()));
        std::fs::create_dir_all(dir.join(".texos")).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        let p = Project { conn, root: dir.clone(), cas_root: dir.join(".texos").join("cas") };
        p.conn
            .execute(
                "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
                 VALUES ('filter:sentinel','application/vnd.texos.filter',0,?1)",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        for id in ["note", "cat", "ai", "loop", "sink"] {
            p.conn
                .execute(
                    "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind)
                     VALUES (?1,'filter:sentinel',?1,0.0,0.0,?2,'filter')",
                    rusqlite::params![id, db::now_ms()],
                )
                .unwrap();
        }
        let mut edge = |from: &str, to: &str, attrs: Option<&str>| {
            p.conn
                .execute(
                    "INSERT INTO relations (from_id, to_id, kind, attrs, created_at)
                     VALUES (?1, ?2, 'flow', ?3, ?4)",
                    rusqlite::params![from, to, attrs, db::now_ms()],
                )
                .unwrap();
        };
        // note → cat → ai → loop → sink、そして loop → cat が戻り矢印。
        edge("note", "cat", None);
        edge("cat", "ai", None);
        edge("ai", "loop", None);
        edge("loop", "sink", None);
        edge("loop", "cat", Some(r#"{"role":"previous"}"#));

        let mut up = upstream_ids(&p, "loop");
        up.sort();
        assert_eq!(up, vec!["ai".to_string(), "cat".to_string(), "note".to_string()]);
        assert!(!up.contains(&"loop".to_string()), "戻り矢印を辿って自分に戻ってはいけない");
        assert!(!up.contains(&"sink".to_string()), "下流は引き直しの対象外");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod emit_mid_tests {
    use super::*;

    fn setup(tag: &str) -> (PathBuf, Project, AppState) {
        let dir = std::env::temp_dir().join(format!("texos_emit_{tag}_{}", db::now_ms()));
        let cas_root = dir.join(".texos").join("cas");
        std::fs::create_dir_all(&cas_root).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        let p = Project { conn, root: dir.clone(), cas_root };
        p.conn
            .execute(
                "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
                 VALUES ('filter:sentinel','application/vnd.texos.filter',0,?1)",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        let state = AppState {
            current: Mutex::new(None),
            app_data: dir.clone(),
            jobs: Jobs::new(),
            resources: ResourceLimits::new(),
            write_locks: Mutex::new(std::collections::HashMap::new()),
            t2i_server: Mutex::new(None),
            t23d_server: Mutex::new(None),
        };
        (dir, p, state)
    }

    fn add_filter(p: &Project, id: &str, label: &str, spec: &str) {
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
                 VALUES (?1,'filter:sentinel',?2,100.0,200.0,?3,'filter',?4)",
                rusqlite::params![id, label, db::now_ms(), spec],
            )
            .unwrap();
    }

    fn exe_out(bytes: &[u8]) -> EvalOut {
        EvalOut::data(
            vec![FlowPayload {
                bytes: bytes.to_vec(),
                ext: "exe".into(),
                mime: "application/octet-stream".into(),
                stem: "問題".into(),
                meta: None,
            }],
            Some(0),
        )
    }

    fn empty_report() -> FlowReport {
        FlowReport { created: vec![], updated: vec![], outputs: vec![], skipped: vec![], errors: vec![], shell_capture: None }
    }

    fn cards_from(p: &Project, fid: &str) -> Vec<(String, f64, f64)> {
        let mut st = p
            .conn
            .prepare(
                "SELECT v.label, v.pos_x, v.pos_y FROM relations r JOIN virtual_bodies v ON v.id = r.to_id
                 WHERE r.from_id = ?1 AND r.kind='flow' AND v.deleted_at IS NULL ORDER BY v.label",
            )
            .unwrap();
        st.query_map(rusqlite::params![fid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|x| x.unwrap())
            .collect()
    }

    /// 途中のカードは、終端と同じ規則（prefix＋seq＝連番履歴）でキャンバスへ出る。
    /// 置き場所はカードの下（右は次段のカードと重なる）。
    #[test]
    fn mid_chain_card_lands_on_canvas_with_its_own_naming() {
        let (dir, p, state) = setup("mid");
        let spec_json = r#"{"cmd":"gcc {in} -o {out}","prefix":"fibo","seq":1,"emit_mid":"on"}"#;
        add_filter(&p, "cc", "Cコンパイラ", spec_json);
        let spec = flow::parse_spec(spec_json).unwrap();
        let job = state.jobs.register_flow("shell_terminal", "コマンドプロンプト", "").unwrap();
        job.trace.begin("cc", "Cコンパイラ", "外部コマンド", "");

        let mut report = empty_report();
        emit_mid_outputs(&state, &p, &job, "cc", &spec, Some(spec_json), &exe_out(b"MZ-1"), &mut report);
        assert_eq!(std::fs::read(dir.join("fibo001.exe")).unwrap(), b"MZ-1");
        assert_eq!(report.created.len(), 1);
        let cards = cards_from(&p, "cc");
        assert_eq!(cards, vec![("fibo001.exe".to_string(), 100.0, 350.0)], "カードの下に出る");

        // 2回目（試行錯誤の次の周）: seq が進んで履歴として残る。spec は DB から読み直す。
        let spec_json2: String = p
            .conn
            .query_row("SELECT filter_ref FROM virtual_bodies WHERE id='cc'", [], |r| r.get(0))
            .unwrap();
        let spec2 = flow::parse_spec(&spec_json2).unwrap();
        let mut report = empty_report();
        emit_mid_outputs(&state, &p, &job, "cc", &spec2, Some(&spec_json2), &exe_out(b"MZ-2"), &mut report);
        assert_eq!(std::fs::read(dir.join("fibo002.exe")).unwrap(), b"MZ-2");
        assert!(dir.join("fibo001.exe").exists(), "前回分は残る");
        assert!(spec_json2.contains("\"emit_mid\":\"on\""), "seq の書き戻しでスイッチが消えない");

        let t = job.trace.snapshot();
        assert!(t[0].notes.iter().any(|(k, v)| k.starts_with("キャンバスへ出力") && v.contains("fibo001.exe")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1.8.7 途中出力の失敗はそのカードの last_run に warning として残る（成否は ok のまま）。
    /// 起動できなかった実行は error（ok=false）。次の評価の write_last_run で消える。
    #[test]
    fn failures_are_marked_on_the_card() {
        let (dir, p, state) = setup("mark");
        let spec_json = r#"{"cmd":"gcc","out_dir":"存在しないフォルダ","emit_mid":"on"}"#;
        add_filter(&p, "cc", "Cコンパイラ", spec_json);
        let spec = flow::parse_spec(spec_json).unwrap();
        let job = state.jobs.register_flow("term", "T", "").unwrap();
        let out = exe_out(b"MZ");
        write_last_run(&p, "cc", &out, 0);
        let mut r = empty_report();
        let w = emit_mid_outputs(&state, &p, &job, "cc", &spec, Some(spec_json), &out, &mut r);
        let w = w.expect("出力フォルダが無いので警告");
        assert!(w.contains("出力フォルダがありません"));
        mark_last_run(&p, "cc", "warn", &w);
        let lr: serde_json::Value = serde_json::from_str(
            &p.conn.query_row("SELECT last_run FROM virtual_bodies WHERE id='cc'", [], |r| r.get::<_, String>(0)).unwrap(),
        )
        .unwrap();
        assert_eq!(lr["ok"], true, "評価そのものは成功のまま");
        assert!(lr["warning"].as_str().unwrap().contains("出力フォルダ"));

        mark_last_run(&p, "cc", "error", "入力がありません");
        let lr: serde_json::Value = serde_json::from_str(
            &p.conn.query_row("SELECT last_run FROM virtual_bodies WHERE id='cc'", [], |r| r.get::<_, String>(0)).unwrap(),
        )
        .unwrap();
        assert_eq!(lr["ok"], false);
        assert_eq!(lr["message"], "入力がありません");

        // 直して再実行 → 上書きで消える
        write_last_run(&p, "cc", &out, 0);
        let lr: serde_json::Value = serde_json::from_str(
            &p.conn.query_row("SELECT last_run FROM virtual_bodies WHERE id='cc'", [], |r| r.get::<_, String>(0)).unwrap(),
        )
        .unwrap();
        assert_eq!(lr["ok"], true);
        assert!(lr.get("warning").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 出さない場合: スイッチ OFF／終端カード自身（run_flow_inner が出す）／失敗／フロー以外のジョブ。
    #[test]
    fn switch_off_terminal_failure_and_chat_do_not_emit() {
        let (dir, p, state) = setup("no");
        let on = r#"{"cmd":"x","prefix":"fixed","emit_mid":"on"}"#;
        add_filter(&p, "a", "A", on);
        let spec_on = flow::parse_spec(on).unwrap();
        let spec_off = flow::parse_spec(r#"{"cmd":"x","prefix":"fixed"}"#).unwrap();

        let job = state.jobs.register_flow("a", "A", "").unwrap(); // a が終端
        let mut r = empty_report();
        emit_mid_outputs(&state, &p, &job, "a", &spec_on, Some(on), &exe_out(b"1"), &mut r);
        assert!(r.created.is_empty(), "終端は二重に出さない");

        let job2 = state.jobs.register_flow("other", "B", "").unwrap();
        emit_mid_outputs(&state, &p, &job2, "a", &spec_off, Some(on), &exe_out(b"1"), &mut r);
        assert!(r.created.is_empty(), "スイッチ OFF");
        let failed = EvalOut::failed(vec!["gcc: error".into()], Some(1), vec![]);
        emit_mid_outputs(&state, &p, &job2, "a", &spec_on, Some(on), &failed, &mut r);
        assert!(r.created.is_empty(), "失敗は出さない");

        let chat = state.jobs.register_chat("chat", "");
        emit_mid_outputs(&state, &p, &chat, "a", &spec_on, Some(on), &exe_out(b"1"), &mut r);
        assert!(r.created.is_empty(), "AI チャットの文脈評価では出さない");
        assert!(!dir.join("fixed.exe").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
