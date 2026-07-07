#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Project lifecycle
// ---------------------------------------------------------------------------

pub(crate) fn texos_dir(root: &Path) -> PathBuf {
    root.join(".texos")
}

/// v1.3.11 #10: Robust「このパスは parent の中か？」判定。Windows では std の
/// `Path::starts_with` はコンポーネント単位の大文字小文字を区別するため、ドロップ経路
/// （DragQueryFileW のディスク表記）とプロジェクトルートの表記（設定保存値）で大小が
/// 違うと内部ファイルを外部と誤判定し、本来 実身 にすべき取り込みが 仮身 になっていた。
/// 両者を canonicalize（取れなければ原文）し、区切り正規化＋小文字化した文字列前方一致で
/// 判定する。
pub(crate) fn path_inside(child: &Path, parent: &Path) -> bool {
    let norm = |p: &Path| -> String {
        let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        c.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let c = norm(child);
    let pa = norm(parent);
    if pa.is_empty() {
        return false;
    }
    c == pa || c.starts_with(&(pa.clone() + "\\"))
}

pub(crate) fn open_project_at(root: &Path) -> Result<Project, String> {
    let texos = texos_dir(root);
    let cas_root = texos.join("cas");
    std::fs::create_dir_all(&cas_root).map_err(|e| e.to_string())?;
    let db_path = texos.join("texos.db");
    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    repair_mime_types(&conn);
    backfill_vbody_ext(&conn);
    Ok(Project {
        conn,
        root: root.to_path_buf(),
        cas_root,
    })
}

pub(crate) fn project_info(root: &Path) -> ProjectInfo {
    ProjectInfo {
        path: root.to_string_lossy().to_string(),
        name: root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string(),
    }
}

pub(crate) fn config_path(app_data: &Path) -> PathBuf {
    app_data.join("config.json")
}

pub(crate) fn read_config(app_data: &Path) -> Config {
    std::fs::read_to_string(config_path(app_data))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn write_config(app_data: &Path, c: &Config) {
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(config_path(app_data), s);
    }
}

pub(crate) fn add_recent(app_data: &Path, path: &str) {
    let mut c = read_config(app_data);
    c.recent.retain(|p| p != path);
    c.recent.insert(0, path.to_string());
    c.recent.truncate(12);
    c.last = Some(path.to_string());
    write_config(app_data, &c);
}

/// Create a new project rooted at `path` (folder is created if missing).
#[tauri::command]
pub(crate) fn new_project(path: String, state: State<AppState>) -> Result<ProjectInfo, String> {
    // v1.5.2 WS-C: 切替ガード撤廃（実行中ジョブは起動元プロジェクトへの専用接続で束縛済み）。
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
pub(crate) fn open_project(path: String, state: State<AppState>) -> Result<ProjectInfo, String> {
    // v1.5.2 WS-C: 切替ガード撤廃（同上）。
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
pub(crate) fn get_current_project(state: State<AppState>) -> Option<ProjectInfo> {
    let guard = state.current.lock().unwrap();
    guard.as_ref().map(|p| project_info(&p.root))
}

/// List recently opened projects that still exist on disk.
#[tauri::command]
pub(crate) fn list_recent_projects(state: State<AppState>) -> Vec<ProjectInfo> {
    let c = read_config(&state.app_data);
    c.recent
        .iter()
        .filter(|p| texos_dir(Path::new(p)).join("texos.db").exists())
        .map(|p| project_info(Path::new(p)))
        .collect()
}

pub(crate) const PROJECT_MIME: &str = "application/vnd.texos.project";

/// Ensure a sentinel real body exists for a project reference and return its id.
pub(crate) fn ensure_project_real_body(conn: &Connection, path: &str) -> Result<String, String> {
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
pub(crate) fn fork_structure_into(src: &Project, new_root: &Path) -> Result<(), String> {
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
                z_order,folder_ref,origin_path,kind,filter_ref,ext,mime_type,note_text,style_ref)
               SELECT id,real_id,label,pos_x,pos_y,desktop_id,created_at,etag,last_modified,
                NULL,NULL,project_ref,expanded,url_ref,memo,
                z_order,folder_ref,origin_path,kind,filter_ref,ext,mime_type,note_text,style_ref
               FROM main.virtual_bodies
               WHERE deleted_at IS NULL;
             INSERT INTO fork.relations (id,from_id,to_id,kind,created_at)
               SELECT id,from_id,to_id,kind,created_at FROM main.relations rel
               WHERE rel.deleted_at IS NULL
                 AND NOT EXISTS (
                   SELECT 1 FROM main.virtual_bodies d
                   WHERE d.deleted_at IS NOT NULL AND (d.id = rel.from_id OR d.id = rel.to_id)
                 );
             INSERT INTO fork.shapes
               (id,kind,pos_x,pos_y,width,height,rotation,fill,opacity,z_order,label,created_at)
               SELECT id,kind,pos_x,pos_y,width,height,rotation,fill,opacity,z_order,label,created_at
               FROM main.shapes
               WHERE deleted_at IS NULL;
             INSERT OR IGNORE INTO fork.meta (key,value) SELECT key,value FROM main.meta;",
        )?;
        // Record where the real bodies live as a deep fallback. 以前はこれが唯一の実体源
        // だったが、下で参照ブロブを子CASへコピーするので、生きたカードはもう親CASに依存しない。
        src.conn.execute(
            "INSERT OR REPLACE INTO fork.meta (key,value) VALUES ('fork_source_cas', ?1)",
            rusqlite::params![source_cas],
        )?;
        // v1.4.17: 付箋(kind='note')の file_ref は親のコミット先を指すので外す
        //（旧 fork.notes の file_ref=NULL と同じ扱い。style_ref JSON から該当キーだけ除去）。
        {
            let rows: Vec<(String, String)> = {
                let mut stmt = src.conn.prepare(
                    "SELECT id, style_ref FROM fork.virtual_bodies
                     WHERE kind = 'note' AND style_ref IS NOT NULL",
                )?;
                let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                mapped.flatten().collect()
            };
            for (id, style) in rows {
                if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&style) {
                    if v.get("file_ref").is_some() {
                        if let Some(o) = v.as_object_mut() {
                            o.remove("file_ref");
                        }
                        src.conn.execute(
                            "UPDATE fork.virtual_bodies SET style_ref = ?1 WHERE id = ?2",
                            rusqlite::params![v.to_string(), id],
                        )?;
                    }
                }
            }
        }
        // v1.3.11: フォークの仮身化を「外部ファイルの取り込み」と同じ形にする（ユーザー指摘の
        // “ぬけ”の修正）。従来は committed_path を NULL にして origin をコピーするだけで、親で
        // origin/window_of を持たないカード（内部実身・仮身の複製）が**指し先ゼロの孤児**になり、
        // 閲覧も実身化も空になっていた。import_file の外部分岐と同様に、各ファイルカードを
        //   1) 親の今のライブ実体を解決（committed→origin→CAS→親チェーン）
        //   2) 子CASへ内容アドレスでスナップ（store_file）→ 新 real_id = H
        //   3) origin_path = 親のライブファイル（最新を見る窓）／committed_path = NULL（仮身）
        // にする。＝子は「親ファイルを仮身として取り込んだ」状態。閲覧③(origin)／実身化／自前CAS
        // バックアップ④ が外部取り込みと同じルーチンで成立し、親CAS（⑤）にも依存しない。
        let fork_cas = texos_dir(new_root).join("cas");
        let mut file_cards: Vec<(String, String, String, Option<String>)> = Vec::new();
        {
            let mut stmt = src.conn.prepare(
                "SELECT id, real_id, COALESCE(mime_type,'application/octet-stream'), ext
                 FROM fork.virtual_bodies
                 WHERE (kind IS NULL OR kind = 'file')
                   AND url_ref IS NULL AND folder_ref IS NULL AND project_ref IS NULL",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            for r in rows {
                file_cards.push(r?);
            }
        }
        for (cid, rid, mime, ext) in &file_cards {
            // 親側でこのカードの今のライブ実体を解決（main 側＝親カードを引く）。
            let live = resolve_view_path(src, cid, rid);
            if !live.is_file() {
                continue; // 実体が解決できない（喪失）カードは据え置き（赤シグナルになる）
            }
            let h = match cas::store_file(&fork_cas, &live) {
                Ok(h) => h,
                Err(_) => continue,
            };
            let size = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0) as i64;
            src.conn.execute(
                "INSERT OR IGNORE INTO fork.real_bodies (id,mime_type,size,created_at,ext)
                 VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![h, mime, size, db::now_ms(), ext],
            )?;
            src.conn.execute(
                "UPDATE fork.virtual_bodies SET real_id = ?1, origin_path = ?2 WHERE id = ?3",
                rusqlite::params![h, live.to_string_lossy().to_string(), cid],
            )?;
        }
        Ok(())
    })();
    let _ = src.conn.execute("DETACH DATABASE fork", []);
    result.map_err(|e| e.to_string())?;
    Ok(())
}

