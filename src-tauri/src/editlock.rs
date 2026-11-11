//! v2.1 編集権と「表示のみ」（引き継ぎ書 §2-2）。
//!
//! 同じプロジェクトを複数の PC で開いたままにし、座った PC で作業する運用のための仕組み。
//! 他人との同時編集は想定しない（利用者は1人・PC は 2〜3 台）。
//!
//! - プロジェクトごとに **持ち主は1台**（`.texos/lock.json` に PC 名・生存信号を書く）。
//!   持ち主だけが DB へ書き、タイマ／ウォッチャを発火させる。
//! - 持ち主はしばらく操作が無いと `idle=true`（＝手放した）を書く。画面は「表示のみ」。
//!   それでも持ち主のままなので、戻ってきて操作すればそのまま続けられる。
//! - 持ち主でない PC は DB 接続を `PRAGMA query_only=ON` にし（書き込みを物理的に止める）、
//!   変更を数秒おきに検知して画面を読み直す。
//! - 持ち主でない PC は「表示のみ」。パンとズームはできる。編集するには画面の
//!   「編集権を取得」を押す（v2.1.1。v2.1.0 は触っただけで引き継いでいた）。
//!   持ち主が不在（ファイルが無い／生存信号切れ）のときだけ、開いた時点で自動で取る。
//! - 引き継がれた側は次の tick（5秒以内）で気づいて「表示のみ」になる。
//!
//! ロック順: `EditLock.inner` → `AppState.current`。逆順（current を持ったまま inner）は
//! 取らないこと。ファイル I/O は inner を持たずに行う（NAS が詰まっても UI を巻き込まない）。

use crate::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};

/// 持ち主が生存信号を書く間隔。
const BEAT_SECS: u64 = 15;
/// 生存信号がこれより古ければ「不在」と見なす（PC 間の時計ずれ込み）。
const STALE_MS: i64 = 60_000;
/// 操作が無いと手放すまでの既定時間（config.json の edit_idle_secs で変更）。
pub(crate) const DEFAULT_IDLE_SECS: u64 = 300;
/// tick 間隔。
const TICK_SECS: u64 = 5;

/// `.texos/lock.json` の中身。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct LockFile {
    pub host: String,
    /// アプリ起動ごとの識別子（同じ PC で2つ開いても区別できる）。
    pub session: String,
    /// true = 手放した（他の PC は確認なしで引き継いでよい）。
    pub idle: bool,
    /// 生存信号（unix ms）。
    pub beat: i64,
    /// 編集権を取った時刻（unix ms）。
    pub since: i64,
    /// 持ち主のプロセス ID（同じ PC で強制終了された後の残骸を見分ける）。
    #[serde(default)]
    pub pid: u32,
}

/// ロックファイルから見た自分の立場。
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum Claim {
    /// 誰も持っていない。
    Free,
    /// 自分が持っている。
    Mine,
    /// 他の PC が持っていたが、生存信号が切れている（閉じ損ね・スリープ）。
    OtherGone,
    /// 他の PC が持っていて動いているが、操作していない（手放している）。
    OtherIdle,
    /// 他の PC がまさに操作中。
    OtherActive,
}

pub(crate) fn judge(lock: Option<&LockFile>, session: &str, now_ms: i64) -> Claim {
    match lock {
        None => Claim::Free,
        Some(l) if l.session == session => Claim::Mine,
        Some(l) if (now_ms - l.beat).abs() > STALE_MS => Claim::OtherGone,
        Some(l) if l.idle => Claim::OtherIdle,
        Some(_) => Claim::OtherActive,
    }
}

pub(crate) fn lock_path(root: &Path) -> PathBuf {
    texos_dir(root).join("lock.json")
}

