mod cas;
mod db;
#[cfg(windows)]
mod drag_drop;
mod flow;
mod os_icon;

use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Manager, State};

/// A currently-open project: a self-contained folder.
///   <root>/                  project folder
///     ├── <committed files>  editable real files (human-named)
///     └── .texos/
///         ├── texos.db       object graph
///         ├── cas/           immutable real bodies
///         └── snapshots/     full copies (next phase)
pub struct Project {
    pub conn: Connection,
    pub root: PathBuf,
    pub cas_root: PathBuf,
}

pub struct AppState {
    pub current: Mutex<Option<Project>>,
    pub app_data: PathBuf,
}

#[derive(Serialize, Deserialize, Default)]
struct Config {
    recent: Vec<String>,
    last: Option<String>,
}

#[derive(Serialize)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct VirtualBodyInfo {
    pub id: String,
    pub real_id: String,
    pub label: String,
    pub mime_type: String,
    pub pos_x: f64,
    pub pos_y: f64,
    pub committed_path: Option<String>,
    pub committed_modified: bool,
    /// If set, this card references another TexoS project (its folder path).
    pub project_ref: Option<String>,
    /// Original file extension (no dot), for icon selection.
    pub ext: Option<String>,
    /// Whether the in-canvas preview/editor is expanded.
    pub expanded: bool,
    /// If set, this card is a web URL link.
    pub url_ref: Option<String>,
    /// Optional manual comment shown on the card.
    pub memo: Option<String>,
    /// Stacking order on the canvas (higher = front).
    pub z_order: i64,
    /// If set, this card is a link to an OS folder (not a TexoS project).
    pub folder_ref: Option<String>,
    /// Where the file was imported from (借用元; informational only).
    pub origin_path: Option<String>,
    /// Current mtime (unix ms) of the committed file, for session-change detection.
    pub cur_mtime: Option<i64>,
    /// How the view resolves (v1.3-⑩): committed/sibling/origin/cas/fork-live/
    /// fork-cas/none. None for sentinel cards (project/url/folder).
    pub view_source: Option<String>,
    /// Width of the in-canvas text editor panel (v1.3.1, user-resizable).
    pub panel_w: Option<f64>,
    /// カード変種（v1.4 カード統一モデル §1）: file|url|project|folder|filter（拡張予定: note 等）
    pub kind: Option<String>,
    /// kind='filter' のフィルタ定義 JSON（v1.4 オートフロー⑤）
    pub filter_ref: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ViewTransform {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}

#[derive(Serialize)]
pub struct NoteInfo {
    pub id: String,
    pub text: String,
    pub pos_x: f64,
    pub pos_y: f64,
    pub width: f64,
    pub font_size: f64,
    pub bold: bool,
    pub file_ref: Option<String>,
}

#[derive(Serialize)]
pub struct RelationInfo {
    pub id: i64,
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
}

// ---------------------------------------------------------------------------
// Project lifecycle
// ---------------------------------------------------------------------------

fn texos_dir(root: &Path) -> PathBuf {
    root.join(".texos")
}

fn open_project_at(root: &Path) -> Result<Project, String> {
    let texos = texos_dir(root);
    let cas_root = texos.join("cas");
    std::fs::create_dir_all(&cas_root).map_err(|e| e.to_string())?;
    let db_path = texos.join("texos.db");
    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    repair_mime_types(&conn);
    Ok(Project {
        conn,
        root: root.to_path_buf(),
        cas_root,
    })
}

fn project_info(root: &Path) -> ProjectInfo {
    ProjectInfo {
        path: root.to_string_lossy().to_string(),
        name: root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string(),
    }
}

fn config_path(app_data: &Path) -> PathBuf {
    app_data.join("config.json")
}

fn read_config(app_data: &Path) -> Config {
    std::fs::read_to_string(config_path(app_data))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_config(app_data: &Path, c: &Config) {
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(config_path(app_data), s);
    }
}

fn add_recent(app_data: &Path, path: &str) {
    let mut c = read_config(app_data);
    c.recent.retain(|p| p != path);
    c.recent.insert(0, path.to_string());
    c.recent.truncate(12);
    c.last = Some(path.to_string());
    write_config(app_data, &c);
}

/// Create a new project rooted at `path` (folder is created if missing).
#[tauri::command]
fn new_project(path: String, state: State<AppState>) -> Result<ProjectInfo, String> {
    let root = PathBuf::from(&path);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let project = open_project_at(&root)?;
    let info = project_info(&root);
    *state.current.lock().unwrap() = Some(project);
    add_recent(&state.app_data, &info.path);
    Ok(info)
}

/// Open an existing TexoS project folder.
#[tauri::command]
fn open_project(path: String, state: State<AppState>) -> Result<ProjectInfo, String> {
    let root = PathBuf::from(&path);
    if !texos_dir(&root).join("texos.db").exists() {
        return Err("このフォルダはTexoSプロジェクトではありません".into());
    }
    let project = open_project_at(&root)?;
    let info = project_info(&root);
    *state.current.lock().unwrap() = Some(project);
    add_recent(&state.app_data, &info.path);
    Ok(info)
}

/// Return the currently open project, if any.
#[tauri::command]
fn get_current_project(state: State<AppState>) -> Option<ProjectInfo> {
    let guard = state.current.lock().unwrap();
    guard.as_ref().map(|p| project_info(&p.root))
}

/// List recently opened projects that still exist on disk.
#[tauri::command]
fn list_recent_projects(state: State<AppState>) -> Vec<ProjectInfo> {
    let c = read_config(&state.app_data);
    c.recent
        .iter()
        .filter(|p| texos_dir(Path::new(p)).join("texos.db").exists())
        .map(|p| project_info(Path::new(p)))
        .collect()
}

const PROJECT_MIME: &str = "application/vnd.texos.project";

