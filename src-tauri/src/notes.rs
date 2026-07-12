#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Notes (free text annotations on the desktop)
// ---------------------------------------------------------------------------

#[tauri::command]
pub(crate) fn create_note(text: String, x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let id = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, note_text)
             VALUES (?1, 'note:sentinel', ?2, ?3, ?4, ?5, 'note', ?6)",
            rusqlite::params![id, note_label(&text), x, y, db::now_ms(), text],
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

// --- v1.4.17 カード統一 §1: 付箋は kind='note' の仮身（実身は共有センチネル・CAS 非依存）。
//     本文は note_text、変種固有の属性は style_ref(JSON) に寄せる。NoteInfo の形は不変＝
//     フロントは無改修。フロー矢印で繋ぐと本文を text/plain として供給する「定数カード」になる。

/// kind='note' のスタイル置き場（style_ref JSON）。
#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct NoteStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_ref: Option<String>,
}

pub(crate) fn note_style_of(s: Option<&str>) -> NoteStyle {
    s.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
}

pub(crate) fn read_note_style(p: &Project, id: &str) -> Result<NoteStyle, String> {
    let s: Option<String> = p
        .conn
        .query_row(
            "SELECT style_ref FROM virtual_bodies WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .map_err(|_| "コメントが見つかりません".to_string())?;
    Ok(note_style_of(s.as_deref()))
}

pub(crate) fn write_note_style(p: &Project, id: &str, st: &NoteStyle) -> Result<(), String> {
    let json = serde_json::to_string(st).map_err(|e| e.to_string())?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET style_ref = ?1 WHERE id = ?2 AND kind = 'note'",
            rusqlite::params![json, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 付箋のラベル＝先頭行（一括メニュー・ツールチップ用の短い名前）。
pub(crate) fn note_label(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().chars().take(40).collect()
}

#[tauri::command]
pub(crate) fn list_notes(state: State<AppState>) -> Result<Vec<NoteInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        .prepare(
            "SELECT id, note_text, pos_x, pos_y, style_ref
             FROM virtual_bodies WHERE kind = 'note' AND deleted_at IS NULL ORDER BY created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let style: Option<String> = row.get(4)?;
            let st = note_style_of(style.as_deref());
            Ok(NoteInfo {
                id: row.get(0)?,
                text: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                pos_x: row.get(2)?,
                pos_y: row.get(3)?,
                width: st.width.unwrap_or(220.0),
                font_size: st.font_size.unwrap_or(13.0),
                bold: st.bold.unwrap_or(false),
                file_ref: st.file_ref,
                color: st.color,
                title_size: st.title_size,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn update_note(id: String, text: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET note_text = ?1, label = ?2 WHERE id = ?3 AND kind = 'note'",
            rusqlite::params![text, note_label(&text), id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn move_note(id: String, x: f64, y: f64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET pos_x = ?1, pos_y = ?2 WHERE id = ?3 AND kind = 'note'",
            rusqlite::params![x, y, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn delete_note(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // v1.3.2: soft delete — 矢印リンクの行も残す（復活時に一緒に戻るため、
    // 表示は list_relations 側で隠す）。
    p.conn
        .execute(
            "UPDATE virtual_bodies SET deleted_at = ?1 WHERE id = ?2 AND kind = 'note'",
            rusqlite::params![db::now_ms(), id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.2: コメントの復活の呪文 (Ctrl+Z)。矢印リンクも一緒に表示へ戻る。
#[tauri::command]
pub(crate) fn restore_note(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET deleted_at = NULL WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.5.16: プレビューのサイズ永続化（テキスト/画像/動画=幅のみ・PDF/Doc=幅＋高さ）。
/// v1.3.1 の set_panel_width（テキストエディタ幅）を高さ対応で置き換えた統合版。
/// height が None のときは panel_h を触らない（幅ハンドルだけの種別が高さを潰さない）。
#[tauri::command]
pub(crate) fn set_panel_size(
    id: String,
    width: f64,
    height: Option<f64>,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    match height {
        Some(h) => p
            .conn
            .execute(
                "UPDATE virtual_bodies SET panel_w = ?1, panel_h = ?2 WHERE id = ?3",
                rusqlite::params![width, h, id],
            )
            .map_err(|e| e.to_string())?,
        None => p
            .conn
            .execute(
                "UPDATE virtual_bodies SET panel_w = ?1 WHERE id = ?2",
                rusqlite::params![width, id],
            )
            .map_err(|e| e.to_string())?,
    };
    Ok(())
}

/// Update a note's visual style (width, font size, bold).
#[tauri::command]
pub(crate) fn set_note_style(
    id: String,
    width: f64,
    font_size: f64,
    bold: bool,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut st = read_note_style(p, &id)?;
    st.width = Some(width);
    st.font_size = Some(font_size);
    st.bold = if bold { Some(true) } else { None };
    write_note_style(p, &id, &st)
}

/// v1.4.16 付箋の充実: 付箋の色を設定（`color=None` で既定＝テーマの黄に戻す）。
#[tauri::command]
pub(crate) fn set_note_color(id: String, color: Option<String>, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut st = read_note_style(p, &id)?;
    st.color = color;
    write_note_style(p, &id, &st)
}

/// v1.4.16 付箋の充実: 先頭行を見出しにするフォントサイズを設定（`title_size=None` で
/// 見出しなし＝全体を本文サイズで表示）。
#[tauri::command]
pub(crate) fn set_note_title(
    id: String,
    title_size: Option<f64>,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut st = read_note_style(p, &id)?;
    st.title_size = title_size;
    write_note_style(p, &id, &st)
}

/// Create a note linked to a text file; its content is loaded from the file.
#[tauri::command]
pub(crate) fn create_note_linked(
    path: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let id = uuid();
    let st = NoteStyle {
        width: Some(320.0),
        font_size: Some(13.0),
        file_ref: Some(path),
        ..Default::default()
    };
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at, kind, note_text, style_ref)
             VALUES (?1, 'note:sentinel', ?2, ?3, ?4, ?5, 'note', ?6, ?7)",
            rusqlite::params![
                id,
                note_label(&text),
                x,
                y,
                db::now_ms(),
                text,
                serde_json::to_string(&st).map_err(|e| e.to_string())?
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// v1.3-④: コミットしてテキストファイルとして保存 — write a note's text into
/// the project folder and link the note to it (edits then write back to file).
#[tauri::command]
pub(crate) fn commit_note_to_file(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let text: String = p
        .conn
        .query_row(
            "SELECT note_text FROM virtual_bodies WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
            |r| Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default()),
        )
        .map_err(|e| e.to_string())?;
    let mut st = read_note_style(p, &id)?;
    if let Some(f) = st.file_ref {
        return Ok(f); // already a file-linked note
    }
    // Filename from the first line of the comment (sanitized).
    let first = text.lines().next().unwrap_or("").trim();
    let mut stem: String = first
        .chars()
        .take(24)
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect::<String>()
        .trim()
        .to_string();
    if stem.is_empty() {
        stem = "コメント".into();
    }
    let dest = unique_path(&p.root, &format!("{}.txt", stem));
    std::fs::write(&dest, &text).map_err(|e| e.to_string())?;
    let dest_str = dest.to_string_lossy().to_string();
    st.file_ref = Some(dest_str.clone());
    write_note_style(p, &id, &st)?;
    Ok(dest_str)
}

/// Read a UTF-8 text file (for file-linked notes).
#[tauri::command]
pub(crate) fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// v1.4.13: Decode arbitrary text bytes for a read-only 仮身 view, WITHOUT
/// touching the source bytes. Tries BOM → strict UTF-8 → chardetng-detected
/// legacy encoding (Shift_JIS / EUC-JP / UTF-16 …). Converting the file to
/// UTF-8 (実身化) is the filter pipeline's job, not this view's.
pub(crate) fn decode_text_bytes(bytes: &[u8]) -> (String, String) {
    // BOM が付いていればそれを最優先（encoding_rs が BOM 除去まで面倒を見る）。
    if let Some((enc, _bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (cow, _, _) = enc.decode(bytes);
        return (cow.into_owned(), enc.name().to_string());
    }
    // 速い道: 正当な UTF-8 ならそのまま（大半のケース）。
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), "UTF-8".to_string());
    }
    // それ以外はレガシーエンコーディングを推定してデコード。
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let enc = det.guess(None, true);
    let (cow, _, _) = enc.decode(bytes);
    (cow.into_owned(), enc.name().to_string())
}

/// 仮身ビューの読み取り結果。`encoding` は推定に使ったエンコ名（UI のバッジ用）。
#[derive(Serialize)]
pub struct ViewText {
    pub text: String,
    pub encoding: String,
}

/// v1.3.9 ①: Read a 仮身's LIVE text content for a read-only inline view,
/// without committing (= without creating a 実身). Resolves the same window as
/// 閲覧 (committed → sibling → origin → CAS → fork source).
/// v1.4.13: 非UTF-8（Shift_JIS 等）は推定デコードして表示する（非破壊）。
#[tauri::command]
pub(crate) fn read_view_text(id: String, state: State<AppState>) -> Result<ViewText, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let real_id: String = p
        .conn
        .query_row(
            "SELECT real_id FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let src = resolve_view_path(p, &id, &real_id);
    let bytes = std::fs::read(&src).map_err(|e| e.to_string())?;
    let (text, encoding) = decode_text_bytes(&bytes);
    Ok(ViewText { text, encoding })
}

/// Write a UTF-8 text file (for file-linked notes → in-canvas editing).
#[tauri::command]
pub(crate) fn write_text_file(path: String, text: String) -> Result<(), String> {
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

