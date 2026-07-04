#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// --- v1.4.8 Whisper 自動導入（音声→テキスト・A案: app_data 共有）-------------------
// whisper.cpp の CLI とモデルを %APPDATA%\com.yoshi.texos\whisper\ に置き、
// Whisper カードは絶対パスで参照する（巨大モデルをプロジェクト間で重複させない）。
// バイナリは安定版にピン留め（重大な不具合があれば上げる）。HTTP は reqwest::blocking を
// tokio 文脈外スレッドで呼ぶ（[[project_texos_ai_card]] の落とし穴回避と同じ作法）。

/// 安定版にピン留め（ggml-org/whisper.cpp リリース）。
pub(crate) const WHISPER_TAG: &str = "v1.9.1";

pub(crate) fn whisper_root(app_data: &Path) -> PathBuf {
    app_data.join("whisper")
}
pub(crate) fn whisper_bin_dir(app_data: &Path) -> PathBuf {
    whisper_root(app_data).join("bin")
}
pub(crate) fn whisper_models_dir(app_data: &Path) -> PathBuf {
    whisper_root(app_data).join("models")
}
pub(crate) fn whisper_cli_path(app_data: &Path) -> PathBuf {
    whisper_bin_dir(app_data).join("whisper-cli.exe")
}

/// ffmpeg が PATH（または WinGet Links）にあるか。フィルタ実行は name="ffmpeg.exe" を
/// .texos\filters→PATH で解決するため、ここでも PATH を基本に判定する。
pub(crate) fn ffmpeg_available() -> bool {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("ffmpeg.exe").is_file() {
                return true;
            }
        }
    }
    // winget でユーザースコープに入れた直後は現プロセスの PATH に未反映なので Links も見る。
    if let Some(local) = dirs::data_local_dir() {
        if local
            .join("Microsoft")
            .join("WinGet")
            .join("Links")
            .join("ffmpeg.exe")
            .is_file()
        {
            return true;
        }
    }
    false
}

/// 取得済みモデル名（ggml-<name>.bin → <name>）の一覧。
pub(crate) fn list_whisper_models(app_data: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(whisper_models_dir(app_data)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(s) = name.strip_prefix("ggml-").and_then(|s| s.strip_suffix(".bin")) {
                out.push(s.to_string());
            }
        }
    }
    out.sort();
    out
}

#[derive(Serialize, Clone)]
pub(crate) struct WhisperStatus {
    /// ffmpeg（前段の WAV 変換用）が見つかるか
    ffmpeg: bool,
    /// whisper-cli.exe が導入済みか
    whisper: bool,
    /// 取得済みモデル名の一覧
    models: Vec<String>,
    /// 現在選択中のモデル（既定 small）
    current_model: String,
    /// 現在のモデルが取得済みか
    current_ready: bool,
    /// winget で ffmpeg を自動導入できるか
    can_winget: bool,
    /// whisper-cli.exe の絶対パス（カード作成時に焼き込む）
    cli_path: String,
    /// 現在のモデルの絶対パス（同上）
    model_path: String,
}

#[tauri::command(async)]
pub(crate) fn whisper_status(state: State<AppState>) -> WhisperStatus {
    let app_data = state.app_data.clone();
    let models = list_whisper_models(&app_data);
    let current = read_config(&app_data)
        .whisper_model
        .unwrap_or_else(|| "small".to_string());
    let model_path = whisper_models_dir(&app_data).join(format!("ggml-{current}.bin"));
    WhisperStatus {
        ffmpeg: ffmpeg_available(),
        whisper: whisper_cli_path(&app_data).is_file(),
        current_ready: model_path.is_file(),
        models,
        current_model: current,
        can_winget: has_winget(),
        cli_path: whisper_cli_path(&app_data).to_string_lossy().to_string(),
        model_path: model_path.to_string_lossy().to_string(),
    }
}

/// winget で ffmpeg（Gyan.FFmpeg）を自動インストール。導入後は PATH 反映のため要再起動。
#[tauri::command(async)]
pub(crate) fn whisper_install_ffmpeg() -> Result<(), String> {
    if !has_winget() {
        return Err(
            "winget が無いため自動インストールできません。ffmpeg.org から入れてください".into(),
        );
    }
    let mut cmd = std::process::Command::new("winget");
    cmd.args([
        "install",
        "--id",
        "Gyan.FFmpeg",
        "-e",
        "--source",
        "winget",
        "--accept-source-agreements",
        "--accept-package-agreements",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "ffmpeg のインストールに失敗 (exit {:?})。ffmpeg.org からの手動導入をお試しください",
            s.code()
        )),
        Err(e) => Err(format!("winget の実行に失敗: {e}")),
    }
}

