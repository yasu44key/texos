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
pub(crate) const T2I_SERVER_VERSION: &str = "9";

/// 既知モデル（friendly name → 表示名）。t2i_server.py の MODELS と対応させること。
/// (name, label, gated, hf_url)。gated=true は HF 認証（HF_TOKEN）が必要。
/// hf_url はライセンス承諾ページ（ユーザーが開いて Agree する先）。
pub(crate) const T2I_KNOWN_MODELS: &[(&str, &str, bool, &str)] = &[
    ("sd1.5", "Stable Diffusion 1.5（軽量・高速・~2GB）", false,
     "https://huggingface.co/stable-diffusion-v1-5/stable-diffusion-v1-5"),
    ("dreamshaper", "Dreamshaper 8（SD1.5系・高品質・~2GB）", false,
     "https://huggingface.co/Lykon/dreamshaper-8"),
    ("flux-lite", "FLUX.1 Lite 8B（オープン・高品質・~26GB）", false,
     "https://huggingface.co/Freepik/flux.1-lite-8B"),
    ("flux-schnell", "FLUX.1 schnell（公式・要HF認証・~34GB）", true,
     "https://huggingface.co/black-forest-labs/FLUX.1-schnell"),
    ("flux-dev", "FLUX.1 dev（公式・非商用・要HF認証・~34GB）", true,
     "https://huggingface.co/black-forest-labs/FLUX.1-dev"),
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

