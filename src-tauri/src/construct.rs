// --- v1.12 ④ コンストラクタ（builtin:"construct"）— docs/V1.12_声の窓口と思考役計画.md §6 ----------
// 依頼文から AI に設計図（ビルダーの呼び出し一覧の JSON）を書かせ、texos_flow.py の validate() で
// 検査し（違反は理由ごと AI に返して直させる）、いま開いているプロジェクトに並べる。
//
// 検査の正本は scripts/texos_flow.py（規約違反を理由つきで弾く）。Rust 側にルールは持たない
// ＝ルールを2か所に置かない。訳し役は python/texos_plan.py（任意の Python は実行しない）。
//
// 置く処理は MCP と同じ `*_in(&Project)`。並べたカードは実行しない（人が ▶ を押してから）。
// UNDO は最後に1回分まとめて積む（`mcp:changed` の undo）。
//
// lib.rs の glob 再エクスポートには載せない（`crate::construct::` で参照）。

use crate::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Emitter, Manager, State};

const PLAN_PY: &str = include_str!("../../python/texos_plan.py");
const FLOW_PY: &str = include_str!("../../scripts/texos_flow.py");
/// AI に渡す構築の要点（規約全文は約 15k トークン＝ローカルモデルでは真ん中が薄れる。
/// 細かい違反は validate() が弾いて返す分担・計画 §6.3）。
const GUIDE: &str = include_str!("../../python/construct_guide.md");

const DEFAULT_RETRIES: u64 = 3;
/// 1枚ずつ置く間（画面にパラパラと出す）。
const PLACE_GAP: Duration = Duration::from_millis(120);

fn extra_of(spec_json: Option<&str>) -> serde_json::Value {
    spec_json.and_then(|s| serde_json::from_str(s).ok()).unwrap_or(serde_json::Value::Null)
}

fn xs<'a>(v: &'a serde_json::Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

/// 設計図の検査結果（texos_plan.py の出力）。
#[derive(serde::Deserialize, Default)]
struct Checked {
    ok: bool,
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    cards: Vec<PlanCard>,
    #[serde(default)]
    edges: Vec<PlanEdge>,
    #[serde(default)]
    terminals: Vec<(String, String)>,
}

#[derive(serde::Deserialize)]
struct PlanCard {
    key: String,
    label: String,
    kind: String,
    spec: Option<serde_json::Value>,
    x: f64,
    y: f64,
    memo: Option<String>,
    note_text: Option<String>,
    style: Option<serde_json::Value>,
    file: Option<PlanFile>,
}

#[derive(serde::Deserialize)]
struct PlanFile {
    name: String,
    text: String,
}

#[derive(serde::Deserialize)]
struct PlanEdge {
    from: String,
    to: String,
    role: Option<String>,
}

/// 管理 env（音声・t2i）の python、無ければシステムの python。texos_flow.py は標準ライブラリだけで動く。
fn python_exe(app_data: &Path) -> Option<PathBuf> {
    for env in ["voice", "t2i"] {
        let p = env_python(app_data, env);
        if p.is_file() {
            return Some(p);
        }
    }
    find_system_python().map(PathBuf::from)
}

