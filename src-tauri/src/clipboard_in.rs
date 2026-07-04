#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Clipboard paste (v1.3-④)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ClipboardPaste {
    /// "image" (object was created) | "text" (frontend decides note/URL) | "empty"
    pub kind: String,
    pub text: Option<String>,
    /// id of the created object (image paste) — 作成UNDOの記録用 (v1.3.2)
    pub id: Option<String>,
}

/// Paste the OS clipboard at (x, y). An image becomes a new 仮身 backed by a
/// PNG real body in the CAS; text is returned so the frontend can create a
/// comment (note) or URL card. MIME types can be added here over time.
#[tauri::command]
pub(crate) fn paste_clipboard(x: f64, y: f64, state: State<AppState>) -> Result<ClipboardPaste, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;

    if let Ok(img) = cb.get_image() {
        let (w, h) = (img.width as u32, img.height as u32);
        let rgba = image::RgbaImage::from_raw(w, h, img.bytes.into_owned())
            .ok_or("クリップボード画像の変換に失敗しました")?;
        let mut png: Vec<u8> = Vec::new();
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;

        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let hash = cas::store_bytes(&p.cas_root, &png).map_err(|e| e.to_string())?;
        db::insert_real_body(&p.conn, &hash, "image/png", png.len() as u64, Some("png"))
            .map_err(|e| e.to_string())?;
        let vid = uuid();
        db::insert_virtual_body(&p.conn, &vid, &hash, "貼り付け画像", x, y, None, None)
            .map_err(|e| e.to_string())?;
        return Ok(ClipboardPaste {
            kind: "image".into(),
            text: None,
            id: Some(vid),
        });
    }

    if let Ok(text) = cb.get_text() {
        if !text.trim().is_empty() {
            return Ok(ClipboardPaste {
                kind: "text".into(),
                text: Some(text),
                id: None,
            });
        }
    }
    // v1.3.4: クリップボードがファイル一覧（CF_HDROP）の場合 — TexoS自身がコピー
    // したファイルも他アプリでコピーしたファイルもここに来る。「実身のコピーは実身」
    // という一般原則と仮身モデルの整合のため、内部ペーストでファイルを取り込むことは
    // せず、正しい持ち込み方（D&D で仮身化）を案内する（フロントでダイアログ）。
    #[cfg(windows)]
    if clipboard_out::clipboard_has_files() {
        return Ok(ClipboardPaste {
            kind: "files".into(),
            text: None,
            id: None,
        });
    }
    Ok(ClipboardPaste {
        kind: "empty".into(),
        text: None,
        id: None,
    })
}

/// v1.3.1: pasted text becomes a REGULAR object — a .txt real body in the CAS
/// with a 仮身 card (画像貼り付けと同格). コミットでファイル化、矢印リンク可。
#[tauri::command]
pub(crate) fn create_text_object(text: String, x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let bytes = text.as_bytes();
    let hash = cas::store_bytes(&p.cas_root, bytes).map_err(|e| e.to_string())?;
    db::insert_real_body(&p.conn, &hash, "text/plain", bytes.len() as u64, Some("txt"))
        .map_err(|e| e.to_string())?;
    // Label from the first non-empty line.
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let label: String = if first.is_empty() {
        "貼り付けテキスト".into()
    } else {
        first.chars().take(24).collect()
    };
    let vid = uuid();
    db::insert_virtual_body(&p.conn, &vid, &hash, &label, x, y, None, None)
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

