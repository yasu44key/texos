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

/// v1.9.1 内部レーンの相対化（V2.0_マルチプラットフォーム検討 §3）: `child` が `root` の中なら
/// root 基準の相対パス（区切りは `/`）を返す。プロジェクトをフォルダごと移動・持ち出しても
/// 内包ファイルのリンクが切れないよう、DB にはこの形で保存する。Windows の大文字小文字差は
/// コンポーネント単位で吸収し、表記で合わなければ canonicalize して再判定する。
pub(crate) fn rel_to_root(child: &Path, root: &Path) -> Option<String> {
    fn strip(child: &Path, root: &Path) -> Option<String> {
        let key = |c: std::path::Component| -> String {
            let s = c.as_os_str().to_string_lossy();
            if cfg!(windows) {
                s.trim_start_matches(r"\\?\").to_lowercase()
            } else {
                s.to_string()
            }
        };
        let mut cs = child.components();
        for rc in root.components() {
            if matches!(rc, std::path::Component::CurDir) {
                continue;
            }
            let cc = cs.next()?;
            if key(cc) != key(rc) {
                return None;
            }
        }
        let rest: Vec<String> = cs
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        if rest.is_empty() || rest.iter().any(|s| s == "..") {
            return None;
        }
        Some(rest.join("/"))
    }
    if !child.is_absolute() {
        return None;
    }
    strip(child, root).or_else(|| {
        let c = std::fs::canonicalize(child).ok()?;
        let r = std::fs::canonicalize(root).ok()?;
        strip(&c, &r)
    })
}

/// DB へ書く形: プロジェクト内なら相対、外（別ドライブ等の正当な外部参照）は絶対のまま。
/// 既に相対ならそのまま。パス文字列で照合する SQL の引数もこれを通すこと。
pub(crate) fn store_path(root: &Path, s: &str) -> String {
    rel_to_root(Path::new(s), root).unwrap_or_else(|| s.to_string())
}

/// DB から読んだ形を実行機の絶対パスへ。相対ならこのプロジェクトの root 基準で展開する。
/// 展開結果を DB へ書き戻さないこと（書くときは store_path）。
pub(crate) fn load_path(root: &Path, s: &str) -> String {
    if s.is_empty() || Path::new(s).is_absolute() {
        return s.to_string();
    }
    let mut pb = root.to_path_buf();
    for seg in s.split(['/', '\\']).filter(|x| !x.is_empty() && *x != ".") {
        pb.push(seg);
    }
    pb.to_string_lossy().to_string()
}

pub(crate) fn load_path_opt(root: &Path, s: Option<String>) -> Option<String> {
    s.map(|v| load_path(root, &v))
}

/// v1.9.1: 旧 DB の移行。現在の root の中を指す絶対パスの committed_path / origin_path を
/// 相対へ畳む（決定的な書き換えのみ・推測はしない）。root の外を指す絶対パス（移動前の
/// 古い場所を指してリンク切れになったもの等）には触れない＝「リンク切れを修復」の担当。
pub(crate) fn relativize_internal_paths(conn: &Connection, root: &Path) {
    for col in ["committed_path", "origin_path"] {
        let rows: Vec<(String, String)> = match conn.prepare(&format!(
            "SELECT id, {col} FROM virtual_bodies WHERE {col} IS NOT NULL AND {col} <> ''"
        )) {
            Ok(mut st) => st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.flatten().collect())
                .unwrap_or_default(),
            Err(_) => continue,
        };
        for (id, v) in rows {
            if let Some(rel) = rel_to_root(Path::new(&v), root) {
                let _ = conn.execute(
                    &format!("UPDATE virtual_bodies SET {col} = ?1 WHERE id = ?2"),
                    rusqlite::params![rel, id],
                );
            }
        }
    }
}

