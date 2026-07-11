#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// --- v1.5 Text-To-Image（builtin:"t2i"）: 常駐 diffusers デーモンの管理 ----------------
// Whisper/Ollama と同じ流儀: 共有 app-data 配下にモデル/スクリプトを置き、status コマンド＋
// 進捗イベント（"t2i-model"）でセットアップを案内する。デーモン本体（t2i_server.py）は
// ビルドに埋め込み、起動時に app-data へ書き出す（リリースでも resources 設定不要）。

/// デーモン本体を実行ファイルへ埋め込む（src-tauri/src → ../../python）。
pub(crate) const T2I_SERVER_PY: &str = include_str!("../../../python/t2i_server.py");

/// 期待するデーモンの版（t2i_server.py の SERVER_VERSION と一致させること）。
/// 稼働中デーモンの版がこれと違えば「古い」と見なして起動し直す（再ビルド反映）。
pub(crate) const T2I_SERVER_VERSION: &str = "12";

/// 既知モデル（friendly name → 表示名）。t2i_server.py の MODELS と対応させること。
/// (name, label, gated, hf_url)。gated=true は HF 認証（HF_TOKEN）が必要。
/// hf_url はライセンス承諾ページ（ユーザーが開いて Agree する先）。
// v1.5.13 方針: 定番3本柱（SD1.5 / SDXL / Flux-lite）＋ DreamShaper を推奨に。
// 公式 FLUX dev/schnell（24GB+・gated）は known から外し、上級者の手動配置に委ねる。
// t2i_server.py の MODELS と一致させること。
pub(crate) const T2I_KNOWN_MODELS: &[(&str, &str, bool, &str)] = &[
    ("sd1.5", "Stable Diffusion 1.5（軽量・高速・~2GB）", false,
     "https://huggingface.co/stable-diffusion-v1-5/stable-diffusion-v1-5"),
    ("dreamshaper", "Dreamshaper 8（SD1.5系・高品質・~2GB）", false,
     "https://huggingface.co/Lykon/dreamshaper-8"),
    ("sdxl", "Stable Diffusion XL 1.0（高解像度・~6.9GB）", false,
     "https://huggingface.co/stabilityai/stable-diffusion-xl-base-1.0"),
    ("flux-lite", "FLUX.1 Lite 8B（オープン・高品質・~26GB）", false,
     "https://huggingface.co/Freepik/flux.1-lite-8B"),
];

pub(crate) fn t2i_root(app_data: &Path) -> PathBuf {
    app_data.join("t2i")
}
pub(crate) fn t2i_models_dir(app_data: &Path) -> PathBuf {
    t2i_root(app_data).join("models")
}
pub(crate) fn t2i_server_script(app_data: &Path) -> PathBuf {
    t2i_root(app_data).join("t2i_server.py")
}

/// config の HF トークンをデーモンが読むファイル（<home>/hf_token）へ書き出す（無ければ削除）。
/// デーモンは pull のたびにこのファイルを読むので、これだけで再起動なしにトークンが効く。
pub(crate) fn sync_t2i_token_file(app_data: &Path) {
    let f = t2i_root(app_data).join("hf_token");
    match read_config(app_data).t2i_hf_token.filter(|t| !t.trim().is_empty()) {
        Some(tok) => {
            let _ = std::fs::create_dir_all(t2i_root(app_data));
            let _ = std::fs::write(&f, tok.trim());
        }
        None => {
            let _ = std::fs::remove_file(&f);
        }
    }
}

