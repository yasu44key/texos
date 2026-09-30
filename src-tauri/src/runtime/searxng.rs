#![allow(unused_imports)]
use crate::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{Manager, State};

// --- Web検索 P1: SearXNG 常駐デーモンの管理 ------------------------------------------------
// docs/Web検索_SearXNG計画.md §2。Docker を使わず、TexoS 管理の uv / CPython 3.13 / venv で
// SearXNG を直接動かす（t2i 常駐デーモンと同じ流儀＝中身が普通のフォルダで見える）。
// 上流ソースは改変しない。Windows で足りない分は shim/（TexoS 所有）を PYTHONPATH で差す。
//
// ディレクトリ構成（runtime/ 配下・python.rs と同居）:
//   runtime/searxng/src/<pin>/     … 固定コミットの tarball から searx/ と requirements.txt だけ展開
//   runtime/searxng/shim/          … pwd.py / sitecustomize.py（Windows 補完）
//   runtime/searxng/settings.yml   … 起動のたびに生成（ポートは毎回空きを選ぶ）
//   runtime/searxng/secret_key     … 導入時に一度だけ生成
//   runtime/searxng/server.log     … デーモンの stderr
//   runtime/envs/searxng-<pin12>/  … pin ごとの venv（更新時は並べて作ってから切り替える）

/// 検証済みの SearXNG コミット（上げるときはここだけ。実機で検索が通ったものに限る）。
pub(crate) const SEARXNG_PIN: &str = "a9d99003344ac3879ec45c56c661283482a92a39";
/// SearXNG 用の CPython（t2i の MANAGED_PYTHON_VERSION とは独立）。
pub(crate) const SEARXNG_PYTHON: &str = "3.13";
/// 最後に使ってからこの時間が過ぎたら止める（起動は2秒弱なので常駐させない）。
const SEARXNG_IDLE: Duration = Duration::from_secs(600);
/// 進捗イベント（"python-env"）の env 名。UI はこれで自分宛てを見分ける。
const PROGRESS_ENV: &str = "searxng";

pub(crate) fn searxng_root(app_data: &Path) -> PathBuf {
    runtime_root(app_data).join("searxng")
}
fn src_dir(app_data: &Path, pin: &str) -> PathBuf {
    searxng_root(app_data).join("src").join(pin)
}
fn shim_dir(app_data: &Path) -> PathBuf {
    searxng_root(app_data).join("shim")
}
fn settings_path(app_data: &Path) -> PathBuf {
    searxng_root(app_data).join("settings.yml")
}
fn secret_path(app_data: &Path) -> PathBuf {
    searxng_root(app_data).join("secret_key")
}
fn log_path(app_data: &Path) -> PathBuf {
    searxng_root(app_data).join("server.log")
}
fn env_name(pin: &str) -> String {
    format!("searxng-{}", &pin[..12.min(pin.len())])
}
/// 導入完了の印（venv の中に置く＝venv を消せば未導入に戻る）。
fn marker_path(app_data: &Path, pin: &str) -> PathBuf {
    env_dir(app_data, &env_name(pin)).join(".texos_searxng_ok")
}

/// 固定版が導入済みか（venv・ソース・完了印の3点）。
pub(crate) fn searxng_installed(app_data: &Path) -> bool {
    env_python(app_data, &env_name(SEARXNG_PIN)).is_file()
        && src_dir(app_data, SEARXNG_PIN).join("searx").join("webapp.py").is_file()
        && marker_path(app_data, SEARXNG_PIN).is_file()
}

// ---- Windows 補完（shim）---------------------------------------------------------------
// searx/valkeydb.py が Unix 専用の pwd を import する（valkey 接続失敗時のエラー文言用の1か所）。
const SHIM_PWD_PY: &str = r#""""TexoS shim: SearXNG の valkeydb.py はエラー文言のためだけに pwd を使う。"""
import getpass


class _PW:
    def __init__(self):
        self.pw_name = getpass.getuser()
        self.pw_uid = 0