/// 設計図を texos_plan.py で検査する。
fn check_plan(app_data: &Path, plan: &serde_json::Value, spec: &flow::FilterSpec, extra: &serde_json::Value) -> Result<Checked, String> {
    let home = app_data.join("construct");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    std::fs::write(home.join("texos_plan.py"), PLAN_PY).map_err(|e| e.to_string())?;
    std::fs::write(home.join("texos_flow.py"), FLOW_PY).map_err(|e| e.to_string())?;
    let py = python_exe(app_data).ok_or("Python が見つかりません（音声カードの「音声環境を構築」で TexoS の管理環境を作れます）")?;
    // 並べる AI カードの既定モデル: card_model（無ければコンストラクタ自身のモデル）。
    let req = serde_json::json!({
        "plan": plan,
        "model": xs(extra, "card_model").or(spec.model.as_deref()).unwrap_or("gemma3:latest"),
        "provider": xs(extra, "card_provider").or(spec.provider.as_deref()).unwrap_or("ollama"),
        "endpoint": xs(extra, "card_endpoint").or(spec.endpoint.as_deref()),
    });
    let mut c = std::process::Command::new(&py);
    c.arg("-I").arg(home.join("texos_plan.py")).current_dir(&home);
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let mut child = c.spawn().map_err(|e| format!("検査役を起動できません: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("検査役に渡せません")?
        .write_all(req.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("検査役が失敗しました: {}", String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("検査役の結果を読めません: {e}"))
}

/// AI の答えから JSON を取り出す（```json で囲まれていても、前後に文があっても）。
pub(crate) fn extract_json(text: &str) -> Result<serde_json::Value, String> {
    let (a, b) = (text.find('{'), text.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else {
        return Err("JSON が見つかりません".into());
    };
    if b <= a {
        return Err("JSON が見つかりません".into());
    }
    serde_json::from_str(&text[a..=b]).map_err(|e| format!("JSON として読めません（{e}）"))
}

/// いまあるカード・付箋・図形の下端（新しい枠はその下に置く）。
fn bottom_of(p: &Project) -> f64 {
    let cards: f64 = p
        .conn
        .query_row("SELECT COALESCE(MAX(pos_y), 0) FROM virtual_bodies WHERE deleted_at IS NULL", [], |r| r.get(0))
        .unwrap_or(0.0);
    let shapes: f64 = p
        .conn
        .query_row("SELECT COALESCE(MAX(pos_y + height), 0) FROM shapes WHERE deleted_at IS NULL", [], |r| r.get(0))
        .unwrap_or(0.0);
    // カードの高さ（約 160）の分を足す。
    (cards + 160.0).max(shapes)
}

/// この PC 固有の値を埋める（texos_plan.py PRESETS の {{WHISPER_CLI}} {{WHISPER_MODEL}} {{MIC_DEVICE}}）。
/// 見つからないものは注意として返す（カードは置く＝導入したあと設定を直せば動く）。
fn resolve_presets(app_data: &Path, c: &mut Checked) -> Vec<String> {
    let mut notes = Vec::new();
    let uses = |key: &str| c.cards.iter().any(|k| k.spec.as_ref().map(|s| s.to_string().contains(key)).unwrap_or(false));
    let (need_whisper, need_mic) = (uses("{{WHISPER_"), uses("{{MIC_DEVICE}}"));
    let cli = whisper_cli_path(app_data);
    let model_name = read_config(app_data).whisper_model.unwrap_or_else(|| "small".to_string());
    let model = whisper_models_dir(app_data).join(format!("ggml-{model_name}.bin"));
    if need_whisper && !(cli.is_file() && model.is_file()) {
        notes.push("Whisper が未導入です。メニュー「Whisper 文字起こし」で導入してから、このカードを置き直すか設定の path・cmd を直してください".into());
    }
    let mic = if need_mic { proc::list_audio_devices().ok().and_then(|d| d.into_iter().next()) } else { None };
    if need_mic && mic.is_none() {
        notes.push("マイクが見つかりません（ffmpeg で dshow の音声デバイスを列挙できない）。録音カードの cmd の audio= を直してください".into());
    }
    let (cli_s, model_s) = (cli.to_string_lossy().to_string(), model.to_string_lossy().to_string());
    for k in c.cards.iter_mut() {
        let Some(spec) = k.spec.as_mut().and_then(|s| s.as_object_mut()) else { continue };
        for key in ["path", "cmd"] {
            if let Some(v) = spec.get(key).and_then(|v| v.as_str()).map(str::to_string) {
                let mut v = v.replace("{{WHISPER_CLI}}", &cli_s).replace("{{WHISPER_MODEL}}", &model_s);
                if let Some(m) = &mic {
                    v = v.replace("{{MIC_DEVICE}}", m);
                }
                spec.insert(key.into(), serde_json::Value::String(v));
            }
        }
        if spec.get("record").and_then(|r| r.as_bool()) == Some(true) {
            let dev = mic.clone().unwrap_or_else(|| "（マイク名）".into());
            spec.insert("note".into(), serde_json::Value::String(format!(
                "マイク録音（{dev}）。▶実行で録音開始 → 実行中パネルの「停止」で終了し WAV を確定（16kHz mono・Whisper 直結可）。")));
        }
    }
    notes
}

struct Placed {
    objects: Vec<String>,
    notes: Vec<String>,
    relations: Vec<i64>,
    shapes: Vec<String>,
}

fn notify(app: &tauri::AppHandle, root: &Path, undo: serde_json::Value, touched: Vec<String>) {
    let _ = app.emit(
        "mcp:changed",
        serde_json::json!({ "project": project_info(root).path, "undo": undo, "touched": touched }),
    );
}

/// 検査を通った設計図を、コンストラクタの下（いまある全カードの下端の下）へ枠で囲んで並べる。
fn place(app: &tauri::AppHandle, p: &Project, fid: &str, c: &Checked) -> Result<Placed, String> {
    let fx: f64 = p
        .conn
        .query_row("SELECT pos_x FROM virtual_bodies WHERE id = ?1", rusqlite::params![fid], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let (ox, oy) = (fx, bottom_of(p) + 160.0);
    let w = c.cards.iter().map(|k| k.x).fold(0.0, f64::max) + 300.0 + 80.0;
    let h = c.cards.iter().map(|k| k.y).fold(0.0, f64::max) + 180.0 + 100.0;
    let mut placed = Placed { objects: vec![], notes: vec![], relations: vec![], shapes: vec![] };

    // 枠（図形）に設計図の名前。
    let sid = uuid();
    p.conn
        .execute(
            "INSERT INTO shapes (id, kind, pos_x, pos_y, width, height, created_at, label)
             VALUES (?1, 'rect', ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![sid, ox - 40.0, oy - 60.0, w, h, db::now_ms(), format!("🏗 {}", c.title)],
        )
        .map_err(|e| e.to_string())?;
    placed.shapes.push(sid);

    let mut ids: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for k in &c.cards {
        let (x, y) = (ox + k.x, oy + k.y);
        let id = match k.kind.as_str() {
            "note" => {
                let id = create_note_in(p, k.note_text.clone().unwrap_or_default(), x, y)?;
                if let Some(st) = k.style.clone().and_then(|s| serde_json::from_value::<NoteStyle>(s).ok()) {
                    let _ = write_note_style(p, &id, &st);
                }
                placed.notes.push(id.clone());
                id
            }
            "file" => {
                let f = k.file.as_ref().ok_or("ファイルの中身がありません")?;
                let path = free_path(&p.root.join(&f.name));
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(&path, &f.text).map_err(|e| format!("{}: {e}", f.name))?;
                let id = objects::import_file_into(p, path.to_string_lossy().to_string(), String::new(), x, y)?;
                placed.objects.push(id.clone());
                id
            }
            _ => {
                let spec = k.spec.clone().ok_or("カードの設定がありません")?;
                let id = create_filter_in(p, k.label.clone(), spec.to_string(), x, y)?;
                placed.objects.push(id.clone());
                id
            }
        };
        if let Some(m) = k.memo.as_deref().filter(|m| !m.trim().is_empty()) {
            let _ = p.conn.execute("UPDATE virtual_bodies SET memo = ?1 WHERE id = ?2", rusqlite::params![m, id]);
        }
        ids.insert(k.key.as_str(), id.clone());
        // report.created には入れない（あれは「実行結果のカード」＝音声の報告やダイアログが読む）。
        // 緑のお知らせは touched で出る。
        notify(app, &p.root, serde_json::json!([]), vec![id]);
        std::thread::sleep(PLACE_GAP);
    }
    for e in &c.edges {
        let (Some(f), Some(t)) = (ids.get(e.from.as_str()), ids.get(e.to.as_str())) else { continue };
        if let Some(rid) = create_relation_in(p, f.clone(), t.clone(), None)? {
            if let Some(role) = e.role.clone() {
                set_relation_attrs_in(p, rid, None, None, None, Some(role))?;
            }
            placed.relations.push(rid);
        }
    }
    // UNDO 1回で全部消せるように、最後にまとめて積む。
    notify(
        app,
        &p.root,
        serde_json::json!([
            { "kind": "deleteRelations", "ids": placed.relations },
            { "kind": "delete", "ids": placed.objects },
            { "kind": "deleteNotes", "ids": placed.notes },
            { "kind": "deleteShapes", "ids": placed.shapes },
        ]),
        vec![],
    );
    Ok(placed)
}

/// 同名のファイルがあれば「名前 (2).拡張子」…にする（人のファイルを上書きしない）。
fn free_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for n in 2.. {
        let q = p.with_file_name(format!("{stem} ({n}){ext}"));
        if !q.exists() {
            return q;
        }
    }
    unreachable!()
}

/// AI に頼む。**tokio の外の素の OS スレッドで呼ぶ**（run_flow は tokio ワーカー上で走り、
/// reqwest::blocking の内部ランタイムをそこで drop すると panic する＝flow.rs run_ai と同じ対策。
/// 2026-10-23 実機: これを忘れて「Cannot drop a runtime…」で panic し、ジョブが「作成中」のまま残った）。
fn ask_ai(spec: &flow::FilterSpec, messages: &[serde_json::Value], cancel: &std::sync::atomic::AtomicBool) -> Result<String, String> {
    std::thread::scope(|s| {
        s.spawn(|| flow::run_ai_chat_stream_rec(spec, messages, cancel, &mut |_, _| {}, &mut None))
            .join()
            .map_err(|_| "AI の呼び出しが異常終了しました".to_string())?
    })
}

fn payload(text: String, stem: &str) -> FlowPayload {
    FlowPayload { bytes: text.into_bytes(), ext: "md".into(), mime: "text/markdown".into(), stem: stem.into(), meta: None }
}

/// 本体（flow_run.rs eval_filter から・入力の収集の後）。出力＝構築レポート（Markdown）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_construct(
    app: &tauri::AppHandle,
    p: &Project,
    fid: &str,
    spec: &flow::FilterSpec,
    spec_json: Option<&str>,
    edges: &[EdgeOut],
    role_system: Option<&str>,
    job: &Job,
    _report: &mut FlowReport,
) -> EvalOut {
    let extra = extra_of(spec_json);
    let stem = spec.prefix.clone().unwrap_or_else(|| "構築レポート".into());
    let request: String = edges
        .iter()
        .filter(|e| !e.nodata)
        .flat_map(|e| e.payloads.iter())
        .filter(|pl| pl.mime.starts_with("text/"))
        .map(|pl| String::from_utf8_lossy(&pl.bytes).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if request.is_empty() {
        return EvalOut::failed(vec!["依頼がありません（依頼の付箋をつなぐか、音声カードから頼んでください）".into()], None, vec![]);
    }
    let state: State<AppState> = app.state();
    let app_data = state.app_data.clone();
    let retries = extra.get("max_retries").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_RETRIES).clamp(1, 6);
    let mut system = GUIDE.to_string();
    // 追加の作法: 設定画面の欄（spec.system）と役割の付箋（role:"system"）。両方あれば両方足す。
    if let Some(r) = spec.system.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        system.push_str("\n\n## 追加の作法（カードの設定）\n\n");
        system.push_str(r);
    }
    if let Some(r) = role_system.map(str::trim).filter(|s| !s.is_empty()) {
        system.push_str("\n\n## 追加の作法（役割の付箋）\n\n");
        system.push_str(r);
    }
    let mut messages = vec![
        serde_json::json!({ "role": "system", "content": system }),
        serde_json::json!({ "role": "user", "content": format!("依頼:\n{request}\n\n設計図の JSON だけを出してください。") }),
    ];
    let mut log: Vec<String> = Vec::new();
    let mut last: Option<(Checked, serde_json::Value)> = None;
    for round in 1..=retries {
        if job.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return EvalOut::failed(vec!["中断されました".into()], None, vec![]);
        }
        emit_job_progress(app, job, Some(&format!("設計図を作成中（{round}回目）")), None);
        let answer = match ask_ai(spec, &messages, &job.cancel) {
            Ok(a) => a,
            Err(e) => return EvalOut::failed(vec![format!("AI に頼めません: {e}")], None, vec![]),
        };
        let problems: Vec<String> = match extract_json(&answer) {
            Err(e) => vec![e],
            Ok(plan) => match check_plan(&app_data, &plan, spec, &extra) {
                Err(e) => return EvalOut::failed(vec![e], None, vec![]),
                Ok(c) if c.ok => {
                    last = Some((c, plan));
                    break;
                }
                Ok(c) => c.errors,
            },
        };
        log.push(format!("- {round}回目: {}件の問題\n{}", problems.len(),
            problems.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n")));
        job.trace.note(fid, &format!("検査 {round}回目"), &problems.join(" / "));
        messages.push(serde_json::json!({ "role": "assistant", "content": answer }));
        messages.push(serde_json::json!({ "role": "user", "content": format!(
            "検査で次の問題が見つかりました。直した設計図の JSON 全体をもう一度出してください。\n{}",
            problems.iter().map(|e| format!("- {e}")).collect::<Vec<_>>().join("\n")) }));
    }
    let Some((mut checked, plan)) = last else {
        let md = format!("# 構築できませんでした\n\n## 依頼\n\n{request}\n\n## 検査の記録\n\n{}\n", log.join("\n"));
        return EvalOut {
            payloads: vec![payload(md, &stem)],
            nodata: false,
            ok: false,
            exit: None,
            errors: vec![format!("{retries}回直しても検査を通りませんでした（構築レポートに理由）")],
            failed_inputs: vec![],
        };
    };
    let preset_notes = resolve_presets(&app_data, &mut checked);
    checked.warnings.extend(preset_notes);
    let draft = xs(&extra, "apply") == Some("draft");
    // 冒頭の1文＝結果（音声カードはレポートの最初の段落を読み上げる・計画 §5 ③）。
    let run_names: Vec<String> = checked.terminals.iter().map(|(l, _)| format!("「{l}」")).collect();
    let run_hint = if run_names.is_empty() { String::new() } else { format!("実行するのは{}です。", run_names.join("と")) };
    let outcome = if draft {
        format!("案を作りました（並べていません）。カード {} 枚の設計です。{run_hint}", checked.cards.len())
    } else {
        emit_job_progress(app, job, Some("カードを並べています"), None);
        match place(app, p, fid, &checked) {
            Ok(pl) => format!(
                "カード {} 枚・付箋 {} 枚・矢印 {} 本を並べました。{run_hint}",
                pl.objects.len(),
                pl.notes.len(),
                pl.relations.len()
            ),
            Err(e) => return EvalOut::failed(vec![format!("並べる途中で失敗しました: {e}")], None, vec![]),
        }
    };
    let mut md = format!("# 🏗 {}\n\n{outcome}\n\n## 依頼\n\n{request}\n\n", checked.title);
    if !draft {
        md.push_str(&format!("枠「🏗 {}」の中に置きました（元に戻すは Ctrl+Z）。\n\n", checked.title));
    }
    if !checked.terminals.is_empty() {
        md.push_str("## 実行するカード（▶ を押すと出来るもの）\n\n");
        for (l, f) in &checked.terminals {
            md.push_str(&format!("- {l} → {f}\n"));
        }
        md.push('\n');
    }
    let programs: Vec<&PlanCard> = checked
        .cards
        .iter()
        .filter(|k| k.kind == "filter" && k.spec.as_ref().map(|s| s.get("builtin").is_none()).unwrap_or(false))
        .collect();
    if !programs.is_empty() {
        md.push_str("## 外部プログラム（実行前に中身を確かめてください）\n\n");
        for k in programs {
            let s = k.spec.as_ref().unwrap();
            md.push_str(&format!("- {}: `{}` `{}`\n", k.label, xs(s, "name").unwrap_or(""), xs(s, "cmd").unwrap_or("")));
        }
        md.push('\n');
    }
    if !checked.warnings.is_empty() {
        md.push_str("## 検査の注意\n\n");
        for w in &checked.warnings {
            md.push_str(&format!("- {w}\n"));
        }
        md.push('\n');
    }
    if !log.is_empty() {
        md.push_str(&format!("## 直した記録\n\n{}\n\n", log.join("\n")));
    }
    md.push_str(&format!("## 設計図\n\n```json\n{}\n```\n", serde_json::to_string_pretty(&plan).unwrap_or_default()));
    EvalOut::data(vec![payload(md, &stem)], None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_inside_prose_and_fences() {
        let v = extract_json("はい、設計図です。\n```json\n{\"title\":\"t\",\"calls\":[]}\n```\n以上").unwrap();
        assert_eq!(v["title"], "t");
        assert!(extract_json("JSON なし").is_err());
    }

    /// 回帰テスト（2026-10-23 実機）: 本番の run_flow と同じく tokio の文脈から AI を呼んでも panic しない。
    /// つながらないアドレスに向けて Err が返ればよい（素の OS スレッドに逃がしていないと
    /// 「Cannot drop a runtime in a context where blocking is not allowed」で panic する）。
    #[test]
    fn ask_ai_from_tokio_context_does_not_panic() {
        let spec: flow::FilterSpec = serde_json::from_value(serde_json::json!({
            "builtin": "construct", "provider": "openai", "model": "x", "endpoint": "http://127.0.0.1:9/v1" })).unwrap();
        let msgs = vec![serde_json::json!({ "role": "user", "content": "hi" })];
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let r = tauri::async_runtime::block_on(async { ask_ai(&spec, &msgs, &cancel) });
        assert!(r.is_err());
    }

    #[test]
    fn free_path_does_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("texos_free_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("資料.md");
        std::fs::write(&a, "x").unwrap();
        assert_eq!(free_path(&a), dir.join("資料 (2).md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 実機の AI と検査役で1回通す（cargo test --lib construct_live -- --ignored --nocapture）。
    /// CONSTRUCT_LIVE_PROVIDER / _MODEL / _ENDPOINT で頭を選ぶ（既定 Strata）。並べはしない。
    #[test]
    #[ignore]
    fn construct_live() {
        let provider = std::env::var("CONSTRUCT_LIVE_PROVIDER").unwrap_or_else(|_| "openai".into());
        let model = std::env::var("CONSTRUCT_LIVE_MODEL").unwrap_or_else(|_| "swift-1.5-iq2_xs".into());
        let ep = std::env::var("CONSTRUCT_LIVE_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8080/v1".into());
        let spec: flow::FilterSpec = serde_json::from_value(serde_json::json!({
            "builtin": "construct", "provider": provider, "model": model, "endpoint": ep })).unwrap();
        let app_data = std::env::temp_dir().join("texos_construct_live");
        let req = std::env::var("CONSTRUCT_LIVE_REQUEST")
            .unwrap_or_else(|_| "生成AIの最新動向をWebで調べて、要点をレポートにまとめるフローを構築して".into());
        let mut messages = vec![
            serde_json::json!({ "role": "system", "content": GUIDE }),
            serde_json::json!({ "role": "user", "content": format!("依頼:\n{req}\n\n設計図の JSON だけを出してください。") }),
        ];
        let cancel = std::sync::atomic::AtomicBool::new(false);
        for round in 1..=3 {
            let t0 = std::time::Instant::now();
            let answer = flow::run_ai_chat_stream_rec(&spec, &messages, &cancel, &mut |_, _| {}, &mut None).expect("AI");
            println!("--- {round}回目 {:.1}s\n{answer}", t0.elapsed().as_secs_f32());
            let problems = match extract_json(&answer) {
                Err(e) => vec![e],
                Ok(plan) => {
                    let c = check_plan(&app_data, &plan, &spec, &serde_json::Value::Null).expect("検査役");
                    if c.ok {
                        println!("検査 OK: {} 枚・矢印 {} 本・注意 {:?}・終端 {:?}", c.cards.len(), c.edges.len(), c.warnings, c.terminals);
                        return;
                    }
                    c.errors
                }
            };
            println!("問題: {problems:#?}");
            messages.push(serde_json::json!({ "role": "assistant", "content": answer }));
            messages.push(serde_json::json!({ "role": "user", "content": format!(
                "検査で次の問題が見つかりました。直した設計図の JSON 全体をもう一度出してください。\n{}",
                problems.iter().map(|e| format!("- {e}")).collect::<Vec<_>>().join("\n")) }));
        }
        panic!("3回で検査を通りませんでした");
    }
}