/// 取得済みモデル（diffusers 構造＝model_index.json がある）を列挙。
/// モデルフォルダ配下に `.incomplete`（HF の DL 途中マーカー）があるか（再帰）。
pub(crate) fn t2i_has_incomplete(dir: &Path) -> bool {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if t2i_has_incomplete(&p) {
                    return true;
                }
            } else if e
                .file_name()
                .to_str()
                .map(|n| n.ends_with(".incomplete"))
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// diffusers フォルダ d が**完全**か（daemon の _diffusers_complete と同判定）。
/// model_index.json＋重みコンポーネントの重み一式＋シャード・`.incomplete` 無し。
pub(crate) fn t2i_dir_complete(d: &Path) -> bool {
    let idx = d.join("model_index.json");
    if !idx.is_file() || t2i_has_incomplete(d) {
        return false;
    }
    let mi: serde_json::Value = match std::fs::read_to_string(&idx)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
    {
        Some(v) => v,
        None => return false,
    };
    let obj = match mi.as_object() {
        Some(o) => o,
        None => return false,
    };
    const NO_WEIGHT: &[&str] = &[
        "scheduler", "tokenizer", "tokenizer_2", "tokenizer_3",
        "feature_extractor", "safety_checker", "image_encoder",
    ];
    for (comp, val) in obj {
        if comp.starts_with('_') || NO_WEIGHT.contains(&comp.as_str()) {
            continue;
        }
        // 値は [library, class]。library が null（未使用）なら対象外。
        match val.as_array() {
            Some(a) if a.len() == 2 && !a[0].is_null() => {}
            _ => continue,
        }
        let sub = d.join(comp);
        if !sub.is_dir() {
            return false;
        }
        let mut has_weight = false;
        let mut shard_idxs: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&sub) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.ends_with(".index.json") {
                    shard_idxs.push(e.path());
                } else if n.ends_with(".safetensors") || n.ends_with(".bin") {
                    has_weight = true;
                }
            }
        }
        if !shard_idxs.is_empty() {
            for ix in shard_idxs {
                let v: serde_json::Value = match std::fs::read_to_string(&ix)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                {
                    Some(v) => v,
                    None => return false,
                };
                if let Some(wm) = v.get("weight_map").and_then(|w| w.as_object()) {
                    for sh in wm.values().filter_map(|s| s.as_str()) {
                        if !sub.join(sh).is_file() {
                            return false;
                        }
                    }
                }
            }
        } else if !has_weight {
            return false;
        }
    }
    true
}

/// フォルダ d が単一ファイルモデル（model_index.json 無し・チェックポイント1つ）か。
pub(crate) fn t2i_dir_has_single_checkpoint(d: &Path) -> bool {
    if d.join("model_index.json").is_file() {
        return false;
    }
    let mut count = 0;
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if n.ends_with(".safetensors") || n.ends_with(".ckpt") {
                    count += 1;
                }
            }
        }
    }
    count == 1
}

