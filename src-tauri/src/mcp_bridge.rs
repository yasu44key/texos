// ============================================================================
// v1.11 MCP 接続（AIエージェントからの操作口）
// ----------------------------------------------------------------------------
// 外部の MCP サーバー（scripts/texos_mcp.py・stdio）が、起動中の TexoS とローカル IPC で
// 話すための口。TexoS 側はここで要求を受けて、UI と**同じ処理**（notes/objects/filters の
// `*_in(&Project)`・run_flow_job）を呼ぶ。SQLite へ外から直接書く方式は採らない
// （起動中の TexoS が自分の接続を持っているため面に出ない・規約 §3-8）。
//
// 口: Windows は名前付きパイプ（`\\.\pipe\texos-mcp-<ユーザー名>`・リモート接続拒否）、
//     それ以外は app_data/mcp.sock（Unix ドメインソケット・0600）。
//     TCP を開かない＝LAN/インターネットにも Web ページにも届かない。
// 認証: 有効化のたびに作るトークン（app_data/mcp.json に口の名前と一緒に置く）を
//       毎要求に添える。設定 `mcp_enabled`（既定オフ）で口ごと閉じられる。
// 通信: 1行1 JSON の要求／応答（{token,id,method,params} → {id,ok,result|error}）。
// 画面への反映: 変更のたびに `mcp:changed`（project・undo・touched）を emit し、
//   フロントが同じプロジェクトを開いていれば UNDO に積んで refresh する。
// ============================================================================
#![allow(unused_imports)]
use crate::*;
use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, Manager, State};

/// run_flow_job が「いま開いているプロジェクトが違う」ときに返す文字列（エラーコード判定にも使う）。
pub(crate) const PROJECT_MISMATCH: &str =
    "project_mismatch: 指定したプロジェクトは現在開かれていません（TexoS で別のプロジェクトが開かれています）";

/// 一覧の既定件数と上限。
const LIST_DEFAULT: usize = 100;
const LIST_MAX: usize = 500;
/// 本文取得の既定文字数と上限。
const TEXT_DEFAULT: usize = 20_000;
const TEXT_MAX: usize = 200_000;
/// 本文取得で読むファイルの上限（これより大きいものは断る）。
const TEXT_FILE_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// 1行（1要求）の上限。
const LINE_MAX: usize = 8 * 1024 * 1024;
/// 冪等キャッシュと完了ジョブの保持数。
const IDEM_KEEP: usize = 1000;
const JOBS_KEEP: usize = 200;

/// MCP から作れる・設定を変えられる内蔵カード（副作用がプロジェクト内に閉じるものだけ）。
const BUILTINS: &[&str] = &[
    "text_concat",
    "text_diff",
    "text_paste",
    "text_grep",
    "text_replace",
    "text_head",
    "text_tail",
    "text_sort",
    "text_count",
    "text_cut",
    "text_convert",
    "md_codeblock",
    "tap",
];
/// 内蔵カードの設定で受け付けるキー。
const BUILTIN_KEYS: &[&str] = &[
    "pattern",
    "replacement",
    "lines",
    "order",
    "compare",
    "unique",
    "delimiter",
    "fields",
    "encoding",
    "separator",
    "lang",
    "prefix",
    "seq",
    "output_ext",
    "accept_mime",
    "note",
];
/// AI 変換カードの設定で受け付けるキー。
const AI_KEYS: &[&str] = &[
    "provider",
    "endpoint",
    "model",
    "system",
    "temperature",
    "prefix",
    "seq",
    "think",
    "num_ctx",
    "gpu",
    "send_reasoning",
    "note",
];
/// OUT カードの設定で受け付けるキー。
const OUT_KEYS: &[&str] = &["prefix"];

// ---------------------------------------------------------------------------
// 状態
// ---------------------------------------------------------------------------

struct Bridge {
    app: tauri::AppHandle,
    /// 口が開いているか（受付中）。要求ごとにも見る＝無効化した瞬間から既存接続も断る。
    enabled: AtomicBool,
    /// 待受スレッドが生きているか。
    listening: AtomicBool,
    token: Mutex<String>,
    endpoint: Mutex<String>,
    /// 冪等キャッシュ: "method\0request_id" → 結果。
    /// 値は (要求の指紋, 結果)。指紋で「同じ要求の再送」と「request_id の使い回し」を見分ける。
    idem: Mutex<(HashMap<String, (String, Value)>, VecDeque<String>)>,
    /// request_id 付きの変更を直列化する（同じ request_id の同時再送で二重にしない）。
    idem_lock: Mutex<()>,
    /// MCP から起動したジョブ（実行中は Job を握り、完了後は結果を残す）。
    jobs: Mutex<(HashMap<JobId, McpJob>, VecDeque<JobId>)>,
}

struct McpJob {
    card: String,
    label: String,
    project: String,
    started_at: i64,
    job: Option<Arc<Job>>,
    finished: Option<Value>,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

fn bridge() -> Option<&'static Bridge> {
    BRIDGE.get()
}

/// 起動時（setup）に1回。設定でオンなら口を開く。
pub(crate) fn init(app: tauri::AppHandle) {
    let state: State<AppState> = app.state();
    let enabled = read_config(&state.app_data).mcp_enabled;
    let _ = BRIDGE.set(Bridge {
        app,
        enabled: AtomicBool::new(false),
        listening: AtomicBool::new(false),
        token: Mutex::new(String::new()),
        endpoint: Mutex::new(String::new()),
        idem: Mutex::new((HashMap::new(), VecDeque::new())),
        idem_lock: Mutex::new(()),
        jobs: Mutex::new((HashMap::new(), VecDeque::new())),
    });
    if enabled {
        if let Err(e) = start() {
            eprintln!("[mcp] 口を開けませんでした: {e}");
        }
    }
}

fn discovery_path(app_data: &Path) -> PathBuf {
    app_data.join("mcp.json")
}

/// 口を開く（トークンを作り直し、発見ファイルを書き、待受スレッドを起こす）。
fn start() -> Result<(), String> {
    let b = bridge().ok_or("MCP 接続が初期化されていません")?;
    // 直前に閉じた待受がまだ抜けきっていなければ少し待つ（開閉の連打）。
    for _ in 0..20 {
        if !b.listening.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let reuse = b.listening.load(Ordering::SeqCst);
    let state: State<AppState> = b.app.state();
    let token = format!("{}{}", uuid(), uuid()).replace('-', "");
    let endpoint = endpoint_name(&state.app_data);
    *b.token.lock().unwrap() = token.clone();
    *b.endpoint.lock().unwrap() = endpoint.clone();
    b.enabled.store(true, Ordering::SeqCst);
    let info = json!({
        "endpoint": endpoint,
        "transport": if cfg!(windows) { "named_pipe" } else { "unix_socket" },
        "token": token,
        "pid": std::process::id(),
        "version": b.app.package_info().version.to_string(),
    });
    std::fs::write(
        discovery_path(&state.app_data),
        serde_json::to_string_pretty(&info).unwrap_or_default(),
    )
    .map_err(|e| format!("mcp.json を書けません: {e}"))?;
    restrict_file(&discovery_path(&state.app_data));
    if reuse {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::spawn(move || serve(endpoint, tx));
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            b.enabled.store(false, Ordering::SeqCst);
            let _ = std::fs::remove_file(discovery_path(&state.app_data));
            Err(e)
        }
        Err(_) => Err("待受の開始を確認できませんでした".into()),
    }
}

/// 口を閉じる（発見ファイルを消し、待受を止める。実行中のフローには触らない）。
fn stop() {
    let Some(b) = bridge() else { return };
    b.enabled.store(false, Ordering::SeqCst);
    let state: State<AppState> = b.app.state();
    let _ = std::fs::remove_file(discovery_path(&state.app_data));
    // 待受（accept/ConnectNamedPipe）で眠っているスレッドを、自分で1回つないで起こす。
    let ep = b.endpoint.lock().unwrap().clone();
    if !ep.is_empty() && b.listening.load(Ordering::SeqCst) {
        poke(&ep);
    }
    *b.token.lock().unwrap() = String::new();
}

#[derive(serde::Serialize)]
pub struct McpStatus {
    pub enabled: bool,
    pub listening: bool,
    pub endpoint: Option<String>,
    pub discovery_file: String,
}