/// Ok(None) = ファイルが無い／壊れている（＝空き）。Err = 読めなかった（判断を保留する）。
pub(crate) fn read_lock(root: &Path) -> Result<Option<LockFile>, String> {
    match std::fs::read_to_string(lock_path(root)) {
        Ok(s) => Ok(serde_json::from_str(&s).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 一時ファイルへ書いてから置き換える（読む側が書きかけを掴まないように）。
pub(crate) fn write_lock(root: &Path, lf: &LockFile) -> Result<(), String> {
    let path = lock_path(root);
    let tmp = path.with_extension(format!("json.{}.tmp", &lf.session));
    let body = serde_json::to_string_pretty(lf).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &body).map_err(|e| e.to_string())?;
    if std::fs::rename(&tmp, &path).is_err() {
        // 置き換えられない環境では直接書く。
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&path, &body).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 同じ PC の、もう動いていないプロセスが残したロックは「無い」ものとして扱う
/// （強制終了のあとすぐ起動し直しても、自分自身に待たされない）。
fn drop_dead_local(lock: Option<LockFile>, host: &str) -> Option<LockFile> {
    match lock {
        Some(l) if l.host == host && l.pid != 0 && l.pid != std::process::id() && !pid_alive(l.pid) => None,
        other => other,
    }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let mut code = 0u32;
                let ok = GetExitCodeProcess(h, &mut code).is_ok();
                let _ = CloseHandle(h);
                !ok || code == STILL_ACTIVE
            }
            // 開けない理由が「存在しない」(ERROR_INVALID_PARAMETER) のときだけ死んでいると見る。
            Err(e) => e.code() != windows::Win32::Foundation::ERROR_INVALID_PARAMETER.to_hresult(),
        }
    }
}

#[cfg(not(windows))]
fn pid_alive(_pid: u32) -> bool {
    true
}

fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "この PC".to_string())
}

/// 画面へ渡す状態。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct EditStatus {
    /// "none"（プロジェクトなし）/"edit"（編集中）/"idle"（持ち主だが手放している）/
    /// "view"（持ち主でない＝表示のみ）
    pub mode: &'static str,
    /// 他の PC が持ち主のとき、その PC 名。
    pub holder: Option<String>,
    /// その PC がまさに操作中か（true のときだけ「引き継ぐ」を出す）。
    pub holder_active: bool,
    /// この PC の名前。
    pub me: String,
    /// この呼び出しで編集権を新しく取った（画面は最新を読み直す）。
    pub took: bool,
}

struct Inner {
    root: Option<PathBuf>,
    owner: bool,
    idle: bool,
    since: i64,
    last_input: Instant,
    last_beat: Instant,
    holder: Option<LockFile>,
    /// 表示のみのとき、変更検知に使う DB の指紋。
    sig: Option<DbSig>,
    idle_secs: u64,
}

pub struct EditLock {
    pub session: String,
    pub host: String,
    inner: Mutex<Inner>,
}

type DbSig = (i64, i64, u64, i64, u64);

impl EditLock {
    pub fn new(idle_secs: u64) -> Self {
        let host = host_name();
        let session = format!("{}-{}-{}", host, std::process::id(), db::now_ms());
        EditLock {
            session,
            host,
            inner: Mutex::new(Inner {
                root: None,
                owner: false,
                idle: false,
                since: 0,
                last_input: Instant::now(),
                last_beat: Instant::now(),
                holder: None,
                sig: None,
                idle_secs: idle_secs.max(30),
            }),
        }
    }

    /// いま開いているプロジェクトへ書いてよいか（プロジェクトなしは true）。
    pub fn can_write(&self) -> bool {
        let g = self.inner.lock().unwrap();
        g.root.is_none() || g.owner
    }

    pub fn idle_secs(&self) -> u64 {
        self.inner.lock().unwrap().idle_secs
    }

    pub fn set_idle_secs(&self, secs: u64) {
        self.inner.lock().unwrap().idle_secs = secs.max(30);
    }

    fn status_of(&self, g: &Inner, took: bool) -> EditStatus {
        let now = db::now_ms();
        let mode = if g.root.is_none() {
            "none"
        } else if !g.owner {
            "view"
        } else if g.idle {
            "idle"
        } else {
            "edit"
        };
        let other = if g.owner { None } else { g.holder.as_ref() };
        EditStatus {
            mode,
            holder: other.map(|l| l.host.clone()),
            holder_active: judge(other, &self.session, now) == Claim::OtherActive,
            me: self.host.clone(),
            took,
        }
    }

    fn lock_file(&self, idle: bool, since: i64) -> LockFile {
        LockFile {
            host: self.host.clone(),
            session: self.session.clone(),
            idle,
            beat: db::now_ms(),
            since,
            pid: std::process::id(),
        }
    }
}

/// 表示のみの PC が書けないと分かる文言。
pub(crate) fn ensure_editable(state: &AppState) -> Result<(), String> {
    if state.edit.can_write() {
        return Ok(());
    }
    let who = {
        let g = state.edit.inner.lock().unwrap();
        g.holder.as_ref().map(|l| l.host.clone())
    };
    Err(match who {
        Some(h) => format!(
            "この PC は「表示のみ」です（編集権は {h}）。画面を操作すると編集に切り替わります"
        ),
        None => "この PC は「表示のみ」です。画面を操作すると編集に切り替わります".to_string(),
    })
}

