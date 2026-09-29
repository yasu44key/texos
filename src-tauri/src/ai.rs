#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// --- v1.4.1 AIチャット（builtin:"ai" mode:"chat"）---------------------------
// 履歴は <root>/.texos/chats/<cardId>.json（source of truth）。system は履歴に
// 入れず、毎リクエストで spec.system から前置する。応答はストリーミングで
// "ai-chat-token" / "ai-chat-done" イベントにより UI へ逐次配信。

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ChatMsg {
    role: String,
    content: String,
    /// v1.8.9 assistant 発話の実行条件（モデル・digest・能力・実際に送ったパラメータ）。
    /// 対話はフローのトレースに乗らないので、発話ごとに履歴 JSON へ残す。
    /// 旧履歴（キー無し）もそのまま読める。API へ送るメッセージには含めない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<crate::ai_models::AiRunRecord>,
}

#[derive(Serialize, Clone)]
pub(crate) struct ChatTokenEvent {
    id: String,
    delta: String,
    /// v1.8.6 true＝思考（Thinking）の断片。パネルは折りたたみで表示するだけで、
    /// 履歴JSON にも MDログにも残さない（次ターンのコンテキストにも入れない）。
    thinking: bool,
}

#[derive(Serialize, Clone)]
pub(crate) struct ChatDoneEvent {
    id: String,
    error: Option<String>,
}

pub(crate) fn chats_dir(root: &Path) -> PathBuf {
    texos_dir(root).join("chats")
}

pub(crate) fn chat_file(root: &Path, id: &str) -> PathBuf {
    // id は uuid（安全文字）だが念のためファイル名に使えない文字を落とす。
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    chats_dir(root).join(format!("{safe}.json"))
}