/// UI 用: MCP 接続の状態。
#[tauri::command]
pub(crate) fn mcp_status(state: State<AppState>) -> McpStatus {
    let b = bridge();
    let enabled = b.map(|b| b.enabled.load(Ordering::SeqCst)).unwrap_or(false);
    McpStatus {
        enabled,
        listening: b.map(|b| b.listening.load(Ordering::SeqCst)).unwrap_or(false),
        endpoint: b
            .filter(|_| enabled)
            .map(|b| b.endpoint.lock().unwrap().clone()),
        discovery_file: discovery_path(&state.app_data).to_string_lossy().into_owned(),
    }
}

/// UI 用: MCP 接続の有効化／無効化（設定に保存し、その場で口を開閉する）。
#[tauri::command]
pub(crate) fn mcp_set_enabled(on: bool, state: State<AppState>) -> Result<McpStatus, String> {
    let mut c = read_config(&state.app_data);
    c.mcp_enabled = on;
    write_config(&state.app_data, &c);
    if on {
        start()?;
    } else {
        stop();
    }
    Ok(mcp_status(state))
}

// ---------------------------------------------------------------------------
// 口（OS 別）
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn endpoint_name(_app_data: &Path) -> String {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!(r"\\.\pipe\texos-mcp-{user}")
}

#[cfg(not(windows))]
fn endpoint_name(app_data: &Path) -> String {
    app_data.join("mcp.sock").to_string_lossy().into_owned()
}

#[cfg(windows)]
fn restrict_file(_p: &Path) {
    // %APPDATA% はユーザー本人だけが読める ACL（既定）なので追加の処理はしない。
}

#[cfg(not(windows))]
fn restrict_file(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}

#[cfg(windows)]
fn poke(ep: &str) {
    let _ = std::fs::OpenOptions::new().read(true).write(true).open(ep);
}

#[cfg(not(windows))]
fn poke(ep: &str) {
    let _ = std::os::unix::net::UnixStream::connect(ep);
}

/// 名前付きパイプの待受。1接続1スレッド。最初のインスタンスは FILE_FLAG_FIRST_PIPE_INSTANCE
/// で作る＝同名の口を他プロセスが先に作っていたら失敗する（なりすまし防止）。
#[cfg(windows)]
fn serve(ep: String, ready: std::sync::mpsc::Sender<Result<(), String>>) {
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    let b = bridge().expect("bridge");
    let wname: Vec<u16> = ep.encode_utf16().chain(std::iter::once(0)).collect();
    let mut ready = Some(ready);
    let mut first = true;
    b.listening.store(true, Ordering::SeqCst);
    loop {
        if !b.enabled.load(Ordering::SeqCst) {
            break;
        }
        let mode = if first {
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            PIPE_ACCESS_DUPLEX
        };
        let h = unsafe {
            CreateNamedPipeW(
                PCWSTR(wname.as_ptr()),
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                None,
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_invalid() {
            let err = std::io::Error::last_os_error();
            if let Some(tx) = ready.take() {
                let _ = tx.send(Err(format!(
                    "名前付きパイプを作れません（別の TexoS が同じ口を使っている可能性があります）: {err}"
                )));
                break;
            }
            eprintln!("[mcp] パイプの再作成に失敗: {err}");
            std::thread::sleep(std::time::Duration::from_millis(500));
            continue;
        }
        if let Some(tx) = ready.take() {
            let _ = tx.send(Ok(()));
        }
        first = false;
        // ERROR_PIPE_CONNECTED（作成〜待受の間に既に繋がった）も成功として扱う。
        let connected = unsafe { ConnectNamedPipe(h, None) };
        if let Err(e) = &connected {
            if e.code() != windows::Win32::Foundation::ERROR_PIPE_CONNECTED.to_hresult() {
                unsafe {
                    let _ = CloseHandle(h);
                }
                continue;
            }
        }
        if !b.enabled.load(Ordering::SeqCst) {
            unsafe {
                let _ = CloseHandle(h);
            }
            break;
        }
        let file = unsafe { std::fs::File::from_raw_handle(h.0 as _) };
        std::thread::spawn(move || {
            let reader = match file.try_clone() {
                Ok(f) => f,
                Err(_) => return,
            };
            handle_conn(reader, file);
        });
    }
    b.listening.store(false, Ordering::SeqCst);
}

#[cfg(not(windows))]
fn serve(ep: String, ready: std::sync::mpsc::Sender<Result<(), String>>) {
    use std::os::unix::fs::PermissionsExt;
    let b = bridge().expect("bridge");
    let _ = std::fs::remove_file(&ep);
    let listener = match std::os::unix::net::UnixListener::bind(&ep) {
        Ok(l) => l,
        Err(e) => {
            let _ = ready.send(Err(format!("ソケットを作れません: {e}")));
            return;
        }
    };
    let _ = std::fs::set_permissions(&ep, std::fs::Permissions::from_mode(0o600));
    b.listening.store(true, Ordering::SeqCst);
    let _ = ready.send(Ok(()));
    for conn in listener.incoming() {
        if !b.enabled.load(Ordering::SeqCst) {
            break;
        }
        if let Ok(s) = conn {
            std::thread::spawn(move || {
                if let Ok(r) = s.try_clone() {
                    handle_conn(r, s);
                }
            });
        }
    }
    let _ = std::fs::remove_file(&ep);
    b.listening.store(false, Ordering::SeqCst);
}

/// 1接続の処理: 1行1要求を読み、1行1応答を返す。
fn handle_conn(reader: impl std::io::Read, mut writer: impl Write) {
    let Some(b) = bridge() else { return };
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = match (&mut reader).take(LINE_MAX as u64 + 1).read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(_) => return,
        };
        if n == 0 {
            return;
        }
        if buf.len() > LINE_MAX {
            let _ = write_line(&mut writer, &err_resp(Value::Null, McpErr::new("request_too_large", "要求が大きすぎます")));
            return;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(line) {
            Ok(req) => process(b, &req),
            Err(e) => err_resp(Value::Null, McpErr::new("invalid_json", &format!("JSON として読めません: {e}"))),
        };
        if write_line(&mut writer, &resp).is_err() {
            return;
        }
        if !b.enabled.load(Ordering::SeqCst) {
            return;
        }
    }
}

fn write_line(w: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let mut s = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    w.write_all(s.as_bytes())?;
    w.flush()
}

// ---------------------------------------------------------------------------
// 要求の処理
// ---------------------------------------------------------------------------

pub(crate) struct McpErr {
    code: String,
    message: String,
    details: Option<Value>,
}

impl McpErr {
    fn new(code: &str, message: &str) -> Self {
        McpErr { code: code.into(), message: message.into(), details: None }
    }
    fn with(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
    /// 内部処理のエラー文字列を、判別できるコードへ寄せる。
    fn from_internal(default_code: &str, e: String) -> Self {
        let code = if e.starts_with("project_mismatch") {
            "project_mismatch"
        } else if e.contains("このフローは実行中です") {
            "busy"
        } else if e.contains("プロジェクトが開かれていません") {
            "no_project"
        } else {
            default_code
        };
        McpErr::new(code, &e)
    }
}

type R = Result<Value, McpErr>;

fn err_resp(id: Value, e: McpErr) -> Value {
    let mut err = json!({ "code": e.code, "message": e.message });
    if let Some(d) = e.details {
        err["details"] = d;
    }
    json!({ "id": id, "ok": false, "error": err })
}

fn process(b: &Bridge, req: &Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    if !b.enabled.load(Ordering::SeqCst) {
        return err_resp(id, McpErr::new("mcp_disabled", "TexoS の MCP 接続は無効です（キャンバス右クリック「表示・設定」で有効にできます）"));
    }
    let token = req.get("token").and_then(Value::as_str).unwrap_or("");
    let expected = b.token.lock().unwrap().clone();
    if expected.is_empty() || !ct_eq(token.as_bytes(), expected.as_bytes()) {
        return err_resp(id, McpErr::new("unauthorized", "トークンが一致しません（TexoS を再起動した／MCP を切り替えた場合は mcp.json を読み直してください）"));
    }
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let empty = Value::Object(Map::new());
    let params = req.get("params").unwrap_or(&empty);
    match dispatch(b, method, params) {
        Ok(result) => json!({ "id": id, "ok": true, "result": result }),
        Err(e) => err_resp(id, e),
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 要求の指紋: request_id を除いた引数を、キー順を揃え project を正規化して文字列にする。
fn request_fingerprint(params: &Value) -> String {
    fn canon(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut out = Map::new();
                for k in keys {
                    out.insert(k.clone(), canon(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            other => other.clone(),
        }
    }
    let mut p = params.clone();
    if let Some(m) = p.as_object_mut() {
        m.remove("request_id");
        let pr = m.get("project").and_then(Value::as_str).map(|s| norm_root(Path::new(s)));
        if let Some(pr) = pr {
            m.insert("project".into(), json!(pr));
        }
    }
    canon(&p).to_string()
}

/// 変更系の冪等化: 同じ method＋request_id の2回目以降は、1回目の結果を replayed:true で返す。
/// ただし再送でも ①中身（対象プロジェクト・本文・座標…）が1回目と違えば request_id_conflict、
/// ②いま開いているプロジェクトが指定と違えば project_mismatch で断る（成功と誤認させない）。
fn idempotent(b: &Bridge, method: &str, params: &Value, f: impl FnOnce() -> R) -> R {
    let rid = match params.get("request_id").and_then(Value::as_str) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return f(),
    };
    let _serial = b.idem_lock.lock().unwrap();
    let key = format!("{method}\u{0}{rid}");
    let fp = request_fingerprint(params);
    let hit = b.idem.lock().unwrap().0.get(&key).cloned();
    if let Some((first_fp, mut v)) = hit {
        if first_fp != fp {
            return Err(McpErr::new(
                "request_id_conflict",
                "この request_id は別の内容の要求に使われています（再送なら1回目と同じ引数で、新しい要求なら新しい request_id で送ってください）",
            )
            .with(json!({ "request_id": rid, "method": method, "first_result": v })));
        }
        if let Some(want) = params.get("project").and_then(Value::as_str) {
            with_project(b, Some(want), |_| Ok(()))?;
        }
        v["replayed"] = json!(true);
        return Ok(v);
    }
    let out = f()?;
    let mut g = b.idem.lock().unwrap();
    g.0.insert(key.clone(), (fp, out.clone()));
    g.1.push_back(key);
    while g.1.len() > IDEM_KEEP {
        if let Some(old) = g.1.pop_front() {
            g.0.remove(&old);
        }
    }
    Ok(out)
}

fn dispatch(b: &Bridge, method: &str, params: &Value) -> R {
    match method {
        "ping" => Ok(json!({ "pong": true })),
        "status" => status(b),
        "get_project" => get_project(b, params),
        "list_cards" => list_cards(b, params),
        "get_card" => get_card(b, params),
        "list_relations" => list_relations_m(b, params),
        "read_card_text" => read_card_text(b, params),
        "list_ai_models" => list_ai_models(params),
        "create_note" => idempotent(b, method, params, || create_note_m(b, params)),
        "update_note" => update_note_m(b, params),
        "create_card" => idempotent(b, method, params, || create_card(b, params)),
        "update_card" => update_card(b, params),
        "move_card" => move_card(b, params),
        "connect" => idempotent(b, method, params, || connect(b, params)),
        "update_relation" => update_relation(b, params),
        "disconnect" => disconnect(b, params),
        "import_file" => idempotent(b, method, params, || import_file_m(b, params)),
        "run_card" => idempotent(b, method, params, || run_card(b, params)),
        "get_job" => get_job(b, params),
        "cancel_job" => cancel_job(b, params),
        "list_jobs" => list_jobs_m(b),
        _ => Err(McpErr::new("unknown_method", &format!("未知のメソッドです: {method}"))),
    }
}

// --- 引数 -------------------------------------------------------------------

fn p_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, McpErr> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| McpErr::new("invalid_params", &format!("引数 {key}（文字列）が必要です")))
}

fn p_opt_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn p_f64(params: &Value, key: &str) -> Result<f64, McpErr> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or_else(|| McpErr::new("invalid_params", &format!("引数 {key}（数値）が必要です")))
}

fn p_i64(params: &Value, key: &str) -> Result<i64, McpErr> {
    params
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| McpErr::new("invalid_params", &format!("引数 {key}（整数）が必要です")))
}

fn p_usize(params: &Value, key: &str, default: usize, max: usize) -> usize {
    params
        .get(key)
        .and_then(Value::as_u64)
        .map(|v| (v as usize).min(max))
        .unwrap_or(default)
}

// --- プロジェクトの照合 ------------------------------------------------------

/// 2つのプロジェクトルートが同じ場所か（区切り・末尾・大小（Windows）を吸収）。
/// プロジェクトルートの比較用の正規形（区切り・末尾・大小（Windows）を吸収）。
fn norm_root(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.trim_end_matches('\\').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.replace('\\', "/")
    }
}