/// 取得済み（ロード可能）モデル名。完全な diffusers / 単一ファイル（直置き or フォルダ内）。
/// daemon の list_available と一致させる（部分DLは含めない＝再取得ボタンが出る）。
pub(crate) fn list_t2i_models(app_data: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(t2i_models_dir(app_data)) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if t2i_dir_complete(&p) || t2i_dir_has_single_checkpoint(&p) {
                    out.push(name);
                }
            } else if name.ends_with(".safetensors") || name.ends_with(".ckpt") {
                // 直置き単一ファイル: 名前＝拡張子抜き
                out.push(name.rsplitn(2, '.').nth(1).unwrap_or(&name).to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// モディファイア用アセット（loras/embeddings/vae）を home/<sub> から列挙。拡張子なしの名前。
/// daemon の Engine._list_assets と一致させる（daemon 非依存＝サーバー未起動でも UI セレクトに出せる）。
/// vae は diffusers フォルダ（単一ファイルでない VAE）も許可するので dirs=true。
pub(crate) fn list_t2i_assets(app_data: &Path, sub: &str, exts: &[&str], dirs: bool) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(t2i_root(app_data).join(sub)) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if dirs {
                    out.push(name);
                }
            } else if let Some(dot) = name.rfind('.') {
                let ext = name[dot..].to_ascii_lowercase();
                if exts.contains(&ext.as_str()) {
                    out.push(name[..dot].to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 部分ダウンロード（中断/失敗）のモデル名: フォルダに model_index.json はあるが不完全。
/// 「不完全（続きから再取得）」を UI に出すため。
pub(crate) fn t2i_partial_models(app_data: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(t2i_models_dir(app_data)) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("model_index.json").is_file() && !t2i_dir_complete(&p) {
                out.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

// detect_python は WS2（v1.4.27）で runtime/python.rs へ移動し、解決順を
// 「① TexoS 管理 env → ② システム Python」に変更した（config.python_env で明示切替も可）。

/// デーモンへ GET（tokio 文脈外スレッドで blocking 実行）。
pub(crate) fn t2i_http_get(path: &str, timeout_ms: u64) -> Result<serde_json::Value, String> {
    let url = format!(
        "http://127.0.0.1:{}/{}",
        flow::T2I_PORT,
        path.trim_start_matches('/')
    );
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

pub(crate) fn t2i_running() -> bool {
    t2i_http_get("health", 800).is_ok()
}

/// 稼働中デーモンが報告する版（取得できなければ空）。古い版の検出に使う。
pub(crate) fn t2i_daemon_version() -> String {
    t2i_http_get("health", 1000)
        .ok()
        .and_then(|v| v["version"].as_str().map(String::from))
        .unwrap_or_default()
}

/// 稼働中デーモンが「取得済み」と認識しているモデル名。取得できなければ空。
pub(crate) fn t2i_daemon_models() -> Vec<String> {
    t2i_http_get("models", 2000)
        .ok()
        .and_then(|v| {
            v.get("available").and_then(|a| a.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// T2I_PORT を Listen しているプロセスを止める（孤児デーモンや古いデーモンの掃除）。
/// 自分の子だけでなく、前セッションの取り残しも確実に終わらせるため port 基準で kill。
#[cfg(windows)]
pub(crate) fn t2i_kill_on_port() {
    use std::os::windows::process::CommandExt;
    let ps = format!(
        "Get-NetTCPConnection -LocalPort {} -State Listen -ErrorAction SilentlyContinue | \
         ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }}",
        flow::T2I_PORT
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .creation_flags(0x0800_0000)
        .status();
}
#[cfg(not(windows))]
pub(crate) fn t2i_kill_on_port() {}

/// デーモンを起動して health が立つまで待つ。
/// 既に稼働中でも、ディスク上の取得済みモデルをすべて認識していなければ「古いデーモン」と
/// 見なして再起動する（モデルをDLしたのにデーモンが見ない不一致の自己修復）。
/// 子プロセスは state.t2i_server に保持して停止できるようにする。
pub(crate) fn t2i_ensure_server(app_data: &Path, state: &AppState) -> Result<(), String> {
    // 稼働中/新規どちらでも、デーモンが読むトークンファイルを最新化しておく
    // （pull のたびに読む方式なので、これだけで再起動なしにトークンが効く）。
    sync_t2i_token_file(app_data);
    if t2i_running() {
        // 版が一致し、ディスクの取得済みモデルを全部認識していれば、そのまま使う。
        let disk = list_t2i_models(app_data);
        let seen = t2i_daemon_models();
        let version_ok = t2i_daemon_version() == T2I_SERVER_VERSION;
        if version_ok && disk.iter().all(|m| seen.contains(m)) {
            return Ok(());
        }
        // 古い版／取得済みを見ていない → 止めて最新スクリプトで起動し直す。
        {
            let mut g = state.t2i_server.lock().unwrap();
            if let Some(mut old) = g.take() {
                let _ = old.kill();
                let _ = old.wait();
            }
        }
        t2i_kill_on_port();
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
    let env = detect_python(app_data);
    let py = env.path.ok_or(
        "Python 環境がありません。画像生成の設定画面で「環境を構築する」を押すと自動構築されます",
    )?;
    if !env.deps_ok {
        return Err(
            "生成ライブラリ（torch / diffusers）が未導入です。画像生成の設定画面で「環境を構築する」を押すと自動構築されます"
                .into(),
        );
    }
    let root = t2i_root(app_data);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let script = t2i_server_script(app_data);
    std::fs::write(&script, T2I_SERVER_PY).map_err(|e| format!("サーバースクリプトの書き出しに失敗: {e}"))?;

    let log = std::fs::File::create(root.join("server.log")).ok();
    let mut c = std::process::Command::new(&py);
    c.arg(&script)
        .args(["--port", &flow::T2I_PORT.to_string()])
        .args(["--home", &root.to_string_lossy()]);
    // ゲート付き FLUX 公式モデル用の HF トークンを環境変数で渡す（設定されていれば）。
    if let Some(tok) = read_config(app_data).t2i_hf_token.filter(|t| !t.trim().is_empty()) {
        c.env("HF_TOKEN", tok);
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
    let child = c.spawn().map_err(|e| format!("画像生成サーバーの起動に失敗: {e}"))?;
    // 旧プロセスがいれば置換（先に殺す）。
    {
        let mut g = state.t2i_server.lock().unwrap();
        if let Some(mut old) = g.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        *g = Some(child);
    }
    // health を最大 ~30 秒待つ（torch の import に時間がかかる）。
    for _ in 0..60 {
        if t2i_running() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err("画像生成サーバーが起動しませんでした（t2i/server.log を確認してください）".into())
}

#[derive(Serialize, Clone)]
pub(crate) struct T2iKnownModel {
    name: String,
    label: String,
    /// HF 認証（HF_TOKEN）が必要なゲート付きモデルか
    gated: bool,
    /// ライセンス承諾ページ URL（gated のときに UI でコピー提示する）
    hf_url: String,
}

#[derive(Serialize, Clone)]
pub(crate) struct T2iStatus {
    /// 使える Python が見つかったか
    python_ok: bool,
    /// その Python の絶対パス
    python_path: String,
    /// torch/diffusers 等が揃っているか
    deps_ok: bool,
    /// Python の解決元（WS2）: "managed"（TexoS 構築 env）| "system" | "none"
    python_kind: String,
    /// config の明示切替（WS2）: "auto" | "managed" | "system"
    env_mode: String,
    /// デーモンが応答するか（=すぐ生成できる）
    server_running: bool,
    /// CUDA(GPU) が使えるか（デーモン稼働時のみ判明）
    gpu: bool,
    /// 取得済みモデル名
    models: Vec<String>,
    /// 既知モデル（DL候補）
    known: Vec<T2iKnownModel>,
    /// 現在選択中のモデル（既定 sd1.5）
    current_model: String,
    /// HF トークンが設定済みか（ゲート付きモデル取得の可否）
    hf_token_set: bool,
    /// 部分DL（中断/失敗）のモデル名。「不完全（続きから再取得）」表示用。
    partial: Vec<String>,
    /// モディファイア用アセット（home/loras・embeddings・vae の名前一覧）。設定UIのセレクト用。
    loras: Vec<String>,
    embeddings: Vec<String>,
    vae: Vec<String>,
}

#[tauri::command(async)]
pub(crate) fn t2i_status(state: State<AppState>) -> T2iStatus {
    let app_data = state.app_data.clone();
    let env = detect_python(&app_data);
    let running = t2i_running();
    // GPU: サーバー稼働中は health の cuda を、未起動なら Python 直問い合わせの結果を使う。
    let gpu = if running {
        t2i_http_get("health", 1500)
            .ok()
            .and_then(|v| v["cuda"].as_bool())
            .unwrap_or(env.cuda)
    } else {
        env.cuda
    };
    let current = read_config(&app_data)
        .t2i_model
        .unwrap_or_else(|| "sd1.5".to_string());
    T2iStatus {
        python_ok: env.path.is_some(),
        python_path: env.path.unwrap_or_default(),
        deps_ok: env.deps_ok,
        python_kind: env.kind.to_string(),
        env_mode: read_config(&app_data)
            .python_env
            .unwrap_or_else(|| "auto".to_string()),
        server_running: running,
        gpu,
        models: list_t2i_models(&app_data),
        known: T2I_KNOWN_MODELS
            .iter()
            .map(|(n, l, gated, url)| T2iKnownModel {
                name: n.to_string(),
                label: l.to_string(),
                gated: *gated,
                hf_url: url.to_string(),
            })
            .collect(),
        current_model: current,
        hf_token_set: read_config(&app_data)
            .t2i_hf_token
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false),
        partial: t2i_partial_models(&app_data),
        loras: list_t2i_assets(&app_data, "loras", &[".safetensors", ".pt", ".ckpt"], false),
        embeddings: list_t2i_assets(&app_data, "embeddings", &[".safetensors", ".pt", ".bin"], false),
        vae: list_t2i_assets(&app_data, "vae", &[".safetensors", ".ckpt", ".pt", ".bin"], true),
    }
}

/// デーモンを起動する（モデルのロードはまだしない＝最初の生成で読む）。
#[tauri::command(async)]
pub(crate) fn t2i_start_server(state: State<AppState>) -> Result<(), String> {
    let app_data = state.app_data.clone();
    t2i_ensure_server(&app_data, state.inner())
}

/// デーモンを停止する（VRAM を解放）。
#[tauri::command(async)]
pub(crate) fn t2i_stop_server(state: State<AppState>) -> Result<(), String> {
    let mut g = state.t2i_server.lock().unwrap();
    if let Some(mut child) = g.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// モデルを取得（共有 app-data へ DL）。進捗は "t2i-model"（{model,status,percent}）で配信。
/// デーモンが diffusers/huggingface_hub でダウンロードするので、先にサーバーを起動する。
#[tauri::command(async)]
pub(crate) fn t2i_download_model(
    window: tauri::WebviewWindow,
    state: State<AppState>,
    model: String,
) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let model = if model.trim().is_empty() {
        "sd1.5".to_string()
    } else {
        model.trim().to_string()
    };
    t2i_ensure_server(&app_data, state.inner())?;
    let app = window.app_handle().clone();
    let url = format!("http://127.0.0.1:{}/pull", flow::T2I_PORT);
    // v1.5.2 WS-C: 台帳ジョブ化＝job:* も配信・進捗行ごとに中断可能（デーモン側の DL は
    // 継続するが huggingface_hub はレジューム可能。ポーリング打ち切りで UI は解放される）。
    run_setup_job(
        state.inner(),
        &app,
        &format!("画像生成モデル取得（{model}）"),
        |job| {
    std::thread::scope(|s| {
        s.spawn(|| {
            use std::io::BufRead;
            let client = reqwest::blocking::Client::builder()
                .build()
                .map_err(|e| format!("HTTPクライアント生成に失敗: {e}"))?;
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "name": model }))
                .send()
                .map_err(|e| format!("ダウンロードを開始できません: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("モデル取得に失敗 ({})", resp.status()));
            }
            for line in std::io::BufReader::new(resp).lines() {
                if job.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("中断されました".into());
                }
                let line = line.map_err(|e| format!("進捗の読み取りに失敗: {e}"))?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(err) = v["error"].as_str() {
                        return Err(err.to_string());
                    }
                    if v["done"].as_bool().unwrap_or(false) {
                        break;
                    }
                    let _ = app.emit(
                        "t2i-model",
                        serde_json::json!({
                            "model": model,
                            "status": v["status"].as_str().unwrap_or(""),
                            "percent": v["percent"].as_u64().unwrap_or(0),
                        }),
                    );
                    emit_job_progress(
                        &app,
                        job,
                        None,
                        Some(v["percent"].as_u64().unwrap_or(0) as u32),
                    );
                }
            }
            // 既定モデルとして記録。
            let mut cfg = read_config(&app_data);
            cfg.t2i_model = Some(model.clone());
            write_config(&app_data, &cfg);
            let _ = app.emit(
                "t2i-model",
                serde_json::json!({ "model": model, "status": "完了", "percent": 100 }),
            );
            Ok::<(), String>(())
        })
        .join()
        .map_err(|_| "ダウンロードスレッドが異常終了しました".to_string())?
    })
        },
    )
}

/// 既定モデルを切り替え、デーモンに warm ロードさせる（取得済みのものから）。
#[tauri::command(async)]
pub(crate) fn t2i_set_model(state: State<AppState>, model: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("モデル名が空です".into());
    }
    let mut cfg = read_config(&app_data);
    cfg.t2i_model = Some(model.clone());
    write_config(&app_data, &cfg);
    // サーバーが動いていれば warm ロード（重いので best-effort・失敗は致命ではない）。
    if t2i_running() {
        let url = format!("http://127.0.0.1:{}/load", flow::T2I_PORT);
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

/// HF トークンを保存（ゲート付き FLUX 公式モデル用）。空文字でクリア。
/// デーモンは pull のたびに <home>/hf_token を読むので、ファイルを書くだけで再起動不要で効く。
#[tauri::command(async)]
pub(crate) fn t2i_set_hf_token(state: State<AppState>, token: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let mut cfg = read_config(&app_data);
    let t = token.trim().to_string();
    cfg.t2i_hf_token = if t.is_empty() { None } else { Some(t) };
    write_config(&app_data, &cfg);
    sync_t2i_token_file(&app_data); // デーモンが読むファイルへ反映（再起動不要）
    Ok(())
}

/// 手動配置用: モデルの配置先フォルダ <models>/<model>/ を作って Explorer で開く。
/// ユーザーが HF から diffusers 構造（model_index.json＋各サブフォルダ）を手で置けるようにする
/// （自動DLが失敗するゲートモデルの保険）。デーモンは完全性チェックで自動検出する。
#[tauri::command(async)]
pub(crate) fn t2i_reveal_model_dir(state: State<AppState>, model: String) -> Result<String, String> {
    let app_data = state.app_data.clone();
    let m = model.trim();
    let dir = if m.is_empty() {
        t2i_models_dir(&app_data)
    } else {
        t2i_models_dir(&app_data).join(m)
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&p).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&p).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&p).spawn();
    }
    Ok(p)
}

/// モデルのローカルデータを削除（部分DL/壊れたモデルを破棄して取り直す用）。
/// フォルダ <models>/<model>/ と直置き <models>/<model>.safetensors|.ckpt の両方を消す。
#[tauri::command(async)]
pub(crate) fn t2i_delete_model(state: State<AppState>, model: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let m = model.trim();
    if m.is_empty() {
        return Err("モデル名が空です".into());
    }
    let base = t2i_models_dir(&app_data);
    let dir = base.join(m);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("削除に失敗: {e}"))?;
    }
    for ext in ["safetensors", "ckpt"] {
        let f = base.join(format!("{m}.{ext}"));
        if f.is_file() {
            let _ = std::fs::remove_file(&f);
        }
    }
    Ok(())
}

// --- v1.5.13 手持ちモデル診断（daemon 非依存）。エラーで弾く代わりに「使える/使えない＋推奨」を提示 ---
// safetensors ヘッダ（先頭のキー一覧）だけ読んで形式・種別を判定＝高速・サーバー未起動でも動く。
// LoRA/VAE の種別（sd/sdxl/flux）は目安で推定し、UI がモデルとの適合を薄く警告する（厳密ではない）。

#[derive(Serialize, Clone)]
pub(crate) struct T2iModelInfo {
    name: String,
    /// "diffusers" | "single"
    format: String,
    /// "sd" | "sdxl" | "flux" | "unknown"
    kind: String,
    usable: bool,
    /// 使えない理由（usable=false のとき）
    reason: String,
    /// 使えない場合の推奨アクション
    recommend: String,
}

#[derive(Serialize, Clone)]
pub(crate) struct T2iAssetInfo {
    name: String,
    /// "sd" | "sdxl" | "flux"（目安）
    kind: String,
}

#[derive(Serialize, Clone)]
pub(crate) struct T2iInspect {
    models: Vec<T2iModelInfo>,
    loras: Vec<T2iAssetInfo>,
    vae: Vec<T2iAssetInfo>,
}

/// safetensors のヘッダ（先頭 u64 長 + JSON オブジェクト）だけ読んで返す（重みは読まない＝高速）。
/// 各テンソルは {"dtype","shape","data_offsets"} を持つので、shape 判定にも使える。
fn t2i_safetensors_header(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut len8 = [0u8; 8];
    f.read_exact(&mut len8).ok()?;
    let n = u64::from_le_bytes(len8) as usize;
    // 異常に大きい/空のヘッダは弾く（壊れ・別形式）。
    if n == 0 || n > 64 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).ok()?;
    serde_json::from_slice::<serde_json::Value>(&buf)
        .ok()
        .and_then(|v| v.as_object().cloned())
}

/// ヘッダからテンソル名一覧を返す（重みは読まない＝高速）。読取失敗は空。
fn t2i_safetensors_keys(path: &Path) -> Vec<String> {
    match t2i_safetensors_header(path) {
        Some(h) => h.keys().filter(|k| k.as_str() != "__metadata__").cloned().collect(),
        None => Vec::new(),
    }
}

/// 単一ファイルモデルの種別を safetensors キーから推定（daemon の _detect_kind_file と同判定）。
fn t2i_sniff_single_kind(path: &Path) -> &'static str {
    if !path.to_string_lossy().to_ascii_lowercase().ends_with(".safetensors") {
        return "sd"; // .ckpt はヘッダ読み省略 → SD 既定
    }
    let keys = t2i_safetensors_keys(path);
    // 実 SD/SDXL/FLUX は必ず多数のキーを持つ。空＝ヘッダ読取失敗（ロック/破損/巨大ファイル）。
    // ここで安易に "sd"（使える扱い）へ倒すと、未知の巨大ファイルを誤って緑にする。断定しない。
    if keys.is_empty() {
        return "unknown";
    }
    if keys.iter().any(|k| k.contains("double_blocks") || k.contains("single_blocks")) {
        return "flux";
    }
    if keys.iter().any(|k| k.starts_with("conditioner.embedders.1") || k.contains("add_embedding")) {
        return "sdxl";
    }
    "sd"
}

/// LoRA/VAE アセットの種別を推定。kohya LoRA はキー名が SD1.5/SDXL 共通で名前では区別できない。
/// そこで cross-attention（attn2 / comfy の _1_1_）の context 次元を見る＝SD1.5:768 / SDXL:2048。
/// 判定不能（VAE・特殊形式・読取失敗）は "unknown"（UI は警告しない）。厳密ではないが実用十分。
fn t2i_sniff_asset_kind(path: &Path) -> &'static str {
    let header = match t2i_safetensors_header(path) {
        Some(h) => h,
        None => return "unknown",
    };
    let is_key = |k: &String| k.as_str() != "__metadata__";
    if header.keys().filter(|k| is_key(k)).any(|k| {
        k.contains("double_blocks") || k.contains("single_blocks") || k.starts_with("transformer")
    }) {
        return "flux";
    }
    // te2 が明示的にあれば SDXL 確定。
    if header
        .keys()
        .filter(|k| is_key(k))
        .any(|k| k.contains("lora_te2") || k.contains("text_encoder_2"))
    {
        return "sdxl";
    }
    // cross-attn の to_k/to_v の lora_down 入力次元（shape[1]）で SD1.5(768)/SDXL(2048) を判別。
    for (k, v) in header.iter() {
        let lk = k.to_ascii_lowercase();
        let is_cross = lk.contains("attn2") || lk.contains("_1_1_");
        let is_kv = lk.contains("to_k") || lk.contains("to_v");
        let is_down = lk.contains("lora_down") || lk.contains("lora_a");
        if is_cross && is_kv && is_down {
            if let Some(dim) = v
                .get("shape")
                .and_then(|s| s.as_array())
                .and_then(|a| a.get(1))
                .and_then(|d| d.as_u64())
            {
                if dim == 768 {
                    return "sd";
                }
                if dim == 2048 {
                    return "sdxl";
                }
            }
        }
    }
    "unknown"
}

/// diffusers フォルダの種別を model_index.json の _class_name から推定（daemon の _infer_kind と同判定）。
fn t2i_infer_diffusers_kind(dir: &Path) -> &'static str {
    let cls = std::fs::read_to_string(dir.join("model_index.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("_class_name").and_then(|c| c.as_str()).map(String::from))
        .unwrap_or_default();
    if cls.contains("Flux") {
        "flux"
    } else if cls.contains("XL") {
        "sdxl"
    } else {
        "sd"
    }
}

/// (kind, format) → (usable, reason, recommend)。t2i_server.py の model_policy と一致させること。
fn t2i_model_policy(kind: &str, fmt: &str) -> (bool, String, String) {
    if kind == "flux" && fmt == "single" {
        return (
            false,
            "FLUX の単一ファイルは未対応（T5/CLIP/VAE を含む全部入り形式のため）".into(),
            "flux-lite を diffusers 形式で取得するか、SDXL をご利用ください".into(),
        );
    }
    if !matches!(kind, "sd" | "sdxl" | "flux") {
        return (
            false,
            "対応外または種別を判定できませんでした".into(),
            "SD1.5 / SDXL / Flux-lite のいずれかを diffusers 形式で配置してください".into(),
        );
    }
    (true, String::new(), String::new())
}

/// 単一ファイルのチェックポイントを1つだけ含むフォルダから、そのファイルパスを返す。
fn t2i_single_checkpoint_path(dir: &Path) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    let mut count = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                let l = n.to_ascii_lowercase();
                if l.ends_with(".safetensors") || l.ends_with(".ckpt") {
                    count += 1;
                    found = Some(e.path());
                }
            }
        }
    }
    if count == 1 {
        found
    } else {
        None
    }
}

