// --- エージェントカード（builtin:"agent"）— 声で壁打ちする相手 ------------------------------------
// 2026-11 に作り直した（V1.13 の「一手ずつ道具を選んで組む・動かす」は廃止。組むのはコンストラクタ）。
//
// - 壁打ち: カードの下の欄（文字・声）で話しかけると、AI がすぐ短く返す。返事は出た端から画面に流し、
//   おしゃべり中は一文ごとに読み上げる。形の決まった JSON は使わない（遅さと不安定さの元だった）。
// - 調べる: 必要なときだけ、AI が返事の代わりに「【調べる】問い」「【読む】ファイル名」「【ウェブ】検索語」
//   の1行を書く。こちらで実行して【結果】を返し、続きの返事を書かせる（上限 max_lookups 回）。
//     調べる → rag::search_md（📚 索引カードが作った索引）
//     読む   → ファイルカードの中身（PDF・Word は rag::doc_text）
//     ウェブ → web::run_web_search（手元で動かす SearXNG）。カードの設定 web が true のときだけ。
// - 聞き取り: 音声カードと同じ常駐プロセス（python/voice_server.py・モデルを載せたまま・GPU）。
//   黙ったところで区切って頼み、読み上げ中に話しかけると読み上げを止める。
// - 残す: 会話は エージェント/<カード名>の会話.md に追記し、右隣のカードにする（毎回 中身の差し替え）。
//   返事の切り出し（agent_clip）と、会話のまとめ（agent_summarize）は別のカードにして下へ並べる
//   ＝その時点の写しなので、次のフローやコンストラクタの材料にできる。
//
// lib.rs の glob 再エクスポートには載せない（`crate::agent::` で参照）。

use crate::*;
use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, Manager, State};

/// 1回の頼みで調べられる回数の既定と上限。
const DEFAULT_LOOKUPS: u64 = 3;
const MAX_LOOKUPS: u64 = 8;
/// AI に渡す会話履歴の上限（文字数・古い方から落とす）。完全な記録は会話.md。
const HISTORY_CHARS: usize = 8000;
/// 調べた結果を AI に返すときの上限（文字数）。
const RESULT_CHARS: usize = 4000;
/// 調べた結果を次の頼みのために履歴へ残すときの上限（文字数）。
const KEEP_CHARS: usize = 1200;
/// 読むの上限（索引レポートの文書の一覧が数百件になる）。
const READ_CHARS: usize = 10000;
/// パネルに残す行数。
const PANEL_KEEP: usize = 80;
/// 検索で取る件数。
const SEARCH_K: usize = 6;
/// ウェブ検索で取る件数。
const WEB_K: u32 = 5;
/// 1回の返事で読み上げる上限（文字数）。残りは画面で読む。
const SPEAK_CHARS: usize = 400;
/// 読み上げの2つ目以降のまとまり（短い文ごとに読み上げを起動し直すと間が空く）。
const SPEAK_CHUNK: usize = 40;
/// まとめに渡す会話の上限（文字数）。
const SUMMARY_CHARS: usize = 12000;
/// 会話ログ・切り出し・まとめの置き場（プロジェクト直下）。
const LOG_DIR: &str = "エージェント";
/// 切り出し・まとめのカードの縦の間隔（カードの高さ 115 ＋ 隙間 20）。
const SLOT_DY: f64 = 135.0;
/// 切り出し・まとめのカードを置く列（エージェントカードからの横の距離）。会話のパネル（幅 360・
/// AgentPanel.tsx）と会話の記録のカード（LOG_DX 230 ＋ 幅 150）の右＝パネルを開いていても隠れない。
const SLOT_DX: f64 = 420.0;
/// 黙ってから区切るまでの秒（既定）。0.7 では考えながら話す途中の間で切れた（実機）ので 1 秒。
const DEFAULT_SILENCE: f64 = 1.0;
/// 聞き取りのモデル（既定）。base は聞き違いが多く、medium は既定にするには大きい（約 1.5GB）ので small。
const DEFAULT_STT_MODEL: &str = "small";

/// パネルの1行。role = user / agent / tool / error。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Line {
    role: String,
    text: String,
}

#[derive(Default)]
struct Rt {
    /// カードごとの会話履歴（role, content）。調べた一手とその結果も入る。
    history: HashMap<String, Vec<(String, String)>>,
    panel: HashMap<String, Vec<Line>>,
    /// 控え（app_data/agent/talk_<card>.json）を読み込んだカード。
    loaded: HashSet<String>,
    /// いまの進み具合（パネルを開き直したときに戻す）。
    status: HashMap<String, String>,
    /// 聞き取り中の音声プロセス（マイクは1つ＝同時に1枚だけ）。
    ear: Option<Ear>,
    gen: u64,
    say_seq: u64,
}

struct Ear {
    card: String,
    root: String,
    gen: u64,
    child: Child,
    stdin: ChildStdin,
    /// 返事を読み上げる。
    speak: bool,
    /// listening / speech / transcribing / speaking / preparing。
    state: String,
    /// ユーザーが話し始めた＝この頼みの残りは読み上げない。
    cut: Arc<AtomicBool>,
}

fn rt() -> &'static Mutex<Rt> {
    static RT: OnceLock<Mutex<Rt>> = OnceLock::new();
    RT.get_or_init(|| Mutex::new(Rt::default()))
}

fn app_data_of(app: &tauri::AppHandle) -> PathBuf {
    let state: State<AppState> = app.state();
    state.app_data.clone()
}

// --- 控え（アプリを閉じても会話を忘れない） ------------------------------------------------

fn talk_path(app_data: &Path, card: &str) -> PathBuf {
    let safe: String = card.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    app_data.join("agent").join(format!("talk_{safe}.json"))
}

fn ensure_loaded(app_data: &Path, card: &str) {
    let mut r = rt().lock().unwrap();
    if !r.loaded.insert(card.to_string()) {
        return;
    }
    let Ok(bytes) = std::fs::read(talk_path(app_data, card)) else { return };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return };
    if let Ok(h) = serde_json::from_value::<Vec<(String, String)>>(v["history"].clone()) {
        r.history.insert(card.to_string(), h);
    }
    if let Ok(p) = serde_json::from_value::<Vec<Line>>(v["panel"].clone()) {
        r.panel.insert(card.to_string(), p);
    }
}

fn save_talk(app_data: &Path, card: &str) {
    let v = {
        let r = rt().lock().unwrap();
        serde_json::json!({
            "history": r.history.get(card).cloned().unwrap_or_default(),
            "panel": r.panel.get(card).cloned().unwrap_or_default(),
        })
    };
    let path = talk_path(app_data, card);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, v.to_string());
}

// --- 画面へ ---------------------------------------------------------------------------

fn emit(app: &tauri::AppHandle, card: &str, role: &str, text: &str) {
    {
        let mut r = rt().lock().unwrap();
        let v = r.panel.entry(card.to_string()).or_default();
        v.push(Line { role: role.into(), text: text.into() });
        let over = v.len().saturating_sub(PANEL_KEEP);
        v.drain(..over);
    }
    let _ = app.emit("agent:event", serde_json::json!({ "card": card, "role": role, "text": text }));
}

/// 進み具合（パネルの1行・残さない）。空＝終わった。
fn status(app: &tauri::AppHandle, card: &str, text: &str) {
    {
        let mut r = rt().lock().unwrap();
        if text.is_empty() {
            r.status.remove(card);
        } else {
            r.status.insert(card.to_string(), text.to_string());
        }
    }
    let _ = app.emit("agent:event", serde_json::json!({ "card": card, "role": "status", "text": text }));
}

/// 書きかけの返事（出た端から画面に流す・残さない）。空＝消す。
fn draft(app: &tauri::AppHandle, card: &str, text: &str) {
    let _ = app.emit("agent:event", serde_json::json!({ "card": card, "role": "draft", "text": text }));
}