/// Ensure a sentinel real body exists for a project reference and return its id.
fn ensure_project_real_body(conn: &Connection, path: &str) -> Result<String, String> {
    let id = format!("project:{}", cas::hash_bytes(path.as_bytes()));
    conn.execute(
        "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
         VALUES (?1, ?2, 0, ?3)",
        rusqlite::params![id, PROJECT_MIME, db::now_ms()],
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Fork the current project's structure into `new_root` WITHOUT copying the
/// real bodies (CAS). The fork's objects are 仮身 that reference the source's
/// real bodies; the actual bytes are copied lazily only when committed/edited.
/// `fork_source_cas` records where to read those real bodies from until then.
fn fork_structure_into(src: &Project, new_root: &Path) -> Result<(), String> {
    // Point the fork at the IMMEDIATE parent's CAS. Resolution walks the chain
    // parent-by-parent, so each ancestor's latest edits are reflected.
    let source_cas = src.cas_root.to_string_lossy().to_string();

    // Copy the object graph via ATTACH. No CAS files are copied. Committed
    // state is reset (every object starts as an uncommitted 仮身); text-linked
    // notes' file_ref is cleared (it pointed at the source's committed file).
    let child_db = texos_dir(new_root).join("texos.db");
    src.conn
        .execute(
            "ATTACH DATABASE ?1 AS fork",
            rusqlite::params![child_db.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
    let result = (|| -> rusqlite::Result<()> {
        src.conn.execute_batch(
            "INSERT INTO fork.real_bodies (id,mime_type,size,created_at,codec_ref,tags,ext)
               SELECT id,mime_type,size,created_at,codec_ref,tags,ext FROM main.real_bodies;
             INSERT INTO fork.virtual_bodies
               (id,real_id,label,pos_x,pos_y,desktop_id,created_at,etag,last_modified,
                committed_path,committed_mtime,project_ref,expanded,url_ref,memo,
                z_order,folder_ref,origin_path,kind,filter_ref)
               SELECT id,real_id,label,pos_x,pos_y,desktop_id,created_at,etag,last_modified,
                NULL,NULL,project_ref,expanded,url_ref,memo,
                z_order,folder_ref,origin_path,kind,filter_ref FROM main.virtual_bodies
               WHERE deleted_at IS NULL;
             INSERT INTO fork.relations (id,from_id,to_id,kind,created_at)
               SELECT id,from_id,to_id,kind,created_at FROM main.relations rel
               WHERE rel.deleted_at IS NULL
                 AND NOT EXISTS (
                   SELECT 1 FROM main.virtual_bodies d
                   WHERE d.deleted_at IS NOT NULL AND (d.id = rel.from_id OR d.id = rel.to_id)
                 )
                 AND NOT EXISTS (
                   SELECT 1 FROM main.notes dn
                   WHERE dn.deleted_at IS NOT NULL AND (dn.id = rel.from_id OR dn.id = rel.to_id)
                 );
             INSERT INTO fork.notes (id,text,pos_x,pos_y,created_at,width,font_size,bold,file_ref)
               SELECT id,text,pos_x,pos_y,created_at,width,font_size,bold,NULL FROM main.notes
               WHERE deleted_at IS NULL;
             INSERT INTO fork.shapes
               (id,kind,pos_x,pos_y,width,height,rotation,fill,opacity,z_order,label,created_at)
               SELECT id,kind,pos_x,pos_y,width,height,rotation,fill,opacity,z_order,label,created_at
               FROM main.shapes
               WHERE deleted_at IS NULL;
             INSERT OR IGNORE INTO fork.meta (key,value) SELECT key,value FROM main.meta;",
        )?;
        // Record where the (not-yet-copied) real bodies live.
        src.conn.execute(
            "INSERT OR REPLACE INTO fork.meta (key,value) VALUES ('fork_source_cas', ?1)",
            rusqlite::params![source_cas],
        )?;
        Ok(())
    })();
    let _ = src.conn.execute("DETACH DATABASE fork", []);
    result.map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3-⑩「仮身は元ファイルへのウインドウ」: resolve the bytes a 仮身 shows,
/// always preferring the most ALIVE source. Returns (path, source) where
/// source is one of:
///   "committed"  ① own committed file (this 仮身's fixed 実身)
///   "sibling"    ② another 仮身 of the same real body, committed in this folder
///   "origin"     ③ the original imported file (window to the outside)
///   "cas"        ④ import-time CAS snapshot (window lost → 薄赤 candidate)
///   "fork-live" / "fork-cas"  ⑤ same walk inside the fork-source chain
fn resolve_view_conn(
    conn: &Connection,
    cas_root: &Path,
    vid: &str,
    real_id: &str,
    depth: u32,
) -> Option<(PathBuf, &'static str)> {
    if depth > 16 {
        return None;
    }
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT committed_path, origin_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![vid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let (committed, origin) = row.unwrap_or((None, None));
    // ① own committed (possibly edited) file at this level
    if let Some(cp) = committed {
        let p = PathBuf::from(&cp);
        if p.exists() {
            return Some((p, if depth == 0 { "committed" } else { "fork-live" }));
        }
    }
    // ② a committed sibling of the same real body (生きた実身 in this folder)
    if let Ok(mut stmt) = conn.prepare(
        "SELECT committed_path FROM virtual_bodies
         WHERE real_id = ?1 AND id <> ?2 AND committed_path IS NOT NULL
         ORDER BY created_at",
    ) {
        if let Ok(rows) = stmt.query_map(rusqlite::params![real_id, vid], |r| {
            r.get::<_, String>(0)
        }) {
            for cp in rows.flatten() {
                let p = PathBuf::from(&cp);
                if p.exists() {
                    return Some((p, if depth == 0 { "sibling" } else { "fork-live" }));
                }
            }
        }
    }
    // ③ 取り込み元 — the window onto the external original
    if let Some(op) = origin {
        let p = PathBuf::from(&op);
        if p.exists() {
            return Some((p, if depth == 0 { "origin" } else { "fork-live" }));
        }
    }
    // ④ CAS snapshot at this level (保険＋タイムマシン)
    let local = cas::object_path(cas_root, real_id);
    if local.exists() {
        return Some((local, if depth == 0 { "cas" } else { "fork-cas" }));
    }
    // ⑤ recurse into the fork source (immediate parent), and so on
    if let Ok(Some(scas)) = db::meta_get(conn, "fork_source_cas") {
        let scas_path = PathBuf::from(&scas);
        if let Some(sdb) = scas_path.parent().map(|d| d.join("texos.db")) {
            if let Ok(sconn) = Connection::open(&sdb) {
                return resolve_view_conn(&sconn, &scas_path, vid, real_id, depth + 1);
            }
        }
    }
    None
}

/// CAS-chain-only lookup (local CAS, then ancestors' CAS). Used where the
/// import-time snapshot specifically is wanted: materializing the local CAS
/// backup at commit, and 「編集を破棄して再コミット」(force).
fn resolve_cas_path_conn(
    conn: &Connection,
    cas_root: &Path,
    real_id: &str,
    depth: u32,
) -> Option<PathBuf> {
    if depth > 16 {
        return None;
    }
    let local = cas::object_path(cas_root, real_id);
    if local.exists() {
        return Some(local);
    }
    if let Ok(Some(scas)) = db::meta_get(conn, "fork_source_cas") {
        let scas_path = PathBuf::from(&scas);
        if let Some(sdb) = scas_path.parent().map(|d| d.join("texos.db")) {
            if let Ok(sconn) = Connection::open(&sdb) {
                return resolve_cas_path_conn(&sconn, &scas_path, real_id, depth + 1);
            }
        }
    }
    None
}

/// Convenience wrapper over the current project. Falls back to local CAS path.
fn resolve_view_path(p: &Project, vid: &str, real_id: &str) -> PathBuf {
    resolve_view_conn(&p.conn, &p.cas_root, vid, real_id, 0)
        .map(|(path, _)| path)
        .unwrap_or_else(|| cas::object_path(&p.cas_root, real_id))
}

/// v1.3.1 (実機確認の要望): 閲覧時にCASスナップショットを更新する。
/// An uncommitted 仮身 with a LIVE origin file: re-snapshot the origin into the
/// CAS so the "保険の写真" is the latest content actually seen through the
/// window — if the original is later deleted (薄赤), the fallback shows the
/// last viewed state, not the import-time state. CAS objects stay immutable:
/// new content = new hash = new real body; the 仮身 (and its uncommitted
/// siblings sharing the same window) are re-pointed to it.
fn refresh_origin_snapshot(p: &Project, vid: &str) {
    let row: Option<(String, Option<String>, Option<String>)> = p
        .conn
        .query_row(
            "SELECT real_id, committed_path, origin_path FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![vid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((real_id, committed, origin)) = row else { return };
    if committed.is_some() {
        return; // 実身 is fixed; its snapshot lineage stays as-is
    }
    let Some(origin) = origin else { return };
    let origin_path = Path::new(&origin);
    if !origin_path.is_file() {
        return;
    }
    // store_file hashes + dedupes; unchanged content → same hash → no-op.
    let Ok(new_hash) = cas::store_file(&p.cas_root, origin_path) else { return };
    if new_hash == real_id {
        return;
    }
    let (mime, ext): (String, Option<String>) = p
        .conn
        .query_row(
            "SELECT mime_type, ext FROM real_bodies WHERE id = ?1",
            rusqlite::params![real_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or(("application/octet-stream".into(), None));
    let size = origin_path.metadata().map(|m| m.len()).unwrap_or(0);
    if db::insert_real_body(&p.conn, &new_hash, &mime, size, ext.as_deref()).is_err() {
        return;
    }
    // Every uncommitted 仮身 looking through this same window follows along.
    let _ = p.conn.execute(
        "UPDATE virtual_bodies SET real_id = ?1
         WHERE real_id = ?2 AND origin_path = ?3 AND committed_path IS NULL",
        rusqlite::params![new_hash, real_id, origin],
    );
}

/// Create a new project and place a project-object 仮身 referencing it on the
/// current desktop. `sibling=false` → inside the current project (内包/コミット),
/// `sibling=true` → same level as the current project (分岐/フォーク・全構造を継承).
#[tauri::command]
fn create_subproject(
    label: String,
    x: f64,
    y: f64,
    sibling: bool,
    state: State<AppState>,
) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let base = if sibling {
        p.root
            .parent()
            .ok_or("親フォルダが見つかりません")?
            .to_path_buf()
    } else {
        p.root.clone()
    };
    let new_root = unique_path(&base, &label);

    // Initialize the child project (creates .texos), then close its connection.
    drop(open_project_at(&new_root)?);
    add_recent(&state.app_data, &new_root.to_string_lossy());

    // Fork (分岐): inherit the entire structure of the current project —
    // all objects as 仮身, their relations, notes and the referenced real bodies.
    if sibling {
        fork_structure_into(p, &new_root)?;
    }

    let new_root_str = new_root.to_string_lossy().to_string();
    let real_id = ensure_project_real_body(&p.conn, &new_root_str)?;
    let vid = uuid();
    db::insert_virtual_body(
        &p.conn,
        &vid,
        &real_id,
        &label,
        x,
        y,
        None,
        Some(&new_root_str),
    )
    .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// Place a project-object 仮身 referencing an EXISTING TexoS project.
#[tauri::command]
fn link_project(path: String, x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
    let root = PathBuf::from(&path);
    if !texos_dir(&root).join("texos.db").exists() {
        return Err("このフォルダはTexoSプロジェクトではありません".into());
    }
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let label = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project")
        .to_string();
    let real_id = ensure_project_real_body(&p.conn, &path)?;
    let vid = uuid();
    db::insert_virtual_body(&p.conn, &vid, &real_id, &label, x, y, None, Some(&path))
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

const URL_MIME: &str = "application/vnd.texos.url";

/// Create a URL link card (title + url, no preview).
#[tauri::command]
fn create_url(
    title: String,
    url: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // sentinel real body (shared across all URL cards)
    let real_id = "url:sentinel".to_string();
    p.conn
        .execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES (?1, ?2, 0, ?3)",
            rusqlite::params![real_id, URL_MIME, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    let label = if title.trim().is_empty() { url.clone() } else { title };
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, url_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![vid, real_id, label, x, y, db::now_ms(), url],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// Set an object's manual comment (memo) shown on the card.
#[tauri::command]
fn set_memo(id: String, memo: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let val: Option<String> = if memo.trim().is_empty() { None } else { Some(memo) };
    p.conn
        .execute(
            "UPDATE virtual_bodies SET memo = ?1 WHERE id = ?2",
            rusqlite::params![val, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Change a URL card's target URL.
#[tauri::command]
fn update_url(id: String, url: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET url_ref = ?1 WHERE id = ?2",
            rusqlite::params![url, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Open the project referenced by a project-object 仮身 (navigation).
#[tauri::command]
fn open_project_object(id: String, state: State<AppState>) -> Result<ProjectInfo, String> {
    let target: String = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        p.conn
            .query_row(
                "SELECT project_ref FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(|e| e.to_string())?
            .ok_or("これはプロジェクト参照ではありません")?
    };
    let root = PathBuf::from(&target);
    if !texos_dir(&root).join("texos.db").exists() {
        return Err("参照先プロジェクトが見つかりません".into());
    }
    let project = open_project_at(&root)?;
    let info = project_info(&root);
    *state.current.lock().unwrap() = Some(project);
    add_recent(&state.app_data, &info.path);
    Ok(info)
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

const FOLDER_MIME: &str = "application/vnd.texos.folder";

/// Import a file from the local filesystem into the project CAS + DB.
/// Folders become links: a TexoS project folder → project reference card,
/// any other folder → リンクフォルダ card (the folder itself is NOT copied).
/// Files that already live inside the project folder start committed (実身) —
/// they are the project's own real files, so no 仮身 stage is needed.
#[tauri::command]
fn import_file(
    path: String,
    label: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    let src = std::path::Path::new(&path);
    if src.is_dir() {
        if texos_dir(src).join("texos.db").exists() {
            return link_project(path, x, y, state);
        }
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
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

    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let mime = mime_from_path(src);
    let orig_ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    let hash = cas::store_file(&p.cas_root, src).map_err(|e| e.to_string())?;
    let file_size = src.metadata().map_err(|e| e.to_string())?.len();

    db::insert_real_body(&p.conn, &hash, &mime, file_size, orig_ext.as_deref())
        .map_err(|e| e.to_string())?;

    let vid = uuid();
    let stem = src.file_name().and_then(|s| s.to_str()).unwrap_or(&label);
    let display_label = if label.is_empty() { stem } else { &label };

    // A file already inside the project folder (and not under .texos) IS the
    // project's real file: register it committed, pointing at itself.
    let in_project = src.starts_with(&p.root) && !src.starts_with(texos_dir(&p.root));
    let committed_path = in_project.then(|| path.clone());
    let committed_mtime = if in_project { file_mtime_ms(src) } else { None };

    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at,
                 origin_path, committed_path, committed_mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                vid,
                hash,
                display_label,
                x,
                y,
                db::now_ms(),
                path,
                committed_path,
                committed_mtime
            ],
        )
        .map_err(|e| e.to_string())?;

    Ok(vid)
}

#[tauri::command]
fn list_objects(state: State<AppState>) -> Result<Vec<VirtualBodyInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        .prepare(
            "SELECT v.id, v.real_id, v.label, r.mime_type, v.pos_x, v.pos_y,
                    v.committed_path, v.committed_mtime, v.project_ref, r.ext, v.expanded,
                    v.url_ref, v.memo, v.z_order, v.folder_ref, v.origin_path, v.panel_w,
                    v.kind, v.filter_ref
             FROM virtual_bodies v
             JOIN real_bodies r ON r.id = v.real_id
             WHERE v.desktop_id IS NULL AND v.deleted_at IS NULL
             ORDER BY v.created_at",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            let committed_path: Option<String> = row.get(6)?;
            let committed_mtime: Option<i64> = row.get(7)?;
            let cur_mtime = committed_path
                .as_deref()
                .and_then(|p| file_mtime_ms(std::path::Path::new(p)));
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
                project_ref: row.get(8)?,
                ext: row.get(9)?,
                expanded: row.get::<_, i64>(10)? != 0,
                url_ref: row.get(11)?,
                memo: row.get(12)?,
                z_order: row.get(13)?,
                folder_ref: row.get(14)?,
                origin_path: row.get(15)?,
                cur_mtime,
                view_source: None,
                panel_w: row.get(16)?,
                kind: row.get(17)?,
                filter_ref: row.get(18)?,
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
fn get_thumbnail(id: String, state: State<AppState>) -> Result<String, String> {
    // Resolve the path then release the lock before the heavy decode.
    let (obj_path, cache_path) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        refresh_origin_snapshot(p, &id); // 閲覧時にCASスナップショットを更新
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
fn get_media_data_url(id: String, state: State<AppState>) -> Result<String, String> {
    let (obj_path, mime) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        refresh_origin_snapshot(p, &id);
        let (real_id, mime): (String, String) = p
            .conn
            .query_row(
                "SELECT v.real_id, r.mime_type FROM virtual_bodies v
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
fn get_preview(id: String, state: State<AppState>) -> Result<String, String> {
    let obj_path = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        refresh_origin_snapshot(p, &id);
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

#[tauri::command]
fn move_object(id: String, x: f64, y: f64, state: State<AppState>) -> Result<(), String> {
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
fn move_objects(moves: Vec<MoveItem>, state: State<AppState>) -> Result<(), String> {
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
fn set_z_order(id: String, z: i64, state: State<AppState>) -> Result<(), String> {
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
fn set_expanded(id: String, expanded: bool, state: State<AppState>) -> Result<(), String> {
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
fn set_view_transform(x: f64, y: f64, scale: f64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let json = serde_json::to_string(&ViewTransform { x, y, scale }).map_err(|e| e.to_string())?;
    db::meta_set(&p.conn, "view_transform", &json).map_err(|e| e.to_string())?;
    Ok(())
}

/// Load the saved canvas view transform, if any.
#[tauri::command]
fn get_view_transform(state: State<AppState>) -> Result<Option<ViewTransform>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    match db::meta_get(&p.conn, "view_transform").map_err(|e| e.to_string())? {
        Some(s) => Ok(serde_json::from_str(&s).ok()),
        None => Ok(None),
    }
}

/// Trash location for a deleted 仮身's committed file: .texos/trash/<vid>__<name>.
/// Deterministic from (vid, committed_path) so restore needs no extra bookkeeping.
fn trash_path(root: &Path, vid: &str, committed: &Path) -> PathBuf {
    let name = committed
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    texos_dir(root).join("trash").join(format!("{}__{}", vid, name))
}

/// First free "name", "name (2)", "name (3)" … inside dir.
fn unique_file_path(dir: &Path, name: &str) -> PathBuf {
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
fn delete_object(id: String, state: State<AppState>) -> Result<(), String> {
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
fn restore_object(id: String, state: State<AppState>) -> Result<(), String> {
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
fn set_undo_stack(json: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    db::meta_set(&p.conn, "ui_undo_stack", &json).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_undo_stack(state: State<AppState>) -> Result<Option<String>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(None),
    };
    db::meta_get(&p.conn, "ui_undo_stack").map_err(|e| e.to_string())
}

#[tauri::command]
fn duplicate_virtual(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at, folder_ref, origin_path,
                 kind, filter_ref)
             SELECT ?1, real_id, label, pos_x + 36.0, pos_y + 36.0, ?2, folder_ref, origin_path,
                 kind, filter_ref
             FROM virtual_bodies WHERE id = ?3",
            rusqlite::params![vid, db::now_ms(), id],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

#[tauri::command]
fn rename_object(id: String, label: String, state: State<AppState>) -> Result<(), String> {
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

#[tauri::command]
fn create_relation(
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
        let is_filter = |vid: &str| -> bool {
            p.conn
                .query_row(
                    "SELECT 1 FROM virtual_bodies WHERE id = ?1 AND kind = 'filter'",
                    rusqlite::params![vid],
                    |_| Ok(()),
                )
                .optional()
                .ok()
                .flatten()
                .is_some()
        };
        if is_filter(&from_id) || is_filter(&to_id) {
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
fn list_relations(state: State<AppState>) -> Result<Vec<RelationInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        // Soft-deleted cards/notes keep their relation rows (for restore); hide
        // arrows that are deleted themselves or whose endpoint is currently
        // deleted. Endpoints can be cards or notes → anti-join both tables.
        .prepare(
            "SELECT id, from_id, to_id, kind FROM relations rel
             WHERE rel.deleted_at IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM virtual_bodies d
                 WHERE d.deleted_at IS NOT NULL AND (d.id = rel.from_id OR d.id = rel.to_id)
               )
               AND NOT EXISTS (
                 SELECT 1 FROM notes dn
                 WHERE dn.deleted_at IS NOT NULL AND (dn.id = rel.from_id OR dn.id = rel.to_id)
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
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_relation(id: i64, state: State<AppState>) -> Result<(), String> {
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
fn restore_relation(id: i64, state: State<AppState>) -> Result<(), String> {
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
fn commit_object(
    id: String,
    force: Option<bool>,
    state: State<AppState>,
) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    commit_object_inner(p, &id, force.unwrap_or(false))
}

fn commit_object_inner(p: &Project, id: &str, force: bool) -> Result<String, String> {
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

    let (real_id, label, mime, stored_ext): (String, String, String, Option<String>) = p
        .conn
        .query_row(
            "SELECT v.real_id, v.label, r.mime_type, r.ext
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

const PROGRAM_EXTS: &[&str] = &["exe", "msi", "bat", "cmd", "com", "lnk", "ps1", "vbs", "scr"];

/// Commit every object so the project folder becomes a self-contained,
/// portable Windows folder ("データ持ち出し"). External references — URLs and
/// linked projects outside this folder — cannot be materialized and are
/// reported as warnings; program files are committed but flagged.
#[tauri::command]
fn commit_all(dry_run: Option<bool>, state: State<AppState>) -> Result<CommitAllReport, String> {
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
                "SELECT v.id, v.label, v.committed_path, v.project_ref, v.url_ref, r.ext,
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
        // v1.4: フィルタカードはTexoS内の道具であり持ち出し対象のファイルを持たない
        if r.kind.as_deref() == Some("filter") {
            report.warnings.push(CommitAllWarning {
                label: r.label,
                kind: "filter".into(),
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

/// Undo a commit: delete the editable real file and return the object to a 仮身
/// (reference). The CAS object is kept, so it can still be viewed/re-committed.
/// Refused when the committed file IS the original imported file (in-project
/// import, v1.2 仕様16) — deleting it would destroy the user's original data.
#[tauri::command]
fn uncommit_object(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

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

/// Copy a virtual body's real bytes to a throwaway TEMP file with its proper
/// name/extension and return the path.
fn materialize_temp(p: &Project, id: &str) -> Result<String, String> {
    refresh_origin_snapshot(p, id); // 閲覧時にCASスナップショットを更新
    let (real_id, label, mime, stored_ext): (String, String, String, Option<String>) = p
        .conn
        .query_row(
            "SELECT v.real_id, v.label, r.mime_type, r.ext
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
fn view_temp(id: String, state: State<AppState>) -> Result<String, String> {
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
fn materialize_view(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    refresh_origin_snapshot(p, &id);
    let (real_id, mime, stored_ext, committed): (String, String, Option<String>, Option<String>) =
        p.conn
            .query_row(
                "SELECT v.real_id, r.mime_type, r.ext, v.committed_path
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

// ---------------------------------------------------------------------------
// Notes (free text annotations on the desktop)
// ---------------------------------------------------------------------------

#[tauri::command]
fn create_note(text: String, x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let id = uuid();
    p.conn
        .execute(
            "INSERT INTO notes (id, text, pos_x, pos_y, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, text, x, y, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
fn list_notes(state: State<AppState>) -> Result<Vec<NoteInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        .prepare(
            "SELECT id, text, pos_x, pos_y, width, font_size, bold, file_ref
             FROM notes WHERE deleted_at IS NULL ORDER BY created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(NoteInfo {
                id: row.get(0)?,
                text: row.get(1)?,
                pos_x: row.get(2)?,
                pos_y: row.get(3)?,
                width: row.get(4)?,
                font_size: row.get(5)?,
                bold: row.get::<_, i64>(6)? != 0,
                file_ref: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_note(id: String, text: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE notes SET text = ?1 WHERE id = ?2",
            rusqlite::params![text, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn move_note(id: String, x: f64, y: f64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE notes SET pos_x = ?1, pos_y = ?2 WHERE id = ?3",
            rusqlite::params![x, y, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn delete_note(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // v1.3.2: soft delete — 矢印リンクの行も残す（復活時に一緒に戻るため、
    // 表示は list_relations 側で隠す）。
    p.conn
        .execute(
            "UPDATE notes SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![db::now_ms(), id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.2: コメントの復活の呪文 (Ctrl+Z)。矢印リンクも一緒に表示へ戻る。
#[tauri::command]
fn restore_note(id: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE notes SET deleted_at = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist the in-canvas text editor panel width for an object (v1.3.1).
#[tauri::command]
fn set_panel_width(id: String, width: f64, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET panel_w = ?1 WHERE id = ?2",
            rusqlite::params![width, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Update a note's visual style (width, font size, bold).
#[tauri::command]
fn set_note_style(
    id: String,
    width: f64,
    font_size: f64,
    bold: bool,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "UPDATE notes SET width = ?1, font_size = ?2, bold = ?3 WHERE id = ?4",
            rusqlite::params![width, font_size, bold as i64, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Create a note linked to a text file; its content is loaded from the file.
#[tauri::command]
fn create_note_linked(
    path: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let id = uuid();
    p.conn
        .execute(
            "INSERT INTO notes (id, text, pos_x, pos_y, created_at, width, font_size, bold, file_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, 320, 13, 0, ?6)",
            rusqlite::params![id, text, x, y, db::now_ms(), path],
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// v1.3-④: コミットしてテキストファイルとして保存 — write a note's text into
/// the project folder and link the note to it (edits then write back to file).
#[tauri::command]
fn commit_note_to_file(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let (text, file_ref): (String, Option<String>) = p
        .conn
        .query_row(
            "SELECT text, file_ref FROM notes WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if let Some(f) = file_ref {
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
    p.conn
        .execute(
            "UPDATE notes SET file_ref = ?1 WHERE id = ?2",
            rusqlite::params![dest_str, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(dest_str)
}

/// Read a UTF-8 text file (for file-linked notes).
#[tauri::command]
fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// Write a UTF-8 text file (for file-linked notes → in-canvas editing).
#[tauri::command]
fn write_text_file(path: String, text: String) -> Result<(), String> {
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

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
fn paste_clipboard(x: f64, y: f64, state: State<AppState>) -> Result<ClipboardPaste, String> {
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
    Ok(ClipboardPaste {
        kind: "empty".into(),
        text: None,
        id: None,
    })
}

/// v1.3.1: pasted text becomes a REGULAR object — a .txt real body in the CAS
/// with a 仮身 card (画像貼り付けと同格). コミットでファイル化、矢印リンク可。
#[tauri::command]
fn create_text_object(text: String, x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
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

// ---------------------------------------------------------------------------
// オートフロー (v1.4-⑤: UNIXパイプラインの再来 — フィルタカードとフロー実行)
// ---------------------------------------------------------------------------

const FILTER_MIME: &str = "application/vnd.texos.filter";

/// フィルタカードを作成（kind='filter'、定義は filter_ref の JSON）。
#[tauri::command]
fn create_filter(
    label: String,
    spec: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    flow::parse_spec(&spec)?; // 保存前に検証（壊れたJSONをDBに入れない）
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // sentinel real body (URLカードと同じ方式: 全フィルタカードで共有)
    let real_id = "filter:sentinel".to_string();
    p.conn
        .execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES (?1, ?2, 0, ?3)",
            rusqlite::params![real_id, FILTER_MIME, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'filter', ?7)",
            rusqlite::params![vid, real_id, label, x, y, db::now_ms(), spec],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// フィルタ定義（JSON）を更新。フォーム自動生成（JSON Schema）はフェーズ2。
#[tauri::command]
fn update_filter(id: String, spec: String, state: State<AppState>) -> Result<(), String> {
    flow::parse_spec(&spec)?;
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let n = p
        .conn
        .execute(
            "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2 AND kind = 'filter'",
            rusqlite::params![spec, id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("フィルタカードが見つかりません".into());
    }
    Ok(())
}

#[derive(Serialize)]
pub struct FlowReport {
    /// 新しく生まれた結果カードの vid（front は緑シグナル＋UNDO登録に使う）
    pub created: Vec<String>,
    /// 生成されたファイル名（ダイアログ表示用）
    pub outputs: Vec<String>,
    /// 型不一致などでスキップした入力（ラベル＋理由）
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

/// フローを実行（手動トリガー — 確定方針4）。
/// 入力＝このフィルタへ入る flow 矢印の元カード。各入力の「窓の今」（v1.3-⑩の
/// 解決順）を読んでフィルタを通し、結果をプロジェクト直下のコミット済み実身として
/// カード化（緑シグナルで誕生）し、フィルタ→結果の flow 矢印を張る。
/// 実行ログはフィルタカードの memo に記録する。
#[tauri::command]
fn run_flow(id: String, state: State<AppState>) -> Result<FlowReport, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let (spec_json, fx, fy, flabel): (Option<String>, f64, f64, String) = p
        .conn
        .query_row(
            "SELECT filter_ref, pos_x, pos_y, label FROM virtual_bodies
             WHERE id = ?1 AND kind = 'filter' AND deleted_at IS NULL",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|_| "フィルタカードが見つかりません".to_string())?;
    let spec = flow::parse_spec(spec_json.as_deref().unwrap_or("{}"))?;
    let accept = spec.accept_mime.clone().unwrap_or_else(|| "*/*".into());

    // 入力: このフィルタへ入る flow 矢印（生きているカードのみ・接続順）
    let inputs: Vec<(String, String, String, String, Option<String>)> = {
        let mut stmt = p
            .conn
            .prepare(
                "SELECT v.id, v.real_id, v.label, r.mime_type, r.ext
                 FROM relations rel
                 JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                 JOIN real_bodies r ON r.id = v.real_id
                 WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                 ORDER BY rel.created_at",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    if inputs.is_empty() {
        return Err("入力がありません。データカードからフィルタへ矢印を引いてください".into());
    }

    // 出力ファイル名に使えるラベル（Windowsの禁止文字を逃がす）
    let safe_flabel: String = flabel
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(24)
        .collect();

    let mut report = FlowReport {
        created: vec![],
        outputs: vec![],
        skipped: vec![],
        errors: vec![],
    };
    let mut out_i = 0usize;
    for (vid, real_id, vlabel, mime, ext) in &inputs {
        if !flow::mime_matches(&accept, mime) {
            report
                .skipped
                .push(format!("{vlabel}（{mime} は受け入れ対象外）"));
            continue;
        }
        // 仮身の窓の「今」を読む（閲覧時スナップショット更新 → 解決順で実体パス）
        refresh_origin_snapshot(p, vid);
        let path = resolve_view_path(p, vid, real_id);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                report.errors.push(format!("{vlabel}: 読み込み失敗 ({e})"));
                continue;
            }
        };
        let in_ext = ext.clone().unwrap_or_else(|| "bin".into());
        let result: Result<(Vec<u8>, String, String), String> = if let Some(b) = &spec.builtin {
            flow::run_builtin(&spec, b, &data)
        } else if let Some(cmd) = &spec.cmd {
            let out_ext = spec.output_ext.clone().unwrap_or_else(|| in_ext.clone());
            let out_mime = mime_for_ext(&out_ext)
                .unwrap_or("application/octet-stream")
                .to_string();
            flow::run_external(cmd, &data, &in_ext, &out_ext).map(|b| (b, out_ext, out_mime))
        } else {
            Err("フィルタ定義に builtin も cmd もありません".into())
        };
        let (out_bytes, out_ext, out_mime) = match result {
            Ok(v) => v,
            Err(e) => {
                report.errors.push(format!("{vlabel}: {e}"));
                continue;
            }
        };

        // 結果＝プロジェクト直下のコミット済み実身（CASにも保存＝保険）
        let stem = std::path::Path::new(vlabel)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let dest = unique_file_path(&p.root, &format!("{stem}_{safe_flabel}.{out_ext}"));
        if let Err(e) = std::fs::write(&dest, &out_bytes) {
            report.errors.push(format!("{vlabel}: 書き出し失敗 ({e})"));
            continue;
        }
        let hash = match cas::store_bytes(&p.cas_root, &out_bytes) {
            Ok(h) => h,
            Err(e) => {
                report.errors.push(format!("{vlabel}: CAS保存失敗 ({e})"));
                continue;
            }
        };
        if let Err(e) =
            db::insert_real_body(&p.conn, &hash, &out_mime, out_bytes.len() as u64, Some(&out_ext))
        {
            report.errors.push(format!("{vlabel}: {e}"));
            continue;
        }
        let new_vid = uuid();
        let out_label = dest
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string();
        let nx = fx + 190.0;
        let ny = fy + (out_i as f64) * 170.0;
        if let Err(e) = p.conn.execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at,
                 committed_path, committed_mtime, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'file')",
            rusqlite::params![
                new_vid,
                hash,
                out_label,
                nx,
                ny,
                db::now_ms(),
                dest.to_string_lossy(),
                file_mtime_ms(&dest)
            ],
        ) {
            report.errors.push(format!("{vlabel}: {e}"));
            continue;
        }
        // フィルタ → 結果 の flow 矢印（パイプラインの連鎖が目に見える）
        let _ = p.conn.execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at)
             VALUES (?1, ?2, 'flow', ?3)",
            rusqlite::params![id, new_vid, db::now_ms()],
        );
        report.created.push(new_vid);
        report.outputs.push(out_label);
        out_i += 1;
    }

    // 実行ログを memo に記録（最後の実行が常に見える — 確定方針4）
    let mut log = format!(
        "▶ 実行: 入力{} → 出力{}",
        inputs.len(),
        report.outputs.len()
    );
    if !report.skipped.is_empty() {
        log.push_str(&format!("・スキップ{}", report.skipped.len()));
    }
    if !report.errors.is_empty() {
        log.push_str(&format!("・エラー{}", report.errors.len()));
    }
    let _ = p.conn.execute(
        "UPDATE virtual_bodies SET memo = ?1 WHERE id = ?2",
        rusqlite::params![log, id],
    );

    Ok(report)
}

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
}

#[tauri::command]
fn create_shape(
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
fn list_shapes(state: State<AppState>) -> Result<Vec<ShapeInfo>, String> {
    let guard = state.current.lock().unwrap();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return Ok(vec![]),
    };
    let mut stmt = p
        .conn
        .prepare(
            "SELECT id, kind, pos_x, pos_y, width, height, rotation, fill, opacity, z_order, label
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
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Move/resize/rotate a shape.
#[tauri::command]
fn update_shape_geom(
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
fn set_shape_style(
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
fn set_shape_label(id: String, label: String, state: State<AppState>) -> Result<(), String> {
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
fn set_shape_z(id: String, z: i64, state: State<AppState>) -> Result<(), String> {
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
fn delete_shape(id: String, state: State<AppState>) -> Result<(), String> {
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
fn restore_shape(id: String, state: State<AppState>) -> Result<(), String> {
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

// ---------------------------------------------------------------------------
// OS integration (no project required)
// ---------------------------------------------------------------------------

fn os_open(path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn open_with_default(path: String) -> Result<(), String> {
    os_open(&path)
}

/// Show the OS "Open with..." application picker for a file.
#[tauri::command]
fn open_with_chooser(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // OpenWith.exe is the modern, reliable "Open with" picker (works for all
        // registered types and Unicode paths). OpenAs_RunDLL is a flaky legacy hack.
        std::process::Command::new("OpenWith.exe")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        // No standard "open with" picker; fall back to the default app.
        os_open(&path)?;
    }
    Ok(())
}

/// Show the "Open with" picker for a URL via a tiny redirect HTML, so the user
/// can choose which browser opens it.
#[tauri::command]
fn open_url_with_chooser(url: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let dir = temp_view_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let safe = url.replace('"', "%22");
        let html = format!(
            "<!doctype html><meta charset=\"utf-8\">\
             <meta http-equiv=\"refresh\" content=\"0;url={0}\">\
             <body style=\"font-family:sans-serif\">Opening <a href=\"{0}\">{0}</a> …</body>",
            safe
        );
        let file = dir.join("open_link.html");
        std::fs::write(&file, html).map_err(|e| e.to_string())?;
        std::process::Command::new("OpenWith.exe")
            .arg(&file)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        os_open(&url)?;
    }
    Ok(())
}

#[tauri::command]
fn reveal_in_dir(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .args(["/select,", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(path.clone());
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Swap in our own OLE drop target (files + URL/text). Called once from the
/// frontend after mount, when the WebView2 child windows are guaranteed to
/// exist. No-op on non-Windows, where the built-in drag-drop events remain.
#[tauri::command]
fn install_drop_target(window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(windows)]
    {
        let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
        let app = window.app_handle().clone();
        window
            .run_on_main_thread(move || drag_drop::install(hwnd, app))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
    Ok(())
}

fn temp_view_dir() -> PathBuf {
    std::env::temp_dir().join("texos_view")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn file_mtime_ms(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_millis() as i64)
}

/// Prefer the original file extension; fall back to one derived from the MIME.
fn resolve_ext(stored: Option<&str>, mime: &str) -> String {
    match stored {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => ext_from_mime(mime).to_string(),
    }
}

fn ext_from_mime(mime: &str) -> &'static str {
    match mime {
        "text/plain" => "txt",
        "text/markdown" => "md",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/flac" => "flac",
        "application/json" => "json",
        "text/csv" => "csv",
        "text/html" => "html",
        "text/x-python" => "py",
        "text/javascript" => "js",
        "text/x-rust" => "rs",
        "application/pdf" => "pdf",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/rtf" => "rtf",
        "application/zip" => "zip",
        _ => "bin",
    }
}

fn ensure_ext(label: &str, ext: &str) -> String {
    let suffix = format!(".{}", ext.to_lowercase());
    if label.to_lowercase().ends_with(&suffix) {
        label.to_string()
    } else {
        format!("{}.{}", label, ext)
    }
}

fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 2..10000 {
        let name = if ext.is_empty() {
            format!("{} ({})", stem, n)
        } else {
            format!("{} ({}).{}", stem, n, ext)
        };
        let c = dir.join(name);
        if !c.exists() {
            return c;
        }
    }
    candidate
}

/// Canonical MIME by extension (v1.3-⑧). Binary container formats (Office等)
/// MUST be present here so they never fall back to a text-ish guess.
fn mime_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "txt" | "log" | "ini" => "text/plain",
        "md" => "text/markdown",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "json" => "application/json",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "py" => "text/x-python",
        "js" | "ts" => "text/javascript",
        "rs" => "text/x-rust",
        "c" | "cpp" | "h" => "text/x-c",
        "yaml" | "yml" => "text/yaml",
        "toml" => "text/x-toml",
        "pdf" => "application/pdf",
        // Office / document containers — binary, never text
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        // archives
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        _ => return None,
    })
}

fn mime_from_path(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .and_then(|e| mime_for_ext(&e))
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// v1.3-⑧ repair: rows imported when the extension map was smaller carry a
/// wrong MIME (e.g. .docx as text/plain → opened as text). Re-derive from ext.
fn repair_mime_types(conn: &Connection) {
    let rows: Vec<(String, String, String)> = match conn.prepare(
        "SELECT id, mime_type, ext FROM real_bodies WHERE ext IS NOT NULL AND ext <> ''",
    ) {
        Ok(mut stmt) => stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default(),
        Err(_) => return,
    };
    for (id, mime, ext) in rows {
        if let Some(correct) = mime_for_ext(&ext.to_lowercase()) {
            if correct != mime {
                let _ = conn.execute(
                    "UPDATE real_bodies SET mime_type = ?1 WHERE id = ?2",
                    rusqlite::params![correct, id],
                );
            }
        }
    }
}

fn uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", t ^ (t >> 17) ^ (t << 3))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .expect("failed to get app data dir");
            std::fs::create_dir_all(&app_dir)?;

            // Discard last session's temporary view copies.
            let _ = std::fs::remove_dir_all(temp_view_dir());

            // Re-open the last project if it still exists.
            let config = read_config(&app_dir);
            let current = config
                .last
                .as_ref()
                .filter(|p| texos_dir(Path::new(p)).join("texos.db").exists())
                .and_then(|p| open_project_at(Path::new(p)).ok());

            app.manage(AppState {
                current: Mutex::new(current),
                app_data: app_dir,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            new_project,
            open_project,
            get_current_project,
            list_recent_projects,
            create_subproject,
            link_project,
            open_project_object,
            create_url,
            update_url,
            set_memo,
            import_file,
            list_objects,
            move_object,
            move_objects,
            set_z_order,
            set_expanded,
            set_view_transform,
            get_view_transform,
            get_thumbnail,
            get_preview,
            get_media_data_url,
            create_relation,
            list_relations,
            delete_relation,
            delete_object,
            restore_object,
            restore_note,
            restore_shape,
            restore_relation,
            set_undo_stack,
            get_undo_stack,
            rename_object,
            duplicate_virtual,
            commit_object,
            commit_all,
            uncommit_object,
            open_with_default,
            open_with_chooser,
            open_url_with_chooser,
            reveal_in_dir,
            view_temp,
            materialize_view,
            create_note,
            list_notes,
            update_note,
            move_note,
            delete_note,
            set_note_style,
            create_note_linked,
            commit_note_to_file,
            read_text_file,
            write_text_file,
            paste_clipboard,
            create_text_object,
            create_filter,
            update_filter,
            run_flow,
            set_panel_width,
            create_shape,
            list_shapes,
            update_shape_geom,
            set_shape_style,
            set_shape_label,
            set_shape_z,
            delete_shape,
            os_icon::get_os_icon,
            install_drop_target
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