pub(crate) fn same_root(a: &Path, b: &Path) -> bool {
    if norm_root(a) == norm_root(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn project_json(root: &Path) -> Value {
    let info = project_info(root);
    json!({ "path": info.path, "name": info.name })
}

/// 現在のプロジェクトを短くロックして f を実行する。`want` が Some なら一致を確かめる
/// （照合と処理を同じロックの中で行う＝途中で切り替わっても別のプロジェクトに当たらない）。
fn with_project<T>(
    b: &Bridge,
    want: Option<&str>,
    f: impl FnOnce(&Project) -> Result<T, McpErr>,
) -> Result<T, McpErr> {
    let state: State<AppState> = b.app.state();
    let guard = state.current.lock().unwrap();
    let p = guard
        .as_ref()
        .ok_or_else(|| McpErr::new("no_project", "TexoS でプロジェクトが開かれていません"))?;
    if let Some(w) = want {
        if !same_root(&p.root, Path::new(w)) {
            return Err(McpErr::new("project_mismatch", "指定したプロジェクトは現在開かれていません。get_project で現在のプロジェクトを確認してください")
                .with(json!({ "requested": w, "current": project_info(&p.root).path })));
        }
    }
    f(p)
}

/// 変更・実行系: project は必須。
fn require_project(params: &Value) -> Result<&str, McpErr> {
    p_str(params, "project").map_err(|_| {
        McpErr::new("invalid_params", "変更・実行には project（get_project が返す path）が必要です")
    })
}

/// 変更を画面へ知らせる（同じプロジェクトを開いていればフロントが UNDO に積んで読み直す）。
fn notify(b: &Bridge, root: &Path, undo: Value, touched: Vec<String>) {
    let _ = b.app.emit(
        "mcp:changed",
        json!({ "project": project_info(root).path, "undo": undo, "touched": touched }),
    );
}

// --- カード情報 -------------------------------------------------------------

struct CardRow {
    id: String,
    kind: String,
    label: String,
    x: f64,
    y: f64,
    spec: Option<String>,
    note_text: Option<String>,
    mime: Option<String>,
    committed_path: Option<String>,
    memo: Option<String>,
    last_run: Option<String>,
    url_ref: Option<String>,
    project_ref: Option<String>,
}

fn card_row(p: &Project, id: &str) -> Result<CardRow, McpErr> {
    p.conn
        .query_row(
            "SELECT v.id, COALESCE(v.kind, 'file'), v.label, v.pos_x, v.pos_y, v.filter_ref,
                    v.note_text, COALESCE(v.mime_type, r.mime_type), v.committed_path, v.memo,
                    v.last_run, v.url_ref, v.project_ref
             FROM virtual_bodies v LEFT JOIN real_bodies r ON r.id = v.real_id
             WHERE v.id = ?1 AND v.deleted_at IS NULL AND v.desktop_id IS NULL",
            rusqlite::params![id],
            |r| {
                Ok(CardRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    label: r.get(2)?,
                    x: r.get(3)?,
                    y: r.get(4)?,
                    spec: r.get(5)?,
                    note_text: r.get(6)?,
                    mime: r.get(7)?,
                    committed_path: r.get(8)?,
                    memo: r.get(9)?,
                    last_run: r.get(10)?,
                    url_ref: r.get(11)?,
                    project_ref: r.get(12)?,
                })
            },
        )
        .optional()
        .map_err(|e| McpErr::new("internal", &e.to_string()))?
        .ok_or_else(|| McpErr::new("not_found", &format!("カードが見つかりません: {id}")))
}

/// カードの「種別」（エージェント向けの分類）。
fn card_type(kind: &str, spec: Option<&str>) -> String {
    let v: Value = spec.and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
    match kind {
        "filter" => {
            if let Some(bi) = v.get("builtin").and_then(Value::as_str) {
                if bi == "ai" {
                    let mode = v.get("mode").and_then(Value::as_str).unwrap_or("transform");
                    return if mode == "chat" { "ai_chat".into() } else { "ai".into() };
                }
                return format!("builtin:{bi}");
            }
            if v.get("shell").is_some() {
                return "shell".into();
            }
            "program".into()
        }
        "op" => format!("op:{}", v.get("op").and_then(Value::as_str).unwrap_or("?")),
        other => other.to_string(),
    }
}

/// MCP で設定を変えられる種別か。
fn mcp_editable(ty: &str) -> bool {
    ty == "ai" || ty == "op:in" || ty == "op:out" || ty == "note"
        || ty.strip_prefix("builtin:").map(|n| BUILTINS.contains(&n)).unwrap_or(false)
}

fn clip(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        (s.to_string(), false)
    } else {
        (s.chars().take(max).collect(), true)
    }
}

