#![allow(unused_imports)]
use crate::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

// --- v2.0 t23d（builtin:"t23d"）: Hunyuan3D-2 エンジンの常駐デーモン管理 -----------------
// t2i（runtime/t2i.rs）のミラー。ただし Python は system/managed ではなく **Hunyuan3D-2
// WinPortable の python_standalone** を使う＝エンジン一式がモダリティ別 venv の役割を果たす
// （V2.0_t23d設計.md §1）。ここの汎用ヘルパ（gen_http_get 等）は t2v/t2a を足すときに
// run_gen(kind) へ共通化する予定の種（生成カード群企画 §3・論点2）。

/// デーモン本体を実行ファイルへ埋め込む（src-tauri/src → ../../python）。
pub(crate) const T23D_SERVER_PY: &str = include_str!("../../../python/t23d_server.py");

/// 期待するデーモンの版（t23d_server.py の SERVER_VERSION と一致させること）。
pub(crate) const T23D_SERVER_VERSION: &str = "3";

/// 既知モデル（name, 表示ラベル）。t23d_server.py の MODELS と一致させること。
/// ライセンス: Tencent Hunyuan 3D 2.0 Community License（商用可・EU/英国/韓国は使用不可・
/// 生成物は自由）。2mini / 2mv は実測後に追加（V2.0_t23d設計.md §2）。
pub(crate) const T23D_KNOWN_MODELS: &[(&str, &str)] = &[
    ("hunyuan3d-2", "Hunyuan3D-2 標準（shape 1.1B・VRAM 6GB〜）"),
    ("hunyuan3d-2-turbo", "Hunyuan3D-2 Turbo（蒸留版・高速）"),
];

pub(crate) fn t23d_root(app_data: &Path) -> PathBuf {
    app_data.join("t23d")
}
pub(crate) fn t23d_server_script(app_data: &Path) -> PathBuf {
    t23d_root(app_data).join("t23d_server.py")
}

/// エンジン解決の結果。engine = Hunyuan3D-2 リポジトリ（hy3dgen がある場所）、
/// python = ポータブルの python_standalone、hf_cache = 同梱モデルキャッシュ。
pub(crate) struct T23dEngine {
    pub engine: PathBuf,
    pub python: PathBuf,
    pub hf_cache: Option<PathBuf>,
}

const T23D_ENGINE_HINT: &str = "設定（3D生成カード作成時）で Hunyuan3D-2 WinPortable の場所を指定してください（例: F:\\Hunyuan3D2_WinPortable\\Hunyuan3D2_WinPortable）";

/// config.t23d_engine（絶対パス）から実行構成を解決する。WinPortable のルート
/// （Hunyuan3D-2/ と python_standalone/ を含む）でも Hunyuan3D-2 直指しでも受け付ける。
/// ⚠️ 絶対パス設定＝マルチプラットフォーム宿題「パスのトークン化」の対象（設計 §1）。
pub(crate) fn t23d_resolve_engine(app_data: &Path) -> Result<T23dEngine, String> {
    let raw = read_config(app_data)
        .t23d_engine
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("3D生成エンジンが未設定です。{T23D_ENGINE_HINT}"))?;
    let p = PathBuf::from(&raw);
    let engine = if p.join("hy3dgen").is_dir() {
        p.clone()
    } else if p.join("Hunyuan3D-2").join("hy3dgen").is_dir() {
        p.join("Hunyuan3D-2")
    } else {
        return Err(format!(
            "指定の場所に Hunyuan3D-2（hy3dgen フォルダ）が見つかりません: {raw}。{T23D_ENGINE_HINT}"
        ));
    };
    let root = engine
        .parent()
        .ok_or_else(|| format!("エンジンパスが不正です: {raw}"))?
        .to_path_buf();
    let python = root.join("python_standalone").join("python.exe");
    if !python.is_file() {
        return Err(format!(
            "ポータブルの Python が見つかりません（{} を想定）。WinPortable 版の構成（python_standalone 同梱）が必要です",
            python.to_string_lossy()
        ));
    }
    let hf_cache = Some(root.join("HuggingFaceHub")).filter(|d| d.is_dir());
    Ok(T23dEngine { engine, python, hf_cache })
}