fn vad_state(app: &tauri::AppHandle, card: &str, s: &str) {
    if let Some(e) = rt().lock().unwrap().ear.as_mut().filter(|e| e.card == card) {
        e.state = s.to_string();
    }
    let _ = app.emit("agent:vad", serde_json::json!({ "card": card, "state": s }));
}

fn short(s: &str, n: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = one.chars().take(n).collect();
    if one.chars().count() > n {
        out.push('…');
    }
    out
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}\n…（以下 {} 字は省略）", s.chars().take(n).collect::<String>(), s.chars().count() - n)
}

fn safe_name(label: &str) -> String {
    let s: String = label.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    s.trim().to_string()
}

// --- コマンド（パネルから） ----------------------------------------------------------

#[tauri::command]
pub(crate) fn agent_history(card: String, state: State<AppState>) -> Vec<Line> {
    ensure_loaded(&state.app_data, &card);
    rt().lock().unwrap().panel.get(&card).cloned().unwrap_or_default()
}

/// パネルを開いたとき・カードの印のための、いまの様子。
#[tauri::command]
pub(crate) fn agent_state(card: String, state: State<AppState>) -> serde_json::Value {
    let web = card_extra(state.inner(), &card).get("web").and_then(|v| v.as_bool()).unwrap_or(false);
    let r = rt().lock().unwrap();
    let ear = r.ear.as_ref().filter(|e| e.card == card);
    serde_json::json!({
        "status": r.status.get(&card).cloned().unwrap_or_default(),
        "listening": ear.is_some(),
        "vad": ear.map(|e| e.state.clone()).unwrap_or_else(|| "off".into()),
        "speak": ear.map(|e| e.speak).unwrap_or(true),
        "web": web,
    })
}

#[tauri::command]
pub(crate) fn agent_reset(card: String, state: State<AppState>) {
    {
        let mut r = rt().lock().unwrap();
        r.history.remove(&card);
        r.panel.remove(&card);
    }
    save_talk(&state.app_data, &card);
}

/// ■: この頼みと読み上げを止める。
#[tauri::command]
pub(crate) fn agent_stop(card: String, state: State<AppState>) -> usize {
    hush(&card);
    state.jobs.cancel_cards(&[card])
}

/// 頼む。返事はイベント（agent:event）で届く。
#[tauri::command]
pub(crate) fn agent_send(card: String, text: String, app: tauri::AppHandle) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("頼む内容が空です".into());
    }
    std::thread::spawn(move || {
        run_request(&app, &card, &text);
    });
    Ok(())
}

/// 🌐 ウェブ検索の入り切り（カードの設定 web に残す）。
#[tauri::command]
pub(crate) fn agent_set_web(card: String, on: bool, app: tauri::AppHandle, state: State<AppState>) -> Result<(), String> {
    let root = with_db(state.inner(), |p| {
        let fref: Option<String> = p
            .conn
            .query_row("SELECT filter_ref FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL", rusqlite::params![card], |r| r.get(0))
            .map_err(|_| "エージェントカードが見つかりません".to_string())?;
        let mut v: serde_json::Value =
            fref.as_deref().and_then(|s| serde_json::from_str(s).ok()).ok_or("エージェントの設定がありません")?;
        let Some(o) = v.as_object_mut() else { return Err("エージェントの設定を読めません".into()) };
        if on {
            o.insert("web".into(), true.into());
        } else {
            o.remove("web");
        }
        p.conn
            .execute("UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2", rusqlite::params![v.to_string(), card])
            .map_err(|e| e.to_string())?;
        Ok(p.root.clone())
    })?;
    let _ = app.emit("mcp:changed", serde_json::json!({ "project": project_info(&root).path, "undo": [], "touched": [] }));
    Ok(())
}

/// 1つの頼みを最後まで（欄から・声からの両方）。
fn run_request(app: &tauri::AppHandle, card: &str, text: &str) {
    let app_data = app_data_of(app);
    ensure_loaded(&app_data, card);
    emit(app, card, "user", text);
    if let Err(e) = turn(app, card, text) {
        emit(app, card, "error", &e);
    }
    draft(app, card, "");
    status(app, card, "");
    save_talk(&app_data, card);
}

// --- 聞き取り（常駐の音声プロセス） ------------------------------------------------------
// 音声カードと同じ voice_server.py を、このカードのために1つ起動する。マイクと読み上げの持ち主は
// そのプロセス（読み上げ中に話しかけたら止める、をプロセスの中でやっている）。
//   → こちら: ready / state / level / speech_start / utterance / speaking / spoken / error
//   ← こちら: say{id,text,priority} / hush / stop

fn send_ear(ear: &mut Ear, cmd: serde_json::Value) {
    let _ = writeln!(ear.stdin, "{cmd}");
    let _ = ear.stdin.flush();
}

/// 読み上げを止める（待ち行列も捨てる）。
fn hush(card: &str) {
    if let Some(e) = rt().lock().unwrap().ear.as_mut().filter(|e| e.card == card) {
        e.cut.store(true, Ordering::Relaxed);
        send_ear(e, serde_json::json!({ "cmd": "hush" }));
    }
}

/// 読み上げ用に整える: コードブロック・見出しや箇条書きの記号・強調を落として1行にする。
fn speakable(text: &str) -> String {
    let mut in_code = false;
    let mut out: Vec<String> = Vec::new();
    for raw in text.lines() {
        let l = raw.trim();
        if l.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || l.is_empty() || l.starts_with('|') || l.starts_with("---") {
            continue;
        }
        let l = l.trim_start_matches(['#', '-', '*', '>', '・', '•']).trim();
        let l = l.replace("**", "").replace('`', "");
        if !l.is_empty() {
            out.push(l);
        }
    }
    out.join(" ")
}

/// 一文（まとまり）を読み上げ待ちへ。聞き取り中でない・読み上げない設定・話しかけられた後は何もしない。
fn say(card: &str, text: &str) {
    let t = speakable(text);
    if t.is_empty() {
        return;
    }
    let mut r = rt().lock().unwrap();
    r.say_seq += 1;
    let id = format!("a{}", r.say_seq);
    if let Some(e) = r.ear.as_mut().filter(|e| e.card == card && e.speak && !e.cut.load(Ordering::Relaxed)) {
        send_ear(e, serde_json::json!({ "cmd": "say", "id": id, "text": t, "priority": "urgent" }));
    }
}

/// 文の終わり（ここまで来たら読み上げてよい）。
fn is_stop(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '\n')
}

/// `talk[*spoken..]` のうち、読み上げてよい分の終わり（バイト位置・`talk` の先頭から）。
/// 最初は一文目で切る（早く声を出す）。2つ目からは SPEAK_CHUNK 字たまるまで待ってまとめる。
fn speak_cut(talk: &str, spoken: usize, flush: bool) -> Option<usize> {
    let rest = &talk[spoken..];
    if rest.trim().is_empty() {
        return None;
    }
    if flush {
        return Some(talk.len());
    }
    let ends = rest.char_indices().filter(|(_, c)| is_stop(*c)).map(|(i, c)| i + c.len_utf8());
    let end = if spoken == 0 {
        ends.take(1).last()
    } else {
        ends.last().filter(|e| rest[..*e].chars().count() >= SPEAK_CHUNK)
    }?;
    Some(spoken + end)
}