pub(crate) fn load_chat_history(root: &Path, id: &str) -> Vec<ChatMsg> {
    std::fs::read(chat_file(root, id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(crate) fn save_chat_history(root: &Path, id: &str, msgs: &[ChatMsg]) -> Result<(), String> {
    std::fs::create_dir_all(chats_dir(root)).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(msgs).map_err(|e| e.to_string())?;
    std::fs::write(chat_file(root, id), json).map_err(|e| e.to_string())
}

/// 会話履歴から人が読む Markdown 実身ログカードを生成/更新する（毎ターン自動）。
/// 固定名「<ラベル>のログ.md」へ上書き → 既存カードがあれば中身差し替え（位置・矢印維持）、
/// 無ければ AI カードの隣に作って flow 矢印を張る（run_flow の出力カードと同じ流儀）。
pub(crate) fn upsert_chat_log_card(
    p: &Project,
    ai_id: &str,
    ai_label: &str,
    log_name: Option<&str>,
    history: &[ChatMsg],
) -> Result<(), String> {
    // 1) transcript Markdown。v1.5.12 実機FB: 旧「**You:** 本文」の1行連結＋太字囲みは
    // grep 等での切り出しが面倒 →「You:」行の次行から本文（行頭アンカーで機械可読）。
    let all: Vec<&ChatMsg> = history.iter().collect();
    let bytes = render_transcript(ai_label, &all, true).into_bytes();

    // 2) 出力ファイル名: log_name 指定があればそれ（.md を補う）、無ければ「<ラベル>のログ.md」。
    let fname = match log_name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => {
            let mut n = sanitize(name);
            if !n.to_lowercase().ends_with(".md") {
                n.push_str(".md");
            }
            n
        }
        None => format!("{}のログ.md", sanitize(ai_label).chars().take(24).collect::<String>()),
    };
    let dest = p.root.join(&fname);
    std::fs::write(&dest, &bytes).map_err(|e| format!("ログ書き出し失敗: {e}"))?;
    place_md_card(p, ai_id, &dest, &fname, &bytes, 0.0).map(|_| ())
}

/// ファイル名に使えない文字を落とす（ログ・抜粋の共通）。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

/// 会話の Markdown。with_labels=true はログと同じ「You:/AI: の次行から本文」、
/// false は本文だけを空行区切りで並べる（v1.8.9 抜粋の「内容のみ」）。
pub(crate) fn render_transcript(ai_label: &str, msgs: &[&ChatMsg], with_labels: bool) -> String {
    if !with_labels {
        let bodies: Vec<&str> = msgs.iter().map(|m| m.content.trim_end()).collect();
        let mut md = bodies.join("\n\n");
        md.push('\n');
        return md;
    }
    let mut md = format!("# {ai_label}\n\n");
    for m in msgs {
        let who = match m.role.as_str() {
            "user" => "You",
            "assistant" => "AI",
            other => other,
        };
        md.push_str(&format!("{who}:\n{}\n\n", m.content));
    }
    md
}

/// 書き出した MD を実身カードにする: 同じパスのカードがあれば中身差し替え（位置・矢印維持）、
/// 無ければ AI カードの右（dy だけ下）に作り、AI → カードの flow 矢印を張る。返り値＝カード id。
fn place_md_card(p: &Project, ai_id: &str, dest: &Path, fname: &str, bytes: &[u8], dy: f64) -> Result<String, String> {
    let dest_str = dest.to_string_lossy().into_owned();

    // 3) CAS + real_body
    let hash = cas::store_bytes(&p.cas_root, &bytes).map_err(|e| e.to_string())?;
    db::insert_real_body(&p.conn, &hash, "text/markdown", bytes.len() as u64, Some("md"))
        .map_err(|e| e.to_string())?;

    // 4) 既存ログカードがあれば中身差し替え、無ければ作成
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

    let log_id = if let Some(vid) = existing {
        p.conn
            .execute(
                "UPDATE virtual_bodies SET real_id = ?1, committed_mtime = ?2 WHERE id = ?3",
                rusqlite::params![hash, file_mtime_ms(&dest), vid],
            )
            .map_err(|e| e.to_string())?;
        vid
    } else {
        let (fx, fy): (f64, f64) = p
            .conn
            .query_row(
                "SELECT pos_x, pos_y FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![ai_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0.0, 0.0));
        let vid = uuid();
        p.conn
            .execute(
                "INSERT INTO virtual_bodies
                    (id, real_id, label, pos_x, pos_y, created_at,
                     committed_path, committed_mtime, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'file')",
                rusqlite::params![
                    vid,
                    hash,
                    fname,
                    fx + 230.0,
                    fy + dy,
                    db::now_ms(),
                    dest_str,
                    file_mtime_ms(&dest),
                ],
            )
            .map_err(|e| e.to_string())?;
        vid
    };

    // 5) AI → ログ の flow 矢印（無ければ張る）
    let has_rel: bool = p
        .conn
        .query_row(
            "SELECT 1 FROM relations
             WHERE from_id = ?1 AND to_id = ?2 AND kind = 'flow' AND deleted_at IS NULL",
            rusqlite::params![ai_id, log_id],
            |_| Ok(true),
        )
        .optional()
        .unwrap_or(None)
        .unwrap_or(false);
    if !has_rel {
        let _ = p.conn.execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at) VALUES (?1, ?2, 'flow', ?3)",
            rusqlite::params![ai_id, log_id, db::now_ms()],
        );
    }
    Ok(log_id)
}

/// v1.8.9 会話の抜粋を別の MD に書き出す（チャットパネルの「書き出し」）。
/// indices＝履歴の何番目か（パネルのチェック）。with_labels=false で本文だけ（内容のみ）。
/// 毎ターン自動更新のログとは別物: 既存ファイルは上書きせず「<ラベル>の抜粋_2.md」…と
/// 連番で新しいカードを作る（書き出した時点の切り抜きとして残す）。返り値＝ファイル名。
#[tauri::command]
pub(crate) fn ai_chat_export(
    app: tauri::AppHandle,
    state: State<AppState>,
    id: String,
    indices: Vec<usize>,
    with_labels: bool,
) -> Result<String, String> {
    let fname = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        export_chat_excerpt(p, &id, &indices, with_labels)?
    };
    let _ = app.emit("ai-chat-exported", serde_json::json!({ "id": id, "file": fname }));
    Ok(fname)
}