/// アセットフォルダ（loras/vae）を走査し、(名前, 種別) を返す。vae は diffusers フォルダも許可。
fn t2i_scan_assets(dir: &Path, exts: &[&str], allow_dirs: bool) -> Vec<T2iAssetInfo> {
    let mut out: Vec<T2iAssetInfo> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if allow_dirs {
                    out.push(T2iAssetInfo { name, kind: "sd".into() });
                }
            } else if let Some(dot) = name.rfind('.') {
                let ext = name[dot..].to_ascii_lowercase();
                if exts.contains(&ext.as_str()) {
                    out.push(T2iAssetInfo {
                        name: name[..dot].to_string(),
                        kind: t2i_sniff_asset_kind(&p).to_string(),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// 手持ちの models/loras/vae を走査して対応状況を返す（UI の「手持ちモデル診断」用）。
#[tauri::command(async)]
pub(crate) fn t2i_inspect(state: State<AppState>) -> T2iInspect {
    let app_data = state.app_data.clone();
    let mut models: Vec<T2iModelInfo> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(t2i_models_dir(&app_data)) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if p.join("model_index.json").is_file() {
                    if !t2i_dir_complete(&p) {
                        models.push(T2iModelInfo {
                            name,
                            format: "diffusers".into(),
                            kind: "unknown".into(),
                            usable: false,
                            reason: "ダウンロードが不完全です".into(),
                            recommend: "セットアップで「続きから再取得」してください".into(),
                        });
                        continue;
                    }
                    let kind = t2i_infer_diffusers_kind(&p);
                    let (usable, reason, recommend) = t2i_model_policy(kind, "diffusers");
                    models.push(T2iModelInfo {
                        name,
                        format: "diffusers".into(),
                        kind: kind.into(),
                        usable,
                        reason,
                        recommend,
                    });
                } else if let Some(ck) = t2i_single_checkpoint_path(&p) {
                    let kind = t2i_sniff_single_kind(&ck);
                    let (usable, reason, recommend) = t2i_model_policy(kind, "single");
                    models.push(T2iModelInfo {
                        name,
                        format: "single".into(),
                        kind: kind.into(),
                        usable,
                        reason,
                        recommend,
                    });
                }
            } else {
                let l = name.to_ascii_lowercase();
                if l.ends_with(".safetensors") || l.ends_with(".ckpt") {
                    let stem = name.rsplitn(2, '.').nth(1).unwrap_or(&name).to_string();
                    let kind = t2i_sniff_single_kind(&p);
                    let (usable, reason, recommend) = t2i_model_policy(kind, "single");
                    models.push(T2iModelInfo {
                        name: stem,
                        format: "single".into(),
                        kind: kind.into(),
                        usable,
                        reason,
                        recommend,
                    });
                }
            }
        }
    }
    models.sort_by(|a, b| a.name.cmp(&b.name));
    let root = t2i_root(&app_data);
    let loras = t2i_scan_assets(&root.join("loras"), &[".safetensors", ".pt", ".ckpt"], false);
    let vae = t2i_scan_assets(&root.join("vae"), &[".safetensors", ".ckpt", ".pt", ".bin"], true);
    T2iInspect { models, loras, vae }
}