/// 聞き取りを始める（💬 おしゃべり）。黙ったところで区切って頼み、返事を読み上げる。
#[tauri::command(async)]
pub(crate) fn agent_listen_start(
    card: String,
    speak: Option<bool>,
    app: tauri::AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    if crate::voice::voice_running() {
        return Err("音声カードが聞き取り中です。マイクは1つなので、音声カードを止めてから始めてください".into());
    }
    stop_ear(&app);
    let (root, _label, _spec, extra) = load_card(state.inner(), &card)?;
    let num = |k: &str, d: f64| extra.get(k).and_then(|v| v.as_f64()).filter(|f| *f > 0.0).unwrap_or(d);
    let text = |k: &str| extra.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let mut args: Vec<String> = vec![
        "--whisper-model".into(),
        text("stt_model").unwrap_or_else(|| DEFAULT_STT_MODEL.into()),
        "--silence".into(),
        num("silence", DEFAULT_SILENCE).to_string(),
        "--compute".into(),
        "auto".into(),
    ];
    if let Some(d) = text("mic_device") {
        args.push("--device".into());
        args.push(d);
    }
    if extra.get("barge_in").and_then(|v| v.as_bool()) == Some(false) {
        args.push("--no-interrupt".into());
    }
    // 音声環境が無ければ Err("no_env")（パネルが「音声環境を構築」を出す）。
    let mut child = crate::voice::spawn_server(&state.app_data, &args, "agent.log")?;
    let stdin = child.stdin.take().ok_or("音声プロセスの stdin を取れません")?;
    let stdout = child.stdout.take().ok_or("音声プロセスの stdout を取れません")?;
    let cut = Arc::new(AtomicBool::new(false));
    let gen = {
        let mut r = rt().lock().unwrap();
        r.gen += 1;
        let gen = r.gen;
        r.ear = Some(Ear {
            card: card.clone(),
            root: root.to_string_lossy().into_owned(),
            gen,
            child,
            stdin,
            speak: speak.unwrap_or(true),
            state: "preparing".into(),
            cut,
        });
        gen
    };
    // 発話は順番に頼む（頼みの処理中に話した分はここで待つ）。
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    {
        let (app, card) = (app.clone(), card.clone());
        std::thread::spawn(move || {
            for text in rx {
                // 欄から頼んだ分がまだ動いていたら、終わるのを待ってから頼む（声の分を落とさない）。
                let t0 = std::time::Instant::now();
                while rt().lock().unwrap().status.contains_key(&card) && t0.elapsed() < std::time::Duration::from_secs(180) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                run_request(&app, &card, &text);
            }
        });
    }
    std::thread::spawn(move || read_ear(app, card, gen, stdout, tx));
    Ok(())
}

/// 音声プロセスの stdout（JSON 行）を読み、画面へ中継し、発話を頼みに回す。
fn read_ear(app: tauri::AppHandle, card: String, gen: u64, out: std::process::ChildStdout, tx: std::sync::mpsc::Sender<String>) {
    for line in std::io::BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        match v.get("ev").and_then(|e| e.as_str()) {
            Some("state") => {
                let st = s("state");
                vad_state(&app, &card, if st == "recording" { "speech" } else { &st });
            }
            Some("level") => {
                let _ = app.emit("agent:level", serde_json::json!({ "card": card, "rms": v.get("rms").and_then(|x| x.as_f64()).unwrap_or(0.0) }));
            }
            // 話しかけられた: 読み上げの残りを捨てる（いま読んでいる分は音声プロセスが止める）。
            Some("speech_start") => {
                if let Some(e) = rt().lock().unwrap().ear.as_mut().filter(|e| e.gen == gen) {
                    e.cut.store(true, Ordering::Relaxed);
                    send_ear(e, serde_json::json!({ "cmd": "hush" }));
                }
            }
            Some("utterance") => {
                let t = spoken_text(&s("text"));
                if !t.is_empty() {
                    let _ = tx.send(t);
                }
            }
            Some("speaking") => vad_state(&app, &card, "speaking"),
            Some("spoken") => vad_state(&app, &card, "listening"),
            Some("ready") => {
                let _ = app.emit("agent:ear", serde_json::json!({ "card": card, "model": s("model"), "device": s("device") }));
            }
            Some("error") => emit(&app, &card, "error", &s("message")),
            _ => {}
        }
    }
    // EOF: 止めた（stop_ear が先に外す）なら gen が合わない。合うなら勝手に終わった。
    let died = {
        let mut r = rt().lock().unwrap();
        match r.ear.as_ref().map(|e| e.gen == gen) {
            Some(true) => r.ear.take(),
            _ => None,
        }
    };
    if let Some(mut e) = died {
        let _ = e.child.wait();
        emit(&app, &card, "error", "聞き取りが止まりました（詳細は app_data の voice/agent.log）");
        let _ = app.emit("agent:vad", serde_json::json!({ "card": card, "state": "off" }));
    }
}