pub(crate) fn open_project_at(root: &Path) -> Result<Project, String> {
    let texos = texos_dir(root);
    let cas_root = texos.join("cas");
    std::fs::create_dir_all(&cas_root).map_err(|e| e.to_string())?;
    let db_path = texos.join("texos.db");
    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    repair_mime_types(&conn);
    backfill_vbody_ext(&conn);
    relativize_internal_paths(&conn, root);
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
    // real_bodies は OR IGNORE — db::open が新規DBへ 'note:sentinel' 等の共有センチネルを
    // 先にシードするので、親側の同 id と衝突して UNIQUE 違反になっていた（v1.8.4 まで）。
    let child_db = texos_dir(new_root).join("texos.db");
    src.conn
        .execute(
            "ATTACH DATABASE ?1 AS fork",
            rusqlite::params![child_db.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
    let result = (|| -> rusqlite::Result<()> {
        src.conn.execute_batch(
            "INSERT OR IGNORE INTO fork.real_bodies (id,mime_type,size,created_at,codec_ref,tags,ext)
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
        // v1.9.1: 親の origin_path は親 root 基準の相対で保存されうる。子へそのまま写すと
        // 子 root 基準で解かれて別物を指すので、まず親基準で展開し、子の規則で保存し直す
        // （下のファイルカード処理が上書きしない＝実体喪失カードの origin もこれで正しく残る）。
        {
            let rows: Vec<(String, String)> = {
                let mut stmt = src.conn.prepare(
                    "SELECT id, origin_path FROM fork.virtual_bodies WHERE origin_path IS NOT NULL",
                )?;
                let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                mapped.flatten().collect()
            };
            for (id, op) in rows {
                let fixed = store_path(new_root, &load_path(&src.root, &op));
                if fixed != op {
                    src.conn.execute(
                        "UPDATE fork.virtual_bodies SET origin_path = ?1 WHERE id = ?2",
                        rusqlite::params![fixed, id],
                    )?;
                }
            }
        }
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
                rusqlite::params![h, store_path(new_root, &live.to_string_lossy()), cid],
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
/// v1.9.1: CAS ルート（`<root>/.texos/cas`）からプロジェクトルートを逆算する。
/// 接続だけを持つ解決経路（フォーク元チェーンを辿る resolve_view_conn 等）で相対パスを解くため。
pub(crate) fn root_of_cas(cas_root: &Path) -> PathBuf {
    cas_root
        .parent()
        .and_then(|t| t.parent())
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

pub(crate) fn resolve_live_file(conn: &Connection, root: &Path, vid: &str, depth: u32) -> Option<PathBuf> {
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
        let p = PathBuf::from(load_path(root, &cp));
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(w) = window_of {
        return resolve_live_file(conn, root, &w, depth + 1);
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
    // v1.9.1: 保存形（root 基準の相対）をこの接続のプロジェクトで解決してから使う。
    let root = root_of_cas(cas_root);
    let committed = load_path_opt(&root, committed);
    let origin = load_path_opt(&root, origin);
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
        if let Some(p) = resolve_live_file(conn, &root, w, depth + 1) {
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
                let sibs: Vec<String> = rows.flatten().map(|c| load_path(&root, &c)).collect();
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
    // origin = DB の保存形（下の SQL 照合用）／origin_full = FS 操作用。
    let origin_full = load_path(&p.root, &origin);
    let origin_path = Path::new(&origin_full);
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
        if let Err(e) = fork_structure_into(p, &new_root) {
            // 作りかけの兄弟フォルダを残さない（unique_path で今作った新規フォルダなので安全）
            let _ = std::fs::remove_dir_all(&new_root);
            return Err(e);
        }
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
            // v1.7 P2: kind='url' を作成時に確定（従来は migrate() の後追い補正頼みだった）。
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, url_ref, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'url')",
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


#[cfg(test)]
mod fork_tests {
    use super::*;

    /// 回帰: db::open が新規DBへ 'note:sentinel' をシードするため、フォーク時の
    /// real_bodies コピーが UNIQUE 違反で落ちていた（「プロジェクトを作成できませんでした
    /// UNIQUE constraint failed: real_bodies.id」）。
    #[test]
    fn fork_into_fresh_db_does_not_collide_on_sentinel() {
        let tmp = std::env::temp_dir().join(format!("texos_fork_test_{}", uuid()));
        let src_root = tmp.join("src");
        let dst_root = tmp.join("dst");
        std::fs::create_dir_all(&src_root).unwrap();
        let src = open_project_at(&src_root).unwrap();
        // 親に付箋カード（note:sentinel を実身とする）を1枚置く
        db::insert_virtual_body(&src.conn, &uuid(), "note:sentinel", "memo", 0.0, 0.0, None, None)
            .unwrap();
        drop(open_project_at(&dst_root).unwrap()); // 子DBを初期化（ここでセンチネルがシードされる）

        fork_structure_into(&src, &dst_root).expect("fork must not fail on seeded sentinel");

        let dst = open_project_at(&dst_root).unwrap();
        let n: i64 = dst
            .conn
            .query_row("SELECT COUNT(*) FROM virtual_bodies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod portable_path_tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("texos_pp_{}_{}_{}", tag, db::now_ms(), uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paths(p: &Project, id: &str) -> (Option<String>, Option<String>) {
        p.conn
            .query_row(
                "SELECT committed_path, origin_path FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn rel_to_root_inside_outside_and_case() {
        let root = Path::new(r"C:\Proj\Sub");
        assert_eq!(rel_to_root(Path::new(r"C:\Proj\Sub\a.txt"), root).as_deref(), Some("a.txt"));
        assert_eq!(rel_to_root(Path::new(r"C:\Proj\Sub\d\a.txt"), root).as_deref(), Some("d/a.txt"));
        #[cfg(windows)]
        assert_eq!(rel_to_root(Path::new(r"c:\proj\SUB\a.txt"), root).as_deref(), Some("a.txt"));
        // 外・親・自分自身・相対は畳まない
        assert_eq!(rel_to_root(Path::new(r"D:\other\a.txt"), root), None);
        assert_eq!(rel_to_root(Path::new(r"C:\Proj\a.txt"), root), None);
        assert_eq!(rel_to_root(Path::new(r"C:\Proj\Sub"), root), None);
        assert_eq!(rel_to_root(Path::new("a.txt"), root), None);
        // 往復
        #[cfg(windows)]
        assert_eq!(load_path(root, "d/a.txt"), r"C:\Proj\Sub\d\a.txt");
        assert_eq!(load_path(root, r"D:\other\a.txt"), r"D:\other\a.txt");
    }

    /// 本題: プロジェクトをフォルダごと移動しても内包ファイルのリンクが切れない。
    #[test]
    fn moved_project_keeps_internal_links() {
        let a = temp_root("a");
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::write(a.join("メモ.txt"), "hello").unwrap();
        std::fs::write(a.join("sub").join("b.md"), "# b").unwrap();
        let (v1, v2) = {
            let p = open_project_at(&a).unwrap();
            let v1 = import_file_into(&p, a.join("メモ.txt").to_string_lossy().into(), String::new(), 0.0, 0.0).unwrap();
            let v2 = import_file_into(&p, a.join("sub").join("b.md").to_string_lossy().into(), String::new(), 0.0, 0.0).unwrap();
            // 保存は相対（origin も同じ形＝内部実身判定 origin==committed を保つ）
            assert_eq!(paths(&p, &v1), (Some("メモ.txt".into()), Some("メモ.txt".into())));
            assert_eq!(paths(&p, &v2), (Some("sub/b.md".into()), Some("sub/b.md".into())));
            // 同じファイルの2枚目は窓仮身になる（照合が相対でも効く）
            let v3 = import_file_into(&p, a.join("メモ.txt").to_string_lossy().into(), String::new(), 0.0, 0.0).unwrap();
            assert_eq!(paths(&p, &v3), (None, None));
            (v1, v2)
        };

        let b = a.with_file_name(format!("{}_moved", a.file_name().unwrap().to_string_lossy()));
        std::fs::rename(&a, &b).unwrap();
        let p = open_project_at(&b).unwrap();
        for (vid, rid_rel) in [(&v1, "メモ.txt"), (&v2, "sub/b.md")] {
            let rid: String = p
                .conn
                .query_row("SELECT real_id FROM virtual_bodies WHERE id = ?1", [vid], |r| r.get(0))
                .unwrap();
            let live = resolve_view_path(&p, vid, &rid);
            assert_eq!(live, PathBuf::from(load_path(&b, rid_rel)));
            assert!(live.is_file(), "移動後も実ファイルに解決できる: {}", live.display());
        }
        let _ = std::fs::remove_dir_all(&b);
    }

    /// 旧 DB（絶対パス）の移行: 今の root の中を指すものだけ相対へ畳む。外部・リンク切れは触らない。
    #[test]
    fn legacy_absolute_paths_are_relativized_on_open() {
        let a = temp_root("legacy");
        std::fs::write(a.join("x.txt"), "x").unwrap();
        let inside = a.join("x.txt").to_string_lossy().to_string();
        let outside = r"Z:\elsewhere\y.txt".to_string();
        {
            let p = open_project_at(&a).unwrap();
            db::insert_real_body(&p.conn, "h1", "text/plain", 1, Some("txt")).unwrap();
            for (id, c, o) in [("in", Some(&inside), Some(&inside)), ("ext", None, Some(&outside))] {
                p.conn
                    .execute(
                        "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, committed_path, origin_path)
                         VALUES (?1, 'h1', ?1, 0, 0, 0, ?2, ?3)",
                        rusqlite::params![id, c, o],
                    )
                    .unwrap();
            }
        }
        let p = open_project_at(&a).unwrap();
        assert_eq!(paths(&p, "in"), (Some("x.txt".into()), Some("x.txt".into())));
        assert_eq!(paths(&p, "ext"), (None, Some(outside)));
        let _ = std::fs::remove_dir_all(&a);
    }
}