fn parse_last_run(s: Option<&str>) -> Value {
    let mut v: Value = match s.and_then(|s| serde_json::from_str(s).ok()) {
        Some(v) => v,
        None => return Value::Null,
    };
    if let Some(m) = v.get("message").and_then(Value::as_str) {
        let (c, t) = clip(m, 1000);
        v["message"] = json!(c);
        if t {
            v["message_truncated"] = json!(true);
        }
    }
    v
}

/// 秘密になり得るキー（トークン・API キー等）を伏せる。
fn redact(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, val) in m.iter_mut() {
                let lk = k.to_ascii_lowercase();
                if ["token", "key", "secret", "password", "authorization", "bearer"]
                    .iter()
                    .any(|w| lk.contains(w))
                    && !val.is_null()
                {
                    *val = json!("***");
                } else {
                    redact(val);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact),
        _ => {}
    }
}

fn summary(c: &CardRow) -> Value {
    let ty = if c.kind == "note" { "note".to_string() } else { card_type(&c.kind, c.spec.as_deref()) };
    let mut v = json!({
        "id": c.id,
        "kind": c.kind,
        "type": ty,
        "label": c.label,
        "x": c.x,
        "y": c.y,
        "mcp_editable": mcp_editable(&ty),
    });
    if let Some(t) = &c.note_text {
        let (p, tr) = clip(t, 200);
        v["text_preview"] = json!(p);
        v["text_chars"] = json!(t.chars().count());
        if tr {
            v["text_preview_truncated"] = json!(true);
        }
    }
    if c.kind == "file" {
        v["mime"] = json!(c.mime);
    }
    if let Some(cp) = &c.committed_path {
        v["path"] = json!(cp);
    }
    if c.last_run.is_some() {
        v["last_run"] = parse_last_run(c.last_run.as_deref());
    }
    v
}

fn all_cards(p: &Project) -> Result<Vec<CardRow>, McpErr> {
    let mut stmt = p
        .conn
        .prepare(
            "SELECT v.id FROM virtual_bodies v
             WHERE v.deleted_at IS NULL AND v.desktop_id IS NULL ORDER BY v.created_at",
        )
        .map_err(|e| McpErr::new("internal", &e.to_string()))?;
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| McpErr::new("internal", &e.to_string()))?
        .filter_map(|r| r.ok())
        .collect();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(mut c) = card_row(p, &id) {
            c.committed_path = c.committed_path.map(|s| load_path(&p.root, &s));
            out.push(c);
        }
    }
    Ok(out)
}

// --- 状態取得 ---------------------------------------------------------------

fn status(b: &Bridge) -> R {
    let state: State<AppState> = b.app.state();
    let project = state.current.lock().unwrap().as_ref().map(|p| project_json(&p.root));
    Ok(json!({
        "texos_version": b.app.package_info().version.to_string(),
        "mcp_enabled": b.enabled.load(Ordering::SeqCst),
        "project": project,
    }))
}

fn get_project(b: &Bridge, params: &Value) -> R {
    let state: State<AppState> = b.app.state();
    let running: Vec<JobInfo> = state.jobs.list();
    with_project(b, p_opt_str(params, "project"), |p| {
        let count = |sql: &str| -> i64 { p.conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        let root = project_info(&p.root).path;
        let jobs: Vec<Value> = running
            .iter()
            .filter(|j| {
                state
                    .jobs
                    .get(j.id)
                    .map(|job| same_root(Path::new(&job.project_root), &p.root))
                    .unwrap_or(false)
            })
            .map(|j| json!({ "job_id": j.id, "label": j.label, "card_id": j.card }))
            .collect();
        Ok(json!({
            "project": project_json(&p.root),
            "path": root,
            "counts": {
                "cards": count("SELECT COUNT(*) FROM virtual_bodies WHERE deleted_at IS NULL AND desktop_id IS NULL AND COALESCE(kind,'file') <> 'note'"),
                "notes": count("SELECT COUNT(*) FROM virtual_bodies WHERE deleted_at IS NULL AND kind = 'note'"),
                "relations": count("SELECT COUNT(*) FROM relations WHERE deleted_at IS NULL"),
            },
            "running_jobs": jobs,
        }))
    })
}

fn list_cards(b: &Bridge, params: &Value) -> R {
    let limit = p_usize(params, "limit", LIST_DEFAULT, LIST_MAX).max(1);
    let offset = p_usize(params, "offset", 0, usize::MAX);
    let kinds: Option<Vec<String>> = params.get("kinds").and_then(Value::as_array).map(|a| {
        a.iter().filter_map(Value::as_str).map(str::to_string).collect()
    });
    with_project(b, p_opt_str(params, "project"), |p| {
        let cards = all_cards(p)?;
        let filtered: Vec<&CardRow> = cards
            .iter()
            .filter(|c| kinds.as_ref().map(|k| k.contains(&c.kind)).unwrap_or(true))
            .collect();
        let total = filtered.len();
        let page: Vec<Value> = filtered.iter().skip(offset).take(limit).map(|c| summary(c)).collect();
        let next = (offset + page.len() < total).then(|| offset + page.len());
        Ok(json!({
            "project": project_json(&p.root),
            "total": total,
            "offset": offset,
            "limit": limit,
            "next_offset": next,
            "cards": page,
        }))
    })
}

fn relation_json(r: &RelationInfo) -> Value {
    let attrs: Value = r
        .attrs
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    json!({ "id": r.id, "from_id": r.from_id, "to_id": r.to_id, "kind": r.kind, "attrs": attrs })
}

fn get_card(b: &Bridge, params: &Value) -> R {
    let id = p_str(params, "card_id")?;
    with_project(b, p_opt_str(params, "project"), |p| {
        let mut c = card_row(p, id)?;
        c.committed_path = c.committed_path.map(|s| load_path(&p.root, &s));
        let mut v = summary(&c);
        if let Some(spec) = &c.spec {
            let mut sv: Value = serde_json::from_str(spec).unwrap_or(json!({ "_raw": spec }));
            redact(&mut sv);
            let mut truncated = vec![];
            if let Some(m) = sv.as_object_mut() {
                for (k, val) in m.iter_mut() {
                    if let Some(s) = val.as_str() {
                        let (cs, t) = clip(s, 8000);
                        if t {
                            *val = json!(cs);
                            truncated.push(k.clone());
                        }
                    }
                }
            }
            v["settings"] = sv;
            if !truncated.is_empty() {
                v["settings_truncated_fields"] = json!(truncated);
            }
        }
        if let Some(m) = &c.memo {
            v["memo"] = json!(clip(m, 2000).0);
        }
        if let Some(u) = &c.url_ref {
            v["url"] = json!(u);
        }
        if let Some(pr) = &c.project_ref {
            v["project_ref"] = json!(pr);
        }
        let rels = list_relations_in(p).map_err(|e| McpErr::new("internal", &e))?;
        v["inputs"] = json!(rels.iter().filter(|r| r.to_id == id).map(relation_json).collect::<Vec<_>>());
        v["outputs"] = json!(rels.iter().filter(|r| r.from_id == id).map(relation_json).collect::<Vec<_>>());
        v["project"] = project_json(&p.root);
        Ok(v)
    })
}

fn list_relations_m(b: &Bridge, params: &Value) -> R {
    let limit = p_usize(params, "limit", LIST_MAX, 5000).max(1);
    let offset = p_usize(params, "offset", 0, usize::MAX);
    let card = p_opt_str(params, "card_id");
    with_project(b, p_opt_str(params, "project"), |p| {
        let rels = list_relations_in(p).map_err(|e| McpErr::new("internal", &e))?;
        let filtered: Vec<&RelationInfo> = rels
            .iter()
            .filter(|r| card.map(|c| r.from_id == c || r.to_id == c).unwrap_or(true))
            .collect();
        let total = filtered.len();
        let page: Vec<Value> = filtered.iter().skip(offset).take(limit).map(|r| relation_json(r)).collect();
        let next = (offset + page.len() < total).then(|| offset + page.len());
        Ok(json!({
            "project": project_json(&p.root),
            "total": total, "offset": offset, "limit": limit, "next_offset": next,
            "relations": page,
        }))
    })
}

fn is_textual(mime: &str, path: Option<&str>) -> bool {
    if mime.starts_with("text/") {
        return true;
    }
    if matches!(
        mime,
        "application/json" | "application/xml" | "application/javascript" | "application/x-yaml"
            | "application/toml" | "application/x-sh"
    ) {
        return true;
    }
    path.and_then(|p| Path::new(p).extension())
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "txt" | "csv" | "tsv" | "json" | "xml" | "log"))
        .unwrap_or(false)
}