/// whisper.cpp の Windows バイナリ zip を取得して app_data\whisper\bin に展開。
/// 進捗は "whisper-install" イベント（{status, percent}）で配信。
#[tauri::command(async)]
pub(crate) fn whisper_install(window: tauri::WebviewWindow, state: State<AppState>) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let bin_dir = whisper_bin_dir(&app_data);
    let app = window.app_handle().clone();
    let url = format!(
        "https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_TAG}/whisper-bin-x64.zip"
    );
    std::thread::scope(|s| {
        s.spawn(|| {
            use std::io::Read;
            std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
            let _ = app.emit(
                "whisper-install",
                serde_json::json!({ "status": "ダウンロード中…", "percent": 0 }),
            );
            let client = reqwest::blocking::Client::builder()
                .build()
                .map_err(|e| format!("HTTPクライアント生成に失敗: {e}"))?;
            let mut resp = client
                .get(&url)
                .send()
                .map_err(|e| format!("ダウンロードを開始できません: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("ダウンロードに失敗 ({})", resp.status()));
            }
            let total = resp.content_length().unwrap_or(0);
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 65536];
            let mut downloaded = 0u64;
            loop {
                let n = resp.read(&mut chunk).map_err(|e| format!("読み取り失敗: {e}"))?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                downloaded += n as u64;
                let pct = if total > 0 {
                    (downloaded as f64 / total as f64 * 100.0) as u32
                } else {
                    0
                };
                let _ = app.emit(
                    "whisper-install",
                    serde_json::json!({ "status": "ダウンロード中…", "percent": pct }),
                );
            }
            let _ = app.emit(
                "whisper-install",
                serde_json::json!({ "status": "展開中…", "percent": 100 }),
            );
            // zip 展開: 各エントリの basename だけを取り bin_dir 直下へ（Release/ を剥がす）。
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(buf))
                .map_err(|e| format!("zip を開けません: {e}"))?;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
                if f.is_dir() {
                    continue;
                }
                let full = f.name().to_string();
                let base = full.rsplit(['/', '\\']).next().unwrap_or(&full);
                if base.is_empty() {
                    continue;
                }
                let out = bin_dir.join(base);
                let mut o = std::fs::File::create(&out)
                    .map_err(|e| format!("書き出しに失敗 ({base}): {e}"))?;
                std::io::copy(&mut f, &mut o).map_err(|e| e.to_string())?;
            }
            if !whisper_cli_path(&app_data).is_file() {
                return Err("展開しましたが whisper-cli.exe が見つかりません".into());
            }
            let _ = app.emit(
                "whisper-install",
                serde_json::json!({ "status": "完了", "percent": 100 }),
            );
            Ok::<(), String>(())
        })
        .join()
        .map_err(|_| "インストールスレッドが異常終了しました".to_string())?
    })
}

/// GGMLモデルを HuggingFace から取得して app_data\whisper\models に保存し、現在モデルにする。
/// 進捗は "whisper-model" イベント（{model, percent}）で配信。
#[tauri::command(async)]
pub(crate) fn whisper_download_model(
    window: tauri::WebviewWindow,
    state: State<AppState>,
    model: String,
) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let models_dir = whisper_models_dir(&app_data);
    let model = if model.trim().is_empty() {
        "small".to_string()
    } else {
        model.trim().to_string()
    };
    let app = window.app_handle().clone();
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model}.bin");
    std::thread::scope(|s| {
        s.spawn(|| {
            use std::io::{Read, Write};
            std::fs::create_dir_all(&models_dir).map_err(|e| e.to_string())?;
            let dest = models_dir.join(format!("ggml-{model}.bin"));
            let tmp = models_dir.join(format!("ggml-{model}.bin.part"));
            let client = reqwest::blocking::Client::builder()
                .build()
                .map_err(|e| format!("HTTPクライアント生成に失敗: {e}"))?;
            let mut resp = client
                .get(&url)
                .send()
                .map_err(|e| format!("ダウンロードを開始できません: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("モデルの取得に失敗 ({})", resp.status()));
            }
            let total = resp.content_length().unwrap_or(0);
            let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            let mut chunk = [0u8; 131072];
            let mut downloaded = 0u64;
            loop {
                let n = resp.read(&mut chunk).map_err(|e| format!("読み取り失敗: {e}"))?;
                if n == 0 {
                    break;
                }
                f.write_all(&chunk[..n]).map_err(|e| e.to_string())?;
                downloaded += n as u64;
                let pct = if total > 0 {
                    (downloaded as f64 / total as f64 * 100.0) as u32
                } else {
                    0
                };
                let _ = app.emit(
                    "whisper-model",
                    serde_json::json!({ "model": model, "percent": pct }),
                );
            }
            drop(f);
            std::fs::rename(&tmp, &dest).map_err(|e| format!("保存に失敗: {e}"))?;
            let mut cfg = read_config(&app_data);
            cfg.whisper_model = Some(model.clone());
            write_config(&app_data, &cfg);
            let _ = app.emit(
                "whisper-model",
                serde_json::json!({ "model": model, "percent": 100 }),
            );
            Ok::<(), String>(())
        })
        .join()
        .map_err(|_| "ダウンロードスレッドが異常終了しました".to_string())?
    })
}

/// 現在の Whisper モデルを切り替える（取得済みのものから）。
#[tauri::command]
pub(crate) fn whisper_set_model(state: State<AppState>, model: String) -> Result<(), String> {
    let mut cfg = read_config(&state.app_data);
    cfg.whisper_model = Some(model.trim().to_string());
    write_config(&state.app_data, &cfg);
    Ok(())
}