fn current_root(state: &AppState) -> Option<PathBuf> {
    state.current.lock().unwrap().as_ref().map(|p| p.root.clone())
}

/// 開いている接続を読み取り専用にする／戻す（root が一致するときだけ）。
fn set_query_only(state: &AppState, root: &Path, on: bool) {
    let guard = state.current.lock().unwrap();
    if let Some(p) = guard.as_ref() {
        if p.root == root {
            let _ = p
                .conn
                .execute_batch(if on { "PRAGMA query_only=ON" } else { "PRAGMA query_only=OFF" });
        }
    }
}

/// 変更検知用の指紋（data_version＋DB と WAL の更新時刻・大きさ）。
fn db_sig(state: &AppState, root: &Path) -> Option<DbSig> {
    let dv: i64 = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().filter(|p| p.root == root)?;
        p.conn.query_row("PRAGMA data_version", [], |r| r.get(0)).unwrap_or(0)
    };
    let stat = |name: &str| -> (i64, u64) {
        let path = texos_dir(root).join(name);
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        (file_mtime_ms(&path).unwrap_or(0), len)
    };
    let (dm, dl) = stat("texos.db");
    let (wm, wl) = stat("texos.db-wal");
    Some((dv, dm, dl, wm, wl))
}

/// 利用者がこの PC を操作した（またはプロジェクトを開いた）。編集権が取れるなら取る。
/// `force` = 他の PC が操作中でも引き継ぐ。
pub(crate) fn acquire(state: &AppState, force: bool) -> EditStatus {
    let ed = &state.edit;
    let Some(root) = current_root(state) else {
        let mut g = ed.inner.lock().unwrap();
        g.root = None;
        g.owner = false;
        g.holder = None;
        return ed.status_of(&g, false);
    };
    // inner を持たずに読む。
    let read = read_lock(&root);
    let now = db::now_ms();
    let (was_owner, was_idle, since, root_changed) = {
        let g = ed.inner.lock().unwrap();
        let same = g.root.as_deref() == Some(root.as_path());
        (same && g.owner, g.idle, g.since, !same)
    };
    let lock = match read {
        Ok(l) => drop_dead_local(l, &ed.host),
        Err(_) => {
            // 読めない＝判断できない。いまの立場を変えずに返す（新しく開いた所なら表示のみ）。
            let mut g = ed.inner.lock().unwrap();
            if root_changed {
                g.root = Some(root.clone());
                g.owner = false;
                g.holder = None;
                g.sig = None;
                drop(g);
                set_query_only(state, &root, true);
                g = ed.inner.lock().unwrap();
            }
            return ed.status_of(&g, false);
        }
    };
    let claim = judge(lock.as_ref(), &ed.session, now);
    let take = match claim {
        Claim::Mine => true,
        Claim::Free | Claim::OtherGone => true,
        // v2.1.1: 動いている別の PC が持っている間は、利用者が「取得」を押すまで取らない。
        Claim::OtherIdle | Claim::OtherActive => force,
    };
    if take {
        let keep = claim == Claim::Mine && was_owner;
        let since = if keep { since } else { now };
        // 書くのは「取り直し」「手放していた」「生存信号の間隔が来た」ときだけ。
        let need_write = !keep || was_idle || {
            let g = ed.inner.lock().unwrap();
            g.last_beat.elapsed() >= Duration::from_secs(BEAT_SECS)
        };
        if need_write && write_lock(&root, &ed.lock_file(false, since)).is_err() {
            // 書けない（読み取り専用の場所など）→ 表示のみ。
            let mut g = ed.inner.lock().unwrap();
            g.root = Some(root.clone());
            g.owner = false;
            g.holder = lock;
            drop(g);
            set_query_only(state, &root, true);
            let g = ed.inner.lock().unwrap();
            return ed.status_of(&g, false);
        }
        set_query_only(state, &root, false);
        let mut g = ed.inner.lock().unwrap();
        g.root = Some(root);
        g.owner = true;
        g.idle = false;
        g.since = since;
        g.last_input = Instant::now();
        if need_write {
            g.last_beat = Instant::now();
        }
        g.holder = None;
        g.sig = None;
        if !keep {
            eprintln!("[editlock] 編集権を取得（{claim:?}{}）", if force { "・引き継ぎ" } else { "" });
        }
        ed.status_of(&g, !keep)
    } else {
        set_query_only(state, &root, true);
        let sig = db_sig(state, &root);
        let mut g = ed.inner.lock().unwrap();
        let fresh = root_changed || g.owner;
        g.root = Some(root);
        g.owner = false;
        g.idle = false;
        g.holder = lock;
        if fresh || g.sig.is_none() {
            g.sig = sig;
        }
        ed.status_of(&g, false)
    }
}