fn read_card_text(b: &Bridge, params: &Value) -> R {
    let id = p_str(params, "card_id")?;
    let max = p_usize(params, "max_chars", TEXT_DEFAULT, TEXT_MAX).max(1);
    let offset = p_usize(params, "offset", 0, usize::MAX);
    with_project(b, p_opt_str(params, "project"), |p| {
        let mut c = card_row(p, id)?;
        c.committed_path = c.committed_path.map(|s| load_path(&p.root, &s));
        let (text, encoding) = match c.kind.as_str() {
            "note" => (c.note_text.clone().unwrap_or_default(), "UTF-8".to_string()),
            "file" => {
                let mime = c.mime.clone().unwrap_or_default();
                if !is_textual(&mime, c.committed_path.as_deref()) {
                    return Err(McpErr::new("not_text", "このカードはテキストではありません（バイナリ本文は取得できません）")
                        .with(json!({ "mime": mime, "path": c.committed_path })));
                }
                let real_id: String = p
                    .conn
                    .query_row("SELECT real_id FROM virtual_bodies WHERE id = ?1", rusqlite::params![id], |r| r.get(0))
                    .map_err(|e| McpErr::new("internal", &e.to_string()))?;
                let src = resolve_view_path(p, id, &real_id);
                let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
                if size > TEXT_FILE_MAX_BYTES {
                    return Err(McpErr::new("too_large", "ファイルが大きすぎます").with(json!({ "bytes": size })));
                }
                let vt = read_view_text_in(p, id).map_err(|e| McpErr::new("not_readable", &e))?;
                (vt.text, vt.encoding)
            }
            other => {
                return Err(McpErr::new("not_text", &format!(
                    "{other} カードには本文がありません（設定は get_card で読めます）"
                )))
            }
        };
        let total = text.chars().count();
        let body: String = text.chars().skip(offset).take(max).collect();
        let returned = body.chars().count();
        let next = (offset + returned < total).then(|| offset + returned);
        Ok(json!({
            "project": project_json(&p.root),
            "card_id": id,
            "label": c.label,
            "kind": c.kind,
            "path": c.committed_path,
            "encoding": encoding,
            "total_chars": total,
            "offset": offset,
            "returned_chars": returned,
            "truncated": next.is_some(),
            "next_offset": next,
            "text": body,
        }))
    })
}

fn list_ai_models(params: &Value) -> R {
    let provider = p_opt_str(params, "provider").unwrap_or("ollama").to_string();
    if !matches!(provider.as_str(), "ollama" | "lmstudio" | "openai") {
        return Err(McpErr::new("invalid_params", "provider は ollama / lmstudio / openai のいずれかです"));
    }
    let endpoint = p_opt_str(params, "endpoint").map(str::to_string);
    let models = ai_list_models(Some(provider.clone()), endpoint.clone())
        .map_err(|e| McpErr::new("ai_unreachable", &e))?;
    Ok(json!({ "provider": provider, "endpoint": endpoint, "models": models }))
}

// --- 編集 -------------------------------------------------------------------

fn create_note_m(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let text = p_str(params, "text")?.to_string();
    let (x, y) = (p_f64(params, "x")?, p_f64(params, "y")?);
    let (id, root) = with_project(b, Some(want), |p| {
        let id = create_note_in(p, text, x, y).map_err(|e| McpErr::from_internal("internal", e))?;
        Ok((id, p.root.clone()))
    })?;
    notify(b, &root, json!([{ "kind": "deleteNotes", "ids": [id] }]), vec![]);
    Ok(json!({ "project": project_json(&root), "card_id": id, "kind": "note" }))
}

fn update_note_m(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let id = p_str(params, "card_id")?;
    let text = p_str(params, "text")?;
    let root = with_project(b, Some(want), |p| {
        let c = card_row(p, id)?;
        if c.kind != "note" {
            return Err(McpErr::new("unsupported_card_type", "付箋（note）ではありません"));
        }
        update_note_in(p, id, text).map_err(|e| McpErr::from_internal("internal", e))?;
        Ok(p.root.clone())
    })?;
    // 本文の編集は UI でも UNDO の対象外（同じ扱い）。
    notify(b, &root, json!([]), vec![]);
    Ok(json!({ "project": project_json(&root), "card_id": id, "updated": true, "undo": "unsupported" }))
}

/// 設定オブジェクトを検証して spec へマージする（null はキーを消す）。
fn merge_settings(spec: &mut Map<String, Value>, settings: &Value, allowed: &[&str]) -> Result<(), McpErr> {
    let Some(m) = settings.as_object() else {
        if settings.is_null() {
            return Ok(());
        }
        return Err(McpErr::new("invalid_params", "settings はオブジェクトで指定してください"));
    };
    let bad: Vec<&String> = m.keys().filter(|k| !allowed.contains(&k.as_str())).collect();
    if !bad.is_empty() {
        return Err(McpErr::new("unsupported_setting", "この種別では変更できない設定があります")
            .with(json!({ "rejected": bad, "allowed": allowed })));
    }
    for (k, v) in m {
        if v.is_null() {
            spec.remove(k);
        } else {
            spec.insert(k.clone(), v.clone());
        }
    }
    Ok(())
}

fn validate_ai(spec: &Map<String, Value>) -> Result<(), McpErr> {
    let model = spec.get("model").and_then(Value::as_str).unwrap_or("").trim();
    if model.is_empty() {
        return Err(McpErr::new("invalid_spec", "AI カードには model が必要です（list_ai_models で実機のモデルを確認して指定してください）"));
    }
    if let Some(pv) = spec.get("provider").and_then(Value::as_str) {
        if !matches!(pv, "ollama" | "lmstudio" | "openai") {
            return Err(McpErr::new("invalid_spec", "provider は ollama / lmstudio / openai のいずれかです"));
        }
    }
    if let Some(t) = spec.get("think").and_then(Value::as_str) {
        if !matches!(t, "off" | "on" | "low" | "medium" | "high") {
            return Err(McpErr::new("invalid_spec", "think は off / on / low / medium / high のいずれかです"));
        }
    }
    if let Some(g) = spec.get("gpu").and_then(Value::as_str) {
        if !matches!(g, "auto" | "max" | "cpu") {
            return Err(McpErr::new("invalid_spec", "gpu は auto / max / cpu のいずれかです"));
        }
    }
    Ok(())
}

