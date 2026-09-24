#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

pub(crate) const FOLDER_MIME: &str = "application/vnd.texos.folder";

/// Import a file from the local filesystem into the project CAS + DB.
/// Folders become links: a TexoS project folder → project reference card,
/// any other folder → リンクフォルダ card (the folder itself is NOT copied).
/// Files that already live inside the project folder start committed (実身) —
/// they are the project's own real files, so no 仮身 stage is needed.
#[tauri::command]
pub(crate) fn import_file(
    path: String,
    label: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    let src = std::path::Path::new(&path);
    if src.is_dir() && texos_dir(src).join("texos.db").exists() {
        return link_project(path, x, y, state);
    }
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    import_file_into(p, path, label, x, y)
}

/// import_file の本体（v1.6: 「新規カードを登録」ビルトインがジョブ専用接続から呼ぶため
/// State ロックと分離。意味論はコマンド版と同一 — プロジェクト内=実身／外部=窓仮身）。
pub(crate) fn import_file_into(
    p: &Project,
    path: String,
    label: String,
    x: f64,
    y: f64,
) -> Result<String, String> {
    let src = std::path::Path::new(&path);
    if src.is_dir() {
        let real_id = format!("folder:{}", cas::hash_bytes(path.as_bytes()));
        p.conn
            .execute(
                "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
                 VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![real_id, FOLDER_MIME, db::now_ms()],
            )
            .map_err(|e| e.to_string())?;
        let display = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("フォルダ")
            .to_string();
        let vid = uuid();
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, folder_ref)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![vid, real_id, display, x, y, db::now_ms(), path],
            )
            .map_err(|e| e.to_string())?;
        return Ok(vid);
    }

    let mime = mime_from_path(src);
    let orig_ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());

    // A file already inside the project folder (and not under .texos) IS the
    // project's real file: register it committed, pointing at itself.
    // v1.3.11 #10: 大文字小文字・パス表記の差で誤判定しないよう path_inside で堅牢に判定
    // （配置でもドロップでも、プロジェクト内のファイルは実身として取り込む）。
    let in_project =
        path_inside(src, &p.root) && !path_inside(src, &texos_dir(&p.root));

    // 問題点1（同プロジェクト内の実体を2つ以上の実身にしない＝システムの根幹）:
    // 既に同じ実ファイルを committed_path に持つ生きた実身カードがあれば、2枚目以降は
    // 内部仮身として登録する。v1.3.10 窓モデル: window_of = その実身カードの id を持たせ、
    // カードID で追従する（リネーム・編集に強い・内容ハッシュ衝突に左右されない）。
    // committed_path=NULL なのでこの仮身を削除しても実体には触れない。
    if in_project {
        let dup: Option<(String, String)> = p
            .conn
            .query_row(
                "SELECT id, real_id FROM virtual_bodies
                 WHERE committed_path = ?1 AND deleted_at IS NULL
                 ORDER BY created_at LIMIT 1",
                rusqlite::params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((src_id, real_id)) = dup {
            let vid = uuid();
            let stem = src.file_name().and_then(|s| s.to_str()).unwrap_or(&label);
            let display_label = if label.is_empty() { stem } else { &label };
            p.conn
                .execute(
                    "INSERT INTO virtual_bodies
                        (id, real_id, label, pos_x, pos_y, created_at,
                         window_of, ext, mime_type)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        vid,
                        real_id,
                        display_label,
                        x,
                        y,
                        db::now_ms(),
                        src_id,
                        orig_ext,
                        mime
                    ],
                )
                .map_err(|e| e.to_string())?;
            return Ok(vid);
        }
    }

    let hash = cas::store_file(&p.cas_root, src).map_err(|e| e.to_string())?;
    let file_size = src.metadata().map_err(|e| e.to_string())?.len();

    db::insert_real_body(&p.conn, &hash, &mime, file_size, orig_ext.as_deref())
        .map_err(|e| e.to_string())?;

    let vid = uuid();
    let stem = src.file_name().and_then(|s| s.to_str()).unwrap_or(&label);
    let display_label = if label.is_empty() { stem } else { &label };

    let committed_path = in_project.then(|| path.clone());
    let committed_mtime = if in_project { file_mtime_ms(src) } else { None };

    // v1.3.6: 拡張子/MIME は仮身側にも保存する。空ファイル等は実身を共有するため、
    // 実身の ext/mime だけだと別名・別形式が同じ型に潰れる（ビットマップ.bmp が ods 等）。
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at,
                 origin_path, committed_path, committed_mtime, ext, mime_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                vid,
                hash,
                display_label,
                x,
                y,
                db::now_ms(),
                path,
                committed_path,
                committed_mtime,
                orig_ext,
                mime
            ],
        )
        .map_err(|e| e.to_string())?;

    Ok(vid)
}

#[tauri::command]
pub(crate) fn list_objects(state: State<AppState>) -> Result<Vec<VirtualBodyInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        .prepare(
            // v1.3.6: ext/mime は仮身優先（COALESCE）。空ファイル共有で実身に潰れた型を、
            // そのファイル本来の拡張子で表示・分類するため。NULL は実身へフォールバック。
            "SELECT v.id, v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), v.pos_x, v.pos_y,
                    v.committed_path, v.committed_mtime, v.project_ref, COALESCE(v.ext, r.ext), v.expanded,
                    v.url_ref, v.memo, v.z_order, v.folder_ref, v.origin_path, v.panel_w,
                    v.kind, v.filter_ref, v.window_of, v.last_run, v.panel_h, v.web_meta
             FROM virtual_bodies v
             JOIN real_bodies r ON r.id = v.real_id
             WHERE v.desktop_id IS NULL AND v.deleted_at IS NULL
               AND (v.kind IS NULL OR v.kind <> 'note') -- 付箋は list_notes（NoteCard ビュー）が担う
             ORDER BY v.created_at",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            let committed_path: Option<String> = row.get(6)?;
            let committed_mtime: Option<i64> = row.get(7)?;
            let folder_ref: Option<String> = row.get(14)?;
            let project_ref: Option<String> = row.get(8)?;
            // v1.3.11 #12: リンクフォルダカードは folder_ref の存在で cur_mtime を決める
            // （フォルダが消えていれば None → フロントが薄赤シグナルにする）。
            // v1.8.8: プロジェクトカードも同様。参照先に .texos/texos.db が無ければ None
            // （＝リンク切れ）。フォルダだけ残ってプロジェクトでなくなった場合も切れ扱い。
            let cur_mtime = committed_path
                .as_deref()
                .and_then(|p| file_mtime_ms(std::path::Path::new(p)))
                .or_else(|| {
                    folder_ref
                        .as_deref()
                        .and_then(|p| file_mtime_ms(std::path::Path::new(p)))
                })
                .or_else(|| {
                    project_ref.as_deref().and_then(|p| {
                        let root = std::path::Path::new(p);
                        if texos_dir(root).join("texos.db").exists() {
                            file_mtime_ms(root)
                        } else {
                            None
                        }
                    })
                });
            let committed_modified = match (cur_mtime, committed_mtime) {
                (Some(cur), Some(m)) => cur > m,
                _ => false,
            };
            Ok(VirtualBodyInfo {
                id: row.get(0)?,
                real_id: row.get(1)?,
                label: row.get(2)?,
                mime_type: row.get(3)?,
                pos_x: row.get(4)?,
                pos_y: row.get(5)?,
                committed_path,
                committed_modified,
                project_ref,
                ext: row.get(9)?,
                expanded: row.get::<_, i64>(10)? != 0,
                url_ref: row.get(11)?,
                memo: row.get(12)?,
                z_order: row.get(13)?,
                folder_ref,
                origin_path: row.get(15)?,
                cur_mtime,
                view_source: None,
                panel_w: row.get(16)?,
                kind: row.get(17)?,
                filter_ref: row.get(18)?,
                window_of: row.get(19)?,
                last_run: row.get(20)?,
                panel_h: row.get(21)?,
                web_meta: row.get(22)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut items = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    // v1.3-⑩: report how each file 仮身's view resolves, so the frontend can
    // show 薄赤 when the window lost its live target (CAS fallback / nothing).
    for it in &mut items {
        if it.project_ref.is_some()
            || it.url_ref.is_some()
            || it.folder_ref.is_some()
            || it.filter_ref.is_some()
            || it.real_id.starts_with("drop:")
        {
            continue;
        }
        it.view_source = Some(
            match resolve_view_conn(&p.conn, &p.cas_root, &it.id, &it.real_id, 0) {
                Some((_, s)) => s.to_string(),
                None => "none".to_string(),
            },
        );
    }
    Ok(items)
}

#[tauri::command]
pub(crate) fn get_thumbnail(id: String, state: State<AppState>) -> Result<String, String> {
    // Resolve the path then release the lock before the heavy decode.
    let (obj_path, cache_path) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        // v1.3.9 ②: 閲覧では CAS に書き込まない（スナップは実身化時のみ）。
        let real_id: String = p
            .conn
            .query_row(
                "SELECT real_id FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let cache = texos_dir(&p.root)
            .join("thumbs")
            .join(format!("{}.jpg", real_id));
        (resolve_view_path(p, &id, &real_id), cache)
    };

    // Disk cache: real bodies are content-addressed (immutable), so a cached
    // thumbnail at least as new as the source is always valid.
    let src_mtime = file_mtime_ms(&obj_path);
    if let (Some(cache_m), Some(src_m)) = (file_mtime_ms(&cache_path), src_mtime) {
        if cache_m >= src_m {
            if let Ok(bytes) = std::fs::read(&cache_path) {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                return Ok(format!("data:image/jpeg;base64,{}", b64));
            }
        }
    }

    let bytes = std::fs::read(&obj_path).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let thumb = image::DynamicImage::ImageRgb8(img.thumbnail(200, 200).to_rgb8());
    let mut buf: Vec<u8> = Vec::new();
    thumb
        .write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| e.to_string())?;
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
        let _ = std::fs::write(&cache_path, &buf);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
    Ok(format!("data:image/jpeg;base64,{}", b64))
}

/// Return a media file (audio etc.) from the CAS as a base64 data URL.
#[tauri::command]
pub(crate) fn get_media_data_url(id: String, state: State<AppState>) -> Result<String, String> {
    let (obj_path, mime) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        // v1.3.9 ②: 閲覧では CAS に書き込まない。
        let (real_id, mime): (String, String) = p
            .conn
            .query_row(
                "SELECT v.real_id, COALESCE(v.mime_type, r.mime_type) FROM virtual_bodies v
                 JOIN real_bodies r ON r.id = v.real_id WHERE v.id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        (resolve_view_path(p, &id, &real_id), mime)
    };
    let bytes = std::fs::read(&obj_path).map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// Larger aspect-preserving preview as a PNG data URL (max 900px side).
#[tauri::command]
pub(crate) fn get_preview(id: String, state: State<AppState>) -> Result<String, String> {
    let obj_path = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        // v1.3.9 ②: 閲覧では CAS に書き込まない。
        let real_id: String = p
            .conn
            .query_row(
                "SELECT real_id FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        resolve_view_path(p, &id, &real_id)
    };
    let bytes = std::fs::read(&obj_path).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let preview = img.thumbnail(900, 900); // preserves aspect ratio
    let mut buf: Vec<u8> = Vec::new();
    preview
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
    Ok(format!("data:image/png;base64,{}", b64))
}

/// v2.0 t23d Phase 2: カード面サムネイルの保存（3Dプレビューの「サムネイル更新」）。
/// フロントの canvas.toDataURL（PNG data URL）を受け、画像カードと同じキャッシュ
/// （.texos/thumbs/<real_id>.jpg）へ JPEG で書く。get_thumbnail がそのまま配信する
/// ＝カード面の表示機構は画像カードと完全共用（glb 専用の表示コードを持たない）。
#[tauri::command]
pub(crate) fn save_card_thumbnail(
    id: String,
    data_url: String,
    state: State<AppState>,
) -> Result<(), String> {
    let cache_path = {
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
        texos_dir(&p.root)
            .join("thumbs")
            .join(format!("{}.jpg", real_id))
    };
    let b64 = data_url
        .split_once("base64,")
        .map(|(_, b)| b)
        .ok_or("data URL の形式が不正です")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("data URL のデコードに失敗: {e}"))?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    // カード面（150×115）表示用。少し大きめに残してズーム時も荒れないように。
    let thumb = image::DynamicImage::ImageRgb8(img.thumbnail(400, 400).to_rgb8());
    let mut buf: Vec<u8> = Vec::new();
    thumb
        .write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| e.to_string())?;
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&cache_path, &buf).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn move_object(id: String, x: f64, y: f64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET pos_x = ?1, pos_y = ?2 WHERE id = ?3",
            rusqlite::params![x, y, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Deserialize)]