/// 編集権を持っていれば手放してファイルを消す（プロジェクトを離れる・アプリを閉じる）。
pub(crate) fn release(state: &AppState) {
    let ed = &state.edit;
    let root = {
        let mut g = ed.inner.lock().unwrap();
        let r = if g.owner { g.root.clone() } else { None };
        g.owner = false;
        g.root = None;
        g.holder = None;
        g.sig = None;
        r
    };
    if let Some(root) = root {
        if let Ok(Some(l)) = read_lock(&root) {
            if l.session == ed.session {
                let _ = std::fs::remove_file(lock_path(&root));
            }
        }
    }
}

/// 開いているプロジェクトを差し替える唯一の入口。前のプロジェクトの編集権を手放し、
/// 新しいプロジェクトの編集権を（取れるなら）取る。
pub(crate) fn set_current_project(state: &AppState, project: Project) {
    release(state);
    *state.current.lock().unwrap() = Some(project);
    acquire(state, false);
}

fn emit_state(app: &tauri::AppHandle, st: &EditStatus) {
    let _ = app.emit("edit:state", st.clone());
}

/// 5秒ごと。持ち主なら「引き継がれていないか」を確かめて生存信号を書き、
/// 表示のみなら持ち主の様子と DB の変更を見る。
fn tick(app: &tauri::AppHandle) {
    let state: State<AppState> = app.state();
    let ed = &state.edit;
    let Some(root) = current_root(state.inner()) else {
        return;
    };
    let (tracked, owner, idle, since, idle_secs, last_input, last_beat) = {
        let g = ed.inner.lock().unwrap();
        (
            g.root.as_deref() == Some(root.as_path()),
            g.owner,
            g.idle,
            g.since,
            g.idle_secs,
            g.last_input,
            g.last_beat,
        )
    };
    if !tracked {
        // set_current_project を通らずに差し替わった場合の保険。
        let st = acquire(state.inner(), false);
        emit_state(app, &st);
        return;
    }
    let Ok(lock) = read_lock(&root) else {
        return; // 読めない間は立場を変えない。
    };
    if owner {
        if let Some(other) = lock.as_ref().filter(|l| l.session != ed.session) {
            // 引き継がれた → 表示のみへ。
            eprintln!("[editlock] {} に引き継がれた → 表示のみ", other.host);
            set_query_only(state.inner(), &root, true);
            let sig = db_sig(state.inner(), &root);
            let st = {
                let mut g = ed.inner.lock().unwrap();
                g.owner = false;
                g.idle = false;
                g.holder = Some(other.clone());
                g.sig = sig;
                ed.status_of(&g, false)
            };
            emit_state(app, &st);
            let _ = app.emit("edit:changed", ());
            return;
        }
        // 実行中のフローは「操作中」と見なす。
        let busy = state.jobs.any_bound_to(&root.to_string_lossy());
        let idle_now = !busy && last_input.elapsed() >= Duration::from_secs(idle_secs);
        let due = last_beat.elapsed() >= Duration::from_secs(BEAT_SECS);
        if idle_now != idle || due || lock.is_none() {
            let wrote = write_lock(&root, &ed.lock_file(idle_now, since)).is_ok();
            let st = {
                let mut g = ed.inner.lock().unwrap();
                // 書いている間に操作が入って idle が解けていたら、そちらを優先する。
                if g.owner && g.last_input == last_input {
                    g.idle = idle_now;
                }
                if busy {
                    g.last_input = Instant::now();
                }
                if wrote {
                    g.last_beat = Instant::now();
                }
                ed.status_of(&g, false)
            };
            if idle_now != idle {
                emit_state(app, &st);
            }
        }
    } else {
        let sig = db_sig(state.inner(), &root);
        let (st, state_changed, data_changed) = {
            let mut g = ed.inner.lock().unwrap();
            if g.owner {
                return; // この間に編集権を取った。
            }
            let before = ed.status_of(&g, false);
            g.holder = drop_dead_local(lock, &ed.host);
            let data_changed = g.sig.is_some() && sig.is_some() && g.sig != sig;
            if sig.is_some() {
                g.sig = sig;
            }
            let after = ed.status_of(&g, false);
            let changed = before != after;
            (after, changed, data_changed)
        };
        if state_changed {
            emit_state(app, &st);
        }
        if data_changed {
            let _ = app.emit("edit:changed", ());
        }
    }
}

