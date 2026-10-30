// --- V1.13 P2 エージェントカード（builtin:"agent"）— docs/V1.13_エージェントカード計画.md §4 -----------
// 置いたら話せる入り口の1枚。カードの下の欄で頼むと、AI が毎回「次の一手」を形の決まった JSON で選び、
// 結果を会話に足して次を選ぶ（answer で終わり・上限 max_steps）。
//
// 道具は既存のコードを呼ぶだけ（ルールを2か所に置かない・計画 §3-2）:
// - search → rag::search_md（📚 索引カードが作った索引）
// - read   → ファイルカードの中身（PDF・Word は rag::doc_text）
// - build  → construct::build_request（コンストラクタと同じ検査・並べ方・UNDO）
// - run    → run_flow_for（ジョブ台帳・トレースも従来どおり）
//
// 動かしてよい範囲（§4.3）は**並べたカードの種類で Rust 側が決める**。AI の自己申告は見ない。
// 外部プログラム・シェル・オペレーション（トリガ等）が上流にあるフローは実行せず「▶ 待ち」を返す。
//
// lib.rs の glob 再エクスポートには載せない（`crate::agent::` で参照）。

use crate::*;
use rusqlite::OptionalExtension;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::{Emitter, Manager, State};

/// 1回の頼みで使える手数の既定と上限（ヒューズ）。
const DEFAULT_MAX_STEPS: u64 = 8;
const MAX_STEPS_CAP: u64 = 16;
/// AI に渡す会話履歴の上限（文字数・古い方から落とす）。完全な記録は会話ログ.md。
const HISTORY_CHARS: usize = 8000;
/// 道具の結果を AI に返すときの上限（文字数）。
const RESULT_CHARS: usize = 4000;
const READ_CHARS: usize = 6000;
/// パネルに残す行数（画面の付け直しで戻す分）。
const PANEL_KEEP: usize = 80;
/// 検索で取る件数。
const SEARCH_K: usize = 6;
/// 会話ログの置き場（プロジェクト直下）。
const LOG_DIR: &str = "エージェント";
const NONE: &str = "なし";

const ACTIONS: &[&str] = &["answer", "search", "read", "build", "run"];

/// パネルの1行。role = user / agent / tool / error。
#[derive(Clone, serde::Serialize)]
pub(crate) struct Line {
    role: String,
    text: String,
}

#[derive(Default)]
struct Rt {
    /// カードごとの会話履歴（role, content）。アプリを閉じると消える（記録は会話ログ.md）。
    history: HashMap<String, Vec<(String, String)>>,
    panel: HashMap<String, Vec<Line>>,
    /// このカードから実行中のフロー（■ で一緒に止める）。
    fired: HashMap<String, Vec<String>>,
    /// 🎙 録音中（カードごと）。
    mic: HashMap<String, Mic>,
    /// 🔊 おしゃべりモードの読み上げ中（止めるときに消す）。
    speaking: HashMap<String, std::process::Child>,
}

struct Mic {
    child: std::process::Child,
    /// 押して話す: 録った音（16kHz mono）。自動で聞く: 使わない（区切りごとに作業役へ渡す）。
    buf: std::sync::Arc<Mutex<Vec<i16>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    auto: bool,
}

/// 読み上げを止める（■・聞き取りをやめる）。
fn hush(card: &str) {
    if let Some(mut c) = rt().lock().unwrap().speaking.remove(card) {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// 返事を読み上げて終わるまで待つ。その間は `mute` を立てて聞き取りを止める（自分の声を頼みと
/// 取り違えない）。読み上げは読み上げカードと同じ OS の声（flow::speak_child）。
fn speak_reply(card: &str, reply: &str, mute: &std::sync::atomic::AtomicBool) {
    let text = crate::voice::speakable_head(reply, SPEAK_CHARS);
    if text.trim().is_empty() {
        return;
    }
    let Ok(child) = flow::speak_child(text.as_bytes()) else { return };
    mute.store(true, std::sync::atomic::Ordering::Relaxed);
    rt().lock().unwrap().speaking.insert(card.to_string(), child);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut r = rt().lock().unwrap();
        let done = match r.speaking.get_mut(card) {
            Some(c) => c.try_wait().ok().flatten().is_some(),
            None => break, // 止められた
        };
        if done {
            r.speaking.remove(card);
            break;
        }
    }
    // 声の残り（部屋の響き）を拾わない。
    std::thread::sleep(std::time::Duration::from_millis(400));
    mute.store(false, std::sync::atomic::Ordering::Relaxed);
}

fn rt() -> &'static Mutex<Rt> {
    static RT: OnceLock<Mutex<Rt>> = OnceLock::new();
    RT.get_or_init(|| Mutex::new(Rt::default()))
}

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