/// 聞き取りを止める（どのカードのものでも）。
fn stop_ear(app: &tauri::AppHandle) {
    let Some(mut e) = rt().lock().unwrap().ear.take() else { return };
    send_ear(&mut e, serde_json::json!({ "cmd": "stop" }));
    drop(e.stdin);
    let t0 = std::time::Instant::now();
    while e.child.try_wait().ok().flatten().is_none() {
        if t0.elapsed() > std::time::Duration::from_millis(1500) {
            let _ = e.child.kill();
            let _ = e.child.wait();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = app.emit("agent:vad", serde_json::json!({ "card": e.card, "state": "off" }));
}

/// 聞き取りを終える（文字にしている途中・処理中の頼みは最後まで進む。読み上げはしない）。
#[tauri::command(async)]
pub(crate) fn agent_listen_stop(card: String, app: tauri::AppHandle) {
    let mine = rt().lock().unwrap().ear.as_ref().map(|e| e.card == card).unwrap_or(false);
    if mine {
        stop_ear(&app);
    }
}

/// 🔊 返事を読み上げるかの切り替え（聞き取り中でも効く）。
#[tauri::command]
pub(crate) fn agent_set_speak(card: String, on: bool) {
    if let Some(e) = rt().lock().unwrap().ear.as_mut().filter(|e| e.card == card) {
        e.speak = on;
        if !on {
            send_ear(e, serde_json::json!({ "cmd": "hush" }));
        }
    }
}

/// 生死の突き合わせ（ops.rs の 1 秒 tick から）: プロジェクトを切り替えた・カードを消した・
/// 音声カードが動き出した（マイクは1つ）ら、聞き取りを止める。
pub(crate) fn agent_tick(app: &tauri::AppHandle) {
    let Some((card, root)) = rt().lock().unwrap().ear.as_ref().map(|e| (e.card.clone(), e.root.clone())) else { return };
    if crate::voice::voice_running() {
        stop_ear(app);
        emit(app, &card, "error", "音声カードが聞き取りを始めたので、こちらの聞き取りを止めました（マイクは1つです）");
        return;
    }
    let state: State<AppState> = app.state();
    let alive = match state.current.lock() {
        Ok(g) => g.as_ref().is_some_and(|p| {
            p.root.to_string_lossy() == root
                && p.conn
                    .query_row("SELECT 1 FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL", rusqlite::params![card], |_| Ok(()))
                    .is_ok()
        }),
        Err(_) => return,
    };
    if !alive {
        stop_ear(app);
    }
}

/// 文字起こしから話した部分だけ: 無音・音楽で Whisper が書く「(音声)」「[音楽]」だけの行は捨てる。
fn spoken_text(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|l| {
            let bracket = |a: char, b: char| l.starts_with(a) && l.ends_with(b);
            !l.is_empty() && !(bracket('(', ')') || bracket('[', ']') || bracket('（', '）') || bracket('【', '】'))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// --- 1回の頼み --------------------------------------------------------------------

fn load_card(state: &AppState, card: &str) -> Result<(PathBuf, String, flow::FilterSpec, serde_json::Value), String> {
    with_db(state, |p| {
        let (label, fref): (String, Option<String>) = p
            .conn
            .query_row(
                "SELECT label, filter_ref FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![card],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| "エージェントカードが見つかりません".to_string())?;
        let fref = fref.ok_or("エージェントの設定がありません")?;
        let spec: flow::FilterSpec = serde_json::from_str(&fref).map_err(|e| format!("設定を読めません: {e}"))?;
        let extra: serde_json::Value = serde_json::from_str(&fref).unwrap_or(serde_json::Value::Null);
        Ok((p.root.clone(), label, spec, extra))
    })
}

/// カードの設定（JSON）。読めなければ Null。
fn card_extra(state: &AppState, card: &str) -> serde_json::Value {
    load_card(state, card).map(|(_, _, _, x)| x).unwrap_or(serde_json::Value::Null)
}

/// 1回の頼み。
fn turn(app: &tauri::AppHandle, card: &str, text: &str) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let (root, label, spec, extra) = load_card(&state, card)?;
    let job = state
        .jobs
        .register_flow(card, &label, &root.to_string_lossy())
        .map_err(|_| "前の頼みをまだ処理しています（■ で止められます）".to_string())?;
    let _guard = JobGuard::new(&state.jobs, job.id);
    let _ = app.emit("job:started", serde_json::json!({ "job_id": job.id, "kind": "flow", "label": label, "card": card }));
    let r = turn_inner(app, card, &root, &label, &spec, &extra, text, &job);
    let _ = app.emit(if r.is_ok() { "job:done" } else { "job:failed" }, serde_json::json!({ "job_id": job.id }));
    r
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tool {
    Search,
    Read,
    Web,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Search => "調べる",
            Tool::Read => "読む",
            Tool::Web => "ウェブ",
        }
    }
}

/// AI が書いた「調べる一手」。
#[derive(Debug, PartialEq)]
struct Call {
    tool: Tool,
    arg: String,
}

/// 「【調べる】問い」の1行を読む。知らない名前は None（ふつうの返事として扱う）。
fn parse_call(line: &str) -> Option<Call> {
    let l = line.trim().strip_prefix('【')?;
    let (name, rest) = l.split_once('】')?;
    let tool = match name.trim() {
        "調べる" | "資料" | "検索" | "探す" | "さがす" | "索引" => Tool::Search,
        "読む" | "よむ" | "開く" => Tool::Read,
        "ウェブ" | "Web" | "web" | "WEB" | "ウェブ検索" | "Web検索" | "ネット" => Tool::Web,
        _ => return None,
    };
    // 小さいモデルは説明の言葉ごと書くことがある（「問い … ウォッチャ」「検索語 “tauri”」）。飾りは落とす。
    let junk = |c: char| c.is_whitespace() || matches!(c, ':' | '：' | '…' | '_' | '=' | '→' | '「' | '」' | '『' | '』' | '"' | '`' | '“' | '”');
    let mut arg = rest.trim_matches(junk);
    for word in ["問い", "検索語", "ファイル名", "ファイルの名前"] {
        if let Some(r) = arg.strip_prefix(word).filter(|r| r.starts_with(junk)) {
            arg = r.trim_matches(junk);
        }
    }
    Some(Call { tool, arg: arg.to_string() })
}

/// 返事のうち、ふつうの文の部分（最初の「【」で始まる行の手前まで）。出た端から呼ぶので、
/// 「【」で始まった行は一手かどうか決まるまで見せない・読み上げない。
fn talk_part(full: &str) -> &str {
    let mut at = 0;
    for line in full.split_inclusive('\n') {
        if line.trim_start().starts_with('【') {
            return &full[..at];
        }
        at += line.len();
    }
    full
}

/// 書き終わった返事を「ふつうの文」と「調べる一手」に分ける。一手でなければ全部がふつうの文。
fn split_call(full: &str) -> (String, Option<Call>) {
    let talk = talk_part(full);
    if talk.len() < full.len() {
        if let Some(c) = full[talk.len()..].lines().next().and_then(parse_call) {
            return (talk.trim().to_string(), Some(c));
        }
    }
    (full.trim().to_string(), None)
}

#[allow(clippy::too_many_arguments)]
fn turn_inner(
    app: &tauri::AppHandle,
    card: &str,
    root: &Path,
    label: &str,
    spec: &flow::FilterSpec,
    extra: &serde_json::Value,
    text: &str,
    job: &Job,
) -> Result<(), String> {
    let max = extra.get("max_lookups").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_LOOKUPS).clamp(1, MAX_LOOKUPS);
    let web = extra.get("web").and_then(|v| v.as_bool()).unwrap_or(false);
    let history = rt().lock().unwrap().history.get(card).cloned().unwrap_or_default();
    // 新しい頼み＝読み上げてよい（前の頼みへの割り込みの印を下ろす）。
    if let Some(e) = rt().lock().unwrap().ear.as_ref().filter(|e| e.card == card) {
        e.cut.store(false, Ordering::Relaxed);
    }
    // この頼みの中のやりとり（調べる一手と結果）。AI に渡す分と、履歴に残す分（結果を短くしたもの）。
    let mut scratch: Vec<(String, String)> = Vec::new();
    let mut keep: Vec<(String, String)> = Vec::new();
    let mut used: Vec<String> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    let mut last_key: Option<String> = None;
    let mut reply = String::new();
    for round in 0..=max {
        if job.cancel.load(Ordering::Relaxed) {
            return Err("中断しました".into());
        }
        let last = round == max;
        emit_job_progress(app, job, Some(&format!("{label}: {}", if round == 0 { "返事".into() } else { format!("調べる {round}") })), None);
        status(app, card, "考え中…");
        let world = World::scan(root, card)?;
        let mut messages = vec![serde_json::json!({ "role": "system", "content": system_prompt(&world, spec.system.as_deref(), web) })];
        for (role, content) in bounded_history(&history) {
            messages.push(serde_json::json!({ "role": role, "content": content }));
        }
        messages.push(serde_json::json!({ "role": "user", "content": text }));
        for (role, content) in &scratch {
            messages.push(serde_json::json!({ "role": role, "content": content }));
        }
        // 返事を出た端から: 画面へ流し、おしゃべり中は文ごとに読み上げる。
        let mut full = String::new();
        let mut spoken = 0usize;
        let mut spoken_chars = 0usize;
        let mut shown = std::time::Instant::now();
        let mut speak_more = |talk: &str, spoken: &mut usize, flush: bool| {
            while let Some(end) = speak_cut(talk, *spoken, flush) {
                let piece = &talk[*spoken..end];
                *spoken = end;
                if spoken_chars < SPEAK_CHARS {
                    spoken_chars += piece.chars().count();
                    say(card, piece);
                }
            }
        };
        flow::run_ai_chat_stream_rec(
            spec,
            &messages,
            &job.cancel,
            &mut |kind, tok| {
                if kind != flow::ChatChunk::Content {
                    return;
                }
                full.push_str(tok);
                let talk = talk_part(&full);
                if shown.elapsed() >= std::time::Duration::from_millis(60) {
                    shown = std::time::Instant::now();
                    draft(app, card, talk);
                }
                speak_more(talk, &mut spoken, false);
            },
            &mut None,
        )
        .map_err(|e| format!("AI に頼めません: {e}"))?;
        if job.cancel.load(Ordering::Relaxed) {
            return Err("中断しました".into());
        }
        job.trace.note(card, &format!("返事 {}", round + 1), &short(&full, 300));
        let (talk, call) = split_call(&full);
        // 読み上げの残りを言い切る（一手を書いたなら、その手前の文まで）。
        speak_more(if call.is_some() { talk_part(&full) } else { &full }, &mut spoken, true);
        let Some(call) = call.filter(|_| !last) else {
            // ふつうの返事（上限まで調べてまだ一手を書いたら、そこまでの文か、調べきれなかった知らせ）。
            reply = if talk.is_empty() { "調べきれませんでした。もう少し絞って聞いてください。".into() } else { talk };
            break;
        };
        // 調べる一手。手前の文（「確認しますね」など）は返事として残す。
        if !talk.is_empty() {
            draft(app, card, "");
            emit(app, card, "agent", &talk);
            said.push(talk.clone());
        } else if round == 0 {
            say(card, "調べますね。");
        }
        let key = format!("{:?}\u{0}{}", call.tool, call.arg);
        let (result, line) = if last_key.as_deref() == Some(key.as_str()) {
            ("同じことを続けて調べようとしました。ここまでの結果で返事をしてください。".to_string(), String::new())
        } else {
            status(app, card, &format!("{}：{}", call.tool.name(), short(&call.arg, 30)));
            match call.tool {
                Tool::Search => do_search(root, &world, &call.arg, text),
                Tool::Read => do_read(root, &world, &call.arg),
                Tool::Web => do_web(&call.arg, web, &job.cancel),
            }
            .unwrap_or_else(|e| (format!("失敗しました: {e}"), format!("✗ {}", short(&e, 80))))
        };
        last_key = Some(key);
        if !line.is_empty() {
            emit(app, card, "tool", &line);
            used.push(line);
        }
        // 読むは読む用の上限（do_read が切った長さのまま渡す）。
        let lim = if call.tool == Tool::Read { READ_CHARS + 100 } else { RESULT_CHARS };
        let tail = if round + 1 == max { "\n\n（これ以上は調べられません。ここまでの結果で返事をしてください）" } else { "" };
        let head = format!("【結果: {}】\n", call.tool.name());
        scratch.push(("assistant".into(), full.trim().to_string()));
        scratch.push(("user".into(), format!("{head}{}{tail}", cut(&result, lim))));
        keep.push(("assistant".into(), full.trim().to_string()));
        keep.push(("user".into(), format!("{head}{}", cut(&result, KEEP_CHARS))));
    }
    draft(app, card, "");
    emit(app, card, "agent", &reply);
    {
        let mut r = rt().lock().unwrap();
        let h = r.history.entry(card.to_string()).or_default();
        h.push(("user".into(), text.into()));
        // 調べた一手と結果も残す（「さっきの資料だと」に答えられる・上限で止まっても続きから話せる）。
        h.extend(keep);
        h.push(("assistant".into(), reply.clone()));
    }
    said.push(reply);
    let path = log_md(root, label, text, &used, &said.join("\n\n"));
    if let Err(e) = place_log_card(app, root, card, &path) {
        emit(app, card, "error", &format!("会話の記録をカードにできませんでした: {e}"));
    }
    Ok(())
}

/// md を書いてカードにする。エージェントカードから (dx, dy) の位置へ。
fn place_card(app: &tauri::AppHandle, root: &Path, card: &str, path: &Path, dx: f64, dy: f64) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("会話.md").to_string();
    let p = open_project_at(root)?;
    let id = place_md_card(&p, card, path, &fname, &bytes, dx, dy)?;
    let _ = app.emit("mcp:changed", serde_json::json!({ "project": project_info(root).path, "undo": [], "touched": [id] }));
    Ok(())
}

/// 会話の記録をエージェントの出力カードにする（AI カードの対話ログと同じ置き方:
/// 右隣に置いて矢印を張る・2回目からは中身の差し替え）。
fn place_log_card(app: &tauri::AppHandle, root: &Path, card: &str, path: &Path) -> Result<(), String> {
    place_card(app, root, card, path, LOG_DX, 0.0)
}

fn bounded_history(h: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut len = 0;
    for m in h.iter().rev() {
        if !out.is_empty() && len + m.1.chars().count() > HISTORY_CHARS {
            break;
        }
        len += m.1.chars().count();
        out.push(m.clone());
    }
    out.reverse();
    out
}

/// 会話ログ（人が読む md・追記）: プロジェクト直下の エージェント/<カード名>の会話.md。
fn log_md(root: &Path, label: &str, text: &str, used: &[String], reply: &str) -> PathBuf {
    let path = root.join(LOG_DIR).join(format!("{}の会話.md", safe_name(label)));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let fresh = !path.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        if fresh {
            let _ = writeln!(f, "# {label}の会話\n");
        }
        let _ = writeln!(f, "## {}\n\n**あなた**: {text}\n", chrono::Local::now().format("%Y-%m-%d %H:%M"));
        for u in used {
            let _ = writeln!(f, "- {u}");
        }
        if !used.is_empty() {
            let _ = writeln!(f);
        }
        let _ = writeln!(f, "**エージェント**: {reply}\n");
    }
    path
}