/// 取得済み（ロード可能）モデル名をデーモン非依存で列挙（t23d_server.py の
/// resolve_model_dir と同判定: snapshots/*/<subfolder>/config.yaml＋重みファイル）。
pub(crate) fn t23d_list_models(eng: &T23dEngine) -> Vec<String> {
    // (name, repo, subfolder)。t23d_server.py の MODELS と一致させること。
    const SPECS: &[(&str, &str, &str)] = &[
        ("hunyuan3d-2", "tencent/Hunyuan3D-2", "hunyuan3d-dit-v2-0"),
        ("hunyuan3d-2-turbo", "tencent/Hunyuan3D-2", "hunyuan3d-dit-v2-0-turbo"),
    ];
    const CKPTS: &[&str] = &[
        "model.fp16.safetensors",
        "model.safetensors",
        "model.fp16.ckpt",
        "model.ckpt",
    ];
    let Some(cache) = eng.hf_cache.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, repo, sub) in SPECS {
        let repo_dir = cache.join(format!("models--{}", repo.replace('/', "--")));
        let snaps = repo_dir.join("snapshots");
        let found = std::fs::read_dir(&snaps)
            .map(|rd| {
                rd.flatten().any(|e| {
                    let d = e.path().join(sub);
                    d.join("config.yaml").is_file()
                        && CKPTS.iter().any(|c| d.join(c).is_file())
                })
            })
            .unwrap_or(false);
        if found {
            out.push(name.to_string());
        }
    }
    out
}

/// 生成デーモンへ GET（tokio 文脈外スレッドで blocking 実行）。ポート引数版＝
/// t2v/t2a を足すときの run_gen 共通部の種（t2i_http_get はそのまま温存）。
pub(crate) fn gen_http_get(port: u16, path: &str, timeout_ms: u64) -> Result<serde_json::Value, String> {
    let url = format!("http://127.0.0.1:{}/{}", port, path.trim_start_matches('/'));
    std::thread::scope(|s| {
        s.spawn(|| {
            let c = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_millis(timeout_ms))
                .build()
                .map_err(|e| e.to_string())?;
            c.get(&url)
                .send()
                .map_err(|e| e.to_string())?
                .json::<serde_json::Value>()
                .map_err(|e| e.to_string())
        })
        .join()
        .map_err(|_| "HTTPスレッドが異常終了".to_string())?
    })
}

pub(crate) fn t23d_running() -> bool {
    gen_http_get(flow::T23D_PORT, "health", 800).is_ok()
}

/// 稼働中デーモンが報告する版（取得できなければ空）。古い版の検出に使う。
pub(crate) fn t23d_daemon_version() -> String {
    gen_http_get(flow::T23D_PORT, "health", 1000)
        .ok()
        .and_then(|v| v["version"].as_str().map(String::from))
        .unwrap_or_default()
}