/// v1.3.10 窓モデル: 内部仮身チェーンの「生きた実ファイル」だけを辿る。
/// own committed が実在すればそれを、無ければ window_of（複製元の実身カードID）を再帰。
/// CAS・origin・real_id 兄弟には一切落ちない＝喪失なら None を返し、呼び出し側が
/// 赤（CAS 保険）に倒す。指し先カードが削除済みでも None（→赤）。
pub(crate) fn resolve_live_file(conn: &Connection, vid: &str, depth: u32) -> Option<PathBuf> {
    if depth > 16 {
        return None;
    }
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT committed_path, window_of FROM virtual_bodies
             WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![vid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let (committed, window_of) = row.unwrap_or((None, None));
    if let Some(cp) = committed {
        let p = PathBuf::from(&cp);
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(w) = window_of {
        return resolve_live_file(conn, &w, depth + 1);
    }
    None
}

/// v1.3-⑩「仮身は元ファイルへのウインドウ」: resolve the bytes a 仮身 shows,
/// always preferring the most ALIVE source. Returns (path, source) where
/// source is one of:
///   "committed"  ① own committed file (this 仮身's fixed 実身)
///   "window"     ★ v1.3.10 内部レーン: window_of の指し先カードの生きた実ファイル
///   "sibling"    ② another 仮身 of the same real body, committed in this folder
///   "origin"     ③ the original imported file (window to the outside)
///   "cas"        ④ import-time CAS snapshot (window lost → 薄赤 candidate)
///   "fork-live" / "fork-cas"  ⑤ same walk inside the fork-source chain
pub(crate) fn resolve_view_conn(
    conn: &Connection,
    cas_root: &Path,
    vid: &str,
    real_id: &str,
    depth: u32,
) -> Option<(PathBuf, &'static str)> {
    if depth > 16 {
        return None;
    }
    let row: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT committed_path, origin_path, window_of FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![vid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    let (committed, origin, window_of) = row.unwrap_or((None, None, None));
    // ① own committed (possibly edited) file at this level
    if let Some(cp) = committed {
        let p = PathBuf::from(&cp);
        if p.exists() {
            return Some((p, if depth == 0 { "committed" } else { "fork-live" }));
        }
    }
    // ★ v1.3.10 内部レーン: window_of があれば、指し先の実身カードの「生きた実ファイル」
    // だけを辿る（カードIDで追従＝リネーム・編集に強い）。外部・CAS・real_id 兄弟には
    // 落とさない。指し先が実体喪失なら CAS（保険・赤）/ none（赤）で止め、②以降の
    // 外部レーンには進めない（内部と外部を混ぜない）。
    if let Some(w) = &window_of {
        if let Some(p) = resolve_live_file(conn, w, depth + 1) {
            return Some((p, if depth == 0 { "window" } else { "fork-live" }));
        }
        let local = cas::object_path(cas_root, real_id);
        if local.exists() {
            return Some((local, if depth == 0 { "cas" } else { "fork-cas" }));
        }
        return None;
    }
    // ② a committed sibling of the same real body (生きた実身 in this folder).
    // v1.3.9: 同じ real_id を複数の実身が共有しうる（内容一致でCASハッシュが衝突）ため、
    // 単に「同 real_id の最古の実身」を選ぶと、複製仮身が複製元でない別実身に追従してしまう。
    // そこで、この仮身の窓対象（origin_path）と committed_path が一致する兄弟＝複製元を優先し、
    // 一致が無いときだけ最古の生存兄弟にフォールバックする。
    //
    // v1.3.11 #11: 兄弟解決は「何か（外部の原本）への窓」であるカード＝origin_path を
    // 持つカードに限定する。origin も window_of も無いカードは authored な新規コンテンツ
    // （空テキストカード等）で、自分の内容は自分の CAS ブロブ（④）にある。空ファイルは
    // CAS ハッシュ（real_id）が衝突して同じになるため、ここで兄弟にフォールバックすると
    // 「新規に作った2枚目が、最初に実身化した1枚目の仮身（窓）になってしまう」という
    // 不具合になる。origin を持つ窓カードだけが兄弟を辿る。
    if origin.is_some() {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT committed_path FROM virtual_bodies
             WHERE real_id = ?1 AND id <> ?2 AND committed_path IS NOT NULL
             ORDER BY created_at",
        ) {
            if let Ok(rows) = stmt.query_map(rusqlite::params![real_id, vid], |r| {
                r.get::<_, String>(0)
            }) {
                let sibs: Vec<String> = rows.flatten().collect();
                // 優先: origin_path（窓の対象＝複製元の実ファイル）に一致する生存兄弟
                if let Some(op) = &origin {
                    for cp in &sibs {
                        if cp == op {
                            let p = PathBuf::from(cp);
                            if p.exists() {
                                return Some((p, if depth == 0 { "sibling" } else { "fork-live" }));
                            }
                        }
                    }
                }
                // フォールバック: 最古の生存兄弟（外部 origin が移動・喪失した窓カード等）
                for cp in &sibs {
                    let p = PathBuf::from(cp);
                    if p.exists() {
                        return Some((p, if depth == 0 { "sibling" } else { "fork-live" }));
                    }
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
pub(crate) fn resolve_cas_path_conn(
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
pub(crate) fn resolve_view_path(p: &Project, vid: &str, real_id: &str) -> PathBuf {
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
pub(crate) fn refresh_origin_snapshot(p: &Project, vid: &str) {
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
pub(crate) fn create_subproject(
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
pub(crate) fn link_project(path: String, x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
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

pub(crate) const URL_MIME: &str = "application/vnd.texos.url";

/// Create a URL link card (title + url, no preview).
#[tauri::command]
pub(crate) fn create_url(
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
pub(crate) fn set_memo(id: String, memo: String, state: State<AppState>) -> Result<(), String> {
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
pub(crate) fn update_url(id: String, url: String, state: State<AppState>) -> Result<(), String> {
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
pub(crate) fn open_project_object(id: String, state: State<AppState>) -> Result<ProjectInfo, String> {
    // v1.5.2 WS-C: 切替ガード撤廃（同上）。
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

/// v1.3.11 #1: Open the TexoS project that a borrowed (origin_path) file belongs to.
/// Walks up from the file's directory looking for `.texos/texos.db`. If found,
/// opens that project (switches current) and returns its info. If the file is not
/// inside any TexoS project, returns None (the frontend then just reveals the
/// containing folder).
#[tauri::command]
pub(crate) fn open_origin_project(
    path: String,
    state: State<AppState>,
) -> Result<Option<ProjectInfo>, String> {
    // v1.5.2 WS-C: 切替ガード撤廃（同上）。
    let start = PathBuf::from(&path);
    let mut dir: Option<&Path> = if start.is_dir() {
        Some(start.as_path())
    } else {
        start.parent()
    };
    while let Some(d) = dir {
        if texos_dir(d).join("texos.db").exists() {
            let root = d.to_path_buf();
            let project = open_project_at(&root)?;
            let info = project_info(&root);
            *state.current.lock().unwrap() = Some(project);
            add_recent(&state.app_data, &info.path);
            return Ok(Some(info));
        }
        dir = d.parent();
    }
    Ok(None)
}