// --- 切り出し・まとめ（次のフローへ渡すカード） -------------------------------------------

/// 切り出し・まとめを置く段（パネルの右の列を、上から順に）。
fn next_slot(root: &Path, card: &str) -> f64 {
    let n: i64 = open_project_at(root)
        .ok()
        .and_then(|p| {
            p.conn
                .query_row(
                    "SELECT COUNT(*) FROM relations r JOIN virtual_bodies v ON v.id = r.to_id AND v.deleted_at IS NULL
                     WHERE r.from_id = ?1 AND r.kind = 'flow' AND r.deleted_at IS NULL AND v.label NOT LIKE '%の会話.md'",
                    rusqlite::params![card],
                    |r| r.get(0),
                )
                .ok()
        })
        .unwrap_or(0);
    SLOT_DY * n as f64
}

/// `エージェント/<カード名>の<kind>_<日時>.md` に書いてカードにする。返り値＝ファイル名。
fn write_card(app: &tauri::AppHandle, root: &Path, card: &str, label: &str, kind: &str, body: &str) -> Result<String, String> {
    let dir = root.join(LOG_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let fname = format!("{}の{kind}_{}.md", safe_name(label), chrono::Local::now().format("%m%d_%H%M%S"));
    let path = dir.join(&fname);
    std::fs::write(&path, body).map_err(|e| format!("{fname}: {e}"))?;
    place_card(app, root, card, &path, SLOT_DX, next_slot(root, card))?;
    Ok(fname)
}

/// 📎 返事（または選んだ部分）を1枚のカードにする。
#[tauri::command(async)]
pub(crate) fn agent_clip(card: String, text: String, app: tauri::AppHandle, state: State<AppState>) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("切り出す内容が空です".into());
    }
    let (root, label, _, _) = load_card(state.inner(), &card)?;
    let fname = write_card(&app, &root, &card, &label, "切り出し", &format!("{text}\n"))?;
    emit(&app, &card, "tool", &format!("📎 カードにしました：{fname}"));
    save_talk(&state.app_data, &card);
    Ok(fname)
}

/// まとめに渡す会話（調べた一手と結果は除く・新しい方から SUMMARY_CHARS 字）。
fn transcript(h: &[(String, String)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut len = 0;
    for (role, content) in h.iter().rev() {
        let tool_talk = if role == "user" { content.starts_with("【結果") } else { split_call(content).1.is_some() };
        if tool_talk {
            continue;
        }
        len += content.chars().count();
        if !parts.is_empty() && len > SUMMARY_CHARS {
            break;
        }
        parts.push(format!("{}: {content}", if role == "user" { "ユーザー" } else { "エージェント" }));
    }
    parts.reverse();
    parts.join("\n\n")
}

const SUMMARY_PROMPT: &str = "次の壁打ちの会話を、あとの作業にそのまま渡せる文書にまとめてください。\n\
    Markdown で、次の見出しを使います（中身が無い見出しは省く）。\n\
    # （内容を表す短い題）\n\
    ## 目的\n\
    ## 決まったこと\n\
    ## カードの構成の案（何を入れて → どのカードで何をして → 何が出るか、を順に）\n\
    ## 未決の点・次にやること\n\
    決まり: 会話に出ていないことは足さない。話し言葉は書き言葉に直す。前置きや結びの挨拶は書かない。";

/// 📝 ここまでの会話を文書にまとめて1枚のカードにする（まとめたカードをコンストラクタにつなげば組める）。
#[tauri::command]
pub(crate) fn agent_summarize(card: String, app: tauri::AppHandle, state: State<AppState>) -> Result<(), String> {
    ensure_loaded(&state.app_data, &card);
    if rt().lock().unwrap().history.get(&card).map(|h| h.is_empty()).unwrap_or(true) {
        return Err("まだ会話がありません".into());
    }
    std::thread::spawn(move || {
        if let Err(e) = summarize(&app, &card) {
            emit(&app, &card, "error", &e);
        }
        status(&app, &card, "");
        save_talk(&app_data_of(&app), &card);
    });
    Ok(())
}

fn summarize(app: &tauri::AppHandle, card: &str) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let (root, label, spec, _) = load_card(&state, card)?;
    let job = state
        .jobs
        .register_flow(card, &label, &root.to_string_lossy())
        .map_err(|_| "前の頼みをまだ処理しています（■ で止められます）".to_string())?;
    let _guard = JobGuard::new(&state.jobs, job.id);
    let _ = app.emit("job:started", serde_json::json!({ "job_id": job.id, "kind": "flow", "label": label, "card": card }));
    status(app, card, "まとめています…");
    let talk = transcript(&rt().lock().unwrap().history.get(card).cloned().unwrap_or_default());
    let messages = vec![
        serde_json::json!({ "role": "system", "content": SUMMARY_PROMPT }),
        serde_json::json!({ "role": "user", "content": format!("会話:\n\n{talk}") }),
    ];
    let r = flow::run_ai_chat_stream_rec(&spec, &messages, &job.cancel, &mut |_, _| {}, &mut None)
        .map_err(|e| format!("AI に頼めません: {e}"))
        .and_then(|md| {
            if job.cancel.load(Ordering::Relaxed) {
                return Err("中断しました".into());
            }
            if md.trim().is_empty() {
                return Err("まとめが空でした。もう一度試してください".into());
            }
            write_card(app, &root, card, &label, "まとめ", &format!("{}\n", md.trim()))
        });
    let _ = app.emit(if r.is_ok() { "job:done" } else { "job:failed" }, serde_json::json!({ "job_id": job.id }));
    let fname = r?;
    emit(app, card, "tool", &format!("📝 まとめをカードにしました：{fname}（コンストラクタにつなぐと組めます）"));
    say(card, "まとめをカードにしました。");
    Ok(())
}