pub(crate) fn export_chat_excerpt(p: &Project, id: &str, indices: &[usize], with_labels: bool) -> Result<String, String> {
    let label: String = p
        .conn
        .query_row(
            "SELECT label FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .map_err(|_| "AIカードが見つかりません".to_string())?;
    let history = load_chat_history(&p.root, id);
    let mut idx = indices.to_vec();
    idx.sort_unstable();
    idx.dedup();
    let picked: Vec<&ChatMsg> = idx.iter().filter_map(|&i| history.get(i)).collect();
    if picked.is_empty() {
        return Err("書き出す発言が選ばれていません".into());
    }
    let bytes = render_transcript(&label, &picked, with_labels).into_bytes();

    let stem = format!("{}の抜粋", sanitize(&label).chars().take(24).collect::<String>());
    let (fname, n) = (1u32..)
        .map(|n| (if n == 1 { format!("{stem}.md") } else { format!("{stem}_{n}.md") }, n))
        .find(|(f, _)| !p.root.join(f).exists())
        .expect("連番は尽きない");
    let dest = p.root.join(&fname);
    std::fs::write(&dest, &bytes).map_err(|e| format!("書き出しに失敗: {e}"))?;
    // ログカード（AI の右）の下へ、書き出すたびに少しずつずらして並べる。
    place_md_card(p, id, &dest, &fname, &bytes, 90.0 * n as f64)?;
    Ok(fname)
}

/// v1.4.4 マルチコンテキスト: AIカードへ flow 矢印で繋がった上流カードの内容を
/// 接続順（rel.created_at）に集める。データカードは現在値を読み、フィルタカードは
/// 評価して結果を文脈にする（URL→取得→テキスト等のパイプライン結果も使える）。
/// bytes+mime を保持して返すのでマルチモーダル（画像→image_url）への拡張は
/// メッセージ組立側の分岐追加だけで済む。
/// v1.5.2 WS-C: DB 接触はジョブ専用接続（p）。上流フィルタの評価は eval_filter が
/// 資源セマフォを取得して走る（チャット自身の ai permit はストリーム時に別途取る）。
/// v1.5.3 WS-P1: チャットの IN もエッジ属性（when/carry/port）を尊重する。no-data 枝は
/// 落とす（チャットは対話＝吸収相当）。エラーは従来どおり best-effort で黙って除外。
pub(crate) fn collect_context(
    state: &AppState,
    app: &tauri::AppHandle,
    p: &Project,
    ai_id: &str,
    job: &Job,
) -> Vec<FlowPayload> {
    let cancel = &job.cancel;
    let mut inputs: Vec<(String, String, String, String, Option<String>, Option<String>, flow::EdgeAttrs)> = (|| {
        let mut stmt = p
            .conn
            .prepare(
                "SELECT v.id, v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext), v.kind, rel.attrs
                 FROM relations rel
                 JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                 JOIN real_bodies r ON r.id = v.real_id
                 WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                 ORDER BY rel.created_at",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![ai_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(
            rows.filter_map(|r| r.ok())
                .map(|(vid, rid, l, m, e, k, attrs): (String, String, String, String, Option<String>, Option<String>, Option<String>)| {
                    (vid, rid, l, m, e, k, flow::parse_edge_attrs(attrs.as_deref()))
                })
                .collect::<Vec<_>>(),
        )
    })()
    .unwrap_or_default();
    inputs.sort_by_key(|(.., a)| a.port.unwrap_or(i64::MAX));

    let mut out = Vec::new();
    let mut report = FlowReport {
        created: vec![],
        updated: vec![],
        outputs: vec![],
        skipped: vec![],
        errors: vec![],
        shell_capture: None,
    };
    let mut visited = std::collections::HashSet::new();
    let mut memo = EvalMemo::new(); // メモ化は1ターン単位（§5.1-5）
    let mut ctx = crate::flow_run::LoopCtx::default(); // 反復の文脈（チャット経路では常に空）
    for (vid, real_id, vlabel, mime, ext, kind, attrs) in &inputs {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        // 上流の評価結果を得て when/carry を適用（flow_run と同じ意味論・簡略版）。
        let up: Option<EvalOut> = if kind.as_deref() == Some("filter") {
            eval_filter(state, app, p, vid, &mut visited, &mut memo, &mut report, job, &mut ctx)
                .ok()
        } else if real_id.starts_with("drop:") {
            None
        } else {
            let path = {
                refresh_origin_snapshot(p, vid);
                resolve_view_path(p, vid, real_id)
            };
            match std::fs::read(&path) {
                Ok(d) => {
                    let stem = std::path::Path::new(vlabel)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(vlabel)
                        .to_string();
                    Some(EvalOut::data(
                        vec![FlowPayload {
                            bytes: d,
                            ext: ext.clone().unwrap_or_else(|| "bin".into()),
                            mime: mime.clone(),
                            stem,
                            meta: None,
                        }],
                        None,
                    ))
                }
                Err(_) => None,
            }
        };
        if let Some(up) = up {
            let edge = apply_edge(&up, attrs, vlabel, &mut report);
            if !edge.nodata {
                out.extend(edge.payloads);
            }
        }
    }
    out
}

/// 画像か（mime と拡張子の両方で判定 — 取り込み元で mime が欠ける場合に備える）。
pub(crate) fn is_image(mime: &str, ext: &str) -> bool {
    mime.starts_with("image/")
        || matches!(
            ext.to_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
        )
}

/// 文脈に使えるテキストか（マルチモーダルは次段。今はテキストのみ注入）。
pub(crate) fn is_textual(mime: &str, ext: &str) -> bool {
    if mime.starts_with("text/") || mime == "application/json" {
        return true;
    }
    matches!(
        ext.to_lowercase().as_str(),
        "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "log"
            | "html" | "py" | "js" | "ts" | "rs" | "c" | "cpp" | "h" | "css"
    )
}

/// 収集した文脈から system 資料メッセージを組み立てる（接続順・各資料を上限で切る）。
/// テキスト以外（画像等）は今は除外し、件数だけ末尾に注記する（マルチモーダルは次段）。
pub(crate) fn build_context_message(context: &[FlowPayload]) -> Option<String> {
    const PER_ITEM_CAP: usize = 8000; // 1資料あたりの文字数上限（コンテキスト溢れ対策）
    let mut texts: Vec<(String, String)> = Vec::new();
    let mut nontext = 0usize;
    for it in context {
        if is_textual(&it.mime, &it.ext) {
            let mut text = String::from_utf8_lossy(&it.bytes).into_owned();
            if text.chars().count() > PER_ITEM_CAP {
                text = text.chars().take(PER_ITEM_CAP).collect::<String>() + "\n…(長いため以下略)";
            }
            texts.push((it.stem.clone(), text));
        } else if !is_image(&it.mime, &it.ext) {
            // 画像は user 発話に添付するのでここでは数えない（音声/PDF 等のみ未対応注記）。
            nontext += 1;
        }
    }
    if texts.is_empty() {
        return None;
    }
    let mut msg = String::from(
        "以下の資料を参考に、ユーザーの質問へ日本語で答えてください。\
         資料に記載が無い事柄は推測せず「資料には記載がありません」と述べてください。\n\n",
    );
    for (i, (label, text)) in texts.iter().enumerate() {
        msg.push_str(&format!("=== 資料{}: {} ===\n{}\n\n", i + 1, label, text));
    }
    if nontext > 0 {
        msg.push_str(&format!(
            "（このほかテキスト以外の入力が{nontext}件接続されていますが、現在は未対応です）\n"
        ));
    }
    Some(msg)
}

/// 1ターン送信。ユーザー発話を履歴に積み、ストリーミングで応答を生成して
/// トークンをイベント配信し、完了後に履歴へ保存する。完成本文を返す。
#[tauri::command(async)]
pub(crate) fn ai_chat_send(
    window: tauri::WebviewWindow,
    state: State<AppState>,
    id: String,
    message: String,
) -> Result<String, String> {
    // 1) カードの spec/label/root を短時間ロックで取得（ストリーム中・文脈評価中は握らない）。
    let (spec, root, card_label) = with_db(state.inner(), |p| {
        let (fref, label): (Option<String>, String) = p
            .conn
            .query_row(
                "SELECT filter_ref, label FROM virtual_bodies
                 WHERE id = ?1 AND kind = 'filter' AND deleted_at IS NULL",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| "AIカードが見つかりません".to_string())?;
        let spec = flow::parse_spec(fref.as_deref().unwrap_or("{}"))?;
        if spec.builtin.as_deref() != Some("ai") {
            return Err("このカードはAIカードではありません".into());
        }
        Ok((spec, p.root.clone(), label))
    })?;

    // v1.5.1 WS-B: チャットも Job 登録。中断は Job.cancel 経由。job:started の card=カードid
    // で ChatPanel が自分の job_id を拾う。v1.5.2 WS-C: 起動元プロジェクトに束縛（専用接続）。
    // job:started を出した以上、どの離脱経路でも job:done/failed を必ず出す（パネルの行が
    // 残らないように）→ 本体を inner に分け、emit は最後の1か所で行う。
    let job = state
        .jobs
        .register_chat(&card_label, &root.to_string_lossy());
    let _guard = JobGuard::new(&state.jobs, job.id);
    let app = window.app_handle().clone();
    let _ = app.emit(
        "job:started",
        serde_json::json!({ "job_id": job.id, "kind": "chat", "label": card_label, "card": id }),
    );
    let result = ai_chat_send_inner(&app, state.inner(), &job, &spec, &root, &id, &card_label, &message);
    let _ = app.emit(
        if result.is_ok() { "job:done" } else { "job:failed" },
        serde_json::json!({ "job_id": job.id }),
    );
    result
}

#[allow(clippy::too_many_arguments)]
fn ai_chat_send_inner(
    app: &tauri::AppHandle,
    state: &AppState,
    job: &Job,
    spec: &flow::FilterSpec,
    root: &Path,
    id: &str,
    card_label: &str,
    message: &str,
) -> Result<String, String> {
    let id = id.to_string();
    // v1.5.2 WS-C: ジョブ専用の DB 接続（起動元プロジェクト束縛）。文脈評価とログカードの
    // 更新をこれで行う＝ストリーム中にプロジェクトを切り替えてもログは起動元へ書かれる。
    let jp = open_project_at(root)?;

    // v1.4.4 マルチコンテキスト: 接続カードを毎ターン収集（編集が即反映）。上流フィルタの
    // 評価は資源セマフォに乗る（このジョブの permit 保持はノード実行中だけ）。
    let context = collect_context(state, app, &jp, &id, job);

    let user = message.trim().to_string();
    if user.is_empty() {
        return Err("メッセージが空です".into());
    }

    let mut history = load_chat_history(&root, &id);
    history.push(ChatMsg {
        role: "user".into(),
        content: user,
        meta: None,
    });

    // リクエスト用 messages（system → 接続文脈 → 会話履歴 の順）
    let mut messages: Vec<serde_json::Value> = Vec::new();
    if let Some(sys) = spec
        .system
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        messages.push(serde_json::json!({ "role": "system", "content": sys }));
    }
    // v1.4.4 接続カードの内容を資料として注入（接続順・テキストのみ・画像は次段）。
    if let Some(ctx) = build_context_message(&context) {
        messages.push(serde_json::json!({ "role": "system", "content": ctx }));
    }
    for m in &history {
        messages.push(serde_json::json!({ "role": m.role, "content": m.content }));
    }

    // v1.4.5 マルチモーダル: 接続された画像カードを現在のユーザー発話に添付する。
    // provider で形式が違う（Ollama=メッセージの images:[bare base64] / OpenAI準拠=
    // content を [text, image_url(data URI)] の配列に）。ビジョン対応モデル（gemma3 等）が必要。
    let images: Vec<(String, String)> = context
        .iter()
        .filter(|it| is_image(&it.mime, &it.ext))
        .map(|it| {
            (
                base64::engine::general_purpose::STANDARD.encode(&it.bytes),
                it.mime.clone(),
            )
        })
        .collect();
    if !images.is_empty() {
        let provider = spec.provider.as_deref().unwrap_or("ollama");
        if let Some(last) = messages.last_mut() {
            if last.get("role").and_then(|v| v.as_str()) == Some("user") {
                if flow::is_openai_compat(provider) {
                    let text = last
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut parts = vec![serde_json::json!({ "type": "text", "text": text })];
                    for (b64, mime) in &images {
                        parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{mime};base64,{b64}") }
                        }));
                    }
                    last["content"] = serde_json::Value::Array(parts);
                } else {
                    let imgs: Vec<serde_json::Value> =
                        images.iter().map(|(b64, _)| serde_json::json!(b64)).collect();
                    last["images"] = serde_json::Value::Array(imgs);
                }
            }
        }
    }

    // v1.5.2 WS-C: エンドポイントの ai permit（既定1）をストリームの間だけ保持。文脈収集の
    // **後**に取る — 上流に同一エンドポイントの AI ノードがいても自分の permit と衝突しない。
    // 使用中は即拒否＋案内（凍結 §11.2。フロー AI ノードと相互排他）。
    let ep_key = normalize_endpoint(&flow::effective_ai_endpoint(spec));
    let ai_permit = state.resources.ai_sem(&ep_key).try_acquire().ok_or_else(|| {
        let e = format!(
            "AIエンドポイント（{ep_key}）は使用中です。実行中のAI処理の完了を待つか中断してください"
        );
        let _ = app.emit(
            "ai-chat-done",
            ChatDoneEvent { id: id.clone(), error: Some(e.clone()) },
        );
        e
    })?;

    // ストリーミングは tokio 文脈外の素のOSスレッドで（run_ai と同じ理由）。
    // 中断フラグはこのジョブのもの（ジョブ別＝毎回新品なのでリセット不要）。
    let mut rec: Option<crate::ai_models::AiRunRecord> = None;
    let result = std::thread::scope(|s| {
        s.spawn(|| {
            let mut on_token = |kind: flow::ChatChunk, tok: &str| {
                let _ = app.emit(
                    "ai-chat-token",
                    ChatTokenEvent {
                        id: id.clone(),
                        delta: tok.to_string(),
                        thinking: kind == flow::ChatChunk::Thinking,
                    },
                );
            };
            flow::run_ai_chat_stream_rec(spec, &messages, &job.cancel, &mut on_token, &mut rec)
        })
        .join()
        .map_err(|_| "AI実行スレッドが異常終了しました".to_string())?
    });
    drop(ai_permit);

    match result {
        // v1.8.6 answer は本文のみ（run_ai_chat_stream が思考を除いて返す）。
        // 思考はここへ来ない＝履歴にもログにも残らない。
        Ok(answer) => {
            history.push(ChatMsg {
                role: "assistant".into(),
                content: answer.clone(),
                meta: rec,
            });
            let _ = save_chat_history(root, &id, &history);
            // MD実身ログを毎ターン自動更新（固定名上書き＝位置と矢印を保ち緑シグナル再誕生）。
            // v1.5.2 WS-C: ジョブ専用接続で**起動元プロジェクトへ**書く（切替に影響されない）。
            if let Err(e) =
                upsert_chat_log_card(&jp, &id, card_label, spec.log_name.as_deref(), &history)
            {
                eprintln!("チャットログ更新失敗: {e}");
            }
            let _ = app.emit(
                "ai-chat-done",
                ChatDoneEvent {
                    id: id.clone(),
                    error: None,
                },
            );
            Ok(answer)
        }
        Err(mut e) => {
            // v1.8.9 能力で画像非対応と分かっているなら、それを先に言う。
            let no_vision = rec
                .as_ref()
                .and_then(|r| r.capabilities)
                .is_some_and(|c| c.vision == crate::ai_models::Tri::No);
            // 画像添付時のエラーはモデル非対応が多い → ヒントを添える。
            if !images.is_empty() && no_vision {
                e.push_str("
（このモデルは画像入力に非対応です — 能力に「画像入力」があるモデルを選んでください）");
            } else if !images.is_empty() && (e.contains("400") || e.to_lowercase().contains("image")) {
                e.push_str("\n（ヒント: 画像を見るにはビジョン対応モデルが必要です。例: gemma3:latest。qwen等のテキスト専用モデルは画像を受け付けません）");
            }
            // 失敗時はユーザー発話を保存しない（次回リトライで二重に積まれない）
            let _ = app.emit(
                "ai-chat-done",
                ChatDoneEvent {
                    id: id.clone(),
                    error: Some(e.clone()),
                },
            );
            Err(e)
        }
    }
}