pub(crate) fn start(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(TICK_SECS));
        tick(&app);
    });
}

// --- コマンド ----------------------------------------------------------------

/// いまの状態（プロジェクトを開いた直後などに画面が読む）。
#[tauri::command]
pub(crate) fn edit_status(state: State<AppState>) -> EditStatus {
    let g = state.edit.inner.lock().unwrap();
    state.edit.status_of(&g, false)
}

/// 利用者がこの PC を操作した。取れるなら編集権を取る（NAS を読むので async）。
#[tauri::command(async)]
pub(crate) fn edit_touch(state: State<AppState>) -> EditStatus {
    acquire(state.inner(), false)
}

/// 他の PC が操作中でも引き継ぐ（画面の「引き継ぐ」）。
#[tauri::command(async)]
pub(crate) fn edit_takeover(state: State<AppState>) -> EditStatus {
    acquire(state.inner(), true)
}

/// 操作が無いと手放すまでの時間（分）。
#[tauri::command]
pub(crate) fn edit_get_idle_minutes(state: State<AppState>) -> u64 {
    (state.edit.idle_secs() / 60).max(1)
}

#[tauri::command]
pub(crate) fn edit_set_idle_minutes(minutes: u64, state: State<AppState>) -> Result<u64, String> {
    let minutes = minutes.clamp(1, 240);
    state.edit.set_idle_secs(minutes * 60);
    let mut c = read_config(&state.app_data);
    c.edit_idle_secs = Some(minutes * 60);
    write_config(&state.app_data, &c);
    Ok(minutes)
}

// --- 表示位置・倍率は PC ごと ---------------------------------------------------
// プロジェクトの DB に置くと「見るだけ」で書き込みが起き、別の PC の表示位置も動く。
// app_data/viewports.json に {プロジェクトのパス: {x,y,scale}} で持つ。

fn viewports_path(app_data: &Path) -> PathBuf {
    app_data.join("viewports.json")
}

const VIEWPORTS_MAX: usize = 300;