fn create_card(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let ty = p_str(params, "type")?;
    let (x, y) = (p_f64(params, "x")?, p_f64(params, "y")?);
    let settings = params.get("settings").cloned().unwrap_or(Value::Null);
    let label_in = p_opt_str(params, "label").map(str::to_string);
    let mut spec = Map::new();
    let label = match ty {
        "ai" => {
            spec.insert("builtin".into(), json!("ai"));
            spec.insert("mode".into(), json!("transform"));
            spec.insert("provider".into(), json!("ollama"));
            spec.insert("system".into(), json!(""));
            spec.insert("accept_mime".into(), json!("text/*"));
            spec.insert("output_ext".into(), json!("md"));
            merge_settings(&mut spec, &settings, AI_KEYS)?;
            validate_ai(&spec)?;
            label_in.unwrap_or_else(|| "AI変換".into())
        }
        "builtin" => {
            let name = p_str(params, "name")?;
            if !BUILTINS.contains(&name) {
                return Err(McpErr::new("unsupported_card_type", "この内蔵カードは MCP からは作れません")
                    .with(json!({ "name": name, "supported": BUILTINS })));
            }
            spec.insert("builtin".into(), json!(name));
            if name == "tap" {
                spec.insert("accept_mime".into(), json!("*/*"));
            } else {
                spec.insert("accept_mime".into(), json!("text/*"));
            }
            if name == "text_concat" {
                spec.insert("separator".into(), json!("rule"));
                spec.insert("output_ext".into(), json!("md"));
            }
            merge_settings(&mut spec, &settings, BUILTIN_KEYS)?;
            let label = label_in.unwrap_or_else(|| name.to_string());
            if name == "tap" && !spec.contains_key("prefix") {
                spec.insert("prefix".into(), json!(label.clone()));
            }
            label
        }
        "out" => {
            spec.insert("op".into(), json!("out"));
            merge_settings(&mut spec, &settings, OUT_KEYS)?;
            label_in.unwrap_or_else(|| "OUT".into())
        }
        "in" => {
            spec.insert("op".into(), json!("in"));
            merge_settings(&mut spec, &settings, &[])?;
            label_in.unwrap_or_else(|| "IN".into())
        }
        _ => {
            return Err(McpErr::new("unsupported_card_type", "type は ai / builtin / in / out のいずれかです（付箋は create_note）"));
        }
    };
    let spec_s = Value::Object(spec.clone()).to_string();
    let (id, root) = with_project(b, Some(want), |p| {
        let id = create_filter_in(p, label.clone(), spec_s, x, y)
            .map_err(|e| McpErr::from_internal("invalid_spec", e))?;
        Ok((id, p.root.clone()))
    })?;
    notify(b, &root, json!([{ "kind": "delete", "ids": [id] }]), vec![]);
    let spec_str = Value::Object(spec.clone()).to_string();
    let ty_out = card_type(if ty == "in" || ty == "out" { "op" } else { "filter" }, Some(spec_str.as_str()));
    Ok(json!({
        "project": project_json(&root),
        "card_id": id,
        "type": ty_out,
        "label": label,
        "settings": Value::Object(spec),
    }))
}

fn update_card(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let id = p_str(params, "card_id")?;
    let settings = params
        .get("settings")
        .ok_or_else(|| McpErr::new("invalid_params", "settings が必要です"))?;
    let (spec, root) = with_project(b, Some(want), |p| {
        let c = card_row(p, id)?;
        let ty = card_type(&c.kind, c.spec.as_deref());
        let mut spec: Map<String, Value> = c
            .spec
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let allowed: &[&str] = match ty.as_str() {
            "ai" => AI_KEYS,
            "op:out" => OUT_KEYS,
            "op:in" => &[],
            t if t.strip_prefix("builtin:").map(|n| BUILTINS.contains(&n)).unwrap_or(false) => BUILTIN_KEYS,
            "note" => {
                return Err(McpErr::new("unsupported_card_type", "付箋の本文は update_note で変更します"));
            }
            _ => {
                return Err(McpErr::new("unsupported_card_type", "この種別のカードは MCP からは設定を変更できません")
                    .with(json!({ "type": ty })));
            }
        };
        merge_settings(&mut spec, settings, allowed)?;
        if ty == "ai" {
            validate_ai(&spec)?;
        }
        update_filter_in(p, id, &Value::Object(spec.clone()).to_string())
            .map_err(|e| McpErr::from_internal("invalid_spec", e))?;
        Ok((spec, p.root.clone()))
    })?;
    let mut sv = Value::Object(spec);
    redact(&mut sv);
    // 設定の変更は UI でも UNDO の対象外（同じ扱い）。
    notify(b, &root, json!([]), vec![]);
    Ok(json!({ "project": project_json(&root), "card_id": id, "settings": sv, "undo": "unsupported" }))
}

fn move_card(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let id = p_str(params, "card_id")?;
    let (x, y) = (p_f64(params, "x")?, p_f64(params, "y")?);
    let (undo, root) = with_project(b, Some(want), |p| {
        let c = card_row(p, id)?;
        let undo = if c.kind == "note" {
            move_note_in(p, id, x, y).map_err(|e| McpErr::from_internal("internal", e))?;
            json!([{ "kind": "notes", "moves": [{ "id": id, "x": c.x, "y": c.y }] }])
        } else {
            move_object_in(p, id, x, y).map_err(|e| McpErr::from_internal("internal", e))?;
            json!([{ "kind": "objects", "moves": [{ "id": id, "x": c.x, "y": c.y }] }])
        };
        Ok((undo, p.root.clone()))
    })?;
    notify(b, &root, undo, vec![]);
    Ok(json!({ "project": project_json(&root), "card_id": id, "x": x, "y": y }))
}

fn check_role(role: Option<&str>) -> Result<(), McpErr> {
    match role {
        None | Some("") | Some("system") | Some("previous") | Some("answer") => Ok(()),
        Some(r) => Err(McpErr::new("invalid_params", &format!("role は system / previous / answer のいずれか（外すなら空文字）です: {r}"))),
    }
}

fn check_when(when: Option<&str>) -> Result<(), McpErr> {
    match when {
        None | Some("") | Some("always") | Some("success") | Some("failure") => Ok(()),
        Some(w) => Err(McpErr::new("invalid_params", &format!("when は always / success / failure のいずれかです: {w}"))),
    }
}

fn check_carry(carry: Option<&str>) -> Result<(), McpErr> {
    match carry {
        None | Some("") | Some("input") | Some("none") => Ok(()),
        Some(c) => Err(McpErr::new("invalid_params", &format!("carry は input / none のいずれか（外すなら空文字）です: {c}"))),
    }
}

fn connect(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let from = p_str(params, "from_id")?.to_string();
    let to = p_str(params, "to_id")?.to_string();
    let role = p_opt_str(params, "role").map(str::to_string);
    let when = p_opt_str(params, "when").map(str::to_string);
    let carry = p_opt_str(params, "carry").map(str::to_string);
    check_role(role.as_deref())?;
    check_when(when.as_deref())?;
    check_carry(carry.as_deref())?;
    let (rid, existed, kind, root) = with_project(b, Some(want), |p| {
        let fc = card_row(p, &from)?;
        let tc = card_row(p, &to)?;
        let tty = card_type(&tc.kind, tc.spec.as_deref());
        match role.as_deref() {
            Some("system") if tty != "ai" && tty != "ai_chat" => {
                return Err(McpErr::new("invalid_params", "role=system は AI カードへの矢印にだけ付けられます"));
            }
            Some("answer") if (tty != "builtin:ask" || fc.kind != "note") => {
                return Err(McpErr::new("invalid_params", "role=answer は付箋から問い合わせカード（ask）への矢印にだけ付けられます"));
            }
            Some("previous") if card_type(&fc.kind, fc.spec.as_deref()) != "builtin:loop" => {
                return Err(McpErr::new("invalid_params", "role=previous は反復カード（loop）から引く矢印にだけ付けられます"));
            }
            _ => {}
        }
        let created = create_relation_in(p, from.clone(), to.clone(), None)
            .map_err(|e| McpErr::from_internal("invalid_params", e))?;
        // 既にあれば（UI と同じく重複は作らない）その矢印を返す。
        let (rid, existed) = match created {
            Some(r) => (r, false),
            None => {
                let r: i64 = p
                    .conn
                    .query_row(
                        "SELECT id FROM relations WHERE from_id = ?1 AND to_id = ?2 AND deleted_at IS NULL
                         ORDER BY created_at DESC LIMIT 1",
                        rusqlite::params![from, to],
                        |r| r.get(0),
                    )
                    .map_err(|e| McpErr::new("internal", &e.to_string()))?;
                (r, true)
            }
        };
        if role.is_some() || when.is_some() || carry.is_some() {
            set_relation_attrs_in(p, rid, when.clone(), carry.clone(), None, role.clone())
                .map_err(|e| McpErr::from_internal("internal", e))?;
        }
        let kind: String = p
            .conn
            .query_row("SELECT kind FROM relations WHERE id = ?1", rusqlite::params![rid], |r| r.get(0))
            .map_err(|e| McpErr::new("internal", &e.to_string()))?;
        Ok((rid, existed, kind, p.root.clone()))
    })?;
    let undo = if existed { json!([]) } else { json!([{ "kind": "deleteRelations", "ids": [rid] }]) };
    notify(b, &root, undo, vec![]);
    let mut out = json!({
        "project": project_json(&root),
        "relation_id": rid,
        "kind": kind,
        "already_existed": existed,
    });
    if kind != "flow" {
        out["note"] = json!(match kind.as_str() {
            "req" => "発行（req）の矢印です。トリガ等から下流を起動する線で、データは流れません",
            _ => "データの流れない関連線（link）です。実行の配線にするにはどちらかが処理カードである必要があります",
        });
    }
    Ok(out)
}