def getpwuid(_uid):
    return _PW()
"#;
const SHIM_SITECUSTOMIZE_PY: &str = r#"# TexoS shim: Windows の os には getuid が無い。
import os

if not hasattr(os, "getuid"):
    os.getuid = lambda: 0
"#;

/// tarball 展開スクリプト（管理 Python の tarfile を使う＝Rust 側に tar 依存を足さない）。
/// ・pax ヘッダの comment（GitHub が入れるコミットハッシュ）が pin と一致するか検証
/// ・searx/ と requirements.txt だけ取り出す
/// ・Windows で無効な名前（`:` 等）は飛ばす（Linux サーバー用テンプレートにしか無い）
const EXTRACT_PY: &str = r#"import os, shutil, sys, tarfile
src, dst, pin = sys.argv[1], sys.argv[2], sys.argv[3]
BAD = set('<>:"|?*')
t = tarfile.open(src)
got = t.pax_headers.get("comment", "")
if got != pin:
    sys.stderr.write(f"commit mismatch: {got!r} != {pin!r}\n")
    sys.exit(2)
ms = []
for m in t.getmembers():
    parts = m.name.split("/", 1)
    if len(parts) < 2 or not parts[1]:
        continue
    rel = parts[1]
    if not (rel == "requirements.txt" or rel.startswith("searx/")):
        continue
    if any(ch in BAD for ch in rel):
        continue
    m.name = rel
    ms.append(m)
if os.path.exists(dst):
    shutil.rmtree(dst)
t.extractall(dst, members=ms, filter="data")
print(len(ms))
"#;

fn settings_yml(secret: &str, port: u16) -> String {
    // use_default_settings で上流の既定に重ねる。engines は名前で上書きされる。
    format!(
        "# TexoS が起動のたびに生成します（手で編集しても次の起動で上書きされます）。\n\
use_default_settings: true\n\
general:\n  debug: false\n  instance_name: \"TexoS SearXNG\"\n\
server:\n  secret_key: \"{secret}\"\n  bind_address: \"127.0.0.1\"\n  port: {port}\n  limiter: false\n  public_instance: false\n  image_proxy: false\n\
search:\n  formats: [html, json]\n\
valkey:\n  url: false\n\
engines:\n  - name: ahmia\n    disabled: true\n  - name: torch\n    disabled: true\n"
    )
}