pub(crate) fn viewport_get(app_data: &Path, root: &Path) -> Option<ViewTransform> {
    let s = std::fs::read_to_string(viewports_path(app_data)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    serde_json::from_value(v.get(root.to_string_lossy().as_ref())?.clone()).ok()
}

pub(crate) fn viewport_set(app_data: &Path, root: &Path, t: &ViewTransform) -> Result<(), String> {
    let path = viewports_path(app_data);
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let key = root.to_string_lossy().into_owned();
    map.remove(&key);
    // 増えすぎたら先頭から捨てる（表示位置が初期値に戻るだけ）。
    while map.len() >= VIEWPORTS_MAX {
        let Some(first) = map.keys().next().cloned() else { break };
        map.remove(&first);
    }
    map.insert(key, serde_json::to_value(t).map_err(|e| e.to_string())?);
    let body = serde_json::to_string(&map).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lf(session: &str, idle: bool, beat: i64) -> LockFile {
        LockFile { host: "PC-A".into(), session: session.into(), idle, beat, since: 0, pid: 0 }
    }

    #[test]
    fn judge_covers_the_four_cases() {
        let now = 1_000_000;
        assert_eq!(judge(None, "me", now), Claim::Free);
        assert_eq!(judge(Some(&lf("me", false, now)), "me", now), Claim::Mine);
        // 手放している → 確認なしで引き継げる
        assert_eq!(judge(Some(&lf("a", true, now)), "me", now), Claim::OtherIdle);
        // 生存信号が古い → 不在
        assert_eq!(judge(Some(&lf("a", false, now - STALE_MS - 1)), "me", now), Claim::OtherGone);
        assert_eq!(judge(Some(&lf("a", true, now - STALE_MS - 1)), "me", now), Claim::OtherGone);
        // 操作中
        assert_eq!(judge(Some(&lf("a", false, now - 5_000)), "me", now), Claim::OtherActive);
        // 相手の時計が少し進んでいても操作中と見る
        assert_eq!(judge(Some(&lf("a", false, now + 5_000)), "me", now), Claim::OtherActive);
    }

    fn temp_project(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("texos_editlock_{}_{}", tag, db::now_ms()));
        let root = base.join("proj");
        let app_data = base.join("app");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        (root, app_data)
    }

    fn state_for(root: &Path, app_data: &Path) -> AppState {
        let p = open_project_at(root).unwrap();
        AppState {
            current: Mutex::new(Some(p)),
            app_data: app_data.to_path_buf(),
            jobs: Jobs::new(),
            resources: ResourceLimits::new(),
            write_locks: Mutex::new(std::collections::HashMap::new()),
            t2i_server: Mutex::new(None),
            t23d_server: Mutex::new(None),
            edit: EditLock::new(DEFAULT_IDLE_SECS),
        }
    }

    fn can_insert(state: &AppState) -> bool {
        let g = state.current.lock().unwrap();
        db::meta_set(&g.as_ref().unwrap().conn, "editlock_test", "1").is_ok()
    }

    #[test]
    fn second_pc_is_view_only_until_first_goes_idle() {
        let (root, app_data) = temp_project("two");
        let a = state_for(&root, &app_data);
        let mut b = state_for(&root, &app_data);
        b.edit = EditLock { session: "other-session".into(), host: "PC-B".into(), ..EditLock::new(300) };

        // A が開く → 編集権を取る
        let st = acquire(&a, false);
        assert_eq!(st.mode, "edit");
        assert!(st.took);
        assert!(can_insert(&a));

        // B が開く → A が操作中なので表示のみ・書けない
        let st = acquire(&b, false);
        assert_eq!(st.mode, "view");
        assert_eq!(st.holder.as_deref(), Some(a.edit.host.as_str()));
        assert!(st.holder_active);
        assert!(!can_insert(&b));
        assert!(ensure_editable(&b).is_err());

        // A が手放す（idle を書く）→ B は触っただけでは取らない。「取得」（force）で取れる
        let mut l = read_lock(&root).unwrap().unwrap();
        l.idle = true;
        write_lock(&root, &l).unwrap();
        let st = acquire(&b, false);
        assert_eq!(st.mode, "view");
        assert!(!st.holder_active);
        assert!(!can_insert(&b));
        let st = acquire(&b, true);
        assert_eq!(st.mode, "edit");
        assert!(st.took);
        assert!(can_insert(&b));
        assert_eq!(read_lock(&root).unwrap().unwrap().session, "other-session");

        // A が戻って操作 → B は操作中なので A は表示のみ。引き継ぐ（force）なら取れる
        let st = acquire(&a, false);
        assert_eq!(st.mode, "view");
        assert!(!can_insert(&a));
        let st = acquire(&a, true);
        assert_eq!(st.mode, "edit");
        assert!(can_insert(&a));

        // 手放すとファイルが消える（自分の物のときだけ）
        release(&a);
        assert!(read_lock(&root).unwrap().is_none());
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn dead_process_on_this_pc_does_not_block() {
        let me = "THIS-PC";
        let mut l = lf("old", false, db::now_ms());
        l.host = me.into();
        l.pid = 0xFFFF_FFF0; // 存在しない PID
        assert!(drop_dead_local(Some(l.clone()), me).is_none());
        // 別の PC のロックは PID では判断しない
        assert!(drop_dead_local(Some(l.clone()), "OTHER-PC").is_some());
        // 動いているプロセス（自分の親でも何でもよいので自分自身以外…は用意しづらいので pid=0＝旧形式）
        l.pid = 0;
        assert!(drop_dead_local(Some(l), me).is_some());
    }

    #[test]
    fn release_leaves_someone_elses_lock() {
        let (root, app_data) = temp_project("rel");
        let a = state_for(&root, &app_data);
        acquire(&a, false);
        write_lock(&root, &lf("someone", false, db::now_ms())).unwrap();
        release(&a);
        assert_eq!(read_lock(&root).unwrap().unwrap().session, "someone");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn viewport_is_kept_per_project_outside_the_db() {
        let (root, app_data) = temp_project("vp");
        assert!(viewport_get(&app_data, &root).is_none());
        viewport_set(&app_data, &root, &ViewTransform { x: 10.0, y: -5.0, scale: 1.5 }).unwrap();
        let other = root.join("other");
        viewport_set(&app_data, &other, &ViewTransform { x: 1.0, y: 2.0, scale: 0.5 }).unwrap();
        let t = viewport_get(&app_data, &root).unwrap();
        assert_eq!((t.x, t.y, t.scale), (10.0, -5.0, 1.5));
        assert_eq!(viewport_get(&app_data, &other).unwrap().scale, 0.5);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