/// チャット履歴の読み出し（パネル描画用）。
#[tauri::command]
pub(crate) fn ai_chat_history(state: State<AppState>, id: String) -> Result<Vec<ChatMsg>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    Ok(load_chat_history(&p.root, &id))
}

#[derive(Serialize)]
pub(crate) struct ContextLabel {
    label: String,
    /// "image"（添付対象）/ "text"（資料注入）/ "other"（未対応）。📎表示の診断用。
    kind: String,
}

/// v1.4.4 接続中の文脈カード（接続順）。パネルの「📎資料」表示用。
/// mime/ext から種別を付けて返す（画像が image として認識されているか目視できる）。
#[tauri::command]
pub(crate) fn ai_context_labels(state: State<AppState>, id: String) -> Result<Vec<ContextLabel>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut stmt = p
        .conn
        .prepare(
            "SELECT v.label, COALESCE(v.mime_type, r.mime_type, ''), COALESCE(v.ext, r.ext, '')
             FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
             JOIN real_bodies r ON r.id = v.real_id
             WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
             ORDER BY rel.created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .filter_map(|r| r.ok())
        .map(|(label, mime, ext)| {
            let kind = if is_image(&mime, &ext) {
                "image"
            } else if is_textual(&mime, &ext) {
                "text"
            } else {
                "other"
            };
            ContextLabel {
                label,
                kind: kind.into(),
            }
        })
        .collect())
}