/// 進み具合（パネルの1行・残さない）。
fn status(app: &tauri::AppHandle, card: &str, text: &str) {
    let _ = app.emit("agent:event", serde_json::json!({ "card": card, "role": "status", "text": text }));
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

// --- コマンド（パネルから） ----------------------------------------------------------

#[tauri::command]
pub(crate) fn agent_history(card: String) -> Vec<Line> {
    rt().lock().unwrap().panel.get(&card).cloned().unwrap_or_default()
}

#[tauri::command]
pub(crate) fn agent_reset(card: String) {
    let mut r = rt().lock().unwrap();
    r.history.remove(&card);
    r.panel.remove(&card);
}

/// ■: この頼みと、そこから動かしているフローを止める。
#[tauri::command]
pub(crate) fn agent_stop(card: String, state: State<AppState>) -> usize {
    hush(&card);
    let mut cards = rt().lock().unwrap().fired.get(&card).cloned().unwrap_or_default();
    cards.push(card);
    state.jobs.cancel_cards(&cards)
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

/// 1つの頼みを最後まで（欄から・おしゃべりモードの両方）。返り値＝返事（失敗なら None）。
fn run_request(app: &tauri::AppHandle, card: &str, text: &str) -> Option<String> {
    emit(app, card, "user", text);
    let r = turn(app, card, text);
    if let Err(e) = &r {
        emit(app, card, "error", e);
    }
    status(app, card, "");
    r.ok()
}

// --- 🎙 音声で頼む ------------------------------------------------------------------
// 録音は録音カードと同じ ffmpeg（dshow・16kHz mono）だが、ファイルではなく生の PCM を標準出力で受ける
// （音量を測ってパネルのレベル表示へ送る・自動で聞くときの区切りを見つけるため）。文字起こしは
// 「Whisper 文字起こし」で入れた whisper-cli（同じモデル設定）。音声カードの常駐プロセスは使わない。
//
// - 押して話す（auto=false）: 止めるまで溜め、止めたら文字にして画面へ返す（画面が頼む）。
// - 自動で聞く（auto=true）: 音声カード（voice_server.py）と同じ区切り方＝しきい値・1.2 秒の無音・
//   最長 30 秒で、ひと区切りごとに文字にしてそのまま頼む。頼みの処理中に話した分は順番に待つ。

const RATE: usize = 16_000;
/// 1ブロック 50ms（音声カードと同じ刻み）。
const BLOCK: usize = RATE / 20;
const VAD_THRESHOLD: f32 = 0.012;
/// 1.2 秒黙ったら区切り。
const VAD_SILENCE_BLOCKS: usize = 24;
/// 30 秒で打ち切って区切る。
const VAD_MAX_BLOCKS: usize = 600;
/// 話し始めの手前 0.5 秒も入れる（頭の音が切れない）。2026-10-31 実機: 0.3 秒では
/// 「おしゃべりモードを…」が「モードを…」になった（出だしの子音は小さく、しきい値を超えるのが遅れる）。
const VAD_PRE_BLOCKS: usize = 10;
/// 声の大きさを超えたブロックが 0.3 秒に満たない区切りは物音として捨てる。
const VAD_MIN_VOICED: usize = 6;
/// 押して話すの上限（5 分・メモリを食い尽くさない）。
const MANUAL_MAX_SAMPLES: usize = RATE * 300;
/// おしゃべりモードで読み上げる返事の長さ（最初の段落・句点で切る）。
const SPEAK_CHARS: usize = 200;

fn no_window(c: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// Whisper の実行ファイルとモデル（無ければ導入の案内）。
fn whisper_paths(app_data: &Path) -> Result<(PathBuf, PathBuf), String> {
    let cli = whisper_cli_path(app_data);
    let name = read_config(app_data).whisper_model.unwrap_or_else(|| "small".to_string());
    let model = whisper_models_dir(app_data).join(format!("ggml-{name}.bin"));
    if !(cli.is_file() && model.is_file()) {
        return Err("音声で頼むには Whisper が要ります — メニュー「Whisper 文字起こし」で導入してください".into());
    }
    Ok((cli, model))
}

fn rms(block: &[i16]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let s: f64 = block.iter().map(|&x| (x as f64 / 32768.0).powi(2)).sum();
    (s / block.len() as f64).sqrt() as f32
}

/// 区切りを見つける（音声カードの voice_server.py と同じ規則）。ブロックを入れていき、区切れたら発話を返す。
#[derive(Default)]
struct Vad {
    pre: std::collections::VecDeque<Vec<i16>>,
    cur: Vec<i16>,
    talking: bool,
    quiet: usize,
    blocks: usize,
    voiced: usize,
}

impl Vad {
    fn push(&mut self, block: &[i16], level: f32) -> Option<Vec<i16>> {
        let voiced = level >= VAD_THRESHOLD;
        if !self.talking {
            self.pre.push_back(block.to_vec());
            if self.pre.len() > VAD_PRE_BLOCKS {
                self.pre.pop_front();
            }
            if voiced {
                self.talking = true;
                self.quiet = 0;
                self.blocks = 1;
                self.voiced = 1;
                self.cur = self.pre.drain(..).flatten().collect();
            }
            return None;
        }
        self.cur.extend_from_slice(block);
        self.blocks += 1;
        if voiced {
            self.quiet = 0;
            self.voiced += 1;
        } else {
            self.quiet += 1;
        }
        if self.quiet >= VAD_SILENCE_BLOCKS || self.blocks >= VAD_MAX_BLOCKS {
            self.talking = false;
            let seg = std::mem::take(&mut self.cur);
            return (self.voiced >= VAD_MIN_VOICED).then_some(seg);
        }
        None
    }
}

/// 16kHz mono 16bit の WAV（whisper-cli に渡す）。
fn write_wav(path: &Path, s: &[i16]) -> Result<(), String> {
    let data_len = (s.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + s.len() * 2);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&(RATE as u32).to_le_bytes());
    b.extend_from_slice(&((RATE * 2) as u32).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for x in s {
        b.extend_from_slice(&x.to_le_bytes());
    }
    std::fs::write(path, b).map_err(|e| e.to_string())
}

/// 録った音を文字にする（話していない部分だけなら空）。
fn transcribe(app_data: &Path, card: &str, samples: &[i16]) -> Result<String, String> {
    let (cli, model) = whisper_paths(app_data)?;
    let dir = app_data.join("agent");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = card.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let wav = dir.join(format!("mic_{safe}.wav"));
    write_wav(&wav, samples)?;
    let base = wav.with_extension("");
    let txt = wav.with_extension("txt");
    let _ = std::fs::remove_file(&txt);
    let mut c = std::process::Command::new(&cli);
    c.arg("-m").arg(&model).arg("-f").arg(&wav).args(["-l", "ja", "-nt", "-otxt", "-of"]).arg(&base);
    c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    no_window(&mut c);
    let out = c.output().map_err(|e| format!("whisper-cli を起動できません: {e}"))?;
    let text = std::fs::read(&txt).map(|b| decode_text_bytes(&b).0).unwrap_or_default();
    let text = spoken_text(&text);
    if text.is_empty() && !out.status.success() {
        return Err(format!("文字起こしに失敗しました: {}", short(&String::from_utf8_lossy(&out.stderr), 120)));
    }
    Ok(text)
}

/// 自動で聞くときの状態（パネルの表示）: listening / speech / transcribing / off。
fn vad_state(app: &tauri::AppHandle, card: &str, s: &str) {
    let _ = app.emit("agent:vad", serde_json::json!({ "card": card, "state": s }));
}

/// 録音を始める。返り値＝使ったマイクの名前。マイクはカード設定の mic（無ければ最初に見つかったもの）。
#[tauri::command(async)]
pub(crate) fn agent_mic_start(
    card: String,
    auto: Option<bool>,
    speak: Option<bool>,
    app: tauri::AppHandle,
    state: State<AppState>,
) -> Result<String, String> {
    let auto = auto.unwrap_or(false);
    let speak = auto && speak.unwrap_or(false);
    // 読み上げの間は聞き取りを止める（作業役が立て、読み取り役が見る）。
    let mute = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if rt().lock().unwrap().mic.contains_key(&card) {
        return Err("録音中です".into());
    }
    whisper_paths(&state.app_data)?;
    let configured = with_db(state.inner(), |p| {
        let fref: Option<String> = p
            .conn
            .query_row("SELECT filter_ref FROM virtual_bodies WHERE id = ?1", rusqlite::params![card], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        Ok(fref
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("mic").and_then(|m| m.as_str()).map(str::trim).filter(|m| !m.is_empty()).map(String::from)))
    })?;
    let dev = match configured {
        Some(d) => d,
        None => list_audio_devices()?.into_iter().next().ok_or("マイクが見つかりません（ffmpeg で音声デバイスを列挙できません）")?,
    };
    let mut c = std::process::Command::new("ffmpeg");
    c.args(["-hide_banner", "-loglevel", "error", "-f", "dshow", "-i"])
        .arg(format!("audio={dev}"))
        .args(["-ar", "16000", "-ac", "1", "-f", "s16le", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    no_window(&mut c);
    let mut child = c.spawn().map_err(|e| format!("ffmpeg を起動できません（PATH にありますか）: {e}"))?;
    // すぐ落ちる＝マイクを開けなかった。
    std::thread::sleep(std::time::Duration::from_millis(300));
    if let Ok(Some(_)) = child.try_wait() {
        return Err(format!("録音を始められません（マイク「{dev}」）"));
    }
    let mut stdout = child.stdout.take().ok_or("録音の出力を受けられません")?;
    let buf = std::sync::Arc::new(Mutex::new(Vec::<i16>::new()));
    // 自動で聞く: 区切りを順番に文字にして頼む作業役（頼みの処理中に話した分はここで待つ）。
    let tx = auto.then(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<i16>>();
        let (app, card, app_data, mute) = (app.clone(), card.clone(), state.app_data.clone(), mute.clone());
        std::thread::spawn(move || {
            for seg in rx {
                vad_state(&app, &card, "transcribing");
                match transcribe(&app_data, &card, &seg) {
                    Ok(t) if !t.is_empty() => {
                        let reply = run_request(&app, &card, &t);
                        // 聞き取りをやめた後に終わった頼みは読み上げない。
                        let live = rt().lock().unwrap().mic.contains_key(&card);
                        if let (true, true, Some(r)) = (speak, live, reply) {
                            vad_state(&app, &card, "speaking");
                            speak_reply(&card, &r, &mute);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => emit(&app, &card, "error", &e),
                }
                let live = rt().lock().unwrap().mic.contains_key(&card);
                vad_state(&app, &card, if live { "listening" } else { "off" });
            }
        });
        tx
    });
    let reader = {
        let (app, card, buf, mute) = (app.clone(), card.clone(), buf.clone(), mute.clone());
        std::thread::spawn(move || {
            use std::io::Read;
            let mut raw = vec![0u8; BLOCK * 2];
            let mut vad = Vad::default();
            let mut peak = 0f32;
            let mut n = 0usize;
            while stdout.read_exact(&mut raw).is_ok() {
                let block: Vec<i16> = raw.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                let level = rms(&block);
                // レベル表示は 0.1 秒ごと（2 ブロックの大きい方）。
                peak = peak.max(level);
                n += 1;
                if n % 2 == 0 {
                    let _ = app.emit("agent:level", serde_json::json!({ "card": card, "rms": peak }));
                    peak = 0.0;
                }
                match &tx {
                    // 読み上げ中は区切りを探さない（話しかけ途中の分も捨てて聞き直す）。
                    Some(_) if mute.load(std::sync::atomic::Ordering::Relaxed) => {
                        vad = Vad::default();
                    }
                    Some(tx) => {
                        let was = vad.talking;
                        if let Some(seg) = vad.push(&block, level) {
                            let _ = tx.send(seg);
                        }
                        if vad.talking != was {
                            vad_state(&app, &card, if vad.talking { "speech" } else { "listening" });
                        }
                    }
                    None => {
                        let mut b = buf.lock().unwrap();
                        if b.len() < MANUAL_MAX_SAMPLES {
                            b.extend_from_slice(&block);
                        }
                    }
                }
            }
        })
    };
    rt().lock().unwrap().mic.insert(card.clone(), Mic { child, buf, reader: Some(reader), auto });
    if auto {
        vad_state(&app, &card, "listening");
    }
    Ok(dev)
}

fn stop_proc(mic: &mut Mic) {
    if let Some(mut si) = mic.child.stdin.take() {
        let _ = si.write_all(b"q\n");
    }
    // ffmpeg は q で終わる。2 秒待っても終わらなければ止める。
    let t0 = std::time::Instant::now();
    while mic.child.try_wait().ok().flatten().is_none() {
        if t0.elapsed() > std::time::Duration::from_secs(2) {
            let _ = mic.child.kill();
            let _ = mic.child.wait();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if let Some(h) = mic.reader.take() {
        let _ = h.join();
    }
}

/// 録音を止める。押して話す: 文字にして返す（頼むのは画面側）。自動で聞く: 聞き取りを終える（空を返す・
/// 話し途中の分は捨てる・文字にしている途中の分は最後まで頼む）。
#[tauri::command(async)]
pub(crate) fn agent_mic_stop(card: String, app: tauri::AppHandle, state: State<AppState>) -> Result<String, String> {
    let mut mic = rt().lock().unwrap().mic.remove(&card).ok_or("録音していません")?;
    stop_proc(&mut mic);
    if mic.auto {
        hush(&card);
        vad_state(&app, &card, "off");
        return Ok(String::new());
    }
    let samples = std::mem::take(&mut *mic.buf.lock().unwrap());
    if samples.len() < RATE / 4 {
        return Err("録音が短すぎます".into());
    }
    let text = transcribe(&state.app_data, &card, &samples)?;
    if text.is_empty() {
        return Err("聞き取れませんでした".into());
    }
    Ok(text)
}

/// 文字起こしから話した部分だけ: 無音・音楽で Whisper が書く「(音声)」「[音楽]」だけの行は捨てる
/// （2026-10-30 実測: 無音 3 秒で「(音声)」）。
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

/// 録音をやめる（文字にしない）。
#[tauri::command]
pub(crate) fn agent_mic_cancel(card: String, app: tauri::AppHandle) {
    hush(&card);
    let mic = rt().lock().unwrap().mic.remove(&card);
    if let Some(mut m) = mic {
        let _ = m.child.kill();
        let _ = m.child.wait();
        if m.auto {
            vad_state(&app, &card, "off");
        }
    }
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

/// 1回の頼み。返り値＝返事（おしゃべりモードが読み上げる）。
fn turn(app: &tauri::AppHandle, card: &str, text: &str) -> Result<String, String> {
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

/// AI が選んだ一手。
#[derive(Debug, Default, PartialEq)]
struct Step {
    action: String,
    target: String,
    input: String,
    reply: String,
}

fn parse_step(raw: &str) -> Step {
    match construct::extract_json(raw) {
        Ok(v) => {
            let f = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let action = f("action");
            Step {
                action: if ACTIONS.contains(&action.as_str()) { action } else { "answer".into() },
                target: f("target"),
                input: f("input"),
                reply: f("reply"),
            }
        }
        // 形が崩れていたら、言ったことをそのまま返事にする（何もしない）。
        Err(_) => Step { action: "answer".into(), reply: raw.trim().to_string(), ..Default::default() },
    }
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
) -> Result<String, String> {
    let max_steps = extra.get("max_steps").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_MAX_STEPS).clamp(1, MAX_STEPS_CAP);
    let history = rt().lock().unwrap().history.get(card).cloned().unwrap_or_default();
    let mut scratch: Vec<serde_json::Value> = Vec::new();
    let mut used: Vec<String> = Vec::new();
    let mut last_key: Option<String> = None;
    let mut reply: Option<String> = None;
    for step in 1..=max_steps {
        if job.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("中断しました".into());
        }
        emit_job_progress(app, job, Some(&format!("{label}: {step}手目")), None);
        status(app, card, &format!("考え中（{step}手目）…"));
        let world = World::scan(root, card)?;
        let mut messages = vec![serde_json::json!({ "role": "system", "content": system_prompt(&world, spec.system.as_deref()) })];
        for (role, content) in bounded_history(&history) {
            messages.push(serde_json::json!({ "role": role, "content": content }));
        }
        messages.push(serde_json::json!({ "role": "user", "content": text }));
        messages.extend(scratch.iter().cloned());
        let raw = ask_step(spec, &messages, &world.schema(), &job.cancel)?;
        let st = parse_step(&raw);
        job.trace.note(card, &format!("{step}手目 {}", st.action), &short(&raw, 300));
        if st.action == "answer" {
            reply = Some(if st.reply.is_empty() { st.input.clone() } else { st.reply.clone() });
            break;
        }
        let key = format!("{}\u{0}{}\u{0}{}", st.action, st.target, st.input);
        let (result, line) = if last_key.as_deref() == Some(key.as_str()) {
            ("同じ一手を続けて選びました。ここまでの結果で answer してください。".to_string(), String::new())
        } else {
            match st.action.as_str() {
                "search" => do_search(root, &world, &st, text),
                "read" => do_read(root, &world, &st),
                "build" => do_build(app, root, card, spec, extra, &st, job),
                _ => do_run(app, root, card, &world, &st),
            }
            .unwrap_or_else(|e| (format!("失敗しました: {e}"), format!("✗ {}", short(&e, 80))))
        };
        last_key = Some(key);
        if !line.is_empty() {
            emit(app, card, "tool", &line);
            used.push(line);
        }
        scratch.push(serde_json::json!({ "role": "assistant", "content": raw }));
        scratch.push(serde_json::json!({ "role": "user", "content": format!("【結果: {}】\n{}", st.action, cut(&result, RESULT_CHARS)) }));
    }
    let reply = reply.unwrap_or_else(|| format!("手数の上限（{max_steps}手）まで使いました。続けるなら、もう一度頼んでください。"));
    emit(app, card, "agent", &reply);
    {
        let mut r = rt().lock().unwrap();
        let h = r.history.entry(card.to_string()).or_default();
        h.push(("user".into(), text.into()));
        // 次の頼みで「何をしたか」が分かるように、使った道具を返事に添えて残す。
        let note = if used.is_empty() { String::new() } else { format!("\n（使った道具: {}）", used.join(" / ")) };
        h.push(("assistant".into(), format!("{reply}{note}")));
    }
    let path = log_md(root, label, text, &used, &reply);
    if let Err(e) = place_log_card(app, root, card, &path) {
        emit(app, card, "error", &format!("会話の記録をカードにできませんでした: {e}"));
    }
    Ok(reply)
}

/// 会話の記録をエージェントの出力カードにする（AI カードの対話ログと同じ置き方:
/// 右隣に置いて矢印を張る・2回目からは中身の差し替え）。つないだ先へ会話をそのまま渡せる。
fn place_log_card(app: &tauri::AppHandle, root: &Path, card: &str, path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("会話.md").to_string();
    let p = open_project_at(root)?;
    let id = place_md_card(&p, card, path, &fname, &bytes, LOG_DX, 0.0)?;
    let _ = app.emit(
        "mcp:changed",
        serde_json::json!({ "project": project_info(root).path, "undo": [], "touched": [id] }),
    );
    Ok(())
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
    let name: String = label.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    let path = root.join(LOG_DIR).join(format!("{}の会話.md", name.trim()));
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

// --- いまの状況（AI に見せる選択肢） --------------------------------------------------

/// 実行できるカード（下流にフィルタが無いフィルタ・OUT）。
struct RunCard {
    r: String,
    id: String,
    label: String,
    kind: String,
}

struct World {
    files: Vec<construct::ExFile>,
    runs: Vec<RunCard>,
    indexes: Vec<String>,
}

impl World {
    fn scan(root: &Path, card: &str) -> Result<World, String> {
        let p = open_project_at(root)?;
        Ok(World { files: construct::existing_files(&p), runs: runnable(&p, card), indexes: rag::list_indexes(root) })
    }

    /// 一手の形（Ollama format / OpenAI 互換 json_schema）。target は選択肢の外を出させない。
    fn schema(&self) -> serde_json::Value {
        let mut targets: Vec<String> = self.indexes.clone();
        targets.extend(self.files.iter().map(|f| f.r.clone()));
        targets.extend(self.runs.iter().map(|r| r.r.clone()));
        targets.push(NONE.into());
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ACTIONS },
                "target": { "type": "string", "enum": targets },
                "input": { "type": "string" },
                "reply": { "type": "string" }
            },
            "required": ["action", "target", "input", "reply"],
            "additionalProperties": false
        })
    }
}

fn kind_of(spec: &serde_json::Value) -> String {
    if let Some(op) = spec.get("op").and_then(|v| v.as_str()) {
        return op.to_uppercase();
    }
    if spec.get("shell").and_then(|v| v.as_str()).is_some() {
        return "シェル".into();
    }
    match spec.get("builtin").and_then(|v| v.as_str()) {
        Some(b) => b.to_string(),
        None => "外部プログラム".into(),
    }
}

/// 新しい順・上限 40 枚。エージェント・コンストラクタ自身は除く（動かすと話がねじれる）。
fn runnable(p: &Project, card: &str) -> Vec<RunCard> {
    let Ok(mut st) = p.conn.prepare(
        "SELECT v.id, v.label, COALESCE(v.filter_ref, '') FROM virtual_bodies v
         WHERE v.deleted_at IS NULL AND v.kind IN ('filter', 'op') AND v.id != ?1
           AND NOT EXISTS (
             SELECT 1 FROM relations r JOIN virtual_bodies d ON d.id = r.to_id AND d.deleted_at IS NULL
             WHERE r.from_id = v.id AND r.kind = 'flow' AND r.deleted_at IS NULL AND d.kind IN ('filter', 'op'))
         ORDER BY v.created_at DESC LIMIT 60",
    ) else {
        return Vec::new();
    };
    let rows = st.query_map(rusqlite::params![card], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)));
    let Ok(rows) = rows else { return Vec::new() };
    rows.flatten()
        .filter_map(|(id, label, fref)| {
            let v: serde_json::Value = serde_json::from_str(&fref).unwrap_or(serde_json::Value::Null);
            let op = v.get("op").and_then(|o| o.as_str());
            if op.is_some() && op != Some("out") {
                return None;
            }
            if matches!(v.get("builtin").and_then(|b| b.as_str()), Some("agent") | Some("construct")) {
                return None;
            }
            Some((id, label, kind_of(&v)))
        })
        .take(40)
        .enumerate()
        .map(|(i, (id, label, kind))| RunCard { r: format!("T{}", i + 1), id, label, kind })
        .collect()
}

/// エージェントへの指示（system）。道具の説明＋いまの状況＋カード設定の追加の指示。
fn system_prompt(w: &World, extra: Option<&str>) -> String {
    let mut s = String::from(
        "あなたは TexoS のエージェントです。TexoS はカードを矢印でつないで処理の流れ（フロー）を作り、\
         キャンバスに残すアプリです。ユーザーの頼みに、下の道具を使って応えます。\n\
         毎回、次の一手を1つだけ JSON で返します（action / target / input / reply）。\n\n\
         道具（action）:\n\
         - answer: ユーザーに返事をして、この頼みを終える。reply に返事（日本語・簡潔に）。質問への答え・確認の質問・結果の報告もこれ。\n\
         - search: 手元の文書の索引から、問いに近い箇所を探す。target に索引名、input に問い。\n\
         - read: ファイルカードの中身を読む。target に F番号。\n\
         - build: 頼まれた処理をするカードのフローを組んで、キャンバスに並べる（実行はしない）。\
           input に、組む処理を具体的に1〜3文で書く（使うファイルカードがあれば名前も）。\n\
         - run: フローを実行して、出来た結果を読む。target に T番号。\n\n\
         決まり:\n\
         - 雑談や、知っていることで答えられる質問は answer。何も作らない。\n\
         - 処理を頼まれたら（〜して・〜を作って・〜をまとめて など）、build で組み、run で実行し、結果を answer で伝える。\
           同じ処理をするフローが実行できるカードにすでにあれば、組まずにそれを run する。\n\
         - 手元の資料について聞かれたら search か read で調べてから answer。返事に出典（ファイル名）を添える。\n\
         - 道具の結果は【結果】として届く。失敗したら理由を見て、直せるなら別の一手、無理ならそのまま伝える。\n\
         - run の結果が「▶ 待ち」なら、理由と、どのカードの ▶ を押せばよいかを answer で伝えて終える。\n\
         - answer 以外の reply は空でよい。target が要らない道具では target は なし。\n\
         - F番号・T番号は道具を選ぶための内部の記号。ユーザーへの返事では使わず、ファイル名・カード名で言う。\
           一覧を頼まれたら、全部を並べず種類ごとにまとめ、主なものを挙げる。\n\n\
         いまの状況:\n",
    );
    s.push_str("索引: ");
    if w.indexes.is_empty() {
        s.push_str("なし（search は使えない。📚 索引カードで作れる）\n");
    } else {
        s.push_str(&format!("{}\n", w.indexes.join("、")));
    }
    s.push_str("ファイルカード:\n");
    if w.files.is_empty() {
        s.push_str("（なし）\n");
    }
    for f in &w.files {
        s.push_str(&format!("- {} {}（{}）\n", f.r, f.label, if f.mime.is_empty() { "種類不明" } else { &f.mime }));
    }
    s.push_str("実行できるカード（新しい順）:\n");
    if w.runs.is_empty() {
        s.push_str("（なし）\n");
    }
    for r in &w.runs {
        s.push_str(&format!("- {} {}（{}）\n", r.r, r.label, r.kind));
    }
    if let Some(x) = extra.map(str::trim).filter(|x| !x.is_empty()) {
        s.push_str(&format!("\n追加の指示（カードの設定）:\n{x}\n"));
    }
    s
}

/// 一手を頼む。形の指定（format / response_format）を知らないサーバ向けに、断られたら指定なしでもう一度。
fn ask_step(
    spec: &flow::FilterSpec,
    messages: &[serde_json::Value],
    schema: &serde_json::Value,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String, String> {
    let mut s = spec.clone();
    let mut ex = s.extra_params.clone().unwrap_or_default();
    if flow::is_openai_compat(spec.provider.as_deref().unwrap_or("ollama")) {
        ex.insert(
            "response_format".into(),
            serde_json::json!({ "type": "json_schema", "json_schema": { "name": "agent_step", "strict": true, "schema": schema } }),
        );
    } else {
        ex.insert("format".into(), schema.clone());
    }
    s.extra_params = Some(ex);
    match construct::ask_ai(&s, messages, cancel) {
        Ok(a) => Ok(a),
        Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => Err("中断しました".into()),
        Err(e) => {
            eprintln!("[agent] 形の指定つきで断られたので指定なしでもう一度: {e}");
            construct::ask_ai(spec, messages, cancel).map_err(|e| format!("AI に頼めません: {e}"))
        }
    }
}

// --- 道具 --------------------------------------------------------------------------

type ToolOut = Result<(String, String), String>;

fn do_search(root: &Path, w: &World, st: &Step, text: &str) -> ToolOut {
    let name = if w.indexes.contains(&st.target) {
        st.target.clone()
    } else {
        match w.indexes.as_slice() {
            [] => return Err("索引がありません（📚 索引カードで作れます）".into()),
            [one] => one.clone(),
            _ => return Err(format!("索引を target で選んでください（{}）", w.indexes.join("、"))),
        }
    };
    let query = if st.input.is_empty() { text } else { st.input.as_str() };
    // 埋め込みの接続先は既定（Ollama）。モデルは索引に記録したもの。
    let espec: flow::FilterSpec =
        serde_json::from_value(serde_json::json!({ "builtin": "rag_search" })).map_err(|e| e.to_string())?;
    match rag::search_md(root, &espec, &name, query, SEARCH_K)? {
        Some(md) => Ok((md, format!("🔎 索引「{name}」で調べました：{}", short(query, 40)))),
        None => Ok(("近い箇所はありませんでした".into(), format!("🔎 索引「{name}」：見つからず"))),
    }
}

fn do_read(root: &Path, w: &World, st: &Step) -> ToolOut {
    let f = w
        .files
        .iter()
        .find(|f| f.r == st.target)
        .ok_or_else(|| format!("ファイルカード {} はありません（F番号で選んでください）", st.target))?;
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

fn do_build(
    app: &tauri::AppHandle,
    root: &Path,
    card: &str,
    spec: &flow::FilterSpec,
    extra: &serde_json::Value,
    st: &Step,
    job: &Job,
) -> ToolOut {
    if st.input.is_empty() {
        return Err("組む処理（input）が空です".into());
    }
    status(app, card, &format!("組んでいます：{}", short(&st.input, 40)));
    let p = open_project_at(root)?;
    // エージェントの「追加の指示」はコンストラクタの作法ではない（渡さない）。
    let mut bspec = spec.clone();
    bspec.system = None;
    let b = construct::build_request(app, &p, card, &bspec, extra, &st.input, None, job)?;
    if !b.ok {
        return Ok((format!("組めませんでした: {}", b.outcome), format!("🏗 組めませんでした：{}", short(&b.outcome, 60))));
    }
    let runs = if b.terminals.is_empty() {
        "実行するカードはありません。".to_string()
    } else {
        format!(
            "実行するカード: {}（実行できるカードの一覧の先頭にあります）。",
            b.terminals.iter().map(|(l, _)| format!("「{l}」")).collect::<Vec<_>>().join("・")
        )
    };
    Ok((
        format!("枠「🏗 {}」に並べました。{} {runs}", b.title, b.outcome),
        format!("🏗 組みました「{}」：{}", b.title, short(&b.outcome, 60)),
    ))
}

/// 人の ▶ を待つべき理由（無ければ None）。上流のカードの種類だけで決める（§4.3）。
fn gate_reason(p: &Project, tid: &str) -> Option<String> {
    let mut ids = upstream_ids(p, tid);
    ids.push(tid.to_string());
    for id in ids {
        let row: Option<(String, Option<String>, String)> = p
            .conn
            .query_row(
                "SELECT COALESCE(kind, 'file'), filter_ref, label FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((kind, fref, label)) = row else { continue };
        let v: serde_json::Value = fref.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(serde_json::Value::Null);
        match kind.as_str() {
            "filter" if v.get("builtin").and_then(|b| b.as_str()).is_none() => {
                return Some(format!("「{label}」は{}で、PC の上で何でもできるため", kind_of(&v)));
            }
            "op" if !matches!(v.get("op").and_then(|o| o.as_str()), Some("in") | Some("out")) => {
                return Some(format!("「{label}」はオペレーション（{}）のため", kind_of(&v)));
            }
            _ => {}
        }
    }
    None
}

fn do_run(app: &tauri::AppHandle, root: &Path, card: &str, w: &World, st: &Step) -> ToolOut {
    let r = w
        .runs
        .iter()
        .find(|r| r.r == st.target)
        .ok_or_else(|| format!("実行できるカード {} はありません（T番号で選んでください）", st.target))?;
    let why = gate_reason(&open_project_at(root)?, &r.id);
    if let Some(why) = why {
        return Ok((
            format!("▶ 待ち: {why}、エージェントからは実行しません。ユーザーがカード「{}」の ▶ を押すと実行されます。", r.label),
            format!("✋ 「{}」は ▶ 待ち（{why}）", r.label),
        ));
    }
    status(app, card, &format!("実行中：{}", r.label));
    rt().lock().unwrap().fired.entry(card.to_string()).or_default().push(r.id.clone());
    let res = run_flow_for(&r.id, app);
    if let Some(v) = rt().lock().unwrap().fired.get_mut(card) {
        v.retain(|x| x != &r.id);
    }
    let report = res?;
    if !report.errors.is_empty() {
        return Ok((
            format!("実行しましたが、エラーがありました:\n{}", report.errors.join("\n")),
            format!("✗ 「{}」：{}", r.label, short(&report.errors[0], 60)),
        ));
    }
    let (body, names) = outputs_text(root, &report);
    Ok((
        format!("実行しました。出来たもの: {}\n\n{body}", if names.is_empty() { "なし".into() } else { names.join("・") }),
        format!("▶ 「{}」を実行しました{}", r.label, if names.is_empty() { String::new() } else { format!(" → {}", names.join("・")) }),
    ))
}

/// 実行で出来た（更新された）カードの中身。文字でないものは名前だけ。
fn outputs_text(root: &Path, r: &FlowReport) -> (String, Vec<String>) {
    let Ok(p) = open_project_at(root) else { return (String::new(), r.outputs.clone()) };
    let mut body = String::new();
    let mut names = Vec::new();
    for id in r.created.iter().chain(r.updated.iter()) {
        let row: Option<(Option<String>, String, String)> = p
            .conn
            .query_row(
                "SELECT v.committed_path, v.label, COALESCE(v.mime_type, rb.mime_type, '') FROM virtual_bodies v
                 JOIN real_bodies rb ON rb.id = v.real_id WHERE v.id = ?1 AND v.deleted_at IS NULL",
                rusqlite::params![id],
                |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((path, name, mime)) = row else { continue };
        names.push(name.clone());
        let ext = Path::new(&name).extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        let texty = mime.starts_with("text/") || mime == "application/json" || matches!(ext.as_str(), "pdf" | "docx");
        let text = path
            .filter(|_| texty)
            .and_then(|s| std::fs::read(load_path(root, &s)).ok())
            .and_then(|b| rag::doc_text(&ext, &b).ok());
        match text {
            Some(t) => body.push_str(&format!("### {name}\n\n{}\n\n", cut(&t, RESULT_CHARS))),
            None => body.push_str(&format!("### {name}\n\n（{mime}・中身は文字ではありません）\n\n")),
        }
    }
    (body, names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_is_parsed_and_falls_back_to_answer() {
        let s = parse_step(r#"{"action":"run","target":"T1","input":"","reply":""}"#);
        assert_eq!((s.action.as_str(), s.target.as_str()), ("run", "T1"));
        // 知らない action・崩れた形は「返事」扱い（何もしない）。
        assert_eq!(parse_step(r#"{"action":"delete","reply":"x"}"#).action, "answer");
        let s = parse_step("こんにちは。");
        assert_eq!((s.action.as_str(), s.reply.as_str()), ("answer", "こんにちは。"));
    }

    #[test]
    fn schema_limits_targets_to_what_exists() {
        let w = World {
            files: vec![construct::ExFile { r: "F1".into(), id: "a".into(), label: "資料.md".into(), mime: "text/markdown".into() }],
            runs: vec![RunCard { r: "T1".into(), id: "b".into(), label: "要約".into(), kind: "ai".into() }],
            indexes: vec!["索引".into()],
        };
        let s = w.schema();
        let t: Vec<&str> = s["properties"]["target"]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(t, vec!["索引", "F1", "T1", "なし"]);
        let p = system_prompt(&w, Some("敬語で"));
        assert!(p.contains("F1 資料.md") && p.contains("T1 要約（ai）") && p.contains("敬語で"));
    }

    fn project() -> (PathBuf, Project) {
        let dir = std::env::temp_dir().join(format!("texos_agent_{}_{}", std::process::id(), db::now_ms()));
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
    fn runnable_lists_terminals_and_gate_sees_programs_upstream() {
        let (dir, p) = project();
        card(&p, "agent", "filter", r#"{"builtin":"agent"}"#, 1);
        card(&p, "prog", "filter", r#"{"name":"x.exe","cmd":"x {in} {out}"}"#, 2);
        card(&p, "sum", "filter", r#"{"builtin":"ai","model":"m"}"#, 3);
        card(&p, "cat", "filter", r#"{"builtin":"text_concat"}"#, 4);
        card(&p, "ask", "filter", r#"{"builtin":"ai","model":"m"}"#, 5);
        card(&p, "timer", "op", r#"{"op":"timer","every":60}"#, 6);
        arrow(&p, "prog", "sum");
        arrow(&p, "cat", "ask");
        let runs = runnable(&p, "agent");
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        // 下流にフィルタがあるもの（prog・cat）・タイマ・エージェント自身は出さない。新しい順。
        assert_eq!(ids, vec!["ask", "sum"]);
        assert_eq!(runs[0].r, "T1");
        assert!(gate_reason(&p, "sum").unwrap().contains("外部プログラム"));
        assert!(gate_reason(&p, "ask").is_none());
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 実機の AI で一手の選び方を見る（cargo test --lib agent_live -- --ignored --nocapture）。
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
            runs: vec![RunCard { r: "T1".into(), id: "c".into(), label: "英訳".into(), kind: "ai".into() }],
            indexes: vec!["資料".into()],
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let cases: &[(&str, &[&str])] = &[
            ("こんにちは。今日は寒いね", &["answer"]),
            ("資料の中でウォッチャの使い方を調べて", &["search"]),
            ("議事録_10月.md の要点を3行で教えて", &["read", "build"]),
            ("英訳を動かして", &["run"]),
            ("議事録_10月.md を英語に翻訳して要約するフローを作って", &["build"]),
        ];
        let mut ok = 0;
        for (req, want) in cases {
            let messages = vec![
                serde_json::json!({ "role": "system", "content": system_prompt(&w, None) }),
                serde_json::json!({ "role": "user", "content": req }),
            ];
            let t0 = std::time::Instant::now();
            let raw = ask_step(&spec, &messages, &w.schema(), &cancel).expect("AI");
            let st = parse_step(&raw);
            let hit = want.contains(&st.action.as_str());
            ok += hit as usize;
            println!("{} {:.1}s 「{req}」→ {} {} / {} / {}", if hit { "○" } else { "×" }, t0.elapsed().as_secs_f32(),
                st.action, st.target, short(&st.input, 50), short(&st.reply, 50));
        }
        // 組んだあとの次の一手（結果を受けて run に進むか）。
        let messages = vec![
            serde_json::json!({ "role": "system", "content": system_prompt(&w, None) }),
            serde_json::json!({ "role": "user", "content": "議事録_10月.md を英訳して" }),
            serde_json::json!({ "role": "assistant", "content": r#"{"action":"build","target":"なし","input":"議事録_10月.md を英訳する","reply":""}"# }),
            serde_json::json!({ "role": "user", "content": "【結果: build】\n枠「🏗 議事録の英訳」に並べました。実行するカード: 「英訳」（実行できるカードの一覧の先頭にあります）。" }),
        ];
        let st = parse_step(&ask_step(&spec, &messages, &w.schema(), &cancel).expect("AI"));
        println!("組んだあと → {} {}", st.action, st.target);
        println!("一手目 {ok}/{}", cases.len());
    }

    #[test]
    fn vad_cuts_after_silence_and_drops_blips() {
        let quiet = vec![0i16; BLOCK];
        let loud = vec![3000i16; BLOCK];
        let mut v = Vad::default();
        let mut segs = Vec::new();
        let mut feed = |v: &mut Vad, b: &Vec<i16>, n: usize, segs: &mut Vec<Vec<i16>>| {
            for _ in 0..n {
                if let Some(s) = v.push(b, rms(b)) {
                    segs.push(s);
                }
            }
        };
        feed(&mut v, &quiet, 10, &mut segs);
        feed(&mut v, &loud, 20, &mut segs); // 1 秒話す
        feed(&mut v, &quiet, VAD_SILENCE_BLOCKS - 1, &mut segs);
        assert!(segs.is_empty(), "黙って 1.2 秒たつまでは区切らない");
        feed(&mut v, &quiet, 1, &mut segs);
        assert_eq!(segs.len(), 1);
        // 手前 0.3 秒＋話した 1 秒＋無音 1.2 秒。
        assert_eq!(segs[0].len(), BLOCK * (VAD_PRE_BLOCKS + 20 + VAD_SILENCE_BLOCKS - 1));
        feed(&mut v, &loud, 2, &mut segs); // 0.1 秒の物音
        feed(&mut v, &quiet, VAD_SILENCE_BLOCKS, &mut segs);
        assert_eq!(segs.len(), 1, "短い物音は捨てる");
    }

    #[test]
    fn wav_header_is_16k_mono_pcm() {
        let p = std::env::temp_dir().join(format!("texos_agent_wav_{}.wav", std::process::id()));
        write_wav(&p, &[1, -1, 2]).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(b.len(), 44 + 6);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 6);
        let _ = std::fs::remove_file(&p);
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
}