fn update_relation(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let rid = p_i64(params, "relation_id")?;
    let role = p_opt_str(params, "role").map(str::to_string);
    let when = p_opt_str(params, "when").map(str::to_string);
    let carry = p_opt_str(params, "carry").map(str::to_string);
    let port = params.get("port").and_then(Value::as_i64);
    check_role(role.as_deref())?;
    check_when(when.as_deref())?;
    check_carry(carry.as_deref())?;
    let (attrs, root) = with_project(b, Some(want), |p| {
        set_relation_attrs_in(p, rid, when, carry, port, role).map_err(|e| {
            if e.contains("見つかりません") {
                McpErr::new("not_found", &format!("矢印が見つかりません: {rid}"))
            } else {
                McpErr::from_internal("internal", e)
            }
        })?;
        let a: Option<String> = p
            .conn
            .query_row("SELECT attrs FROM relations WHERE id = ?1", rusqlite::params![rid], |r| r.get(0))
            .map_err(|e| McpErr::new("internal", &e.to_string()))?;
        Ok((a.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null), p.root.clone()))
    })?;
    notify(b, &root, json!([]), vec![]);
    Ok(json!({ "project": project_json(&root), "relation_id": rid, "attrs": attrs, "undo": "unsupported" }))
}

fn disconnect(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let rid = p_i64(params, "relation_id")?;
    let root = with_project(b, Some(want), |p| {
        let n = delete_relation_in(p, rid).map_err(|e| McpErr::from_internal("internal", e))?;
        if n == 0 {
            return Err(McpErr::new("not_found", &format!("矢印が見つかりません: {rid}")));
        }
        Ok(p.root.clone())
    })?;
    notify(b, &root, json!([{ "kind": "restoreRelations", "ids": [rid] }]), vec![]);
    Ok(json!({ "project": project_json(&root), "relation_id": rid, "deleted": true }))
}

fn import_file_m(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?;
    let path = p_str(params, "path")?.to_string();
    let (x, y) = (p_f64(params, "x")?, p_f64(params, "y")?);
    let src = Path::new(&path);
    if !src.is_absolute() {
        return Err(McpErr::new("invalid_params", "path は絶対パスで指定してください"));
    }
    if !src.is_file() {
        return Err(McpErr::new("not_found", "ファイルが見つかりません（フォルダは取り込めません）"));
    }
    let label = p_opt_str(params, "label")
        .map(str::to_string)
        .unwrap_or_else(|| src.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string());
    let (id, root) = with_project(b, Some(want), |p| {
        let id = import_file_into(p, path.clone(), label.clone(), x, y)
            .map_err(|e| McpErr::from_internal("import_failed", e))?;
        Ok((id, p.root.clone()))
    })?;
    notify(b, &root, json!([{ "kind": "delete", "ids": [id] }]), vec![]);
    Ok(json!({ "project": project_json(&root), "card_id": id, "label": label }))
}

// --- 実行 -------------------------------------------------------------------

fn run_card(b: &Bridge, params: &Value) -> R {
    let want = require_project(params)?.to_string();
    let card = p_str(params, "card_id")?.to_string();
    // 種別の事前確認（終端になれないカードを分かりやすく断る）。
    with_project(b, Some(&want), |p| {
        let c = card_row(p, &card)?;
        let ty = card_type(&c.kind, c.spec.as_deref());
        if c.kind != "filter" && ty != "op:out" {
            return Err(McpErr::new("not_runnable", "終端にできるのは処理カード（filter）か OUT カードだけです")
                .with(json!({ "type": ty })));
        }
        if ty == "ai_chat" {
            return Err(McpErr::new("not_runnable", "対話モードの AI カードは MCP からは実行できません（変換モードを使ってください）"));
        }
        Ok(())
    })?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<JobId, McpErr>>();
    let app = b.app.clone();
    let root = PathBuf::from(&want);
    let card2 = card.clone();
    let want2 = want.clone();
    std::thread::spawn(move || {
        let b = bridge().expect("bridge");
        let mut registered: Option<Arc<Job>> = None;
        let tx2 = tx.clone();
        let result = run_flow_job(&card2, &app, Some(&root), |job| {
            let mut g = b.jobs.lock().unwrap();
            g.0.insert(
                job.id,
                McpJob {
                    card: card2.clone(),
                    label: job.label.clone(),
                    project: job.project_root.clone(),
                    started_at: job.started_at,
                    job: Some(job.clone()),
                    finished: None,
                },
            );
            g.1.push_back(job.id);
            registered = Some(job.clone());
            let _ = tx2.send(Ok(job.id));
        });
        let Some(job) = registered else {
            let e = result.err().unwrap_or_else(|| "実行を開始できませんでした".into());
            let _ = tx.send(Err(McpErr::from_internal("run_failed", e)));
            return;
        };
        let finished = finish_value(&job, &want2, &result);
        // 結果カードを画面へ（UI の ▶ と同じく、新しく生まれたカードは UNDO で消せる）。
        if let Ok(rep) = &result {
            let undo = if rep.created.is_empty() {
                json!([])
            } else {
                json!([{ "kind": "delete", "ids": rep.created }])
            };
            let mut touched = rep.created.clone();
            touched.extend(rep.updated.iter().cloned());
            notify(b, Path::new(&want2), undo, touched);
        }
        let mut g = b.jobs.lock().unwrap();
        if let Some(mj) = g.0.get_mut(&job.id) {
            mj.finished = Some(finished);
            mj.job = None;
        }
        while g.1.len() > JOBS_KEEP {
            let Some(old) = g.1.pop_front() else { break };
            if g.0.get(&old).map(|j| j.finished.is_some()).unwrap_or(true) {
                g.0.remove(&old);
            } else {
                g.1.push_back(old);
                break;
            }
        }
    });
    let job_id = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .map_err(|_| McpErr::new("internal", "ジョブの登録を確認できませんでした"))??;
    Ok(json!({
        "project": project_json(Path::new(&want)),
        "job_id": job_id,
        "card_id": card,
        "status": "running",
    }))
}