fn hidden(c: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// tarball をダウンロード（進捗は 25〜45%）。
fn download_tarball(app: &tauri::AppHandle, url: &str, dest: &Path) -> Result<(), String> {
    use std::io::{Read, Write};
    let status = "SearXNG のソースをダウンロード中…";
    emit_env_progress(app, PROGRESS_ENV, status, 25);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| format!("HTTPクライアント生成に失敗: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("SearXNG のダウンロードを開始できません（オフライン？）: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("SearXNG のダウンロードに失敗 ({})", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let tmp = dest.with_extension("part");
    let mut out = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut chunk = [0u8; 65536];
    let mut got = 0u64;
    loop {
        let n = resp
            .read(&mut chunk)
            .map_err(|e| format!("SearXNG の読み取りに失敗: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&chunk[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        // GitHub の自動生成 tarball は Content-Length を返さないことが多い → 受信量を表示。
        let pct = if total > 0 { 25 + (got as f64 / total as f64 * 20.0) as u32 } else { 30 };
        emit_env_progress(app, PROGRESS_ENV, &format!("{status} {:.1}MB", got as f64 / 1e6), pct);
    }
    drop(out);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

/// 管理 Python を無窓で実行し、失敗時は stderr を含むエラーにする。
fn run_py(py: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Result<String, String> {
    let mut c = std::process::Command::new(py);
    c.args(args).env("PYTHONUTF8", "1").stdin(std::process::Stdio::null());
    for (k, v) in envs {
        c.env(k, v);
    }
    let o = hidden(&mut c)
        .output()
        .map_err(|e| format!("Python の起動に失敗: {e}"))?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn write_shims(app_data: &Path) -> Result<(), String> {
    let d = shim_dir(app_data);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join("pwd.py"), SHIM_PWD_PY).map_err(|e| e.to_string())?;
    std::fs::write(d.join("sitecustomize.py"), SHIM_SITECUSTOMIZE_PY).map_err(|e| e.to_string())
}

/// 現 pin と「その直前に使っていた1つ」以外のソースと venv を消す
/// （更新後も1世代は戻せるように残す＝水泡に帰さない）。
fn prune_old(app_data: &Path) {
    let src_root = searxng_root(app_data).join("src");
    let Ok(rd) = std::fs::read_dir(&src_root) else { return };
    let mut olds: Vec<(std::time::SystemTime, String)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name == SEARXNG_PIN || name.ends_with(".tmp") {
                return None;
            }
            let t = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((t, name))
        })
        .collect();
    olds.sort();
    olds.reverse();
    for (_, pin) in olds.into_iter().skip(1) {
        let _ = std::fs::remove_dir_all(src_root.join(&pin));
        let _ = std::fs::remove_dir_all(env_dir(app_data, &env_name(&pin)));
    }
}

/// SearXNG を導入する: uv → CPython 3.13 → venv → ソース（pin 検証付き）→ 依存 → shim。
/// 進捗は "python-env"（env="searxng"）。完了印を最後に書く＝途中失敗は未導入のまま。
pub(crate) fn ensure_searxng(app: &tauri::AppHandle, app_data: &Path) -> Result<(), String> {
    let env = env_name(SEARXNG_PIN);
    let root = searxng_root(app_data);
    std::fs::create_dir_all(root.join("src")).map_err(|e| e.to_string())?;

    ensure_uv(app, app_data, PROGRESS_ENV)?;
    run_uv(
        app,
        app_data,
        PROGRESS_ENV,
        // uv 0.8 は既定で ~/.local/bin に python3.x.exe を置き、Windows レジストリにも登録する
        // （＝TexoS の外を触る。既存の非 uv 製 exe があると失敗もする・実機で確認）。両方止める。
        &["python", "install", SEARXNG_PYTHON, "--no-bin", "--no-registry"],
        &format!("Python {SEARXNG_PYTHON} を取得中…"),
        12,
    )?;
    // 完了印の無い venv は途中で止まったもの → 作り直す。
    let edir = env_dir(app_data, &env);
    if edir.exists() && !marker_path(app_data, SEARXNG_PIN).is_file() {
        std::fs::remove_dir_all(&edir)
            .map_err(|e| format!("作りかけの環境を削除できません（{}）: {e}", edir.display()))?;
    }
    if !edir.exists() {
        std::fs::create_dir_all(edir.parent().unwrap()).map_err(|e| e.to_string())?;
        let edir_s = edir.to_string_lossy().to_string();
        run_uv(
            app,
            app_data,
            PROGRESS_ENV,
            &["venv", &edir_s, "--python", SEARXNG_PYTHON],
            "仮想環境を作成中…",
            20,
        )?;
    }
    let py = env_python(app_data, &env);

    // ソース: 固定コミットの tarball（git clone は Windows で無効なファイル名のため失敗する）。
    let src = src_dir(app_data, SEARXNG_PIN);
    if !src.join("searx").join("webapp.py").is_file() {
        let tgz = root.join(format!("{SEARXNG_PIN}.tar.gz"));
        let url = format!("https://codeload.github.com/searxng/searxng/tar.gz/{SEARXNG_PIN}");
        download_tarball(app, &url, &tgz)?;
        emit_env_progress(app, PROGRESS_ENV, "ソースを展開・検証中…", 48);
        let script = root.join("extract.py");
        std::fs::write(&script, EXTRACT_PY).map_err(|e| e.to_string())?;
        let tmp = root.join("src").join(format!("{SEARXNG_PIN}.tmp"));
        let r = run_py(
            &py,
            &[
                &script.to_string_lossy(),
                &tgz.to_string_lossy(),
                &tmp.to_string_lossy(),
                SEARXNG_PIN,
            ],
            &[],
        );
        let _ = std::fs::remove_file(&tgz);
        r.map_err(|e| format!("SearXNG のソース展開に失敗しました: {e}"))?;
        if src.exists() {
            let _ = std::fs::remove_dir_all(&src);
        }
        std::fs::rename(&tmp, &src).map_err(|e| e.to_string())?;
    }

    // 依存（Windows の Python はタイムゾーン DB を持たないので tzdata を足す）。
    let py_s = py.to_string_lossy().to_string();
    let req = src.join("requirements.txt").to_string_lossy().to_string();
    run_uv(
        app,
        app_data,
        PROGRESS_ENV,
        &["pip", "install", "--python", &py_s, "-r", &req, "tzdata"],
        "検索エンジンの部品をインストール中…",
        55,
    )?;

    emit_env_progress(app, PROGRESS_ENV, "設定を準備中…", 92);
    write_shims(app_data)?;
    if !secret_path(app_data).is_file() {
        let s = run_py(&py, &["-c", "import secrets;print(secrets.token_hex(32))"], &[])?;
        std::fs::write(secret_path(app_data), s).map_err(|e| e.to_string())?;
    }

    emit_env_progress(app, PROGRESS_ENV, "導入内容を検証中…", 95);
    let shim = shim_dir(app_data);
    run_py(
        &py,
        &["-c", "import flask, lxml, curl_cffi, msgspec, babel, yaml, tzdata, pwd"],
        &[("PYTHONPATH", shim.as_path())],
    )
    .map_err(|e| format!("導入しましたが部品の読み込み検証に失敗しました: {e}"))?;

    std::fs::write(marker_path(app_data, SEARXNG_PIN), SEARXNG_PIN).map_err(|e| e.to_string())?;
    prune_old(app_data);
    emit_env_progress(app, PROGRESS_ENV, "完了", 100);
    Ok(())
}

// ---- デーモン ---------------------------------------------------------------------------

struct Daemon {
    child: std::process::Child,
    port: u16,
    last_used: Instant,
}

static DAEMON: Mutex<Option<Daemon>> = Mutex::new(None);
static IDLE_WATCHER: OnceLock<()> = OnceLock::new();
/// app_data（setup で一度だけ設定）。フロー実行（execute_node）は AppState を持たないので、
/// web_search はここから SearXNG の置き場所を引く。
static APP_DATA: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn searxng_init(app_data: &Path) {
    let _ = APP_DATA.set(app_data.to_path_buf());
}
pub(crate) fn searxng_app_data() -> Result<&'static Path, String> {
    APP_DATA
        .get()
        .map(|p| p.as_path())
        .ok_or_else(|| "Web検索エンジンの置き場所が未設定です（TexoS の起動処理が済んでいません）".into())
}

/// デーモン宛ての HTTP クライアント（localhost 固定・システムプロキシを通さない）。
fn local_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())
}

/// tokio 文脈外の素のスレッドで blocking HTTP を実行する（ai.rs と同じ落とし穴対策）。
fn off_runtime<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| s.spawn(f).join().expect("HTTP スレッドが異常終了"))
}

