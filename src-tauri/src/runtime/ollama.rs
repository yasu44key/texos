#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// --- v1.4.6 Ollama セットアップ案内（未導入PC向け）-----------------------------

#[derive(Serialize, Clone)]
pub(crate) struct OllamaStatus {
    /// サーバ（localhost:11434 等）が応答するか＝すぐ使える
    running: bool,
    /// 実行ファイルが見つかるか（起動するだけで使える）
    installed: bool,
    /// winget があるか（自動インストール可否）
    can_winget: bool,
}

pub(crate) fn ollama_base(endpoint: Option<&str>) -> String {
    endpoint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("http://localhost:11434")
        .trim_end_matches('/')
        .to_string()
}

/// Ollama 実行ファイルを探す（既定インストール先 → 環境変数PATH）。
pub(crate) fn ollama_binary() -> Option<PathBuf> {
    if let Some(local) = dirs::data_local_dir() {
        let p = local.join("Programs").join("Ollama").join("ollama.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["ollama.exe", "ollama"] {
                let c = dir.join(name);
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    None
}

pub(crate) fn has_winget() -> bool {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("winget.exe").is_file() {
                return true;
            }
        }
    }
    false
}

/// サーバが応答するか（短時間・tokio文脈外スレッドで blocking）。
pub(crate) fn ollama_running(base: &str) -> bool {
    let url = format!("{base}/api/version");
    std::thread::scope(|s| {
        s.spawn(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .ok()
                .and_then(|c| c.get(&url).send().ok())
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        })
        .join()
        .unwrap_or(false)
    })
}

/// AIパネル起動時の状態確認。
#[tauri::command(async)]
pub(crate) fn ai_ollama_status(endpoint: Option<String>) -> OllamaStatus {
    let base = ollama_base(endpoint.as_deref());
    OllamaStatus {
        running: ollama_running(&base),
        installed: ollama_binary().is_some(),
        can_winget: has_winget(),
    }
}

/// winget で Ollama を自動インストール（Windows）。長時間ブロックする。
#[tauri::command(async)]
pub(crate) fn ai_install_ollama() -> Result<(), String> {
    if !has_winget() {
        return Err(
            "winget が無いため自動インストールできません。ダウンロードページから入れてください"
                .into(),
        );
    }
    let mut cmd = std::process::Command::new("winget");
    cmd.args([
        "install",
        "--id",
        "Ollama.Ollama",
        "-e",
        "--source",
        "winget",
        "--accept-source-agreements",
        "--accept-package-agreements",
        "--scope",
        "user",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "インストールに失敗しました (exit {:?})。ダウンロードページからの手動導入をお試しください",
            s.code()
        )),
        Err(e) => Err(format!("winget の実行に失敗: {e}")),
    }
}

/// インストール済みだがサーバが起動していないとき、サーバを起動する。
#[tauri::command(async)]
pub(crate) fn ai_start_ollama() -> Result<(), String> {
    let bin = ollama_binary().ok_or("Ollama が見つかりません")?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("serve");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW（バックグラウンド常駐）
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Ollama の起動に失敗: {e}"))?;
    Ok(())
}

/// モデルを取得（既定 gemma3）。/api/pull の進捗を "ollama-pull" イベントで配信。
#[tauri::command(async)]
pub(crate) fn ai_pull_model(
    window: tauri::WebviewWindow,
    model: String,
    endpoint: Option<String>,
) -> Result<(), String> {
    use std::io::BufRead;
    let base = ollama_base(endpoint.as_deref());
    let model = if model.trim().is_empty() {
        "gemma3:latest".to_string()
    } else {
        model.trim().to_string()
    };
    let app = window.app_handle().clone();
    std::thread::scope(|s| {
        s.spawn(|| {
            let client = reqwest::blocking::Client::builder()
                .build()
                .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))?;
            let resp = client
                .post(format!("{base}/api/pull"))
                .json(&serde_json::json!({ "model": model, "stream": true }))
                .send()
                .map_err(|e| format!("取得を開始できません（Ollamaが起動しているか確認）: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("モデル取得に失敗 ({})", resp.status()));
            }
            for line in std::io::BufReader::new(resp).lines() {
                let line = line.map_err(|e| format!("取得ストリーム読み取り失敗: {e}"))?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(err) = v["error"].as_str() {
                        return Err(format!("モデル取得エラー: {err}"));
                    }
                    let status = v["status"].as_str().unwrap_or("");
                    let completed = v["completed"].as_u64().unwrap_or(0);
                    let total = v["total"].as_u64().unwrap_or(0);
                    let percent = if total > 0 {
                        (completed as f64 / total as f64 * 100.0) as u32
                    } else {
                        0
                    };
                    let _ = app.emit(
                        "ollama-pull",
                        serde_json::json!({ "status": status, "percent": percent }),
                    );
                }
            }
            Ok::<(), String>(())
        })
        .join()
        .map_err(|_| "取得スレッドが異常終了しました".to_string())?
    })
}

/// ストリーミング中断（停止ボタン）。DB ロックを取らないので即座に効く。
/// v1.5.1 WS-B: job_id 引数化（Job.cancel 経由）。job_id 未着（job:started 前の停止連打）の
/// フォールバックとして None は Chat ジョブ全部に効く（Chat は現状どおり単一運用）。
#[tauri::command(async)]
pub(crate) fn ai_chat_cancel(state: State<AppState>, job_id: Option<u64>) {
    match job_id {
        Some(id) => {
            if let Some(job) = state.jobs.get(id) {
                job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        None => state.jobs.cancel_chats(),
    }
}

/// 会話をリセット（履歴ファイルを削除）。
#[tauri::command]
pub(crate) fn ai_chat_clear(state: State<AppState>, id: String) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let f = chat_file(&p.root, &id);
    if f.exists() {
        std::fs::remove_file(&f).map_err(|e| e.to_string())?;
    }
    Ok(())
}