/// v1.4.2 設定ダイアログ用: 接続先のモデル一覧を取得（Ollama /api/tags 等）。
/// プロジェクト非依存（接続先だけで決まる）なので State を取らない。
#[tauri::command(async)]
pub(crate) fn ai_list_models(
    provider: Option<String>,
    endpoint: Option<String>,
) -> Result<Vec<String>, String> {
    flow::list_models(
        provider.as_deref().unwrap_or("ollama"),
        endpoint.as_deref().unwrap_or(""),
    )
}

#[cfg(test)]
mod export_tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMsg {
        ChatMsg { role: role.into(), content: content.into(), meta: None }
    }

    fn setup() -> Project {
        let dir = std::env::temp_dir().join(format!("texos_export_{}", uuid()));
        let cas_root = dir.join(".texos").join("cas");
        std::fs::create_dir_all(&cas_root).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        let p = Project { conn, root: dir, cas_root };
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
                 VALUES ('ai1','filter:sentinel','AI対話',100.0,200.0,?1,'filter','{\"builtin\":\"ai\",\"mode\":\"chat\"}')",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        save_chat_history(
            &p.root,
            "ai1",
            &[msg("user", "質問1"), msg("assistant", "答え1
"), msg("user", "質問2"), msg("assistant", "答え2")],
        )
        .unwrap();
        p
    }

    #[test]
    fn transcript_with_and_without_labels() {
        let (a, b) = (msg("user", "Q"), msg("assistant", "A
"));
        assert_eq!(render_transcript("L", &[&a, &b], true), "# L

You:
Q

AI:
A


");
        assert_eq!(render_transcript("L", &[&a, &b], false), "Q

A
", "内容のみ＝見出しなし・空行区切り");
    }

    #[test]
    fn export_picks_selected_in_order_and_never_overwrites() {
        let p = setup();
        // 順不同・重複・範囲外は整える。
        let f1 = export_chat_excerpt(&p, "ai1", &[3, 1, 1, 99], false).unwrap();
        assert_eq!(f1, "AI対話の抜粋.md");
        assert_eq!(std::fs::read_to_string(p.root.join(&f1)).unwrap(), "答え1

答え2
");

        let f2 = export_chat_excerpt(&p, "ai1", &[0, 1], true).unwrap();
        assert_eq!(f2, "AI対話の抜粋_2.md", "既存の抜粋は上書きしない");
        assert!(std::fs::read_to_string(p.root.join(&f2)).unwrap().starts_with("# AI対話

You:
質問1"));
        // 1回目の中身はそのまま。
        assert_eq!(std::fs::read_to_string(p.root.join(&f1)).unwrap(), "答え1

答え2
");

        // カードと AI → 抜粋 の矢印が張られる（2枚とも）。
        let n: i64 = p
            .conn
            .query_row(
                "SELECT COUNT(*) FROM relations WHERE from_id='ai1' AND kind='flow' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);

        assert!(export_chat_excerpt(&p, "ai1", &[], true).is_err(), "未選択はエラー");
        let _ = std::fs::remove_dir_all(&p.root);
    }
}