// --- いまの状況（AI に見せる材料） ----------------------------------------------------

struct World {
    files: Vec<construct::ExFile>,
    indexes: Vec<String>,
    /// エージェントカードへ矢印でつないだ材料（優先して使う）。ファイルカードは F 番号、
    /// 索引レポートはその索引の名前。
    linked: Vec<String>,
}

impl World {
    fn scan(root: &Path, card: &str) -> Result<World, String> {
        let p = open_project_at(root)?;
        let files = construct::existing_files(&p);
        let linked = linked_materials(&p, root, card, &files);
        Ok(World { files, indexes: rag::list_indexes(root), linked })
    }

    /// 索引の既定: つないだ索引 → 索引が1つだけならそれ。
    fn default_index(&self) -> Option<&String> {
        self.linked
            .iter()
            .find(|l| self.indexes.contains(l))
            .or(if self.indexes.len() == 1 { self.indexes.first() } else { None })
    }

    /// 「読む」の相手を名前で探す: F番号 → 同じ名前 → 名前を含む（つないだ材料を先に）。
    fn find_file(&self, name: &str) -> Option<&construct::ExFile> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let linked = |f: &&construct::ExFile| self.linked.contains(&f.r);
        self.files
            .iter()
            .find(|f| f.r == name || f.label == name)
            .or_else(|| self.files.iter().filter(linked).find(|f| f.label.contains(name) || name.contains(&f.label)))
            .or_else(|| self.files.iter().find(|f| f.label.contains(name) || name.contains(&f.label)))
    }
}