fn healthz(port: u16) -> bool {
    off_runtime(|| {
        local_client(Duration::from_millis(800))
            .and_then(|c| {
                c.get(format!("http://127.0.0.1:{port}/healthz"))
                    .send()
                    .map_err(|e| e.to_string())
            })
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    })
}

/// 空きポートを OS に選ばせる（固定しない: 8888 は Windows の予約範囲に入っていた実例あり）。
fn pick_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("空きポートを確保できません: {e}"))?;
    l.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

/// 子プロセスを「TexoS が終われば一緒に終わる」Job に入れる（異常終了時も OS が片付ける）。
/// venv の python.exe は中継で、実体は孫。中継自身が「中継が死ねば子も死ぬ」Job を持つので、
/// 中継をこの Job に入れれば木ごと片付く（計画書 §5）。
#[cfg(windows)]
fn bind_to_texos_lifetime(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    // Job のハンドルはプロセス終了まで閉じない（閉じた瞬間に中の木が終了するため）。
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let Ok(h) = CreateJobObjectW(None, PCWSTR::null()) else { return 0 };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            h,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok.is_err() {
            return 0;
        }
        h.0 as usize
    });
    if job != 0 {
        unsafe {
            let _ = AssignProcessToJobObject(
                HANDLE(job as *mut std::ffi::c_void),
                HANDLE(child.as_raw_handle() as *mut std::ffi::c_void),
            );
        }
    }
}
#[cfg(not(windows))]
fn bind_to_texos_lifetime(_child: &std::process::Child) {}