pub struct MoveItem {
    pub id: String,
    pub x: f64,
    pub y: f64,
}

/// Move several objects at once (group drag / auto layout).
#[tauri::command]
pub(crate) fn move_objects(moves: Vec<MoveItem>, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut stmt = p
        .conn
        .prepare("UPDATE virtual_bodies SET pos_x = ?1, pos_y = ?2 WHERE id = ?3")
        .map_err(|e| e.to_string())?;
    for m in &moves {
        stmt.execute(rusqlite::params![m.x, m.y, m.id])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Set a card's stacking order (higher = front).
#[tauri::command]
pub(crate) fn set_z_order(id: String, z: i64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET z_order = ?1 WHERE id = ?2",
            rusqlite::params![z, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist a card's in-canvas expansion state.
#[tauri::command]
pub(crate) fn set_expanded(id: String, expanded: bool, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET expanded = ?1 WHERE id = ?2",
            rusqlite::params![expanded as i64, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Save the canvas view transform (pan/zoom) for this project.
#[tauri::command]
pub(crate) fn set_view_transform(x: f64, y: f64, scale: f64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let json = serde_json::to_string(&ViewTransform { x, y, scale }).map_err(|e| e.to_string())?;
    db::meta_set(&p.conn, "view_transform", &json).map_err(|e| e.to_string())?;
    Ok(())
}

/// Load the saved canvas view transform, if any.
#[tauri::command]
pub(crate) fn get_view_transform(state: State<AppState>) -> Result<Option<ViewTransform>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    match db::meta_get(&p.conn, "view_transform").map_err(|e| e.to_string())? {
        Some(s) => Ok(serde_json::from_str(&s).ok()),
        None => Ok(None),
    }
}

/// Trash location for a deleted 仮身's committed file: .texos/trash/<vid>__<name>.
/// Deterministic from (vid, committed_path) so restore needs no extra bookkeeping.
pub(crate) fn trash_path(root: &Path, vid: &str, committed: &Path) -> PathBuf {
    let name = committed
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    texos_dir(root).join("trash").join(format!("{}__{}", vid, name))
}

/// First free "name", "name (2)", "name (3)" … inside dir.
pub(crate) fn unique_file_path(dir: &Path, name: &str) -> PathBuf {
    let direct = dir.join(name);
    if !direct.exists() {
        return direct;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), Some(e.to_string())),
        _ => (name.to_string(), None),
    };
    for n in 2u32.. {
        let cand = match &ext {
            Some(e) => dir.join(format!("{} ({}).{}", stem, n, e)),
            None => dir.join(format!("{} ({})", stem, n)),
        };
        if !cand.exists() {
            return cand;
        }
    }
    unreachable!()
}

/// v1.3.2: soft delete (復活の呪文の前提). The row only gets deleted_at set;
/// relations / diff_stack stay so restore_object brings the arrows back too.
/// The committed real file moves to .texos/trash/ instead of being removed.
/// The shared CAS object is kept, as before.
#[tauri::command]
pub(crate) fn delete_object(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let committed: Option<String> = p
        .conn
        .query_row(
            "SELECT committed_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten();
    if let Some(cp) = committed {
        let path = std::path::Path::new(&cp);
        // v1.3.11 問題点3（多層防御）: 同じ実ファイル（committed_path）を指す「生きた」
        // カードが他にもいれば、実ファイルは退避せずカードだけをソフト削除する。問題点1で
        // 通常は実身は1枚に保たれるが、旧データや想定外経路で2枚以上の実身が同じ実体を
        // 指してしまった場合に、片方の削除でもう片方の実体まで消える事故を防ぐ。
        let shared: i64 = p
            .conn
            .query_row(
                "SELECT COUNT(*) FROM virtual_bodies
                 WHERE committed_path = ?1 AND id <> ?2 AND deleted_at IS NULL",
                rusqlite::params![cp, id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let path = if shared > 0 { Path::new("") } else { path };
        // Safety: only touch files inside this project's folder.
        if path.starts_with(&p.root) && path.is_file() {
            let trash = trash_path(&p.root, &id, path);
            if let Some(parent) = trash.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::rename(path, &trash).is_err() {
                // フォールバック: コピー＋削除。両方失敗ならファイルは残す（安全側）。
                if std::fs::copy(path, &trash).is_ok() {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    p.conn
        .execute(
            "UPDATE virtual_bodies SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![db::now_ms(), id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.2: 復活の呪文 — undo a soft delete. Clears deleted_at and moves the
/// trashed committed file back into place. If the spot was taken meanwhile,
/// the file comes back under a "name (2)" style unique name. If the trashed
/// file is gone entirely, the card revives as a 仮身 (CAS still has the
/// import-time content as 保険).
#[tauri::command]
pub(crate) fn restore_object(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let row: Option<(Option<i64>, Option<String>)> = p
        .conn
        .query_row(
            "SELECT deleted_at, committed_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let (deleted_at, committed) = row.ok_or("復活できません（カードが見つかりません）")?;
    if deleted_at.is_none() {
        return Ok(()); // already alive — idempotent for repeated undo
    }

    if let Some(cp) = &committed {
        let target = PathBuf::from(cp);
        let trash = trash_path(&p.root, &id, &target);
        let mut new_committed: Option<PathBuf> = None; // None = 仮身として復活
        if target.is_file() && !trash.is_file() {
            // The file is (still or again) there and nothing was trashed.
            new_committed = Some(target.clone());
        } else if trash.is_file() {
            let dest = if target.exists() {
                let name = target
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file");
                unique_file_path(target.parent().unwrap_or(&p.root), name)
            } else {
                target.clone()
            };
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let moved = std::fs::rename(&trash, &dest).is_ok() || {
                let copied = std::fs::copy(&trash, &dest).is_ok();
                if copied {
                    let _ = std::fs::remove_file(&trash);
                }
                copied
            };
            if moved {
                new_committed = Some(dest);
            }
        }
        match new_committed {
            Some(dest) if dest != target => {
                p.conn
                    .execute(
                        "UPDATE virtual_bodies SET committed_path = ?1 WHERE id = ?2",
                        rusqlite::params![dest.to_string_lossy().to_string(), id],
                    )
                    .map_err(|e| e.to_string())?;
            }
            Some(_) => {}
            None => {
                p.conn
                    .execute(
                        "UPDATE virtual_bodies
                         SET committed_path = NULL, committed_mtime = NULL WHERE id = ?1",
                        rusqlite::params![id],
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    p.conn
        .execute(
            "UPDATE virtual_bodies SET deleted_at = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.2 メモ技術③: UI操作のアンドゥ履歴（操作レコードのJSON）を
/// プロジェクトDBに保存する。プロジェクトごとに独立し、再起動後も巻き戻せる。
#[tauri::command]
pub(crate) fn set_undo_stack(json: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    db::meta_set(&p.conn, "ui_undo_stack", &json).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_undo_stack(state: State<AppState>) -> Result<Option<String>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(None),
    };
    db::meta_get(&p.conn, "ui_undo_stack").map_err(|e| e.to_string())
}

/// ディレクトリ内のファイル数・合計バイトを再帰集計（存在しなければ 0,0）。
pub(crate) fn dir_stats(dir: &Path) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => {
                    let (c, b) = dir_stats(&e.path());
                    count += c;
                    bytes += b;
                }
                Ok(m) => {
                    count += 1;
                    bytes += m.len();
                }
                Err(_) => {}
            }
        }
    }
    (count, bytes)
}

#[derive(Serialize, Default)]
pub(crate) struct CleanReport {
    thumbs_count: u64,
    thumbs_bytes: u64,
    temp_count: u64,
    temp_bytes: u64,
}

/// v1.4.15 データの掃除A（履歴・キャッシュ）: サムネイル（`.texos/thumbs`）と一時プレビュー
/// （`TEMP/texos_view`）のキャッシュを削除する。どちらも内容アドレス（不変）／一時ファイルで、
/// 必要時に自動再生成されるので消して安全。UNDO/REDO 履歴のクリアはフロント（useUndo の
/// clearHistory）が担当。**ゴミ箱（`.texos/trash`＝削除の「復活の呪文」）と孤立CASの掃除は
/// 本コマンドでは触らない**（B＝正規化GCの担当。到達可能性の判定が要るため別リビジョンで）。
#[tauri::command]
pub(crate) fn clean_caches(state: State<AppState>) -> Result<CleanReport, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let thumbs = texos_dir(&p.root).join("thumbs");
    let (thumbs_count, thumbs_bytes) = dir_stats(&thumbs);
    let _ = std::fs::remove_dir_all(&thumbs); // 無ければ無視。次回サムネイル生成で再作成される。
    let temp = temp_view_dir();
    let (temp_count, temp_bytes) = dir_stats(&temp);
    let _ = std::fs::remove_dir_all(&temp);
    Ok(CleanReport {
        thumbs_count,
        thumbs_bytes,
        temp_count,
        temp_bytes,
    })
}

/// v1.4.15 データの掃除B（正規化＝孤立CASのGC）の中核。CAS 内の全ブロブを走査し、**生きた
/// 参照に含まれない孤立ブロブ**の一覧（パスとサイズ）と CAS 総数を返す。
///
/// 到達可能性（生存）の定義＝**`virtual_bodies` に存在する real_id（deleted_at 問わず）**。
/// ソフト削除中（ゴミ箱＝復活の呪文で戻せる）カードの実身も生存扱いにするため、行が存在すれば
/// 参照とみなす。notes は CAS を参照しない。ブロブのハッシュはパス（`<first2>/<rest>`）から
/// 復元するので中身は読まない。
///
/// 注意（既知の限界）: 本判定は**現プロジェクト単体**の視点。v1.3.11 以降のフォークは子CASへ
/// 内容をスナップして自己完結するため親CASに依存しないが、**それ以前の古いフォークが親（この
/// プロジェクト）のCASに依存している場合、親でその実身のカードが消えていると孤立扱いになりうる**。
/// 掃除は dry-run→確認つきで、古いフォークは作り直し（再フォーク／コミット）を案内する。
pub(crate) fn scan_orphan_cas(p: &Project) -> Result<(Vec<(PathBuf, u64)>, u64), String> {
    let mut reachable: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = p
            .conn
            .prepare("SELECT DISTINCT real_id FROM virtual_bodies")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows.flatten() {
            reachable.insert(r);
        }
    }
    let mut orphans: Vec<(PathBuf, u64)> = Vec::new();
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(&p.cas_root) {
        for sub in rd.flatten() {
            let subpath = sub.path();
            let prefix = match subpath.file_name().and_then(|s| s.to_str()) {
                Some(s) if s.len() == 2 => s.to_string(),
                _ => continue, // ハッシュの先頭2文字ディレクトリ以外は無視
            };
            if let Ok(files) = std::fs::read_dir(&subpath) {
                for f in files.flatten() {
                    let meta = match f.metadata() {
                        Ok(m) if m.is_file() => m,
                        _ => continue,
                    };
                    total += 1;
                    let Some(rest) = f.file_name().to_str().map(|s| s.to_string()) else {
                        continue;
                    };
                    let hash = format!("{}{}", prefix, rest);
                    if !reachable.contains(&hash) {
                        orphans.push((f.path(), meta.len()));
                    }
                }
            }
        }
    }
    Ok((orphans, total))
}

#[derive(Serialize, Default)]
pub(crate) struct GcCasReport {
    orphan_count: u64,
    orphan_bytes: u64,
    total_count: u64,
}

// ---------------------------------------------------------------------------
// v1.4.19 ゴミ箱を開く — 削除済み（ソフトデリート）の一覧・復活・完全削除。
// 復活の呪文（Ctrl+Z）は UNDO スタックの寿命に縛られるが、こちらは deleted_at 行を
// 直接読むので履歴を掃除した後でも復活できる。「空にする」で行と trash ファイルを
// 物理削除し、以後は正規化（GC）が実身 CAS を回収できるようになる（＝出口の完成）。
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct DeletedItem {
    id: String,
    /// "card" | "note" | "shape"
    kind: String,
    label: String,
    /// カード=拡張子または種別（filter/url/launcher…）、図形=形（rect/ellipse/arrow）
    detail: Option<String>,
    deleted_at: i64,
}

#[tauri::command]
pub(crate) fn list_deleted(state: State<AppState>) -> Result<Vec<DeletedItem>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut items: Vec<DeletedItem> = Vec::new();
    {
        let mut stmt = p
            .conn
            .prepare(
                "SELECT id, COALESCE(kind,'file'), label, ext, deleted_at
                 FROM virtual_bodies WHERE deleted_at IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let kind: String = r.get(1)?;
                let ext: Option<String> = r.get(3)?;
                Ok(DeletedItem {
                    id: r.get(0)?,
                    kind: if kind == "note" { "note".into() } else { "card".into() },
                    label: r.get(2)?,
                    detail: if kind != "file" && kind != "note" { Some(kind) } else { ext },
                    deleted_at: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        items.extend(rows.flatten());
    }
    {
        let mut stmt = p
            .conn
            .prepare("SELECT id, kind, label, deleted_at FROM shapes WHERE deleted_at IS NOT NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DeletedItem {
                    id: r.get(0)?,
                    kind: "shape".into(),
                    label: r.get(2)?,
                    detail: Some(r.get::<_, String>(1)?),
                    deleted_at: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        items.extend(rows.flatten());
    }
    items.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    Ok(items)
}

#[derive(Serialize, Default)]
pub(crate) struct PurgeReport {
    cards: u64,
    shapes: u64,
    relations: u64,
    files: u64,
    bytes: u64,
}

/// ゴミ箱の完全削除（**元に戻せない**）。`ids=None` は全部＝「ゴミ箱を空にする」
/// （そのときは単独で削除された矢印の行も一緒に掃く）。行の削除に伴い、対応する
/// `.texos/trash/<vid>__*` ファイルと、端点を失う relations 行も物理削除する。
#[tauri::command]
pub(crate) fn purge_deleted(ids: Option<Vec<String>>, state: State<AppState>) -> Result<PurgeReport, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let filter: Option<std::collections::HashSet<String>> =
        ids.map(|v| v.into_iter().collect());
    let wanted = |id: &str| filter.as_ref().map(|s| s.contains(id)).unwrap_or(true);
    let mut report = PurgeReport::default();

    let cards: Vec<String> = {
        let mut stmt = p
            .conn
            .prepare("SELECT id FROM virtual_bodies WHERE deleted_at IS NOT NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.flatten().filter(|id| wanted(id)).collect()
    };
    let trash_dir = texos_dir(&p.root).join("trash");
    for id in &cards {
        // 退避された実ファイル（<vid>__名前）を削除
        if let Ok(rd) = std::fs::read_dir(&trash_dir) {
            let prefix = format!("{}__", id);
            for f in rd.flatten() {
                if f.file_name().to_string_lossy().starts_with(&prefix) {
                    if let Ok(m) = f.metadata() {
                        report.bytes += m.len();
                    }
                    if std::fs::remove_file(f.path()).is_ok() {
                        report.files += 1;
                    }
                }
            }
        }
        // 端点を失う矢印は行ごと削除（復活先が消えるため）
        report.relations += p
            .conn
            .execute(
                "DELETE FROM relations WHERE from_id = ?1 OR to_id = ?1",
                rusqlite::params![id],
            )
            .map_err(|e| e.to_string())? as u64;
        // 旧スキーマの diff_stack FK の保険（現行は未使用テーブル）
        let _ = p.conn.execute(
            "DELETE FROM diff_stack WHERE virtual_id = ?1",
            rusqlite::params![id],
        );
        report.cards += p
            .conn
            .execute(
                "DELETE FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NOT NULL",
                rusqlite::params![id],
            )
            .map_err(|e| e.to_string())? as u64;
    }

    let shapes: Vec<String> = {
        let mut stmt = p
            .conn
            .prepare("SELECT id FROM shapes WHERE deleted_at IS NOT NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.flatten().filter(|id| wanted(id)).collect()
    };
    for id in &shapes {
        report.shapes += p
            .conn
            .execute(
                "DELETE FROM shapes WHERE id = ?1 AND deleted_at IS NOT NULL",
                rusqlite::params![id],
            )
            .map_err(|e| e.to_string())? as u64;
    }

    // 空にする（全部）のときは、単独で削除された矢印も掃く
    if filter.is_none() {
        report.relations += p
            .conn
            .execute("DELETE FROM relations WHERE deleted_at IS NOT NULL", [])
            .map_err(|e| e.to_string())? as u64;
    }
    Ok(report)
}

/// B: 孤立CASの dry-run（削除せずに件数・サイズだけ返す。確認ダイアログ用）。
#[tauri::command]
pub(crate) fn gc_cas_scan(state: State<AppState>) -> Result<GcCasReport, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let (orphans, total) = scan_orphan_cas(p)?;
    let orphan_bytes: u64 = orphans.iter().map(|(_, b)| *b).sum();
    Ok(GcCasReport {
        orphan_count: orphans.len() as u64,
        orphan_bytes,
        total_count: total,
    })
}

/// B: 孤立CASを実際に削除する（元に戻せない・フロントで確認済み前提）。
#[tauri::command]
pub(crate) fn gc_cas_clean(state: State<AppState>) -> Result<GcCasReport, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let (orphans, total) = scan_orphan_cas(p)?;
    let mut removed = 0u64;
    let mut freed = 0u64;
    for (path, bytes) in &orphans {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
            freed += *bytes;
        }
    }
    Ok(GcCasReport {
        orphan_count: removed,
        orphan_bytes: freed,
        total_count: total,
    })
}

#[tauri::command]
pub(crate) fn duplicate_virtual(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let vid = uuid();
    // v1.3.10「仮身を作成」: 複製先の仮身は常に「複製元カードそのもの」を window_of
    // （カードID・不変）で指す。内容(real_id)やパスではなくカードIDで追従するので、複製元の
    // リネーム・編集にも強い。
    //   - 複製元が内部実身（committed_path あり）→ window_of = 複製元の id（その実身の窓）
    //   - 複製元が内部仮身（window_of あり）→ window_of = 複製元の id（仮身の仮身・チェーン）
    //   - 複製元が外部仮身（origin_path のみ）→ 従来どおり origin_path を複製（外部レーン・凍結）
    // ※「窓対象をコピーして元を指す」旧②動作（特別→仮身を複製）は廃止。
    let src: Option<(Option<String>, Option<String>, Option<String>)> = p
        .conn
        .query_row(
            "SELECT committed_path, origin_path, window_of FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let (s_committed, s_origin, s_window) = src.ok_or("複製元が見つかりません")?;
    let (window_of, origin_path): (Option<String>, Option<String>) =
        if s_committed.is_some() || s_window.is_some() {
            (Some(id.clone()), None)
        } else {
            (None, s_origin)
        };
    // v1.3.10 補完: ext/mime_type（そのファイルの属性）も引き継ぐ。空ファイル等は実身を
    // 共有するため、これを継がないと .docx の仮身が実身(real_bodies)側の型に落ちて text 扱いに
    // なり、実身化/リネームで .txt になってしまう（UIの気づき2 問題点3）。
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at, folder_ref, origin_path,
                 window_of, kind, filter_ref, ext, mime_type)
             SELECT ?1, real_id, label, pos_x + 36.0, pos_y + 36.0, ?2, folder_ref, ?4,
                 ?5, kind, filter_ref, ext, mime_type
             FROM virtual_bodies WHERE id = ?3",
            rusqlite::params![vid, db::now_ms(), id, origin_path, window_of],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// v1.4.13 プロジェクト間ワークフロー共有: 選択した部分グラフ（カード＋コメント＋
/// 選択内どうしの関係矢印）を別プロジェクトの DB+CAS へ書き込む（`fork_structure_into`
/// を「現プロジェクト全体」から「選択部分グラフ」へ一般化したもの）。
///   - `move_out=false` ＝仮身化してコピー: 送り先CASへ中身をスナップし、`origin_path` で
///     元プロジェクトのライブファイルを指す仮身にする（＝跨プロジェクトの窓・元に依存・元は残る）。
///   - `move_out=true` ＝実身化して移動: 送り先ルートにコミット済みの独立した実身として置き、
///     元プロジェクトからはソフトデリート（Ctrl+Zで復活可・実ファイルは .texos/trash へ退避）。
/// フィルタ／ランチャー等の設定（filter_ref/kind/url_ref…）も継承する。プロジェクト境界を
/// またぐ矢印は張らない（両端が選択内の relations だけ運ぶ）。
/// v1.4.17: 選択中の図形（コンテナ枠）も一緒に運ぶ（移動なら元をソフトデリート）。
/// 戻り値＝運んだ数＋送り先で新規採番されたID群（v1.4.22 ②: フロントが送り先を開いた
/// ときに選択状態で迎え、重なったカード類をドラッグで分離できるようにする）。
#[derive(Serialize)]
pub struct SendSelectionResult {
    pub moved: usize,
    pub new_card_ids: Vec<String>,
    pub new_note_ids: Vec<String>,
    pub new_shape_ids: Vec<String>,
}

#[tauri::command]
pub(crate) fn send_selection_to_project(
    card_ids: Vec<String>,
    note_ids: Vec<String>,
    shape_ids: Vec<String>,
    target_path: String,
    move_out: bool,
    state: State<AppState>,
) -> Result<SendSelectionResult, String> {
    // v1.5.2 WS-C: 切替ガードの精密版 — カード移動は評価中フローの構造破壊になるため、
    // **現在のプロジェクトに束縛された**実行中ジョブがある間だけ拒否（単なる切替は自由）。
    ensure_no_jobs_bound_to_current(state.inner())?;
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let dest_root = PathBuf::from(&target_path);
    // 送り先が自分自身は不可。
    if let (Ok(a), Ok(b)) = (
        std::fs::canonicalize(&dest_root),
        std::fs::canonicalize(&p.root),
    ) {
        if a == b {
            return Err("送り先が現在のプロジェクトと同じです".into());
        }
    }
    let dest_db = texos_dir(&dest_root).join("texos.db");
    if !dest_db.exists() {
        return Err("送り先はTexoSプロジェクトではありません".into());
    }
    let dest_cas = texos_dir(&dest_root).join("cas");
    std::fs::create_dir_all(&dest_cas).map_err(|e| e.to_string())?;

    // ID再割当（旧→新UUID）。カード・コメント両方を送り先で新IDにして衝突を避け、
    // window_of・relations 端点を貼り替える。
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for old in card_ids.iter().chain(note_ids.iter()) {
        id_map.entry(old.clone()).or_insert_with(uuid);
    }

    struct CardRow {
        real_id: String,
        label: String,
        x: f64,
        y: f64,
        kind: Option<String>,
        filter_ref: Option<String>,
        url_ref: Option<String>,
        folder_ref: Option<String>,
        project_ref: Option<String>,
        memo: Option<String>,
        z: i64,
        ext: Option<String>,
        mime_type: Option<String>,
        window_of: Option<String>,
        mime: String,
        // v1.4.17 カード統一: 付箋(kind='note')も同じ経路で運ぶ。
        note_text: Option<String>,
        style_ref: Option<String>,
    }

    p.conn
        .execute(
            "ATTACH DATABASE ?1 AS dst",
            rusqlite::params![dest_db.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;

    // 送り先がまだ旧バージョンでしか開かれていなくても受け取れるよう、この関数の INSERT が
    // 使う新列だけ best-effort で足しておく（送り先を開けば正規の migrate が走る）。
    let _ = p
        .conn
        .execute("ALTER TABLE dst.virtual_bodies ADD COLUMN note_text TEXT", []);
    let _ = p
        .conn
        .execute("ALTER TABLE dst.virtual_bodies ADD COLUMN style_ref TEXT", []);
    let _ = p
        .conn
        .execute("ALTER TABLE dst.shapes ADD COLUMN deleted_at INTEGER", []);

    let result = (|| -> Result<SendSelectionResult, String> {
        let mut moved = 0usize;
        let mut new_card_ids: Vec<String> = Vec::new();
        let mut new_note_ids: Vec<String> = Vec::new();
        let mut new_shape_ids: Vec<String> = Vec::new();
        let note_set: std::collections::HashSet<&String> = note_ids.iter().collect();

        // v1.4.17 カード統一: 付箋(note_ids)もカードと同じ経路で運ぶ（旧・専用セクションは廃止）。
        for old_id in card_ids.iter().chain(note_ids.iter()) {
            let row: Option<CardRow> = p
                .conn
                .query_row(
                    "SELECT v.real_id, v.label, v.pos_x, v.pos_y, v.kind, v.filter_ref,
                            v.url_ref, v.folder_ref, v.project_ref, v.memo, v.z_order,
                            v.ext, v.mime_type, v.window_of,
                            COALESCE(v.mime_type, r.mime_type), v.note_text, v.style_ref
                     FROM main.virtual_bodies v JOIN main.real_bodies r ON r.id = v.real_id
                     WHERE v.id = ?1 AND v.deleted_at IS NULL",
                    rusqlite::params![old_id],
                    |r| {
                        Ok(CardRow {
                            real_id: r.get(0)?,
                            label: r.get(1)?,
                            x: r.get(2)?,
                            y: r.get(3)?,
                            kind: r.get(4)?,
                            filter_ref: r.get(5)?,
                            url_ref: r.get(6)?,
                            folder_ref: r.get(7)?,
                            project_ref: r.get(8)?,
                            memo: r.get(9)?,
                            z: r.get(10)?,
                            ext: r.get(11)?,
                            mime_type: r.get(12)?,
                            window_of: r.get(13)?,
                            mime: r.get(14)?,
                            note_text: r.get(15)?,
                            style_ref: r.get(16)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some(c) = row else { continue };
            let new_id = id_map.get(old_id).unwrap().clone();
            // window_of は指し先が選択内なら新IDへ貼替、選択外なら NULL（跨プロジェクトの
            // 内部窓は張らない＝実体はスナップ／origin で解決する）。
            let new_window = c.window_of.as_ref().and_then(|w| id_map.get(w).cloned());

            // 参照する real_body 行を送り先へ（filter/url 等の実体無しカードも型情報を運ぶ）。
            p.conn
                .execute(
                    "INSERT OR IGNORE INTO dst.real_bodies
                        (id,mime_type,size,created_at,codec_ref,tags,ext)
                     SELECT id,mime_type,size,created_at,codec_ref,tags,ext
                     FROM main.real_bodies WHERE id = ?1",
                    rusqlite::params![c.real_id],
                )
                .map_err(|e| e.to_string())?;

            // 付箋の file_ref は元プロジェクトのコミット先を指すので運ばない（fork と同じ扱い）。
            let style_out: Option<String> = if c.kind.as_deref() == Some("note") {
                c.style_ref.as_deref().and_then(|s| {
                    serde_json::from_str::<serde_json::Value>(s).ok().map(|mut v| {
                        if let Some(o) = v.as_object_mut() {
                            o.remove("file_ref");
                        }
                        v.to_string()
                    })
                })
            } else {
                c.style_ref.clone()
            };

            // カード本体を新IDで挿入。committed_path/mtime は一旦 NULL、project_ref は
            // 持ち出さない（プロジェクト参照は跨がない）。
            p.conn
                .execute(
                    "INSERT INTO dst.virtual_bodies
                        (id,real_id,label,pos_x,pos_y,created_at,kind,filter_ref,url_ref,
                         folder_ref,memo,z_order,ext,mime_type,window_of,note_text,style_ref)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                    rusqlite::params![
                        new_id,
                        c.real_id,
                        c.label,
                        c.x,
                        c.y,
                        db::now_ms(),
                        c.kind,
                        c.filter_ref,
                        c.url_ref,
                        c.folder_ref,
                        c.memo,
                        c.z,
                        c.ext,
                        c.mime_type,
                        new_window,
                        c.note_text,
                        style_out,
                    ],
                )
                .map_err(|e| e.to_string())?;

            // ファイル種カード（URL/フォルダ/プロジェクトでない）は中身を送り先へ運ぶ。
            let is_file = c.kind.as_deref().map(|k| k == "file").unwrap_or(true)
                && c.url_ref.is_none()
                && c.folder_ref.is_none()
                && c.project_ref.is_none();
            if is_file {
                let live = resolve_view_path(p, old_id, &c.real_id);
                if live.is_file() {
                    // 送り先CASへ内容アドレスでスナップ（新 real_id = H）。
                    if let Ok(h) = cas::store_file(&dest_cas, &live) {
                        let size = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0) as i64;
                        p.conn
                            .execute(
                                "INSERT OR IGNORE INTO dst.real_bodies
                                    (id,mime_type,size,created_at,ext)
                                 VALUES (?1,?2,?3,?4,?5)",
                                rusqlite::params![h, c.mime, size, db::now_ms(), c.ext],
                            )
                            .map_err(|e| e.to_string())?;
                        if move_out {
                            // 実身化: 送り先ルートへ実ファイルを書き出し、独立した実身にする。
                            let ext = resolve_ext(c.ext.as_deref(), &c.mime);
                            let filename = ensure_ext(&c.label, &ext);
                            let dest_file = unique_path(&dest_root, &filename);
                            std::fs::copy(&live, &dest_file).map_err(|e| e.to_string())?;
                            let mtime = file_mtime_ms(&dest_file).unwrap_or_else(db::now_ms);
                            p.conn
                                .execute(
                                    "UPDATE dst.virtual_bodies
                                     SET real_id = ?1, committed_path = ?2, committed_mtime = ?3,
                                         origin_path = NULL, window_of = NULL
                                     WHERE id = ?4",
                                    rusqlite::params![
                                        h,
                                        dest_file.to_string_lossy().to_string(),
                                        mtime,
                                        new_id
                                    ],
                                )
                                .map_err(|e| e.to_string())?;
                        } else {
                            // 仮身化してコピー: 元のライブファイルを origin で指す窓にする
                            // （中身は送り先CASの H が保険。fork の per-file 処理と同型）。
                            // window_of（内部窓）は NULL にする＝跨プロジェクトでは内部レーンを
                            // 使わず外部レーン(origin)で解決させる（内部窓の指し先も同じくコピー
                            // された未コミット仮身なので resolve_live_file が None を返し、CAS
                            // 保険＝薄赤に倒れてしまうのを防ぐ。import_file 外部取り込みと同じ形）。
                            p.conn
                                .execute(
                                    "UPDATE dst.virtual_bodies
                                     SET real_id = ?1, origin_path = ?2, committed_path = NULL,
                                         window_of = NULL
                                     WHERE id = ?3",
                                    rusqlite::params![
                                        h,
                                        live.to_string_lossy().to_string(),
                                        new_id
                                    ],
                                )
                                .map_err(|e| e.to_string())?;
                        }
                    }
                }
            }
            moved += 1;
            if note_set.contains(old_id) {
                new_note_ids.push(new_id);
            } else {
                new_card_ids.push(new_id);
            }
        }

        // 関係矢印: 両端が選択内のものだけ、新IDへ貼り替えて運ぶ。先に Vec へ集めてから挿入
        // （prepared statement と execute の借用が重ならないように）。
        let edges: Vec<(String, String, String)> = {
            let mut stmt = p
                .conn
                .prepare(
                    "SELECT from_id,to_id,kind FROM main.relations WHERE deleted_at IS NULL",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        for (f, t, k) in edges {
            if let (Some(nf), Some(nt)) = (id_map.get(&f), id_map.get(&t)) {
                p.conn
                    .execute(
                        "INSERT INTO dst.relations (from_id,to_id,kind,created_at)
                         VALUES (?1,?2,?3,?4)",
                        rusqlite::params![nf, nt, k, db::now_ms()],
                    )
                    .map_err(|e| e.to_string())?;
            }
        }

        // v1.4.17: 選択中の図形（コンテナとして使う枠）も一緒に運ぶ。図形は関係の端点に
        // ならず ID 参照もされないため、新規採番で挿入するだけでよい。
        for old_id in &shape_ids {
            let s: Option<(String, f64, f64, f64, f64, f64, String, f64, i64, String)> = p
                .conn
                .query_row(
                    "SELECT kind,pos_x,pos_y,width,height,rotation,fill,opacity,z_order,label
                     FROM main.shapes WHERE id = ?1 AND deleted_at IS NULL",
                    rusqlite::params![old_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                            r.get(9)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some((kind, x, y, w, h, rot, fill, opacity, z, label)) = s else {
                continue;
            };
            let new_sid = uuid();
            p.conn
                .execute(
                    "INSERT INTO dst.shapes
                        (id,kind,pos_x,pos_y,width,height,rotation,fill,opacity,z_order,label,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    rusqlite::params![new_sid, kind, x, y, w, h, rot, fill, opacity, z, label, db::now_ms()],
                )
                .map_err(|e| e.to_string())?;
            moved += 1;
            new_shape_ids.push(new_sid);
        }

        Ok(SendSelectionResult {
            moved,
            new_card_ids,
            new_note_ids,
            new_shape_ids,
        })
    })();

    let _ = p.conn.execute("DETACH DATABASE dst", []);
    let res = result?;

    // 移動: 元プロジェクトからソフトデリート（既存の削除フローと同じ deleted_at 方式・
    // Ctrl+Z で復活可）。コミット済み実ファイルは .texos/trash へ退避（delete_object と同じ
    // 共有ガード付き＝同じ実体を指す別の生きたカードがいれば退避しない）。
    if move_out {
        // v1.4.17 カード統一: 付箋も同じソフトデリート経路（committed_path 無し＝退避なし）。
        for old_id in card_ids.iter().chain(note_ids.iter()) {
            let committed: Option<String> = p
                .conn
                .query_row(
                    "SELECT committed_path FROM virtual_bodies WHERE id = ?1",
                    rusqlite::params![old_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .ok()
                .flatten()
                .flatten();
            if let Some(cp) = committed {
                let shared: i64 = p
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM virtual_bodies
                         WHERE committed_path = ?1 AND id <> ?2 AND deleted_at IS NULL",
                        rusqlite::params![cp, old_id],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let path = Path::new(&cp);
                if shared == 0 && path.starts_with(&p.root) && path.is_file() {
                    let trash = trash_path(&p.root, old_id, path);
                    if let Some(parent) = trash.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if std::fs::rename(path, &trash).is_err() {
                        if std::fs::copy(path, &trash).is_ok() {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET deleted_at = ?1 WHERE id = ?2",
                rusqlite::params![db::now_ms(), old_id],
            );
        }
        // 端点が選択（カード or コメント）に触れる矢印もソフトデリート。復活の呪文で戻る。
        let all_ids: Vec<&String> = card_ids.iter().chain(note_ids.iter()).collect();
        for id in all_ids {
            let _ = p.conn.execute(
                "UPDATE relations SET deleted_at = ?1
                 WHERE deleted_at IS NULL AND (from_id = ?2 OR to_id = ?2)",
                rusqlite::params![db::now_ms(), id],
            );
        }
        // v1.4.17: 選択図形も移動なら元からソフトデリート。
        for old_id in &shape_ids {
            let _ = p.conn.execute(
                "UPDATE shapes SET deleted_at = ?1 WHERE id = ?2",
                rusqlite::params![db::now_ms(), old_id],
            );
        }
    }

    Ok(res)
}

#[tauri::command]
pub(crate) fn rename_object(id: String, label: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET label = ?1 WHERE id = ?2",
            rusqlite::params![label, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.10 TexoS内リネーム: 実身（committed_path を持つカード）の実ファイルを TexoS から
/// 安全に改名する。実ファイル改名 + committed_path/mtime/label/ext を一括更新。カードIDは
/// 不変なので window_of で指す仮身は全部自動追従する（OS でリネームすると committed_path が
/// ずれて喪失＝赤になるのを防ぐ）。連番ファイル名（FILE(2)）の付け替えにも使う。
#[tauri::command]
pub(crate) fn rename_real(id: String, new_name: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let committed: Option<String> = p
        .conn
        .query_row(
            "SELECT committed_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten();
    let cp = committed.ok_or("このカードは実身ではありません（仮身は「表示名を変更」を使ってください）")?;
    let old = PathBuf::from(&cp);
    if !old.is_file() {
        return Err("実ファイルが見つかりません（先に再リンク／CASから復元してください）".into());
    }
    // 安全: プロジェクトフォルダ内のファイルだけを改名対象にする。
    if !old.starts_with(&p.root) {
        return Err("プロジェクト外のファイルは TexoS から改名できません".into());
    }
    let dir = old.parent().ok_or("親フォルダが取得できません")?;

    // 新名に拡張子が無ければ、元ファイルの拡張子を維持する。
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("名前が空です".into());
    }
    let filename = match old.extension().and_then(|s| s.to_str()) {
        Some(ext) if Path::new(new_name).extension().is_none() => ensure_ext(new_name, ext),
        _ => new_name.to_string(),
    };
    // 同名なら変更なし（unique_path に渡すと自分自身と衝突して (2) が付くのを防ぐ）。
    if dir.join(&filename) == old {
        return Ok(cp);
    }
    let dest = unique_path(dir, &filename);
    std::fs::rename(&old, &dest).map_err(|e| e.to_string())?;

    let dest_str = dest.to_string_lossy().to_string();
    let mtime = file_mtime_ms(&dest).unwrap_or_else(db::now_ms);
    let new_ext = dest
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    // v1.3.10 補完: label（表示名）は変更しない。表示名は「拡張子あり＝実ファイル名と一致／
    // 拡張子なし＝独自の表示名」という情報を持つので、リネームで .docx を勝手に付けない
    // （ユーザー指摘）。実ファイル名・committed_path・mtime・ext（属性）だけ更新する。
    p.conn
        .execute(
            "UPDATE virtual_bodies
             SET committed_path = ?1, committed_mtime = ?2, ext = ?3
             WHERE id = ?4",
            rusqlite::params![dest_str, mtime, new_ext, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(dest_str)
}

#[tauri::command]
pub(crate) fn create_relation(
    from_id: String,
    to_id: String,
    kind: Option<String>,
    state: State<AppState>,
) -> Result<Option<i64>, String> {
    if from_id == to_id {
        return Err("cannot link an object to itself".into());
    }
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // v1.4 オートフロー⑤: フィルタカードに入る/出る矢印は自動的にフロー
    // （新しい矢印種のUIは作らない — 確定方針1）。
    let kind = kind.unwrap_or_else(|| {
        // v1.5.4 WS-P2: op カード（トリガ/タイマ/ウォッチャ）に触れる矢印は自動的に
        // kind='req'（リクエスト矢印）。eval_filter の入力収集は kind='flow' 限定クエリ
        // なので req はデータレーンに乗らない＝フロー評価から自然に不可視。
        // v1.5.6 WS-M M1: op:"in"/"out"（IN/OUT バーチャルカード）は**データレーンの住人**
        // ＝透過ノードなので req ではなく flow を張る（分岐の精密化・§9.4）。
        let op_spec_of = |vid: &str| -> Option<String> {
            p.conn
                .query_row(
                    "SELECT filter_ref FROM virtual_bodies WHERE id = ?1 AND kind = 'op'",
                    rusqlite::params![vid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .ok()
                .flatten()
                .flatten()
        };
        let is_req_op = |vid: &str| -> bool {
            op_spec_of(vid)
                .map(|s| !ops::is_io_op_spec(&s))
                .unwrap_or(false)
        };
        if is_req_op(&from_id) || is_req_op(&to_id) {
            return "req".to_string();
        }
        // v1.3.11: フィルタに加えランチャー（kind='launcher'）への矢印もフロー扱い
        // （ランチャーに入る {in} 入力＝開くファイルを flow エッジで表す）。
        // v1.5.6: IN/OUT バーチャルカードもフローノード（op_spec_of が Some＝ここに来るのは
        // is_io_op_spec のときだけ）。
        let is_flow_node = |vid: &str| -> bool {
            if op_spec_of(vid).is_some() {
                return true;
            }
            p.conn
                .query_row(
                    "SELECT 1 FROM virtual_bodies WHERE id = ?1 AND kind IN ('filter','launcher')",
                    rusqlite::params![vid],
                    |_| Ok(()),
                )
                .optional()
                .ok()
                .flatten()
                .is_some()
        };
        if is_flow_node(&from_id) || is_flow_node(&to_id) {
            "flow".to_string()
        } else {
            "link".to_string()
        }
    });
    let inserted = p
        .conn
        .execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at)
             SELECT ?1, ?2, ?3, ?4
             WHERE NOT EXISTS (
                 SELECT 1 FROM relations
                 WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3 AND deleted_at IS NULL
             )",
            rusqlite::params![from_id, to_id, kind, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    // 重複でスキップされたときに「最後に挿入された無関係な行のid」を返さない。
    Ok((inserted > 0).then(|| p.conn.last_insert_rowid()))
}

#[tauri::command]
pub(crate) fn list_relations(state: State<AppState>) -> Result<Vec<RelationInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        // Soft-deleted cards keep their relation rows (for restore); hide
        // arrows that are deleted themselves or whose endpoint is currently
        // deleted. v1.4.17: 付箋も virtual_bodies に統一されたので anti-join は1本。
        .prepare(
            "SELECT id, from_id, to_id, kind, attrs FROM relations rel
             WHERE rel.deleted_at IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM virtual_bodies d
                 WHERE d.deleted_at IS NOT NULL AND (d.id = rel.from_id OR d.id = rel.to_id)
               )
             ORDER BY created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RelationInfo {
                id: row.get(0)?,
                from_id: row.get(1)?,
                to_id: row.get(2)?,
                kind: row.get(3)?,
                attrs: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// v1.5.3 WS-P1: 矢印の制御属性（{when, carry, port}）をマージ更新する。
/// when="always"・carry=""・port=0 は「キーを消す＝従来へ戻す」。全キーが消えたら
/// attrs を NULL に戻す（NULL=完全後方互換の原則を保つ）。
#[tauri::command]
pub(crate) fn set_relation_attrs(
    id: i64,
    when: Option<String>,
    carry: Option<String>,
    port: Option<i64>,
    role: Option<String>,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let cur: Option<String> = p
        .conn
        .query_row(
            "SELECT attrs FROM relations WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .map_err(|_| "矢印が見つかりません".to_string())?;
    let mut v: serde_json::Value = cur
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let obj = v.as_object_mut().ok_or("attrs が壊れています")?;
    if let Some(w) = when {
        if w == "always" || w.is_empty() {
            obj.remove("when");
        } else {
            obj.insert("when".into(), serde_json::json!(w));
        }
    }
    if let Some(c) = carry {
        if c.is_empty() {
            obj.remove("carry");
        } else {
            obj.insert("carry".into(), serde_json::json!(c));
        }
    }
    if let Some(n) = port {
        if n <= 0 {
            obj.remove("port");
        } else {
            obj.insert("port".into(), serde_json::json!(n));
        }
    }
    // v1.8 役割ポート: 空文字＝役割を外す（ふつうの入力に戻す）。
    if let Some(r) = role {
        if r.is_empty() {
            obj.remove("role");
        } else {
            obj.insert("role".into(), serde_json::json!(r));
        }
    }
    let new_attrs: Option<String> = if obj.is_empty() { None } else { Some(v.to_string()) };
    p.conn
        .execute(
            "UPDATE relations SET attrs = ?1 WHERE id = ?2",
            rusqlite::params![new_attrs, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.5.3 WS-P1: カードへ入る flow 矢印の一覧（実効順=port昇順→接続順）。
/// 「入力の順序」ダイアログの表示用。
#[derive(Serialize)]
pub struct FlowInputInfo {
    pub relation_id: i64,
    pub label: String,
    pub port: Option<i64>,
    pub when: Option<String>,
}

#[tauri::command]
pub(crate) fn list_flow_inputs(id: String, state: State<AppState>) -> Result<Vec<FlowInputInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut stmt = p
        .conn
        .prepare(
            "SELECT rel.id, v.label, rel.attrs FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
             WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
             ORDER BY rel.created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        })
        .map_err(|e| e.to_string())?;
    let mut items: Vec<FlowInputInfo> = rows
        .filter_map(|r| r.ok())
        .map(|(rid, label, attrs)| {
            let a = crate::flow::parse_edge_attrs(attrs.as_deref());
            FlowInputInfo {
                relation_id: rid,
                label,
                port: a.port,
                when: a.when,
            }
        })
        .collect();
    // eval_filter と同じ実効順（安定ソート: port なしは末尾＝接続順）。
    items.sort_by_key(|i| i.port.unwrap_or(i64::MAX));
    Ok(items)
}

/// v1.5.3 WS-P1: 入力の順序を確定する（ダイアログの並びどおり port=1..N を書く）。
/// {inN} の番号・セレクタの first-match 順がこの並びになる。
#[tauri::command]
pub(crate) fn set_input_order(relation_ids: Vec<i64>, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    for (i, rid) in relation_ids.iter().enumerate() {
        let cur: Option<String> = p
            .conn
            .query_row(
                "SELECT attrs FROM relations WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![rid],
                |r| r.get(0),
            )
            .map_err(|_| "矢印が見つかりません".to_string())?;
        let mut v: serde_json::Value = cur
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("port".into(), serde_json::json!((i + 1) as i64));
        }
        p.conn
            .execute(
                "UPDATE relations SET attrs = ?1 WHERE id = ?2",
                rusqlite::params![v.to_string(), rid],
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn delete_relation(id: i64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // v1.3.2: soft delete（restore_relation で復活）。
    p.conn
        .execute(
            "UPDATE relations SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![db::now_ms(), id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.2: 矢印リンクの復活の呪文 (Ctrl+Z)。
#[tauri::command]
pub(crate) fn restore_relation(id: i64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE relations SET deleted_at = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Commit an object: copy its immutable real body into the project folder root
/// as a named, editable file. "参照から所有への転換".
#[tauri::command]
pub(crate) fn commit_object(
    id: String,
    force: Option<bool>,
    state: State<AppState>,
) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    commit_object_inner(p, &id, force.unwrap_or(false))
}

pub(crate) fn commit_object_inner(p: &Project, id: &str, force: bool) -> Result<String, String> {
    // 通常コミット＝ライブ内容でFix。直前にスナップショットも追従させておくと、
    // CAS（保険）＝コミット内容になり、force再コミットは「コミット時点へ戻る」。
    if !force {
        refresh_origin_snapshot(p, id);
    }
    let existing: Option<String> = p
        .conn
        .query_row(
            "SELECT committed_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    // v1.3.6: ファイル名の拡張子・MIME は仮身優先（COALESCE）。空ファイル共有で実身が
    // ods 等に潰れていても、本来の拡張子（.docx 等）で書き出す／開く（.docx.ods を防ぐ）。
    let (real_id, label, mime, stored_ext): (String, String, String, Option<String>) = p
        .conn
        .query_row(
            "SELECT v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext)
             FROM virtual_bodies v JOIN real_bodies r ON r.id = v.real_id
             WHERE v.id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|e| e.to_string())?;

    // CAS backup: make sure the import-time snapshot exists locally. Inherited
    // fork 仮身 keep it in an ancestor's CAS until their first commit — copy the
    // SNAPSHOT (not the live view), so the CAS stays content-addressed.
    let local_cas = cas::object_path(&p.cas_root, &real_id);
    if !local_cas.exists() {
        if let Some(chain) = resolve_cas_path_conn(&p.conn, &p.cas_root, &real_id, 0) {
            if let Some(parent) = local_cas.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&chain, &local_cas).map_err(|e| e.to_string())?;
        }
    }

    // Source bytes (v1.3-⑩): normal commit fixes the LIVE content the 仮身 is a
    // window onto; force (編集を破棄して再コミット) restores the import-time
    // snapshot from the CAS (タイムマシン).
    let src = if force {
        if !local_cas.exists() {
            return Err("実身（CAS）が見つかりません".into());
        }
        local_cas.clone()
    } else {
        resolve_view_path(p, &id, &real_id)
    };
    if !src.exists() {
        return Err("実身の内容が見つかりません（元ファイル喪失・CASにもありません）".into());
    }

    let dest: PathBuf = match &existing {
        Some(path) if std::path::Path::new(path).exists() => {
            if !force {
                return Ok(path.clone());
            }
            PathBuf::from(path)
        }
        _ => {
            let ext = resolve_ext(stored_ext.as_deref(), &mime);
            let filename = ensure_ext(&label, &ext);
            unique_path(&p.root, &filename)
        }
    };

    std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
    let dest_str = dest.to_string_lossy().to_string();
    let mtime = file_mtime_ms(&dest).unwrap_or_else(db::now_ms);

    p.conn
        .execute(
            "UPDATE virtual_bodies SET committed_path = ?1, committed_mtime = ?2 WHERE id = ?3",
            rusqlite::params![dest_str, mtime, id],
        )
        .map_err(|e| e.to_string())?;

    Ok(dest_str)
}

/// v1.3.9 問題点2: 実体喪失の実身を、リネーム/移動された実ファイルへ繋ぎ直す。
/// committed_path / committed_mtime だけを差し替える（real_id・CAS は不変）。
/// 「正しいファイルを選び直す → 無ければ CASから復元（= commit_object force）」の
/// 前者にあたる。自動 CAS ロールバックの代わりにユーザーが明示的に選ぶための経路。
#[tauri::command]
pub(crate) fn relink_object(id: String, new_path: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let np = std::path::Path::new(&new_path);
    if !np.is_file() {
        return Err("選んだファイルが見つかりません".into());
    }
    let mtime = file_mtime_ms(np);
    p.conn
        .execute(
            "UPDATE virtual_bodies SET committed_path = ?1, committed_mtime = ?2 WHERE id = ?3",
            rusqlite::params![new_path, mtime, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.11 #12: リンクフォルダカードの参照先を選び直す（リネーム・移動でリンク切れに
/// なったフォルダを繋ぎ直す）。folder_ref を更新するだけ。選んだ先が TexoS プロジェクト
/// なら project_ref に切り替える（フォルダ→プロジェクトの昇格は import_file と同じ扱い）。
/// v1.8.8: 上記「プロジェクトなら昇格」はコメントのみで未実装だったので実装（set_project_link）。
#[tauri::command]
pub(crate) fn relink_folder(id: String, new_path: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let np = std::path::Path::new(&new_path);
    if !np.is_dir() {
        return Err("選んだフォルダが見つかりません".into());
    }
    if texos_dir(np).join("texos.db").exists() {
        return set_project_link(p, &id, &new_path);
    }
    p.conn
        .execute(
            "UPDATE virtual_bodies SET folder_ref = ?1 WHERE id = ?2",
            rusqlite::params![new_path, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.8.8: プロジェクトカード（project_ref）の参照先を選び直す。フォルダ整理で
/// プロジェクトを移動・改名するとリンクが切れる（参照は絶対パス）ため、その繋ぎ直し。
/// 選んだ先が TexoS プロジェクトでなければ拒否する（普通のフォルダへ格下げはしない）。
#[tauri::command]
pub(crate) fn relink_project(id: String, new_path: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let np = std::path::Path::new(&new_path);
    if !np.is_dir() {
        return Err("選んだフォルダが見つかりません".into());
    }
    if !texos_dir(np).join("texos.db").exists() {
        return Err("選んだフォルダはTexoSプロジェクトではありません（.texos/texos.db がありません）".into());
    }
    set_project_link(p, &id, &new_path)
}

/// project_ref を差し替える共通部。real_id はパス由来のセンチネル（project:<hash(path)>）
/// なので、新しいパスのセンチネルへ付け替えて link_project で作った場合と同じ状態に揃える。
/// リンクフォルダからの昇格にも使うので folder_ref を外し kind='project' に確定する。
fn set_project_link(p: &Project, id: &str, new_path: &str) -> Result<(), String> {
    let real_id = ensure_project_real_body(&p.conn, new_path)?;
    let n = p
        .conn
        .execute(
            "UPDATE virtual_bodies
                SET project_ref = ?1, real_id = ?2, folder_ref = NULL, kind = 'project'
              WHERE id = ?3",
            rusqlite::params![new_path, real_id, id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("カードが見つかりません".into());
    }
    Ok(())
}

#[derive(Serialize)]
pub struct InnerFolder {
    pub name: String,
    pub path: String,
    /// .texos/texos.db を持つ＝内包プロジェクト
    pub is_project: bool,
    /// このプロジェクトのキャンバスに、既にこのフォルダを指すカード（project_ref /
    /// folder_ref）が生きている
    pub linked: bool,
}

/// v1.8.8: 現在のプロジェクトフォルダ直下のサブフォルダ一覧（「内包フォルダへのリンクを配置」用）。
/// 隠しフォルダ（.texos 等「.」始まり）は除く。内包プロジェクトか普通のフォルダかと、
/// 既にリンクカードがあるかを返す。並びは プロジェクト→フォルダ、各々名前順。
#[tauri::command]
pub(crate) fn list_inner_folders(state: State<AppState>) -> Result<Vec<InnerFolder>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // 既存リンクの照合は表記ゆれ（大小・区切り・末尾区切り）を吸収して行う。
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let linked: std::collections::HashSet<String> = {
        let mut stmt = p
            .conn
            .prepare(
                "SELECT COALESCE(project_ref, folder_ref) FROM virtual_bodies
                  WHERE deleted_at IS NULL AND (project_ref IS NOT NULL OR folder_ref IS NOT NULL)",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).map(|s| norm(&s)).collect()
    };
    let mut out = Vec::new();
    for ent in std::fs::read_dir(&p.root).map_err(|e| e.to_string())? {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        out.push(InnerFolder {
            is_project: texos_dir(&path).join("texos.db").exists(),
            linked: linked.contains(&norm(&path_str)),
            name,
            path: path_str,
        });
    }
    out.sort_by(|a, b| {
        b.is_project
            .cmp(&a.is_project)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[derive(Serialize)]
pub struct CommitAllWarning {
    pub label: String,
    /// "url" | "external_project" | "program"
    pub kind: String,
    pub detail: String,
}

#[derive(Serialize)]
pub struct CommitAllReport {
    pub committed: Vec<String>,
    pub already: usize,
    pub warnings: Vec<CommitAllWarning>,
    pub errors: Vec<String>,
}

pub(crate) const PROGRAM_EXTS: &[&str] = &["exe", "msi", "bat", "cmd", "com", "lnk", "ps1", "vbs", "scr"];

/// Commit every object so the project folder becomes a self-contained,
/// portable Windows folder ("データ持ち出し"). External references — URLs and
/// linked projects outside this folder — cannot be materialized and are
/// reported as warnings; program files are committed but flagged.
#[tauri::command]
pub(crate) fn commit_all(dry_run: Option<bool>, state: State<AppState>) -> Result<CommitAllReport, String> {
    let dry = dry_run.unwrap_or(false);
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    struct Row {
        id: String,
        label: String,
        committed: Option<String>,
        project_ref: Option<String>,
        url_ref: Option<String>,
        ext: Option<String>,
        folder_ref: Option<String>,
        kind: Option<String>,
    }
    let rows: Vec<Row> = {
        let mut stmt = p
            .conn
            .prepare(
                "SELECT v.id, v.label, v.committed_path, v.project_ref, v.url_ref, COALESCE(v.ext, r.ext),
                        v.folder_ref, v.kind
                 FROM virtual_bodies v JOIN real_bodies r ON r.id = v.real_id
                 WHERE v.deleted_at IS NULL
                 ORDER BY v.created_at",
            )
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([], |row| {
                Ok(Row {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    committed: row.get(2)?,
                    project_ref: row.get(3)?,
                    url_ref: row.get(4)?,
                    ext: row.get(5)?,
                    folder_ref: row.get(6)?,
                    kind: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    let mut report = CommitAllReport {
        committed: vec![],
        already: 0,
        warnings: vec![],
        errors: vec![],
    };
    for r in rows {
        // v1.4.17: 付箋はキャンバスの注釈＝従来どおり持ち出し対象外（警告も出さない）。
        if r.kind.as_deref() == Some("note") {
            continue;
        }
        // v1.4: フィルタ・ドロップポイントはTexoS内の道具であり持ち出し対象ではない
        if matches!(r.kind.as_deref(), Some("filter") | Some("drop") | Some("launcher")) {
            report.warnings.push(CommitAllWarning {
                label: r.label,
                kind: r.kind.clone().unwrap_or_default(),
                detail: String::new(),
            });
            continue;
        }
        if let Some(url) = &r.url_ref {
            report.warnings.push(CommitAllWarning {
                label: r.label,
                kind: "url".into(),
                detail: url.clone(),
            });
            continue;
        }
        if let Some(folder) = &r.folder_ref {
            report.warnings.push(CommitAllWarning {
                label: r.label,
                kind: "folder".into(),
                detail: folder.clone(),
            });
            continue;
        }
        if let Some(proj) = &r.project_ref {
            // A contained subproject already lives inside this folder.
            if Path::new(proj).starts_with(&p.root) {
                report.already += 1;
            } else {
                report.warnings.push(CommitAllWarning {
                    label: r.label,
                    kind: "external_project".into(),
                    detail: proj.clone(),
                });
            }
            continue;
        }
        if let Some(ext) = &r.ext {
            if PROGRAM_EXTS.contains(&ext.to_lowercase().as_str()) {
                report.warnings.push(CommitAllWarning {
                    label: r.label.clone(),
                    kind: "program".into(),
                    detail: ext.clone(),
                });
            }
        }
        let exists = r
            .committed
            .as_deref()
            .map(|c| Path::new(c).exists())
            .unwrap_or(false);
        if exists {
            report.already += 1;
            continue;
        }
        if dry {
            report.committed.push(r.label);
            continue;
        }
        match commit_object_inner(p, &r.id, false) {
            Ok(_) => report.committed.push(r.label),
            Err(e) => report.errors.push(format!("{}: {}", r.label, e)),
        }
    }
    Ok(report)
}

#[derive(Serialize)]
pub struct ClipboardExportReport {
    /// labels whose file ended up on the clipboard
    pub copied: Vec<String>,
    /// number of paths put on the clipboard (== copied.len())
    pub count: usize,
    /// objects skipped because they have no file (URL / filter / drop point)
    pub warnings: Vec<CommitAllWarning>,
    pub errors: Vec<String>,
}

/// v1.3.4 クリップボード持ち出し: put the selected cards on the OS clipboard as
/// CF_HDROP (a file list). Mirrors `commit_all`'s "what is a file" rules:
/// URL / filter / drop-point cards have no file and are reported as warnings;
/// uncommitted 仮身 are committed first (エクスポート＝コミット原則) so the path
/// is a stable real file and no TEMP orphan is created. Folder / project links
/// contribute their real folder path directly.
#[tauri::command]
pub(crate) fn copy_objects_to_clipboard(
    ids: Vec<String>,
    state: State<AppState>,
) -> Result<ClipboardExportReport, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let (paths, report) = resolve_export_paths(p, &ids);
    if !paths.is_empty() {
        #[cfg(windows)]
        clipboard_out::set_clipboard_files(&paths)?;
        #[cfg(not(windows))]
        return Err("クリップボード持ち出しは Windows のみ対応です".into());
    }
    Ok(report)
}

/// v1.8.1: 「持ち出せるカード＝実ファイル」の解決規則を一箇所に集約する。
/// Ctrl+C（クリップボードへ CF_HDROP）とドラッグアウト（同じ CF_HDROP を OLE で
/// 手渡す）は運び方が違うだけで、何が持ち出せるかの規則は同一。規則をここにしか
/// 置かないので、両者がズレようがない。
fn resolve_export_paths(p: &Project, ids: &[String]) -> (Vec<String>, ClipboardExportReport) {
    let mut report = ClipboardExportReport {
        copied: vec![],
        count: 0,
        warnings: vec![],
        errors: vec![],
    };
    let mut paths: Vec<String> = vec![];

    for id in ids {
        let row: Result<(String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>), _> =
            p.conn.query_row(
                "SELECT label, committed_path, project_ref, url_ref, folder_ref, kind
                 FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            );
        let (label, committed, project_ref, url_ref, folder_ref, kind) = match row {
            Ok(x) => x,
            Err(_) => continue, // 消えた・存在しないIDは黙ってスキップ
        };

        // フィルタ・ドロップポイントはTexoS内の道具で持ち出し対象外（commit_all と同じ）
        if matches!(kind.as_deref(), Some("filter") | Some("drop") | Some("launcher")) {
            report.warnings.push(CommitAllWarning {
                label,
                kind: kind.unwrap_or_default(),
                detail: String::new(),
            });
            continue;
        }
        if let Some(url) = &url_ref {
            report.warnings.push(CommitAllWarning {
                label,
                kind: "url".into(),
                detail: url.clone(),
            });
            continue;
        }
        // フォルダ／プロジェクトリンクは実フォルダそのものを渡す
        if let Some(folder) = &folder_ref {
            if Path::new(folder).exists() {
                paths.push(folder.clone());
                report.copied.push(label);
            } else {
                report.errors.push(format!("{}: フォルダが見つかりません", label));
            }
            continue;
        }
        if let Some(proj) = &project_ref {
            if Path::new(proj).exists() {
                paths.push(proj.clone());
                report.copied.push(label);
            } else {
                report.errors.push(format!("{}: プロジェクトが見つかりません", label));
            }
            continue;
        }

        // 通常のファイル系カード: 既にコミット済みならそのパス、なければコミット
        let existing_ok = committed
            .as_deref()
            .map(|c| Path::new(c).exists())
            .unwrap_or(false);
        if existing_ok {
            paths.push(committed.unwrap());
            report.copied.push(label);
            continue;
        }
        match commit_object_inner(p, id, false) {
            Ok(path) => {
                paths.push(path);
                report.copied.push(label);
            }
            Err(e) => report.errors.push(format!("{}: {}", label, e)),
        }
    }

    report.count = paths.len();
    (paths, report)
}

/// v1.8.1 ドラッグアウト: 選択カードの実ファイルを OS へ「ドラッグで」手渡す。
/// 持ち出しの規則は Ctrl+C と共通（`resolve_export_paths`）＝未コミットの仮身は
/// ここでコミットされ、URL／フィルタ／ドロップポイントは warnings に落ちる。
///
/// ドラッグ本体は **メインスレッドへ投げっぱなし**にする。`SHDoDragDrop` はマウスを
/// 奪うモーダルループなので、ここで完了を待つとコマンドの応答が止まり、ドラッグ中の
/// 前面が固まる。呼び出し側にはまず解決結果（何枚持ち出せたか）を返し、ドロップの
/// 成否は完了後の `texos://drag-out` イベントで受け取る。
#[tauri::command]
pub(crate) fn start_object_drag(
    ids: Vec<String>,
    window: tauri::WebviewWindow,
    state: State<AppState>,
) -> Result<ClipboardExportReport, String> {
    // プロジェクトのロックはパス解決の間だけ。ドラッグ中ずっと握らない。
    let (paths, report) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        resolve_export_paths(p, &ids)
    };
    if paths.is_empty() {
        return Ok(report); // 持ち出せるものが無い＝ドラッグは始めない
    }
    #[cfg(windows)]
    {
        let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
        let app = window.app_handle().clone();
        window
            .run_on_main_thread(move || crate::drag_out::start_file_drag(hwnd, &paths, &app))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        return Err("ドラッグでの持ち出しは Windows のみ対応です".into());
    }
    Ok(report)
}

/// v1.3.8 (G6): copy ONE image card to the clipboard as a bitmap (CF_DIB) so it
/// can be pasted into image apps (ペイント等). Separate from Ctrl+C, which stays
/// files-only (CF_HDROP). 独立コマンドなので 動画/音声/選択範囲コピーへ拡張できる。
#[tauri::command]
pub(crate) fn copy_image_to_clipboard(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // 画像判定（仮身優先の MIME）。
    let mime: String = p
        .conn
        .query_row(
            "SELECT COALESCE(v.mime_type, r.mime_type) FROM virtual_bodies v
             JOIN real_bodies r ON r.id = v.real_id
             WHERE v.id = ?1 AND v.deleted_at IS NULL",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .map_err(|_| "カードが見つかりません".to_string())?;
    if !mime.starts_with("image/") {
        return Err("このカードは画像ではありません".into());
    }
    // パス解決: コミット済みならそのまま、なければコミット（エクスポート＝コミット原則）。
    let committed: Option<String> = p
        .conn
        .query_row(
            "SELECT committed_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let path = match committed {
        Some(c) if Path::new(&c).exists() => c,
        _ => commit_object_inner(p, &id, false)?,
    };
    #[cfg(windows)]
    {
        clipboard_out::set_clipboard_image(&path)?;
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        return Err("クリップボードへの画像コピーは Windows のみ対応です".into());
    }
    Ok(())
}

/// Undo a commit: delete the editable real file and return the object to a 仮身
/// (reference). The CAS object is kept, so it can still be viewed/re-committed.
/// Refused when the committed file IS the original imported file (in-project
/// import, v1.2 仕様16) — deleting it would destroy the user's original data.
#[tauri::command]
pub(crate) fn uncommit_object(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    // 「仮身に戻す」＝実ファイルを消してオリジナル（origin）を指す仮身に戻す（編集は破棄）。
    // 誌操作防止のため「特別」メニューに置き、危険な2ケースは backend でも二重に拒否する:
    //  ① committed==origin（取り込み元の実ファイルそのもの）→ 消すと元データ喪失（v1.2.1）
    //  ② origin 無しの内部実身 → 戻り先が無く編集を捨てるだけ（v1.3.6）
    // 編集を残したい場合はユーザーが「仮身を作成→実身化」しておく運用（残したい/戻したいの両立）。
    let (committed, origin): (Option<String>, Option<String>) = p
        .conn
        .query_row(
            "SELECT committed_path, origin_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or((None, None));
    if let (Some(c), Some(o)) = (&committed, &origin) {
        if c == o {
            return Err(
                "これは取り込み元の実ファイルそのものです。仮身に戻すと元ファイルが失われるため実行できません。\
                 （ファイルごと削除する場合は「削除」を使用してください）"
                    .into(),
            );
        }
    }
    if committed.is_some() && origin.is_none() {
        return Err(
            "これはプロジェクト内部の実身です。戻り先の外部ファイルが無いため仮身に戻せません。\
             （不要なら「削除」を使用してください）"
                .into(),
        );
    }
    if let Some(cp) = committed {
        let path = std::path::Path::new(&cp);
        if path.starts_with(&p.root) && path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
    p.conn
        .execute(
            "UPDATE virtual_bodies SET committed_path = NULL, committed_mtime = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.6 変更②「実身にする」: この仮身が見ている内容をプロジェクト内に確定して実身にする。
/// v1.3.11: 内部窓（window_of）だけ切り、**origin（外部/親ファイルへのリンク）は保持**する。
/// origin を持つ実身は「仮身に戻す」で元（外部/親）へ戻せる＝フォーク由来・外部取り込み由来の
/// 実身を後から仮身に戻せる。origin が元々無い内部仮身を実身化した場合のみ戻り先が無く対象外。
#[tauri::command]
pub(crate) fn make_real(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // live 内容を CAS へ追従させてからコミット（commit_object_inner と同じ流れ）。
    let path = commit_object_inner(p, &id, false)?;
    // v1.3.10: window_of（内部窓）はクリアする。残すと「仮身を作成」が元の実身を指して
    // しまう（実身化したこの新ファイルの仮身にならない）。実体を持った時点で内部窓では
    // なくなるため。
    // v1.3.11: origin_path（外部/親ファイルへのリンク）は**保持**する。これがあると
    // isInternalReal=false のまま＝「仮身に戻す」が出て、外部取り込み実身やフォーク由来の
    // 実身を仮身（外部参照）へ戻せる。閲覧は①own committed が勝つので表示には影響しない。
    // committed==origin（プロジェクト内取り込み元そのもの）は uncommit/メニュー側のガードが
    // 別途守る。以前は origin も消していたため、フォークの実身化で外部リンクが失われていた。
    p.conn
        .execute(
            "UPDATE virtual_bodies SET window_of = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(path)
}

/// Copy a virtual body's real bytes to a throwaway TEMP file with its proper
/// name/extension and return the path.
pub(crate) fn materialize_temp(p: &Project, id: &str) -> Result<String, String> {
    // v1.3.9 ②: 閲覧では CAS に書き込まない（一時的閲覧も非破壊）。
    // v1.3.6: ファイル名の拡張子・MIME は仮身優先（COALESCE）。空ファイル共有で実身が
    // ods 等に潰れていても、本来の拡張子（.docx 等）で書き出す／開く（.docx.ods を防ぐ）。
    let (real_id, label, mime, stored_ext): (String, String, String, Option<String>) = p
        .conn
        .query_row(
            "SELECT v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext)
             FROM virtual_bodies v JOIN real_bodies r ON r.id = v.real_id
             WHERE v.id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|e| e.to_string())?;

    let dir = temp_view_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ext = resolve_ext(stored_ext.as_deref(), &mime);
    let filename = ensure_ext(&label, &ext);
    let dest = unique_path(&dir, &filename);

    let src = resolve_view_path(p, id, &real_id);
    std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
    Ok(dest.to_string_lossy().to_string())
}

/// View without committing: copy the real body to a throwaway TEMP file and open it.
#[tauri::command]
pub(crate) fn view_temp(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dest = materialize_temp(p, &id)?;
    os_open(&dest)?;
    Ok(dest)
}

/// Materialize an object into the TEMP preview area (with its proper
/// extension) and return the path, for in-canvas viewing through the asset
/// protocol (PDF / video). Prefers the committed file when it exists, so the
/// preview reflects current edits. The copy is skipped when still fresh.
#[tauri::command]
pub(crate) fn materialize_view(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // v1.3.9 ②: 閲覧（インラインPDF/動画プレビュー）では CAS に書き込まない。
    let (real_id, mime, stored_ext, committed): (String, String, Option<String>, Option<String>) =
        p.conn
            .query_row(
                "SELECT v.real_id, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext), v.committed_path
                 FROM virtual_bodies v JOIN real_bodies r ON r.id = v.real_id
                 WHERE v.id = ?1",
                rusqlite::params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| e.to_string())?;

    let src = committed
        .map(PathBuf::from)
        .filter(|c| c.is_file())
        .unwrap_or_else(|| resolve_view_path(p, &id, &real_id));

    let dir = temp_view_dir().join("preview");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ext = resolve_ext(stored_ext.as_deref(), &mime);
    let dest = dir.join(ensure_ext(&id, &ext));

    let fresh = matches!(
        (file_mtime_ms(&dest), file_mtime_ms(&src)),
        (Some(d), Some(s)) if d >= s
    );
    if !fresh {
        std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
    }
    Ok(dest.to_string_lossy().to_string())
}

/// LibreOffice(soffice) を探す: 環境変数PATH → 既定インストール先。
pub(crate) fn find_soffice() -> Option<PathBuf> {
    if let Some(path_var) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path_var) {
            let c = d.join("soffice.exe");
            if c.is_file() {
                return Some(c);
            }
        }
    }
    for base in [
        r"C:\Program Files\LibreOffice\program\soffice.exe",
        r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
    ] {
        let p = PathBuf::from(base);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// v1.3.11 #13 第2階層: オフィス系（pptx/ppt/odp/doc/odt 等）を LibreOffice headless で
/// PDF に変換し、既存の PDF プレビュー（asset protocol）で見られるようにする。変換結果は
/// temp_view_dir/preview/<id>.pdf にキャッシュ（mtime 比較で再利用）。LibreOffice 未導入は
/// 分かりやすいエラーを返す（フロントは「一時的に閲覧」へ誘導）。重い処理なので async。
#[tauri::command(async)]
pub(crate) fn convert_office_to_pdf(id: String, state: State<AppState>) -> Result<String, String> {
    // 1) 変換元を temp に materialize（id.ext の決定名で置く）。ロックはここだけ。
    let (src, ext): (PathBuf, String) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let (real_id, mime, stored_ext, committed): (String, String, Option<String>, Option<String>) =
            p.conn
                .query_row(
                    "SELECT v.real_id, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext), v.committed_path
                     FROM virtual_bodies v JOIN real_bodies r ON r.id = v.real_id
                     WHERE v.id = ?1",
                    rusqlite::params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(|e| e.to_string())?;
        let s = committed
            .map(PathBuf::from)
            .filter(|c| c.is_file())
            .unwrap_or_else(|| resolve_view_path(p, &id, &real_id));
        (s, resolve_ext(stored_ext.as_deref(), &mime))
    };

    let dir = temp_view_dir().join("preview");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let pdf = dir.join(format!("{}.pdf", id));
    // キャッシュ: PDF が変換元より新しければ再利用。
    if matches!(
        (file_mtime_ms(&pdf), file_mtime_ms(&src)),
        (Some(d), Some(s)) if d >= s
    ) {
        return Ok(pdf.to_string_lossy().to_string());
    }

    // soffice は入力の拡張子で形式を判定するので、決定名 <id>.<ext> にコピーしてから変換。
    let input = dir.join(ensure_ext(&id, &ext));
    std::fs::copy(&src, &input).map_err(|e| e.to_string())?;

    let soffice = find_soffice().ok_or(
        "LibreOffice が見つかりません（PowerPoint等のプレビューに必要）。インストールするか、\
         『一時的に閲覧』『既定のアプリで開く』をお使いください。",
    )?;
    // 専用プロファイルで headless 変換（既存インスタンスと衝突しないように）。
    let profile = dir.join("lo_profile");
    let out = std::process::Command::new(&soffice)
        .arg(format!(
            "-env:UserInstallation=file:///{}",
            profile.to_string_lossy().replace('\\', "/")
        ))
        .args(["--headless", "--norestore", "--convert-to", "pdf", "--outdir"])
        .arg(&dir)
        .arg(&input)
        .output()
        .map_err(|e| format!("LibreOffice の実行に失敗しました: {e}"))?;
    // soffice は <input stem>.pdf を outdir に出力する＝<id>.pdf。
    if !pdf.is_file() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "PDF 変換に失敗しました（LibreOffice）。{}",
            stderr.trim()
        ));
    }
    Ok(pdf.to_string_lossy().to_string())
}