fn node_values(job: &Job) -> Vec<Value> {
    job.trace
        .snapshot()
        .into_iter()
        .filter(|e| e.status != "info")
        .map(|e| {
            let errors: Vec<String> = e.errors.iter().map(|s| clip(s, 1000).0).collect();
            json!({
                "card_id": e.card,
                "label": e.label,
                "kind": e.kind,
                "round": e.round,
                "status": e.status,
                "ms": e.ms,
                "exit": e.exit,
                "skipped_inputs": e.skipped,
                "errors": errors,
                "outputs": e.outputs.iter().map(|o| json!({ "name": o.name, "mime": o.mime, "bytes": o.bytes })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// 完了時の結果（全体状態＋失敗・スキップ・生成カード）。
fn finish_value(job: &Job, project: &str, result: &Result<FlowReport, String>) -> Value {
    let cancelled = job.cancel.load(Ordering::Relaxed);
    let nodes = node_values(job);
    let failed_nodes: Vec<&Value> = nodes.iter().filter(|n| n["status"] == "error").collect();
    let mut v = json!({
        "job_id": job.id,
        "card_id": job.card,
        "label": job.label,
        "project": project,
        "started_at": job.started_at,
        "finished_at": db::now_ms(),
    });
    match result {
        Err(e) => {
            v["status"] = json!(if cancelled { "cancelled" } else { "failed" });
            v["error"] = json!(clip(e, 4000).0);
        }
        Ok(rep) => {
            let nodata = rep.skipped.iter().any(|s| s.starts_with("no-data"));
            let status = if cancelled {
                "cancelled"
            } else if !rep.errors.is_empty() || !failed_nodes.is_empty() {
                "completed_with_errors"
            } else if rep.created.is_empty() && rep.updated.is_empty() && nodata {
                "nodata"
            } else {
                "succeeded"
            };
            v["status"] = json!(status);
            v["errors"] = json!(rep.errors.iter().map(|s| clip(s, 2000).0).collect::<Vec<_>>());
            v["skipped"] = json!(rep.skipped);
            v["output_files"] = json!(rep.outputs);
            let outputs = output_cards(project, &rep.created, &rep.updated);
            v["output_cards"] = json!(outputs);
            if let Some(sc) = &rep.shell_capture {
                v["shell_capture"] = json!({ "exit_code": sc.exit_code, "output": clip(&sc.output, 8000).0, "truncated": sc.truncated });
            }
        }
    }
    v["nodes"] = json!(nodes);
    v
}

fn output_cards(project: &str, created: &[String], updated: &[String]) -> Vec<Value> {
    let Ok(p) = open_project_at(Path::new(project)) else {
        return created
            .iter()
            .map(|id| json!({ "card_id": id, "created": true }))
            .chain(updated.iter().map(|id| json!({ "card_id": id, "created": false })))
            .collect();
    };
    let mut out = vec![];
    for (ids, is_new) in [(created, true), (updated, false)] {
        for id in ids {
            let mut v = json!({ "card_id": id, "created": is_new });
            if let Ok(c) = card_row(&p, id) {
                v["label"] = json!(c.label);
                v["mime"] = json!(c.mime);
                v["path"] = json!(c.committed_path.map(|s| load_path(&p.root, &s)));
            }
            out.push(v);
        }
    }
    out
}

fn get_job(b: &Bridge, params: &Value) -> R {
    let job_id = p_i64(params, "job_id")? as JobId;
    let include_nodes = params.get("include_nodes").and_then(Value::as_bool).unwrap_or(true);
    let state: State<AppState> = b.app.state();
    {
        let g = b.jobs.lock().unwrap();
        if let Some(mj) = g.0.get(&job_id) {
            if let Some(f) = &mj.finished {
                let mut f = f.clone();
                if !include_nodes {
                    if let Some(m) = f.as_object_mut() {
                        m.remove("nodes");
                    }
                }
                return Ok(f);
            }
            if let Some(job) = &mj.job {
                return Ok(running_value(job, &mj.project, &mj.card, &mj.label, mj.started_at, include_nodes));
            }
        }
    }
    // MCP 以外（UI・トリガ）から起動した実行中ジョブも見える（結果は保持していない）。
    if let Some(job) = state.jobs.get(job_id) {
        let card = job.card.clone().unwrap_or_default();
        let mut v = running_value(&job, &job.project_root, &card, &job.label, job.started_at, include_nodes);
        v["started_by"] = json!("texos");
        return Ok(v);
    }
    Err(McpErr::new("unknown_job", "ジョブが見つかりません（TexoS の再起動で結果は消えます）"))
}

fn running_value(job: &Job, project: &str, card: &str, label: &str, started: i64, nodes: bool) -> Value {
    let (note, percent) = job.progress.lock().unwrap().clone();
    let mut v = json!({
        "job_id": job.id,
        "status": if job.cancel.load(Ordering::Relaxed) { "cancelling" } else { "running" },
        "card_id": card,
        "label": label,
        "project": project,
        "started_at": started,
        "elapsed_ms": db::now_ms() - started,
        "progress": { "note": note, "percent": percent },
    });
    if nodes {
        v["nodes"] = json!(node_values(job));
    }
    v
}

fn cancel_job(b: &Bridge, params: &Value) -> R {
    let job_id = p_i64(params, "job_id")? as JobId;
    let state: State<AppState> = b.app.state();
    if state.jobs.get(job_id).is_none() {
        let done = b.jobs.lock().unwrap().0.get(&job_id).map(|j| j.finished.is_some()).unwrap_or(false);
        if done {
            return Ok(json!({ "job_id": job_id, "cancel_requested": false, "note": "既に完了しています" }));
        }
        return Err(McpErr::new("unknown_job", "実行中のジョブが見つかりません"));
    }
    // UI の中断ボタンと同じ処理（子プロセスはツリーごと止まる・画像生成は /cancel）。
    break_flow(job_id, state);
    Ok(json!({ "job_id": job_id, "cancel_requested": true }))
}

fn list_jobs_m(b: &Bridge) -> R {
    let state: State<AppState> = b.app.state();
    let running: Vec<Value> = state
        .jobs
        .list()
        .into_iter()
        .map(|j| json!({ "job_id": j.id, "kind": j.kind, "label": j.label, "card_id": j.card, "started_at": j.started_at, "status": "running" }))
        .collect();
    let g = b.jobs.lock().unwrap();
    let recent: Vec<Value> = g
        .1
        .iter()
        .rev()
        .filter_map(|id| g.0.get(id))
        .filter_map(|mj| mj.finished.as_ref())
        .take(20)
        .map(|f| json!({ "job_id": f["job_id"], "label": f["label"], "card_id": f["card_id"], "status": f["status"], "finished_at": f["finished_at"] }))
        .collect();
    Ok(json!({ "running": running, "recent_mcp_jobs": recent }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_root_normalizes() {
        assert!(same_root(Path::new(r"C:\A\B"), Path::new(r"c:/a/b/")));
        assert!(!same_root(Path::new(r"C:\A\B"), Path::new(r"C:\A\C")));
    }

    #[test]
    fn redact_hides_secrets() {
        let mut v = json!({ "model": "m", "extra_params": { "api_key": "x", "n": 1 }, "token": "t" });
        redact(&mut v);
        assert_eq!(v["model"], "m");
        assert_eq!(v["extra_params"]["api_key"], "***");
        assert_eq!(v["extra_params"]["n"], 1);
        assert_eq!(v["token"], "***");
    }

    #[test]
    fn merge_settings_rejects_unknown_and_removes_null() {
        let mut spec = Map::new();
        spec.insert("prefix".into(), json!("a"));
        assert!(merge_settings(&mut spec, &json!({ "cmd": "rm" }), BUILTIN_KEYS).is_err());
        merge_settings(&mut spec, &json!({ "prefix": null, "lines": 3 }), BUILTIN_KEYS).ok().unwrap();
        assert!(!spec.contains_key("prefix"));
        assert_eq!(spec["lines"], 3);
    }

    #[test]
    fn card_type_classifies() {
        assert_eq!(card_type("filter", Some(r#"{"builtin":"ai","mode":"transform"}"#)), "ai");
        assert_eq!(card_type("filter", Some(r#"{"builtin":"ai","mode":"chat"}"#)), "ai_chat");
        assert_eq!(card_type("filter", Some(r#"{"builtin":"text_concat"}"#)), "builtin:text_concat");
        assert_eq!(card_type("filter", Some(r#"{"name":"gcc.exe","cmd":"x"}"#)), "program");
        assert_eq!(card_type("op", Some(r#"{"op":"out"}"#)), "op:out");
        assert!(mcp_editable("builtin:text_concat"));
        assert!(!mcp_editable("program"));
    }

    #[test]
    fn fingerprint_ignores_request_id_key_order_and_path_form() {
        let a = json!({ "project": r"C:\Prj\A", "text": "x", "x": 1, "y": 2, "request_id": "r1" });
        let b = json!({ "y": 2, "x": 1, "text": "x", "project": "c:/prj/a/", "request_id": "r2" });
        assert_eq!(request_fingerprint(&a), request_fingerprint(&b));
        // 対象プロジェクト・本文が違えば別の要求（同じ request_id の使い回しを見分ける）。
        let other_prj = json!({ "project": r"C:\Prj\B", "text": "x", "x": 1, "y": 2 });
        let other_text = json!({ "project": r"C:\Prj\A", "text": "y", "x": 1, "y": 2 });
        assert_ne!(request_fingerprint(&a), request_fingerprint(&other_prj));
        assert_ne!(request_fingerprint(&a), request_fingerprint(&other_text));
    }

    #[test]
    fn validate_ai_requires_model() {
        let mut spec = Map::new();
        spec.insert("builtin".into(), json!("ai"));
        assert!(validate_ai(&spec).is_err());
        spec.insert("model".into(), json!("x:latest"));
        assert!(validate_ai(&spec).is_ok());
        spec.insert("think".into(), json!("maybe"));
        assert!(validate_ai(&spec).is_err());
    }
}