fn log_tail(app_data: &Path, n: usize) -> String {
    let s = std::fs::read_to_string(log_path(app_data)).unwrap_or_default();
    let lines: Vec<&str> = s
        .lines()
        .filter(|l| !l.contains("not a git repository")) // tarball 展開なので毎回出る無害な行
        .collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn kill_daemon(g: &mut Option<Daemon>) {
    if let Some(mut d) = g.take() {
        let _ = d.child.kill();
        let _ = d.child.wait();
    }
}

fn spawn_daemon(app_data: &Path) -> Result<Daemon, String> {
    let py = env_python(app_data, &env_name(SEARXNG_PIN));
    let secret = std::fs::read_to_string(secret_path(app_data))
        .map_err(|_| "SearXNG の設定（secret_key）がありません。導入をやり直してください")?;
    write_shims(app_data)?;
    let port = pick_port()?;
    std::fs::write(settings_path(app_data), settings_yml(secret.trim(), port))
        .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log_path(app_data)).map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(&py);
    c.args(["-m", "searx.webapp"])
        .current_dir(src_dir(app_data, SEARXNG_PIN))
        .env("PYTHONPATH", shim_dir(app_data))
        .env("SEARXNG_SETTINGS_PATH", settings_path(app_data))
        .env("PYTHONUTF8", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log));
    let child = hidden(&mut c)
        .spawn()
        .map_err(|e| format!("SearXNG の起動に失敗: {e}"))?;
    bind_to_texos_lifetime(&child);
    Ok(Daemon { child, port, last_used: Instant::now() })
}

/// 稼働中ならそのポートを返し、止まっていれば起動して healthz を待つ（遅延起動）。
/// 呼ぶたびに「最後に使った時刻」を更新する（アイドル停止の基準）。
pub(crate) fn searxng_ensure_running(app_data: &Path) -> Result<u16, String> {
    if !searxng_installed(app_data) {
        return Err("Web検索エンジン（SearXNG）が未導入です。「Web ...」メニューの「Web検索エンジンの管理」から導入してください".into());
    }
    let mut g = DAEMON.lock().unwrap();
    if let Some(d) = g.as_mut() {
        if matches!(d.child.try_wait(), Ok(None)) && healthz(d.port) {
            d.last_used = Instant::now();
            return Ok(d.port);
        }
    }
    kill_daemon(&mut g);
    // ポート取り合い等で即死したときのために1回だけ選び直して再起動する。
    let mut last_err = String::new();
    for _attempt in 0..2 {
        let mut d = spawn_daemon(app_data)?;
        let t0 = Instant::now();
        last_err.clear();
        while t0.elapsed() < Duration::from_secs(30) {
            if let Ok(Some(st)) = d.child.try_wait() {
                last_err = format!("SearXNG が起動直後に終了しました（{st}）\n{}", log_tail(app_data, 8));
                break;
            }
            if healthz(d.port) {
                let port = d.port;
                *g = Some(d);
                start_idle_watcher();
                return Ok(port);
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        if last_err.is_empty() {
            last_err = format!("SearXNG が30秒以内に応答しませんでした\n{}", log_tail(app_data, 8));
        }
        let mut tmp = Some(d);
        kill_daemon(&mut tmp);
    }
    Err(last_err)
}

fn start_idle_watcher() {
    IDLE_WATCHER.get_or_init(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(30));
            let mut g = DAEMON.lock().unwrap();
            if g.as_ref().is_some_and(|d| d.last_used.elapsed() > SEARXNG_IDLE) {
                kill_daemon(&mut g);
            }
        });
    });
}