/// エージェントカードへ入る矢印の元（ファイルカードだけ）。索引レポートなら索引の名前に読み替える
/// （冒頭の目印行 `<!-- texos-rag-index: 名前 -->`・検索カードと同じ見分け方）。
fn linked_materials(p: &Project, root: &Path, card: &str, files: &[construct::ExFile]) -> Vec<String> {
    let Ok(mut st) = p.conn.prepare(
        "SELECT rel.from_id, COALESCE(v.committed_path, v.origin_path) FROM relations rel
         JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
         WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
         ORDER BY rel.created_at",
    ) else {
        return Vec::new();
    };
    let rows: Vec<(String, Option<String>)> = st
        .query_map(rusqlite::params![card], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for (id, path) in rows {
        let Some(f) = files.iter().find(|f| f.id == id) else { continue };
        let head = path
            .and_then(|s| std::fs::read(load_path(root, &s)).ok())
            .map(|b| String::from_utf8_lossy(&b[..b.len().min(300)]).into_owned());
        // 索引レポートは「その索引」と「文書の一覧のカード」の両方として使える。
        if let Some(index) = head.as_deref().and_then(rag::marker_of) {
            out.push(index);
        }
        out.push(f.r.clone());
    }
    out
}

/// エージェントへの指示（system）。話し方＋調べ方＋いまの状況＋カード設定の追加の指示。
fn system_prompt(w: &World, extra: Option<&str>, web: bool) -> String {
    let mut s = String::from(
        "あなたは TexoS の壁打ち相手です。TexoS は、カードを矢印でつないで処理の流れ（フロー）を作り、\
         キャンバスに残すアプリです。ユーザーは声か文字で話しかけ、考えを整理したり、どんなカードの構成にするか、\
         どんな進め方にするかを、あなたと話しながら固めます。\n\n\
         話し方:\n\
         - 返事は短く。ふつうは2〜4文。声で読み上げられるので、話し言葉で、見出し・箇条書き・記号・絵文字は使わない。\n\
         - 一度に全部を言わない。要点を返し、考えを前に進める問いを1つ添える。相づちだけで終わらない。\n\
         - 案を求められたら具体的に言う。カードの構成を考えるときは「何を入れて、どのカードで何をして、何が出るか」を順に言う。\
           案を並べてほしいと頼まれたときだけ、番号をつけて短く並べる。\n\
         - あなた自身はカードを並べたり実行したりしない。組むのはコンストラクタというカードの役目。\
           話がまとまってきたら、パネルの「まとめてカードに」を勧める（まとめのカードをコンストラクタにつなぐと組める）。\n\
         - 分からないことは分からないと言う。手元の資料に無いことを、あるように言わない。\n\n\
         調べ方（必要なときだけ。雑談や、知っていることで答えられる話には使わない）:\n\
         調べるときは、返事の代わりに、かっこの印で始まる1行だけを書きます。結果が【結果】として届くので、それを踏まえて返事をします。\n\
         - 手元の資料の索引から探す: 【調べる】のすぐ後ろに、探したいことを書く。例: 【調べる】ウォッチャの使い方\n\
         - ファイルカードの中身を読む: 【読む】のすぐ後ろに、ファイルの名前を書く。例: 【読む】議事録.md\
           （フォルダ全体の一覧や分類は、索引レポートを読む）\n",
    );
    if web {
        s.push_str(
            "- ウェブで調べる（手元の資料に無い、外の情報や新しい情報）: 【ウェブ】のすぐ後ろに、検索語を書く。例: 【ウェブ】Tauri 最新版\n",
        );
    }
    s.push_str(
        "資料について聞かれたら、調べてから答え、出典のファイル名を一言添える。\
         ウェブで調べたときは、どのサイトの話かを一言添える（URL は言わない。出典の URL は画面と記録に並ぶ）。\
         調べた結果は長いので、声では要点だけを言う（詳しい中身は記録に残る）。\n\n\
         構成を考えるときの材料（TexoS のカードの種類）:\n\
         ファイルカード（文書・画像・音声など）、AI カード（変換・対話）、Web 取得と Web 検索、索引と検索（手元の文書を調べる）、\
         文字の加工（結合・置換・抽出など）、文字起こしと読み上げ、画像生成、外部プログラムとシェル、\
         タイマ・フォルダ監視・ボタンなどのきっかけ、コンストラクタ（文章からフローを組む）。\n\n\
         いまの状況:\n",
    );
    s.push_str("索引: ");
    if w.indexes.is_empty() {
        s.push_str("なし（【調べる】は使えない。📚 索引カードで作れる）\n");
    } else {
        s.push_str(&format!("{}\n", w.indexes.join("、")));
    }
    s.push_str("ファイルカード:\n");
    if w.files.is_empty() {
        s.push_str("（なし）\n");
    }
    for f in &w.files {
        s.push_str(&format!("- {}（{}）\n", f.label, if f.mime.is_empty() { "種類不明" } else { &f.mime }));
    }
    if !web {
        s.push_str("ウェブ検索: オフ（ユーザーがパネルの 🌐 で入れると使える。外の情報が要るときは、そう伝える）\n");
    }
    if !w.linked.is_empty() {
        let names: Vec<String> = w
            .linked
            .iter()
            .map(|l| match w.files.iter().find(|f| &f.r == l) {
                Some(f) => f.label.clone(),
                None => format!("索引「{l}」"),
            })
            .collect();
        s.push_str(&format!(
            "このエージェントにつないだ材料（ユーザーが選んだもの。調べる・読むときはまずこれを使う）: {}\n",
            names.join("、")
        ));
    }
    if let Some(x) = extra.map(str::trim).filter(|x| !x.is_empty()) {
        s.push_str(&format!("\n追加の指示（カードの設定）:\n{x}\n"));
    }
    s
}

// --- 調べる道具 ----------------------------------------------------------------------

/// (AI に返す結果, パネルと記録に残す1行)。
type ToolOut = Result<(String, String), String>;

/// `索引名｜問い` と書かれていればその索引、無ければ既定（つないだ索引 → 1つだけならそれ → 先頭）。
fn do_search(root: &Path, w: &World, arg: &str, text: &str) -> ToolOut {
    if w.indexes.is_empty() {
        return Err("索引がありません（📚 索引カードで作れます）".into());
    }
    let named = arg.split_once(['｜', '|']).map(|(n, q)| (n.trim(), q.trim())).filter(|(n, _)| w.indexes.iter().any(|i| i == n));
    let (name, query) = match named {
        Some((n, q)) => (n.to_string(), q),
        None => (w.default_index().unwrap_or(&w.indexes[0]).clone(), arg),
    };
    let query = if query.is_empty() { text } else { query };
    // 埋め込みの接続先は既定（Ollama）。モデルは索引に記録したもの。
    let espec: flow::FilterSpec =
        serde_json::from_value(serde_json::json!({ "builtin": "rag_search" })).map_err(|e| e.to_string())?;
    match rag::search_md(root, &espec, &name, query, SEARCH_K)? {
        Some(md) => Ok((md, format!("🔎 索引「{name}」で調べました：{}", short(query, 40)))),
        None => Ok(("近い箇所はありませんでした".into(), format!("🔎 索引「{name}」：見つからず"))),
    }
}

fn do_read(root: &Path, w: &World, arg: &str) -> ToolOut {
    let f = w.find_file(arg).ok_or_else(|| format!("ファイルカード「{arg}」はありません（一覧にある名前で選んでください）"))?;
    let p = open_project_at(root)?;
    let path: Option<String> = p
        .conn
        .query_row(
            "SELECT COALESCE(committed_path, origin_path) FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![f.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let full = PathBuf::from(load_path(root, &path.ok_or("ファイルの場所が分かりません")?));
    let ext = full.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
    let texty = f.mime.starts_with("text/") || f.mime == "application/json" || matches!(ext.as_str(), "pdf" | "docx");
    if !texty {
        return Err(format!("「{}」は文字のファイルではありません（{}）", f.label, f.mime));
    }
    let bytes = std::fs::read(&full).map_err(|e| format!("{}: {e}", f.label))?;
    let text = rag::doc_text(&ext, &bytes)?;
    Ok((cut(&text, READ_CHARS), format!("📄 「{}」を読みました", f.label)))
}

/// ウェブ検索（手元の SearXNG）。外へ出るのは検索語だけ。使ったことは検索語ごと記録に残す。
fn do_web(query: &str, on: bool, cancel: &AtomicBool) -> ToolOut {
    if !on {
        return Ok((
            "ウェブ検索はオフです。手元の知識と資料で答え、外の情報が要るならパネルの 🌐 を入れるよう伝えてください。".into(),
            "🌐 ウェブ検索はオフです（パネルの 🌐 で入れられます）".into(),
        ));
    }
    if query.is_empty() {
        return Err("検索語が空です".into());
    }
    let spec = flow::parse_spec(&serde_json::json!({ "builtin": "web_search", "limit": WEB_K, "max_queries": 1 }).to_string())?;
    match web::run_web_search(&spec, Some(query.as_bytes()), Some(cancel))? {
        Some(o) => {
            let md = String::from_utf8_lossy(&o.bytes).into_owned();
            // 根拠にしたページを、検索語の下に並べる（画面の1行と会話の記録の両方に残る）。
            let line = format!("🌐 ウェブで調べました：{}{}", short(query, 40), sources_of(&md));
            Ok((md, line))
        }
        None => Ok(("結果はありませんでした".into(), format!("🌐 ウェブ：見つからず（{}）", short(query, 40)))),
    }
}

/// 検索結果（web.rs format_search_md: 「### タイトル」の次の行が裸の URL）から、出典の並び
/// （1件1行の `  - [タイトル](URL)`・行頭に改行）を作る。
fn sources_of(md: &str) -> String {
    let mut out = String::new();
    let mut lines = md.lines();
    while let Some(l) = lines.next() {
        let Some(title) = l.strip_prefix("### ") else { continue };
        let Some(url) = lines.next().map(str::trim).filter(|u| u.starts_with("http")) else { continue };
        let title = short(title, 60).replace(['[', ']'], " ");
        out.push_str(&format!("\n  - [{}]({url})", title.trim()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_sources_are_listed_under_the_query() {
        let md = "# Web検索結果\n\n条件: x\n\n## 🔍 q（2件）\n\n### A [公式]\nhttps://a.example/x\n抜粋\n\n### B\nhttps://b.example\n";
        assert_eq!(sources_of(md), "\n  - [A  公式](https://a.example/x)\n  - [B](https://b.example)");
        assert_eq!(sources_of("結果なし"), "");
    }

    #[test]
    fn call_is_read_from_its_own_line_only() {
        assert_eq!(parse_call("【調べる】ウォッチャの使い方"), Some(Call { tool: Tool::Search, arg: "ウォッチャの使い方".into() }));
        assert_eq!(parse_call("  【読む】：「議事録_10月.md」"), Some(Call { tool: Tool::Read, arg: "議事録_10月.md".into() }));
        assert_eq!(parse_call("【ウェブ】Tauri 2 sidecar").unwrap().tool, Tool::Web);
        // 説明の言葉ごと書かれても、中身だけ取る。
        assert_eq!(parse_call("【調べる】問い … ウォッチャ 索引").unwrap().arg, "ウォッチャ 索引");
        assert_eq!(parse_call("【ウェブ】検索語 “tauri latest version”").unwrap().arg, "tauri latest version");
        assert_eq!(parse_call("【読む】ファイル名_議事録_10月.md").unwrap().arg, "議事録_10月.md");
        assert_eq!(parse_call("【調べる】問い合わせ窓口").unwrap().arg, "問い合わせ窓口", "言葉の一部は落とさない");
        // 知らない名前・ふつうの文は一手ではない。
        assert_eq!(parse_call("【注意】ここは大事です"), None);
        assert_eq!(parse_call("調べる 資料"), None);

        assert_eq!(split_call("こんにちは。今日は何を考えますか？"), ("こんにちは。今日は何を考えますか？".into(), None));
        let (talk, call) = split_call("確認しますね。\n【調べる】見積もりの期限\n");
        assert_eq!((talk.as_str(), call.unwrap().arg.as_str()), ("確認しますね。", "見積もりの期限"));
        // 見出しのつもりの【…】は、返事としてそのまま残す。
        let (talk, call) = split_call("案です。\n【注意】費用は未確認です。");
        assert!(call.is_none() && talk.contains("【注意】"));
    }

    #[test]
    fn bracket_lines_are_held_back_while_streaming() {
        assert_eq!(talk_part("調べますね。\n【調"), "調べますね。\n");
        assert_eq!(talk_part("【"), "");
        assert_eq!(talk_part("はい、「【」は使いません"), "はい、「【」は使いません");
    }

    #[test]
    fn speech_starts_at_the_first_sentence_then_in_chunks() {
        let t = "はい。それは良い案ですね";
        assert_eq!(speak_cut(t, 0, false), Some("はい。".len()), "一文目はすぐ読む");
        let s = "はい。".len();
        assert_eq!(speak_cut(t, s, false), None, "文の途中は待つ");
        let t2 = format!("はい。{}。短い。", "あ".repeat(SPEAK_CHUNK));
        assert_eq!(speak_cut(&t2, s, false), Some(t2.len()), "たまったら文の切れ目までまとめて読む");
        assert_eq!(speak_cut("はい。短い。", s, false), None, "短い続きはまとめるために待つ");
        assert_eq!(speak_cut("はい。短い", s, true), Some("はい。短い".len()), "書き終わったら残りを読む");
        assert_eq!(speak_cut("はい。\n", s, true), None);
    }

    #[test]
    fn markdown_is_dropped_before_speaking() {
        assert_eq!(speakable("## 案\n- **A** を使う\n```\ncode\n```\n`x` です"), "案 A を使う x です");
    }

    fn project() -> (PathBuf, Project) {
        // 並列のテストが同じミリ秒に作っても重ならないよう通し番号も付ける。
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("texos_agent_{}_{}_{n}", std::process::id(), db::now_ms()));
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
        (dir, p)
    }

    fn card(p: &Project, id: &str, kind: &str, spec: &str, t: i64) {
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
                 VALUES (?1, 'filter:sentinel', ?1, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![id, t, kind, spec],
            )
            .unwrap();
    }

    fn arrow(p: &Project, from: &str, to: &str) {
        p.conn
            .execute(
                "INSERT INTO relations (from_id, to_id, kind, created_at) VALUES (?1, ?2, 'flow', ?3)",
                rusqlite::params![from, to, db::now_ms()],
            )
            .unwrap();
    }

    #[test]
    fn linked_report_gives_its_index_and_the_card() {
        let (dir, p) = project();
        card(&p, "agent", "filter", r#"{"builtin":"agent"}"#, 1);
        std::fs::write(dir.join("rep.md"), "<!-- texos-rag-index: 書類 -->\n# 索引「書類」\n").unwrap();
        std::fs::write(dir.join("memo.md"), "メモ").unwrap();
        for (id, f, t) in [("rep", "rep.md", 2), ("memo", "memo.md", 3)] {
            p.conn
                .execute(
                    "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, committed_path)
                     VALUES (?1, 'filter:sentinel', ?2, 0, 0, ?3, 'file', ?2)",
                    rusqlite::params![id, f, t],
                )
                .unwrap();
        }
        arrow(&p, "rep", "agent");
        arrow(&p, "memo", "agent");
        let files = construct::existing_files(&p);
        let linked = linked_materials(&p, &dir, "agent", &files);
        let r = |label: &str| files.iter().find(|f| f.label == label).unwrap().r.clone();
        assert_eq!(linked.len(), 3, "{linked:?}");
        assert!(linked.contains(&"書類".to_string()) && linked.contains(&r("rep.md")) && linked.contains(&r("memo.md")));
        let w = World { files, indexes: vec!["他".into(), "書類".into()], linked };
        assert_eq!(w.default_index().map(String::as_str), Some("書類"), "索引が複数でも、つないだ索引を使う");
        assert_eq!(w.find_file("memo").map(|f| f.label.as_str()), Some("memo.md"), "名前の一部でも読める");
        assert!(w.find_file("無い").is_none());
        let p_off = system_prompt(&w, Some("敬語で"), false);
        assert!(p_off.contains("つないだ材料") && p_off.contains("敬語で") && !p_off.contains("例: 【ウェブ】"));
        assert!(system_prompt(&w, None, true).contains("例: 【ウェブ】"));
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_is_refused_while_off() {
        let cancel = AtomicBool::new(false);
        let (result, line) = do_web("Tauri", false, &cancel).unwrap();
        assert!(result.contains("オフ") && line.contains("オフ"));
    }

    #[test]
    fn whisper_markers_are_not_speech() {
        assert_eq!(spoken_text("(音声)\n"), "");
        assert_eq!(spoken_text("[音楽]\n明日の予定を\n（拍手）\n教えて\n"), "明日の予定を 教えて");
    }

    #[test]
    fn history_keeps_newest_within_budget() {
        let long = "あ".repeat(HISTORY_CHARS);
        let h = vec![("user".to_string(), long.clone()), ("assistant".to_string(), "x".to_string())];
        let b = bounded_history(&h);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].1, "x");
    }

    #[test]
    fn summary_reads_the_talk_without_the_lookups() {
        let h: Vec<(String, String)> = [
            ("user", "見積もりの期限は？"),
            ("assistant", "【調べる】見積もりの期限"),
            ("user", "【結果: 調べる】\n10月末"),
            ("assistant", "10月末です。"),
        ]
        .iter()
        .map(|(r, c)| (r.to_string(), c.to_string()))
        .collect();
        assert_eq!(transcript(&h), "ユーザー: 見積もりの期限は？\n\nエージェント: 10月末です。");
    }

    /// 実機の AI で壁打ちの返し方を見る（cargo test --lib agent_live -- --ignored --nocapture）。
    /// AGENT_LIVE_PROVIDER / _MODEL / _ENDPOINT で頭を選ぶ（既定 Ollama gemma3）。道具は呼ばない。
    #[test]
    #[ignore]
    fn agent_live() {
        let provider = std::env::var("AGENT_LIVE_PROVIDER").unwrap_or_else(|_| "ollama".into());
        let model = std::env::var("AGENT_LIVE_MODEL").unwrap_or_else(|_| "gemma3:latest".into());
        let mut spec = serde_json::json!({ "builtin": "agent", "provider": provider, "model": model });
        if let Ok(ep) = std::env::var("AGENT_LIVE_ENDPOINT") {
            spec["endpoint"] = ep.into();
        }
        let spec: flow::FilterSpec = serde_json::from_value(spec).unwrap();
        let w = World {
            files: vec![
                construct::ExFile { r: "F1".into(), id: "a".into(), label: "議事録_10月.md".into(), mime: "text/markdown".into() },
                construct::ExFile { r: "F2".into(), id: "b".into(), label: "見積書.pdf".into(), mime: "application/pdf".into() },
            ],
            indexes: vec!["資料".into()],
            linked: vec![],
        };
        let cancel = AtomicBool::new(false);
        // (頼み, 期待する一手: None＝ふつうの返事)
        let cases: &[(&str, Option<Tool>)] = &[
            ("こんにちは。今日は寒いね", None),
            ("会議の録音から議事録を作る流れを考えたいんだけど", None),
            ("資料の中でウォッチャの使い方を調べて", Some(Tool::Search)),
            ("議事録_10月.md の要点を教えて", Some(Tool::Read)),
            ("Tauri の最新版をウェブで調べて", Some(Tool::Web)),
        ];
        let mut ok = 0;
        for (req, want) in cases {
            let messages = vec![
                serde_json::json!({ "role": "system", "content": system_prompt(&w, None, true) }),
                serde_json::json!({ "role": "user", "content": req }),
            ];
            let t0 = std::time::Instant::now();
            let mut first: Option<f32> = None;
            let full = flow::run_ai_chat_stream_rec(
                &spec,
                &messages,
                &cancel,
                &mut |k, _| {
                    if k == flow::ChatChunk::Content && first.is_none() {
                        first = Some(t0.elapsed().as_secs_f32());
                    }
                },
                &mut None,
            )
            .expect("AI");
            let (talk, call) = split_call(&full);
            let got = call.as_ref().map(|c| c.tool);
            let hit = got == *want;
            ok += hit as usize;
            println!(
                "{} 最初の文字 {:.1}s・全体 {:.1}s 「{req}」→ {:?} {} / {}",
                if hit { "○" } else { "×" },
                first.unwrap_or(0.0),
                t0.elapsed().as_secs_f32(),
                got,
                call.map(|c| c.arg).unwrap_or_default(),
                short(&talk, 80)
            );
        }
        println!("{ok}/{}", cases.len());
    }
}