/// T23D_PORT を Listen しているプロセスを止める（孤児デーモン・古いデーモンの掃除）。
#[cfg(windows)]
pub(crate) fn t23d_kill_on_port() {
    use std::os::windows::process::CommandExt;
    let ps = format!(
        "Get-NetTCPConnection -LocalPort {} -State Listen -ErrorAction SilentlyContinue | \
         ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }}",
        flow::T23D_PORT
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .creation_flags(0x0800_0000)
        .status();
}
#[cfg(not(windows))]
pub(crate) fn t23d_kill_on_port() {}

/// デーモンを起動して health が立つまで待つ（t2i_ensure_server のミラー）。
/// 版が違えば止めて最新スクリプトで起動し直す（再ビルド反映）。
pub(crate) fn t23d_ensure_server(app_data: &Path, state: &AppState) -> Result<(), String> {
    if t23d_running() {
        if t23d_daemon_version() == T23D_SERVER_VERSION {
            return Ok(());
        }
        // 古い版 → 止めて最新スクリプトで起動し直す。
        {
            let mut g = state.t23d_server.lock().unwrap();
            if let Some(mut old) = g.take() {
                let _ = old.kill();
                let _ = old.wait();
            }
        }
        t23d_kill_on_port();
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
    let eng = t23d_resolve_engine(app_data)?;
    let root = t23d_root(app_data);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let script = t23d_server_script(app_data);
    std::fs::write(&script, T23D_SERVER_PY)
        .map_err(|e| format!("サーバースクリプトの書き出しに失敗: {e}"))?;

    let log = std::fs::File::create(root.join("server.log")).ok();
    let mut c = std::process::Command::new(&eng.python);
    // -s: ユーザー site-packages を見ない（ポータブルの bat と同じ・環境の汚染防止）。
    c.arg("-s")
        .arg(&script)
        .args(["--port", &flow::T23D_PORT.to_string()])
        .args(["--engine", &eng.engine.to_string_lossy()]);
    // モデルはポータブル同梱の HuggingFaceHub から読む（bat の HF_HUB_CACHE と同じ）。
    if let Some(cache) = &eng.hf_cache {
        c.env("HF_HUB_CACHE", cache);
    }
    if let Some(f) = log {
        c.stderr(std::process::Stdio::from(f));
    }
    c.stdout(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = c
        .spawn()
        .map_err(|e| format!("3D生成サーバーの起動に失敗: {e}"))?;
    {
        let mut g = state.t23d_server.lock().unwrap();
        if let Some(mut old) = g.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        *g = Some(child);
    }
    // health を最大 ~15 秒待つ（torch は遅延 import なので起動自体は軽い）。
    for _ in 0..30 {
        if t23d_running() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err("3D生成サーバーが起動しませんでした（t23d/server.log を確認してください）".into())
}

#[derive(Serialize, Clone)]
pub(crate) struct T23dKnownModel {
    name: String,
    label: String,
}

#[derive(Serialize, Clone)]
pub(crate) struct T23dStatus {
    /// エンジンパスが設定済みか
    engine_set: bool,
    /// エンジン一式（hy3dgen＋python_standalone）が解決できたか
    engine_ok: bool,
    /// 設定中のエンジンパス（表示用・未設定は空）
    engine_path: String,
    /// engine_ok=false のときの理由（設定案内）
    engine_error: String,
    /// デーモンが応答するか（=すぐ生成できる）
    server_running: bool,
    /// CUDA(GPU) が使えるか（デーモン稼働時のみ判明）
    gpu: bool,
    /// 取得済みモデル名（デーモン非依存のキャッシュ走査）
    models: Vec<String>,
    /// 既知モデル
    known: Vec<T23dKnownModel>,
    /// 現在選択中のモデル（既定 hunyuan3d-2）
    current_model: String,
}

#[tauri::command(async)]
pub(crate) fn t23d_status(state: State<AppState>) -> T23dStatus {
    let app_data = state.app_data.clone();
    let cfg = read_config(&app_data);
    let engine_path = cfg.t23d_engine.clone().unwrap_or_default();
    let (engine_ok, engine_error, models) = match t23d_resolve_engine(&app_data) {
        Ok(eng) => (true, String::new(), t23d_list_models(&eng)),
        Err(e) => (false, e, Vec::new()),
    };
    let running = t23d_running();
    let gpu = running
        && gen_http_get(flow::T23D_PORT, "health", 1500)
            .ok()
            .and_then(|v| v["cuda"].as_bool())
            .unwrap_or(false);
    T23dStatus {
        engine_set: !engine_path.trim().is_empty(),
        engine_ok,
        engine_path,
        engine_error,
        server_running: running,
        gpu,
        models,
        known: T23D_KNOWN_MODELS
            .iter()
            .map(|(n, l)| T23dKnownModel {
                name: n.to_string(),
                label: l.to_string(),
            })
            .collect(),
        current_model: cfg.t23d_model.unwrap_or_else(|| "hunyuan3d-2".to_string()),
    }
}

/// エンジンの場所（WinPortable ルート or Hunyuan3D-2 フォルダ）を保存。空文字でクリア。
/// 保存前に解決を試み、通らないパスはエラーで返す（黙って壊れた設定を持たない）。
#[tauri::command(async)]
pub(crate) fn t23d_set_engine(state: State<AppState>, path: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let p = path.trim().to_string();
    let mut cfg = read_config(&app_data);
    cfg.t23d_engine = if p.is_empty() { None } else { Some(p) };
    write_config(&app_data, &cfg);
    if cfg.t23d_engine.is_some() {
        t23d_resolve_engine(&app_data).map(|_| ())
    } else {
        Ok(())
    }
}

/// デーモンを起動する（モデルのロードはまだしない＝最初の生成で読む）。
#[tauri::command(async)]
pub(crate) fn t23d_start_server(state: State<AppState>) -> Result<(), String> {
    let app_data = state.app_data.clone();
    t23d_ensure_server(&app_data, state.inner())
}

/// デーモンを停止する（VRAM を解放）。
#[tauri::command(async)]
pub(crate) fn t23d_stop_server(state: State<AppState>) -> Result<(), String> {
    let mut g = state.t23d_server.lock().unwrap();
    if let Some(mut child) = g.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// 既定モデルを切り替え、デーモンに warm ロードさせる（best-effort）。
#[tauri::command(async)]
pub(crate) fn t23d_set_model(state: State<AppState>, model: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("モデル名が空です".into());
    }
    let mut cfg = read_config(&app_data);
    cfg.t23d_model = Some(model.clone());
    write_config(&app_data, &cfg);
    if t23d_running() {
        let url = format!("http://127.0.0.1:{}/load", flow::T23D_PORT);
        std::thread::scope(|s| {
            s.spawn(|| {
                let c = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .map_err(|e| e.to_string())?;
                let r = c
                    .post(&url)
                    .json(&serde_json::json!({ "name": model }))
                    .send()
                    .map_err(|e| e.to_string())?;
                if !r.status().is_success() {
                    return Err(format!("モデルのロードに失敗 ({})", r.status()));
                }
                Ok::<(), String>(())
            })
            .join()
            .map_err(|_| "ロードスレッドが異常終了".to_string())?
        })?;
    }
    Ok(())
}