pub(crate) fn searxng_stop_daemon() {
    kill_daemon(&mut DAEMON.lock().unwrap());
}

/// SearXNG の JSON 検索を1回投げる（P2 の web_search もこれを使う）。
/// params は q 以外の追加パラメータ（engines / categories / language / time_range 等）。
pub(crate) fn searxng_query(
    app_data: &Path,
    q: &str,
    params: &[(&str, String)],
) -> Result<serde_json::Value, String> {
    let port = searxng_ensure_running(app_data)?;
    let mut pairs: Vec<(&str, String)> = vec![("q", q.to_string()), ("format", "json".into())];
    pairs.extend(params.iter().cloned());
    let r = off_runtime(|| -> Result<serde_json::Value, String> {
        let resp = local_client(Duration::from_secs(20))?
            .get(format!("http://127.0.0.1:{port}/search"))
            .query(&pairs)
            .send()
            .map_err(|e| format!("SearXNG への問い合わせに失敗: {e}"))?;
        match resp.status().as_u16() {
            200 => resp.json().map_err(|e| format!("SearXNG の応答を読めません: {e}")),
            403 => Err("SearXNG が JSON 出力を拒否しました（設定の search.formats）。TexoS を再起動してください".into()),
            s => Err(format!("SearXNG がエラーを返しました（HTTP {s}）")),
        }
    });
    if let Some(d) = DAEMON.lock().unwrap().as_mut() {
        d.last_used = Instant::now();
    }
    r
}

/// unresponsive_engines（[[名前, 理由], ...]）を「名前: 理由」の列にする。
pub(crate) fn unresponsive_list(v: &serde_json::Value) -> Vec<String> {
    v.get("unresponsive_engines")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let e = e.as_array()?;
                    let name = e.first()?.as_str()?;
                    let why = e.get(1).and_then(|x| x.as_str()).unwrap_or("");
                    Some(if why.is_empty() { name.to_string() } else { format!("{name}: {why}") })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- Tauri コマンド --------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct SearxngStatus {
    /// 固定版が導入済みか
    installed: bool,
    /// 固定コミット（短縮）
    pin: String,
    /// デーモンが応答するか
    running: bool,
    /// 稼働中のポート（停止中は 0）
    port: u16,
    python: String,
    /// 置き場所（中身が見える＝ユーザーが開けるように示す）
    root: String,
}

#[tauri::command(async)]
pub(crate) fn searxng_status(state: State<AppState>) -> SearxngStatus {
    let app_data = state.app_data.clone();
    let (running, port) = {
        let mut g = DAEMON.lock().unwrap();
        match g.as_mut() {
            Some(d) => {
                let alive = matches!(d.child.try_wait(), Ok(None)) && healthz(d.port);
                if alive { (true, d.port) } else { (false, 0) }
            }
            None => (false, 0),
        }
    };
    SearxngStatus {
        installed: searxng_installed(&app_data),
        pin: SEARXNG_PIN[..12].to_string(),
        running,
        port,
        python: SEARXNG_PYTHON.to_string(),
        root: searxng_root(&app_data).to_string_lossy().to_string(),
    }
}

/// 導入（force=true は現 pin の venv とソースを消してから入れ直す）。
#[tauri::command(async)]
pub(crate) fn searxng_install(
    window: tauri::WebviewWindow,
    state: State<AppState>,
    force: Option<bool>,
) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let app = window.app_handle().clone();
    searxng_stop_daemon();
    if force.unwrap_or(false) {
        let _ = std::fs::remove_dir_all(env_dir(&app_data, &env_name(SEARXNG_PIN)));
        let _ = std::fs::remove_dir_all(src_dir(&app_data, SEARXNG_PIN));
    }
    run_setup_job(state.inner(), &app, "Web検索エンジン（SearXNG）の導入", |_job| {
        std::thread::scope(|s| {
            s.spawn(|| ensure_searxng(&app, &app_data))
                .join()
                .map_err(|_| "導入スレッドが異常終了しました".to_string())?
        })
    })
}

