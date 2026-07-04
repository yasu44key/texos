#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// --- v1.4.9 子プロセス stdin チャネル（汎用）＋ 録音デバイス列挙 ---------------------
// A の最小版: 実行中フィルタ（record モード）の stdin へ文字列を送る道。録音停止は "q"。
// 将来は対話プログラムへの入力やキー転送（キャンバス内ターミナル）へ拡張できる。
// 暴走プログラムの停止（CTRL_BREAK シグナル＋Job Object）は別チャネル＝#4 で実装予定。

/// 実行中の子プロセスの stdin に文字列（+改行）を送る。録音の正常停止は text="q"。
#[tauri::command(async)]
pub(crate) fn flow_send_stdin(state: State<AppState>, text: String) -> Result<(), String> {
    use std::io::Write;
    let mut guard = state.rec_stdin.lock().unwrap();
    match guard.as_mut() {
        Some(si) => {
            si.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            si.write_all(b"\n").map_err(|e| e.to_string())?;
            si.flush().map_err(|e| e.to_string())?;
            Ok(())
        }
        None => Err("制御を受け付ける実行中プロセスがありません".into()),
    }
}

/// dshow の音声入力デバイス名一覧（録音プリセットの選択用）。
/// `ffmpeg -list_devices true -f dshow -i dummy` の stderr を解析する（要 ffmpeg）。
#[tauri::command(async)]
pub(crate) fn list_audio_devices() -> Result<Vec<String>, String> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd
        .output()
        .map_err(|e| format!("ffmpeg を実行できません（PATH にありますか）: {e}"))?;
    let text = String::from_utf8_lossy(&out.stderr);
    // 形式: [...] "デバイス名" (audio)
    let re = regex::Regex::new(r#""([^"]+)"\s+\(audio\)"#).map_err(|e| e.to_string())?;
    let names: Vec<String> = re
        .captures_iter(&text)
        .map(|c| c[1].to_string())
        .collect();
    Ok(names)
}

pub(crate) const DROP_MIME: &str = "application/vnd.texos.drop";

/// ドロップポイント（データ入口カード）を作成。中身は空（sentinel）で始まり、
/// ファイルを投入するたびに「最後に投入したファイルへの窓」になる仮身。
#[tauri::command]
pub(crate) fn create_drop_point(x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES ('drop:empty', ?1, 0, ?2)",
            rusqlite::params![DROP_MIME, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind)
             VALUES (?1, 'drop:empty', 'ドロップポイント', ?2, ?3, ?4, 'drop')",
            rusqlite::params![vid, x, y, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

