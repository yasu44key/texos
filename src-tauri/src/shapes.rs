#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Shapes (v1.3-②③: 図形描画＋図形による思考グループ)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ShapeInfo {
    pub id: String,
    pub kind: String,
    pub pos_x: f64,
    pub pos_y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation: f64,
    pub fill: String,
    pub opacity: f64,
    pub z_order: i64,
    pub label: String,
    /// v1.5.6 WS-M M1（§8.1）: メンバーをロック（アウト不可・削除不可・イン警告・🔒表示）。
    pub locked: bool,
    /// v1.5.6 実機FB: ロック時のメンバースナップショット（カード id）。幾何ではなく所属＝
    /// 図形をリサイズ/Shift移動してもメンバーシップは変わらない。非ロック時は空。
    pub members: Vec<String>,
}

#[tauri::command]
pub(crate) fn create_shape(
    kind: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    state: State<AppState>,
) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let id = uuid();
    p.conn
        .execute(
            "INSERT INTO shapes (id, kind, pos_x, pos_y, width, height, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, kind, x, y, width, height, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub(crate) fn list_shapes(state: State<AppState>) -> Result<Vec<ShapeInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        .prepare(
            "SELECT id, kind, pos_x, pos_y, width, height, rotation, fill, opacity, z_order, label, locked, members
             FROM shapes WHERE deleted_at IS NULL ORDER BY z_order, created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ShapeInfo {
                id: r.get(0)?,
                kind: r.get(1)?,
                pos_x: r.get(2)?,
                pos_y: r.get(3)?,
                width: r.get(4)?,
                height: r.get(5)?,
                rotation: r.get(6)?,
                fill: r.get(7)?,
                opacity: r.get(8)?,
                z_order: r.get(9)?,
                label: r.get(10)?,
                locked: r.get::<_, i64>(11)? != 0,
                members: r
                    .get::<_, Option<String>>(12)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// v1.5.6 WS-M M1（§8.1）: 図形「メンバーをロック」トグル。
/// ロック ON = その瞬間に乗っていたカード id をスナップショット（member_ids はフロントの
/// cardsOnShape が計算）。以後のメンバーシップは**所属**であって幾何ではない＝図形の
/// リサイズや Shift 移動では変わらない（実機FB 2026-07-08）。ロック OFF = メンバー消去
///（ゆるいグループ＝動的包含へ戻る）。
#[tauri::command]
pub(crate) fn set_shape_locked(
    id: String,
    locked: bool,
    member_ids: Option<Vec<String>>,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let members: Option<String> = if locked {
        Some(serde_json::to_string(&member_ids.unwrap_or_default()).map_err(|e| e.to_string())?)
    } else {
        None
    };
    p.conn
        .execute(
            "UPDATE shapes SET locked=?1, members=?2 WHERE id=?3",
            rusqlite::params![locked as i64, members, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.5.6: ロック中図形へのドラッグイン（§8.1 警告付きで可）＝メンバーへ追加。
#[tauri::command]
pub(crate) fn add_shape_member(
    shape_id: String,
    card_id: String,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let cur: Option<String> = p
        .conn
        .query_row(
            "SELECT members FROM shapes WHERE id=?1 AND locked=1 AND deleted_at IS NULL",
            rusqlite::params![shape_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let mut members: Vec<String> = cur
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if !members.contains(&card_id) {
        members.push(card_id);
        p.conn
            .execute(
                "UPDATE shapes SET members=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&members).map_err(|e| e.to_string())?, shape_id],
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Move/resize/rotate a shape.
#[tauri::command]
pub(crate) fn update_shape_geom(
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rotation: f64,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE shapes SET pos_x=?1, pos_y=?2, width=?3, height=?4, rotation=?5 WHERE id=?6",
            rusqlite::params![x, y, width, height, rotation, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn set_shape_style(
    id: String,
    fill: String,
    opacity: f64,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE shapes SET fill=?1, opacity=?2 WHERE id=?3",
            rusqlite::params![fill, opacity, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn set_shape_label(id: String, label: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE shapes SET label=?1 WHERE id=?2",
            rusqlite::params![label, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn set_shape_z(id: String, z: i64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE shapes SET z_order=?1 WHERE id=?2",
            rusqlite::params![z, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn delete_shape(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // v1.3.2: soft delete（restore_shape で復活）。
    p.conn
        .execute(
            "UPDATE shapes SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![db::now_ms(), id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.2: 図形の復活の呪文 (Ctrl+Z)。
#[tauri::command]
pub(crate) fn restore_shape(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE shapes SET deleted_at = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