#[derive(Serialize)]
pub(crate) struct SearxngTest {
    port: u16,
    results: usize,
    /// 応答しなかった検索元と理由
    unresponsive: Vec<String>,
    /// 先頭数件のタイトル（動作の目視確認用）
    titles: Vec<String>,
}

/// 起動して試しに1回検索する（自己診断）。
#[tauri::command(async)]
pub(crate) fn searxng_test(state: State<AppState>) -> Result<SearxngTest, String> {
    let app_data = state.app_data.clone();
    let port = searxng_ensure_running(&app_data)?;
    let v = searxng_query(&app_data, "TexoS", &[("language", "ja".into())])?;
    let results = v.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default();
    Ok(SearxngTest {
        port,
        results: results.len(),
        unresponsive: unresponsive_list(&v),
        titles: results
            .iter()
            .take(3)
            .filter_map(|r| r.get("title").and_then(|t| t.as_str()).map(String::from))
            .collect(),
    })
}

#[tauri::command]
pub(crate) fn searxng_stop() {
    searxng_stop_daemon();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_has_required_keys() {
        let s = settings_yml("abc", 12345);
        assert!(s.contains("bind_address: \"127.0.0.1\""));
        assert!(s.contains("port: 12345"));
        assert!(s.contains("limiter: false"));
        assert!(s.contains("formats: [html, json]"));
        assert!(s.contains("secret_key: \"abc\""));
    }

    #[test]
    fn env_name_is_short_pin() {
        assert_eq!(env_name(SEARXNG_PIN), "searxng-a9d99003344a");
    }

    #[test]
    fn unresponsive_parses_pairs() {
        let v = serde_json::json!({"unresponsive_engines": [["brave", "too many requests"], ["x", ""]]});
        assert_eq!(unresponsive_list(&v), vec!["brave: too many requests", "x"]);
    }

    #[test]
    fn pick_port_is_nonzero() {
        assert!(pick_port().unwrap() > 0);
    }

    /// 実機: 導入済みの app_data（TEXOS_TEST_APPDATA）で 起動→検索→再利用→停止 を通す。
    /// cargo test --lib searxng_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn searxng_live() {
        let ad = PathBuf::from(std::env::var("TEXOS_TEST_APPDATA").expect("TEXOS_TEST_APPDATA"));
        assert!(searxng_installed(&ad), "導入済みの app_data を指定すること");
        let t0 = Instant::now();
        let port = searxng_ensure_running(&ad).expect("起動");
        println!("起動 {:?} port={port}", t0.elapsed());
        // 稼働中は同じポートを使い回す（再起動しない）。
        assert_eq!(searxng_ensure_running(&ad).unwrap(), port);
        let v = searxng_query(
            &ad,
            "最新 ガジェット",
            &[("language", "ja".into()), ("categories", "news".into()), ("time_range", "week".into())],
        )
        .expect("検索");
        let n = v["results"].as_array().map(|a| a.len()).unwrap_or(0);
        println!("news/week 結果 {n} 件 unresponsive={:?}", unresponsive_list(&v));
        assert!(n > 0);
        searxng_stop_daemon();
        assert!(!healthz(port), "停止後は応答しない");
    }

    /// 実機: 起動したまま待つ。外からこのテストプロセスを強制終了し、SearXNG の木が
    /// Job Object で一緒に消えることを確かめる（異常終了時の後始末）。
    #[test]
    #[ignore]
    fn searxng_live_hold() {
        let ad = PathBuf::from(std::env::var("TEXOS_TEST_APPDATA").expect("TEXOS_TEST_APPDATA"));
        let port = searxng_ensure_running(&ad).expect("起動");
        println!("READY port={port}");
        std::thread::sleep(Duration::from_secs(120));
    }
}
