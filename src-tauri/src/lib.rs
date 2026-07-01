mod cas;
#[cfg(windows)]
mod clipboard_out;
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
use tauri::{Emitter, Manager, State};

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
    /// フロー実行の中断フラグ（UIの中断ボタン → break_flow）。
    /// run_flow が DB の Mutex を握っている間も break_flow はこれだけを触るので
    /// 外部コマンドがストールしてもブレイクできる。
    pub flow_cancel: std::sync::atomic::AtomicBool,
    /// v1.4.1 チャット: ストリーミング応答の中断フラグ（停止ボタン → ai_chat_cancel）。
    pub chat_cancel: std::sync::atomic::AtomicBool,
    /// v1.4.9 子プロセス stdin チャネル: 実行中フィルタ（録音など record モード）の
    /// stdin 書き込み端。flow_send_stdin が 'q' 等を書いて graceful 停止に使う。
    pub rec_stdin: flow::StdinSlot,
    /// v1.4.10 多重起動ガード: run_flow 実行中フラグ。録音など長時間フロー中に実行ボタンを
    /// 再度押すと、2回目の run_flow が state.current ロック待ちで積み上がり、停止しても
    /// 積まれた次の録音が走ってハングしたように見える（v1.4.9 バグ）。フローは単一スロット
    /// 設計（flow_cancel/rec_stdin が各1個）＝同時実行は1つ前提なので、再入を即エラーで弾く。
    pub flow_running: std::sync::atomic::AtomicBool,
    /// v1.5 Text-To-Image: TexoS が起動・管理する diffusers 常駐デーモン（t2i_server.py）の
    /// 子プロセスハンドル。t2i_stop_server で明示停止できるよう保持する。
    pub t2i_server: Mutex<Option<std::process::Child>>,
}

/// run_flow の多重起動ガード。drop で flow_running を false に戻す
/// （早期 return / panic / `?` のどの離脱でも確実に解放する）。
struct RunningGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// v1.4.11 非ブロッキング実行: current プロジェクトを**短時間だけ**ロックしてクロージャを
/// 実行し、即座に解放する。長時間実行（execute_node = 外部/AI/録音）はこのロックの外で
/// 行うことで「実行中は DB を手放す」を成立させる（録音/AI 中も UI が固まらない）。
/// 注意: std の Mutex は**非再入**。with_db のクロージャ内でさらに with_db を呼ばないこと。
/// （eval_filter / collect_context は各 DB 接触を別々の短い with_db に分け、その間＝
/// 実行・再帰・ファイル読みではロックを保持しない設計。）
fn with_db<T>(
    state: &AppState,
    f: impl FnOnce(&Project) -> Result<T, String>,
) -> Result<T, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    f(p)
}

/// v1.4.11: フロー実行中はプロジェクト切替を拒否する。非ブロッキング実行では run_flow が
/// 実行中に DB ロックを手放すため、途中で current が別プロジェクトに差し替わると、評価や
/// 結果の書き込みが別プロジェクトに当たって壊れる。切替系コマンドの入口でこれを弾く。
fn ensure_no_flow_running(state: &AppState) -> Result<(), String> {
    if state.flow_running.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(
            "フロー実行中はプロジェクトを切り替えられません（完了／録音は「停止」してから）".into(),
        );
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Default)]
struct Config {
    recent: Vec<String>,
    last: Option<String>,
    /// v1.4.8: 現在選択中の Whisper モデル名（"small"/"medium" 等。既定 small）。
    #[serde(default)]
    whisper_model: Option<String>,
    /// v1.5: 現在選択中の Text-To-Image モデル名（"sd1.5" 等。既定 sd1.5）。
    #[serde(default)]
    t2i_model: Option<String>,
    /// v1.5: Hugging Face トークン（ゲート付き FLUX 公式モデルの取得に使う）。
    /// デーモン起動時に環境変数 HF_TOKEN として渡す。
    #[serde(default)]
    t2i_hf_token: Option<String>,
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
    /// v1.3.10 窓モデル: 内部仮身が指す実身カードの id（外部=origin_path とは別レーン）。
    pub window_of: Option<String>,
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

/// v1.3.11 #10: Robust「このパスは parent の中か？」判定。Windows では std の
/// `Path::starts_with` はコンポーネント単位の大文字小文字を区別するため、ドロップ経路
/// （DragQueryFileW のディスク表記）とプロジェクトルートの表記（設定保存値）で大小が
/// 違うと内部ファイルを外部と誤判定し、本来 実身 にすべき取り込みが 仮身 になっていた。
/// 両者を canonicalize（取れなければ原文）し、区切り正規化＋小文字化した文字列前方一致で
/// 判定する。
fn path_inside(child: &Path, parent: &Path) -> bool {
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

fn open_project_at(root: &Path) -> Result<Project, String> {
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
    ensure_no_flow_running(state.inner())?;
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
    ensure_no_flow_running(state.inner())?;
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
                z_order,folder_ref,origin_path,kind,filter_ref,ext,mime_type)
               SELECT id,real_id,label,pos_x,pos_y,desktop_id,created_at,etag,last_modified,
                NULL,NULL,project_ref,expanded,url_ref,memo,
                z_order,folder_ref,origin_path,kind,filter_ref,ext,mime_type FROM main.virtual_bodies
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
        // Record where the real bodies live as a deep fallback. 以前はこれが唯一の実体源
        // だったが、下で参照ブロブを子CASへコピーするので、生きたカードはもう親CASに依存しない。
        src.conn.execute(
            "INSERT OR REPLACE INTO fork.meta (key,value) VALUES ('fork_source_cas', ?1)",
            rusqlite::params![source_cas],
        )?;
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
fn resolve_live_file(conn: &Connection, vid: &str, depth: u32) -> Option<PathBuf> {
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
    ensure_no_flow_running(state.inner())?;
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
fn open_origin_project(
    path: String,
    state: State<AppState>,
) -> Result<Option<ProjectInfo>, String> {
    ensure_no_flow_running(state.inner())?;
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
fn list_objects(state: State<AppState>) -> Result<Vec<VirtualBodyInfo>, String> {
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
                    v.kind, v.filter_ref, v.window_of
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
            let folder_ref: Option<String> = row.get(14)?;
            // v1.3.11 #12: リンクフォルダカードは folder_ref の存在で cur_mtime を決める
            // （フォルダが消えていれば None → フロントが薄赤シグナルにする）。
            let cur_mtime = committed_path
                .as_deref()
                .and_then(|p| file_mtime_ms(std::path::Path::new(p)))
                .or_else(|| {
                    folder_ref
                        .as_deref()
                        .and_then(|p| file_mtime_ms(std::path::Path::new(p)))
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
                project_ref: row.get(8)?,
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
fn get_thumbnail(id: String, state: State<AppState>) -> Result<String, String> {
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
fn get_media_data_url(id: String, state: State<AppState>) -> Result<String, String> {
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
fn get_preview(id: String, state: State<AppState>) -> Result<String, String> {
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

/// v1.3.10 TexoS内リネーム: 実身（committed_path を持つカード）の実ファイルを TexoS から
/// 安全に改名する。実ファイル改名 + committed_path/mtime/label/ext を一括更新。カードIDは
/// 不変なので window_of で指す仮身は全部自動追従する（OS でリネームすると committed_path が
/// ずれて喪失＝赤になるのを防ぐ）。連番ファイル名（FILE(2)）の付け替えにも使う。
#[tauri::command]
fn rename_real(id: String, new_name: String, state: State<AppState>) -> Result<String, String> {
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
        // v1.3.11: フィルタに加えランチャー（kind='launcher'）への矢印もフロー扱い
        // （ランチャーに入る {in} 入力＝開くファイルを flow エッジで表す）。
        let is_flow_node = |vid: &str| -> bool {
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
fn relink_object(id: String, new_path: String, state: State<AppState>) -> Result<(), String> {
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
#[tauri::command]
fn relink_folder(id: String, new_path: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let np = std::path::Path::new(&new_path);
    if !np.is_dir() {
        return Err("選んだフォルダが見つかりません".into());
    }
    p.conn
        .execute(
            "UPDATE virtual_bodies SET folder_ref = ?1 WHERE id = ?2",
            rusqlite::params![new_path, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
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
fn copy_objects_to_clipboard(
    ids: Vec<String>,
    state: State<AppState>,
) -> Result<ClipboardExportReport, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    let mut report = ClipboardExportReport {
        copied: vec![],
        count: 0,
        warnings: vec![],
        errors: vec![],
    };
    let mut paths: Vec<String> = vec![];

    for id in &ids {
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
    if !paths.is_empty() {
        #[cfg(windows)]
        clipboard_out::set_clipboard_files(&paths)?;
        #[cfg(not(windows))]
        return Err("クリップボード持ち出しは Windows のみ対応です".into());
    }
    Ok(report)
}

/// v1.3.8 (G6): copy ONE image card to the clipboard as a bitmap (CF_DIB) so it
/// can be pasted into image apps (ペイント等). Separate from Ctrl+C, which stays
/// files-only (CF_HDROP). 独立コマンドなので 動画/音声/選択範囲コピーへ拡張できる。
#[tauri::command]
fn copy_image_to_clipboard(id: String, state: State<AppState>) -> Result<(), String> {
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
fn uncommit_object(id: String, state: State<AppState>) -> Result<(), String> {
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
fn make_real(id: String, state: State<AppState>) -> Result<String, String> {
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
fn materialize_temp(p: &Project, id: &str) -> Result<String, String> {
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
fn find_soffice() -> Option<PathBuf> {
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
fn convert_office_to_pdf(id: String, state: State<AppState>) -> Result<String, String> {
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

/// v1.4.13: Decode arbitrary text bytes for a read-only 仮身 view, WITHOUT
/// touching the source bytes. Tries BOM → strict UTF-8 → chardetng-detected
/// legacy encoding (Shift_JIS / EUC-JP / UTF-16 …). Converting the file to
/// UTF-8 (実身化) is the filter pipeline's job, not this view's.
fn decode_text_bytes(bytes: &[u8]) -> (String, String) {
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
fn read_view_text(id: String, state: State<AppState>) -> Result<ViewText, String> {
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

/// プロジェクト同梱プログラムの置き場（v1.5: 可搬なフィルタはここに入れて持ち運ぶ）。
fn filters_dir(root: &Path) -> PathBuf {
    texos_dir(root).join("filters")
}

/// 外部フィルタの実行コマンドラインを組み立てる。
/// 新形式（name あり）: プログラムを解決して `"exe" + CMD書式`。旧形式: cmd をそのまま。
fn build_cmdline(root: &Path, spec: &flow::FilterSpec) -> Result<String, String> {
    if let Some(name) = spec.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let exe = flow::resolve_program(name, spec.path.as_deref(), &filters_dir(root))
            .ok_or_else(|| {
                format!(
                    "プログラム「{name}」が見つかりません（指定パス → .texos\\filters → 環境変数PATH の順で探索）"
                )
            })?;
        Ok(format!("\"{}\" {}", exe.display(), spec.cmd.as_deref().unwrap_or(""))
            .trim()
            .to_string())
    } else if let Some(cmd) = &spec.cmd {
        Ok(cmd.clone())
    } else {
        Err("フィルタ定義に builtin も cmd もありません".into())
    }
}

/// 外部プログラムの所在チェック（設定ダイアログの事前チェック）。
/// 見つかれば解決済みのフルパス、見つからなければ null を返す。
#[tauri::command]
fn check_filter_program(
    name: String,
    path: Option<String>,
    state: State<AppState>,
) -> Result<Option<String>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    Ok(
        flow::resolve_program(&name, path.as_deref(), &filters_dir(&p.root))
            .map(|pb| pb.to_string_lossy().into_owned()),
    )
}

/// .texos/filters に同梱されたプログラムの一覧（設定ダイアログの選択肢）。
#[tauri::command]
fn list_filter_programs(state: State<AppState>) -> Result<Vec<String>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(filters_dir(&p.root)) {
        for e in rd.flatten() {
            if e.path().is_file() {
                if let Some(n) = e.file_name().to_str() {
                    // .removed は同梱解除の退避ファイル（一覧に出さない）
                    if !n.ends_with(".removed") {
                        names.push(n.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// 同梱を解除（.texos/filters から退避）。完全削除はせず「<name>.removed」へ
/// 改名して、Ctrl+Z（元に戻す）の余地を残す。.removed は一覧・解決の対象外。
#[tauri::command]
fn remove_filter_program(name: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dir = filters_dir(&p.root);
    let src = dir.join(&name);
    if !src.is_file() {
        return Err(format!("同梱されていません: {name}"));
    }
    let bak = dir.join(format!("{name}.removed"));
    let _ = std::fs::remove_file(&bak); // 古い退避は捨てて最新を残す
    std::fs::rename(&src, &bak).map_err(|e| e.to_string())
}

/// 同梱解除を取り消す（.removed を元の名前へ戻す）。
#[tauri::command]
fn restore_filter_program(name: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dir = filters_dir(&p.root);
    let bak = dir.join(format!("{name}.removed"));
    if !bak.is_file() {
        return Err("元に戻せるファイルがありません".into());
    }
    let dst = dir.join(&name);
    if dst.exists() {
        return Err(format!("既に {name} が同梱されています"));
    }
    std::fs::rename(&bak, &dst).map_err(|e| e.to_string())
}

/// プログラムをプロジェクトに同梱（.texos/filters へコピー）。戻り値はファイル名。
#[tauri::command]
fn import_filter_program(src: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let srcp = Path::new(&src);
    if !srcp.is_file() {
        return Err(format!("ファイルが見つかりません: {src}"));
    }
    let name = srcp
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("ファイル名を取得できません")?
        .to_string();
    let dir = filters_dir(&p.root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::copy(srcp, dir.join(&name)).map_err(|e| format!("コピーに失敗: {e}"))?;
    Ok(name)
}

// --- v1.5 フィルタライブラリ（アプリ共通・プロジェクト横断） ---------------------

/// ライブラリの1エントリ。spec は filter_ref と同じ JSON。
#[derive(Serialize, Deserialize)]
struct LibFilter {
    name: String,
    spec: String,
    created_at: i64,
}

fn library_path(app_data: &Path) -> PathBuf {
    app_data.join("filter_library.json")
}

fn read_library(app_data: &Path) -> Vec<LibFilter> {
    std::fs::read_to_string(library_path(app_data))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_library(app_data: &Path, list: &[LibFilter]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    std::fs::write(library_path(app_data), json).map_err(|e| e.to_string())
}

/// 登録済みフィルタの一覧（メニュー「ユーザーのフィルタ」の素材）。
#[tauri::command]
fn list_filter_library(state: State<AppState>) -> Result<Vec<LibFilter>, String> {
    Ok(read_library(&state.app_data))
}

/// フィルタ設定に名前を付けてライブラリへ登録。同名は上書き。
#[tauri::command]
fn save_filter_library(name: String, spec: String, state: State<AppState>) -> Result<(), String> {
    flow::parse_spec(&spec)?; // 壊れた定義を登録しない
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("フィルタ名を入力してください".into());
    }
    let mut list = read_library(&state.app_data);
    list.retain(|e| e.name != name);
    list.push(LibFilter {
        name,
        spec,
        created_at: db::now_ms(),
    });
    list.sort_by(|a, b| a.name.cmp(&b.name));
    write_library(&state.app_data, &list)
}

/// ライブラリからエントリを削除。
#[tauri::command]
fn delete_filter_library(name: String, state: State<AppState>) -> Result<(), String> {
    let mut list = read_library(&state.app_data);
    let before = list.len();
    list.retain(|e| e.name != name);
    if list.len() == before {
        return Err(format!("「{name}」はライブラリにありません"));
    }
    write_library(&state.app_data, &list)
}

// v1.3.11 ランチャーライブラリ（一度登録→リスト化）。フィルタライブラリと同方式・別ファイル。
fn launcher_library_path(app_data: &Path) -> PathBuf {
    app_data.join("launcher_library.json")
}
fn read_launcher_library(app_data: &Path) -> Vec<LibFilter> {
    std::fs::read_to_string(launcher_library_path(app_data))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn list_launcher_library(state: State<AppState>) -> Result<Vec<LibFilter>, String> {
    Ok(read_launcher_library(&state.app_data))
}

/// ランチャー設定に名前を付けてライブラリへ登録。同名は上書き。
#[tauri::command]
fn save_launcher_library(name: String, spec: String, state: State<AppState>) -> Result<(), String> {
    flow::parse_spec(&spec)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("名前を入力してください".into());
    }
    let mut list = read_launcher_library(&state.app_data);
    list.retain(|e| e.name != name);
    list.push(LibFilter {
        name,
        spec,
        created_at: db::now_ms(),
    });
    list.sort_by(|a, b| a.name.cmp(&b.name));
    let json = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    std::fs::write(launcher_library_path(&state.app_data), json).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_launcher_library(name: String, state: State<AppState>) -> Result<(), String> {
    let mut list = read_launcher_library(&state.app_data);
    let before = list.len();
    list.retain(|e| e.name != name);
    if list.len() == before {
        return Err(format!("「{name}」はランチャーライブラリにありません"));
    }
    let json = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    std::fs::write(launcher_library_path(&state.app_data), json).map_err(|e| e.to_string())
}

/// v1.3.11 #A: スタートメニューのショートカット(.lnk)からインストール済みアプリを列挙。
/// %ProgramData% と %APPDATA% の Start Menu\Programs を再帰走査。.lnk はそのまま起動でき
/// アイコンも取れるので、パスを探さず一覧から選べる。
#[derive(Serialize)]
pub struct InstalledApp {
    pub name: String,
    pub path: String,
}
#[tauri::command]
fn list_installed_apps() -> Result<Vec<InstalledApp>, String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ProgramData", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            roots.push(
                PathBuf::from(base)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs"),
            );
        }
    }
    let mut found: Vec<InstalledApp> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 再帰走査（深さ制限つき）。スタックで回す。
    let mut stack: Vec<(PathBuf, u32)> = roots.into_iter().map(|r| (r, 0u32)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if depth > 5 {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.is_dir() {
                stack.push((path, depth + 1));
            } else if path.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("lnk")).unwrap_or(false) {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                // Uninstall/アンインストール系の .lnk は除外（実用上ノイズ）。
                if name.is_empty() || name.contains("Uninstall") || name.contains("アンインストール") {
                    continue;
                }
                let key = name.to_lowercase();
                if seen.insert(key) {
                    found.push(InstalledApp {
                        name,
                        path: path.to_string_lossy().into_owned(),
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(found)
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

// ============================================================================
// フローレーン（構造の実行）— §4.4 共有2レーンの境界
// ----------------------------------------------------------------------------
// ここから下（FlowReport / FlowPayload / NodeRun / execute_node / eval_filter /
// run_flow / build_cmdline と、フィルタ／ランチャーライブラリ群）は「部分グラフの
// 実行と spec の取り回し」= フローレーン。実身/仮身/origin/CAS の「中身の共有」=
// データレーンとはコードパスを分けて考える（混ぜると食い違う、設計メモ §4.4）。
//
// 将来の整理メモ:
//  - この一群は flow_run.rs として lib.rs から切り出せる（v1.5 のファイル分割候補）。
//  - filter_library / launcher_library は現状「単一カード」だけを共有する。設計メモ §2 の
//    「部分グラフ＝モジュール」へ一般化する余地を残す（execute_node が部分グラフの
//    1ノード実行になっているのはその布石）。
// ============================================================================

#[derive(Serialize)]
pub struct FlowReport {
    /// 新しく生まれた結果カードの vid（front は緑シグナル＋UNDO登録に使う）
    pub created: Vec<String>,
    /// 固定名上書きで中身を差し替えた既存カードの vid（緑シグナルのみ・UNDO対象外）
    pub updated: Vec<String>,
    /// 生成されたファイル名（ダイアログ表示用）
    pub outputs: Vec<String>,
    /// 型不一致などでスキップした入力（ラベル＋理由）
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

/// パイプラインの中間結果。中間ファイル・中間カードは作らず、メモリで次段へ渡す
/// （外部コマンドの一時ファイルは flow.rs 側で即削除）。
struct FlowPayload {
    bytes: Vec<u8>,
    ext: String,
    mime: String,
    /// 連鎖の起点データのファイル名語幹（最終出力の命名に使う）
    stem: String,
}

/// §4.1 リファクタリング: ノード1回分の実行結果。`outputs` は生成物（builtin/ai は1個、
/// 外部は1個、ジェネレータは1個）。`exit_code`/`stderr` は外部プロセスを完走させたときの
/// 情報で、現状 eval_filter は使わない — 将来の exit code 分岐（§4.3「成功時/失敗時」の
/// flow 矢印）でここを見るための土台アンカー。
struct NodeRun {
    outputs: Vec<(Vec<u8>, String, String)>, // (bytes, ext, mime)
    #[allow(dead_code)] // §4.3 で消費予定
    exit_code: Option<i32>,
    #[allow(dead_code)] // §4.3 で消費予定
    stderr: String,
}

/// §4.1 実行ディスパッチの一本道: 「ノード種別 → 実行 → 結果（payload + exit code + stderr）」。
/// 種別判定（flow::NodeKind）に応じて内蔵フィルタ / AI / 外部プロセスへ振り分ける。
/// 入力の収集・型チェック・命名（stem）は呼び出し側（eval_filter）の責務で、ここは
/// 「与えられた入力でこの spec を1回実行する」ことだけに専念する（データ/フローの関心を分離）。
///
/// inputs は (bytes, ext, mime) の並び:
/// - Generator … 空（入力を取らない）
/// - Builtin / Ai … 必ず1個
/// - External … ファンアウト時1個 / 集約時 N 個
fn execute_node(
    root: &Path,
    spec: &flow::FilterSpec,
    kind: flow::NodeKind,
    inputs: &[(&[u8], &str, &str)],
    cancel: &std::sync::atomic::AtomicBool,
    stdin_slot: &flow::StdinSlot,
) -> Result<NodeRun, String> {
    use flow::NodeKind;
    let plain = |bytes: Vec<u8>, ext: String, mime: String| NodeRun {
        outputs: vec![(bytes, ext, mime)],
        exit_code: None,
        stderr: String::new(),
    };
    match kind {
        NodeKind::Builtin => {
            // builtin 名は node_kind が Builtin と判定した時点で Some（"ai" 以外）。
            let b = spec.builtin.as_deref().unwrap_or("");
            let (bytes, ext, mime) = flow::run_builtin(spec, b, inputs[0].0)?;
            Ok(plain(bytes, ext, mime))
        }
        NodeKind::Ai => {
            // v1.4.0 AIフィルタは builtin だがネットワークを使う別経路（同期blocking）。
            let (bytes, ext, mime) = flow::run_ai(spec, inputs[0].0)?;
            Ok(plain(bytes, ext, mime))
        }
        NodeKind::T2i => {
            // v1.5 Text-To-Image: 入力を種別で振り分け＝テキスト→プロンプト / 画像→元画像(img2img)。
            // 集約ノードなので inputs に複数（プロンプト＋元画像）が来る。AI 同様ネットワーク別経路。
            let mut prompt: Option<&[u8]> = None;
            let mut init: Option<&[u8]> = None;
            for (bytes, _ext, mime) in inputs {
                if mime.starts_with("image/") {
                    if init.is_none() {
                        init = Some(*bytes);
                    }
                } else if prompt.is_none() {
                    prompt = Some(*bytes); // テキスト等はプロンプト扱い（最初の1本）
                }
            }
            let prompt = prompt.ok_or("プロンプト（テキスト入力）が接続されていません")?;
            let (bytes, ext, mime) = flow::run_t2i(spec, prompt, init)?;
            Ok(plain(bytes, ext, mime))
        }
        NodeKind::Generator | NodeKind::External => {
            // 出力拡張子: output_ext 指定 > 最初の入力の拡張子 > "bin"
            //（ジェネレータは入力が無いので "bin"、外部は入力拡張子に追従）。
            let out_ext = spec
                .output_ext
                .clone()
                .unwrap_or_else(|| inputs.first().map(|i| i.1.to_string()).unwrap_or_else(|| "bin".into()));
            let out_mime = mime_for_ext(&out_ext)
                .unwrap_or("application/octet-stream")
                .to_string();
            let cmdline = build_cmdline(root, spec)?;
            let ins: Vec<(&[u8], &str)> = inputs.iter().map(|(b, e, _)| (*b, *e)).collect();
            // v1.4.9: record モードのジェネレータ（録音）だけ stdin を開いて 'q' で停止可能に。
            let slot = if kind == NodeKind::Generator && spec.record {
                Some(stdin_slot)
            } else {
                None
            };
            let o = flow::run_external_full(
                &cmdline,
                &ins,
                &out_ext,
                spec.timeout_secs,
                cancel,
                Some(root),
                slot,
            )?;
            let exit_code = o.exit_code;
            let stderr = o.stderr.clone();
            let bytes = o.into_bytes()?; // 非0終了/出力なしは従来どおりエラー（挙動不変）
            Ok(NodeRun {
                outputs: vec![(bytes, out_ext, out_mime)],
                exit_code,
                stderr,
            })
        }
        NodeKind::Invalid => Err("フィルタ定義に builtin も cmd もありません".into()),
    }
}

/// フィルタを再帰評価する（pull型連鎖 — 実行モデル3）。
/// 入力にフィルタカードが繋がっていれば先にそれを評価し、結果をこのフィルタに通す。
/// `visited` は評価中パスのスタックで、循環を検出したらエラー。
fn eval_filter(
    state: &AppState,
    root: &Path,
    fid: &str,
    visited: &mut std::collections::HashSet<String>,
    report: &mut FlowReport,
    cancel: &std::sync::atomic::AtomicBool,
    stdin_slot: &flow::StdinSlot,
) -> Result<Vec<FlowPayload>, String> {
    if !visited.insert(fid.to_string()) {
        return Err("フローに循環があります".into());
    }
    // 短ロック: フィルタ spec を読む（実行はこのあとロック外で）。
    let spec_json: Option<String> = with_db(state, |p| {
        p.conn
            .query_row(
                "SELECT filter_ref FROM virtual_bodies
                 WHERE id = ?1 AND kind = 'filter' AND deleted_at IS NULL",
                rusqlite::params![fid],
                |r| r.get(0),
            )
            .map_err(|_| "フィルタカードが見つかりません".to_string())
    })?;
    let spec = flow::parse_spec(spec_json.as_deref().unwrap_or("{}"))?;
    let accept = spec.accept_mime.clone().unwrap_or_else(|| "*/*".into());

    // ジェネレータ型: 入力を取らず1回実行してファイルを生む（入力矢印は無視）。
    // §4.1: 実行は execute_node に一任し、ここは命名（stem＝prefix or "output"）だけ担う。
    if spec.node_kind() == flow::NodeKind::Generator {
        let run = execute_node(root, &spec, flow::NodeKind::Generator, &[], cancel, stdin_slot);
        visited.remove(fid);
        return run.map(|r| {
            let stem = spec
                .prefix
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "output".into());
            r.outputs
                .into_iter()
                .map(|(bytes, ext, mime)| FlowPayload {
                    bytes,
                    ext,
                    mime,
                    stem: stem.clone(),
                })
                .collect()
        });
    }

    // 入力: このフィルタへ入る flow 矢印（生きているカードのみ・接続順）。
    // §4.3 拡張点アンカー: 将来 relations.kind='flow' に「成功時/失敗時/常に」の制御属性を
    // 持たせる（オートメーション設計メモ §1.2）。そのとき execute_node が返す NodeRun.exit_code
    // を見て分岐し、矢印の制御属性でこの収集をフィルタする。今はモデルを汚さず収集は無条件。
    // 短ロック: このフィルタへ入る flow 矢印（接続順）を取得。
    let inputs: Vec<(String, String, String, String, Option<String>, Option<String>)> =
        with_db(state, |p| {
            let mut stmt = p
                .conn
                .prepare(
                    "SELECT v.id, v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext), v.kind
                     FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                     JOIN real_bodies r ON r.id = v.real_id
                     WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                     ORDER BY rel.created_at",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![fid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        })?;

    let mut produced = Vec::new();
    // フェーズ1: 全入力のペイロードを接続順に収集（{inN} の番号は矢印を張った順）
    let mut collected: Vec<FlowPayload> = Vec::new();
    for (vid, real_id, vlabel, mime, ext, kind) in &inputs {
        // 入力ペイロードを集める: 上流フィルタ＝再帰評価／データカード＝窓の今を読む
        // 中断後は新しいプロセスを起こさない（複数入力の連鎖を断つ）
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            report.errors.push("中断されました".into());
            break;
        }
        let payloads: Vec<FlowPayload> = if kind.as_deref() == Some("filter") {
            // 再帰評価（呼び出し先が自分で短ロックを取る。ここではロックを保持しない）。
            match eval_filter(state, root, vid, visited, report, cancel, stdin_slot) {
                Ok(v) => v,
                Err(e) => {
                    report.errors.push(format!("{vlabel}: {e}"));
                    continue;
                }
            }
        } else if real_id.starts_with("drop:") {
            report
                .skipped
                .push(format!("{vlabel}（まだファイルが投入されていません）"));
            continue;
        } else {
            // 短ロック: 窓の今を解決（snapshot 更新＋ビューパス）。ファイル読みはロック外。
            let path = match with_db(state, |p| {
                refresh_origin_snapshot(p, vid);
                Ok(resolve_view_path(p, vid, real_id))
            }) {
                Ok(path) => path,
                Err(e) => {
                    report.errors.push(format!("{vlabel}: {e}"));
                    continue;
                }
            };
            match std::fs::read(&path) {
                Ok(d) => {
                    let stem = std::path::Path::new(vlabel)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("output")
                        .to_string();
                    vec![FlowPayload {
                        bytes: d,
                        ext: ext.clone().unwrap_or_else(|| "bin".into()),
                        mime: mime.clone(),
                        stem,
                    }]
                }
                Err(e) => {
                    report.errors.push(format!("{vlabel}: 読み込み失敗 ({e})"));
                    continue;
                }
            }
        };
        collected.extend(payloads);
    }

    // フェーズ2: 実行。種別ごとの分岐は execute_node へ一本化（§4.1）。ここに残るのは
    // 「型チェック（受け入れMIME・想定拡張子）」と「集約/ファンアウトの入力の束ね方」だけ。
    // 集約か個別かは CMD の書き方で決まる（{in*} か {in2} 以降があれば全入力を1回 = 集約:
    // gcc分割ソース・画像合成 / {in}/{in1} のみなら入力ごとに実行 = 従来のファンアウト）。
    let kind = spec.node_kind();
    // T2i は集約ノード（プロンプト＋元画像など全入力を集めて1回実行＝1枚生成）。
    if spec.is_aggregate() || kind == flow::NodeKind::T2i {
        // 集約実行は引数の位置・本数が意味を持つ → 合わない入力はスキップではなくエラー
        let mut bad = Vec::new();
        for pl in &collected {
            if !flow::mime_matches(&accept, &pl.mime) {
                bad.push(format!("{}（{} は受け入れ対象外）", pl.stem, pl.mime));
            } else if let Some(exts) = &spec.in_exts {
                let ok = exts.is_empty()
                    || exts
                        .iter()
                        .any(|e| e.trim().trim_start_matches('.').eq_ignore_ascii_case(&pl.ext));
                if !ok {
                    bad.push(format!("{}（.{} は想定拡張子外）", pl.stem, pl.ext));
                }
            }
        }
        if !bad.is_empty() {
            report.errors.extend(bad);
        } else if !collected.is_empty() {
            let ins: Vec<(&[u8], &str, &str)> = collected
                .iter()
                .map(|pl| (pl.bytes.as_slice(), pl.ext.as_str(), pl.mime.as_str()))
                .collect();
            match execute_node(root, &spec, kind, &ins, cancel, stdin_slot) {
                Ok(run) => {
                    let stem = collected[0].stem.clone();
                    for (bytes, ext, mime) in run.outputs {
                        produced.push(FlowPayload { bytes, ext, mime, stem: stem.clone() });
                    }
                }
                Err(e) => report.errors.push(format!("{}: {e}", collected[0].stem)),
            }
        }
    } else {
        for pl in collected {
            if !flow::mime_matches(&accept, &pl.mime) {
                report
                    .skipped
                    .push(format!("{}（{} は受け入れ対象外）", pl.stem, pl.mime));
                continue;
            }
            // 想定拡張子のチェック（新形式 IN欄。空なら制限なし）
            if let Some(exts) = &spec.in_exts {
                let ok = exts.is_empty()
                    || exts
                        .iter()
                        .any(|e| e.trim().trim_start_matches('.').eq_ignore_ascii_case(&pl.ext));
                if !ok {
                    report
                        .skipped
                        .push(format!("{}（.{} は想定拡張子外）", pl.stem, pl.ext));
                    continue;
                }
            }
            let ins = [(pl.bytes.as_slice(), pl.ext.as_str(), pl.mime.as_str())];
            match execute_node(root, &spec, kind, &ins, cancel, stdin_slot) {
                Ok(run) => {
                    for (bytes, ext, mime) in run.outputs {
                        produced.push(FlowPayload {
                            bytes,
                            ext,
                            mime,
                            stem: pl.stem.clone(),
                        });
                    }
                }
                Err(e) => report.errors.push(format!("{}: {e}", pl.stem)),
            }
        }
    }
    visited.remove(fid);
    Ok(produced)
}

/// フローを実行（手動トリガー — 確定方針4）。
/// このフィルタを終端としてパイプラインを pull 評価し（上流のフィルタ連鎖は
/// メモリ上で通過）、最終結果だけをプロジェクト直下のコミット済み実身として
/// カード化（緑シグナルで誕生）。フィルタ→結果の flow 矢印を張り、
/// 実行ログはフィルタカードの memo に記録する。
// (async) でスレッドプール実行にする — 同期コマンドはメインスレッドで走るため、
// 外部コマンドがストールするとイベントループごと固まり中断ボタンが届かない。
#[tauri::command(async)]
fn run_flow(id: String, state: State<AppState>) -> Result<FlowReport, String> {
    // v1.4.10 多重起動ガード（録音ハングの修正）: 実行中なら state.current をロックする前に
    // 即エラーで弾く。ロック前に弾くのが要点 — ここで待つと録音中フローの裏で積み上がる。
    if state
        .flow_running
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return Err(
            "ほかのフローを実行中です。完了（録音は「停止」）してからもう一度実行してください".into(),
        );
    }
    let _running = RunningGuard(&state.flow_running);

    // 短ロック: 終端フィルタの位置・ラベル・spec と root を取得（実行中はロックを手放す）。
    let (fx, fy, flabel, fref, root): (f64, f64, String, Option<String>, PathBuf) =
        with_db(state.inner(), |p| {
            let (fx, fy, flabel, fref): (f64, f64, String, Option<String>) = p
                .conn
                .query_row(
                    "SELECT pos_x, pos_y, label, filter_ref FROM virtual_bodies
                     WHERE id = ?1 AND kind = 'filter' AND deleted_at IS NULL",
                    rusqlite::params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .map_err(|_| "フィルタカードが見つかりません".to_string())?;
            Ok((fx, fy, flabel, fref, p.root.clone()))
        })?;
    // 終端フィルタの設定が出力先と命名を決める（prefix連番 / 出力フォルダ）
    let term_spec = flow::parse_spec(fref.as_deref().unwrap_or("{}"))?;

    // v1.5 Text-To-Image: 終端が t2i カードなら生成デーモンを先に起こしておく（▶実行を
    // 「ただ押せば動く」に）。best-effort — 起動できなければ run_t2i が分かりやすく案内する。
    if term_spec.node_kind() == flow::NodeKind::T2i {
        let _ = t2i_ensure_server(&state.app_data, state.inner());
    }

    // v1.3.11 対話シェル: cmd/PowerShell を detached で開いて一行目（cmd）を実行（捕捉しない）。
    // 接続された最初の入力の生きた実ファイルを {in} に展開する。§4.2: 種別判定は is_shell() に集約。
    if term_spec.is_shell() {
        let shell = term_spec.shell.as_deref().unwrap_or("").to_string();
        // 短ロック: 接続された最初の入力の生きた実ファイルを解決（起動はロック外）。
        let input: Option<PathBuf> = with_db(state.inner(), |p| {
            Ok(p
                .conn
                .query_row(
                    "SELECT v.id, v.real_id, v.committed_path FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                     WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                     ORDER BY rel.created_at LIMIT 1",
                    rusqlite::params![id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| e.to_string())?
                .map(|(vid, real_id, committed)| {
                    committed
                        .map(PathBuf::from)
                        .filter(|c| c.is_file())
                        .unwrap_or_else(|| resolve_view_path(p, &vid, &real_id))
                }))
        })?;
        let firstline = term_spec.cmd.clone().unwrap_or_default();
        let firstline = match &input {
            Some(pb) => firstline.replace("{in}", &format!("\"{}\"", pb.display())),
            None => firstline.replace("{in}", ""),
        };
        launch_interactive_shell(&shell, &firstline, &root)?;
        return Ok(FlowReport {
            created: vec![],
            updated: vec![],
            outputs: vec![],
            skipped: vec![],
            errors: vec![],
        });
    }

    let prefix = term_spec
        .prefix
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut seq = term_spec.seq.unwrap_or(0);
    let out_dir = term_spec
        .out_dir
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (out_root, external_out) = match &out_dir {
        Some(d) => {
            let pb = PathBuf::from(d);
            if !pb.is_dir() {
                return Err(format!("出力フォルダがありません: {d}"));
            }
            (pb, true)
        }
        None => (root.clone(), false),
    };

    let mut report = FlowReport {
        created: vec![],
        updated: vec![],
        outputs: vec![],
        skipped: vec![],
        errors: vec![],
    };
    // 前回の中断フラグを下ろしてから評価を始める
    state
        .flow_cancel
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let mut visited = std::collections::HashSet::new();
    // 評価フェーズ: eval_filter が DB 接触ごとに短ロックを取り、実行（外部/AI/録音）は
    // ロック外で走る。録音/AI 中も他のコマンドが state.current を取れる＝UI が固まらない。
    let payloads = eval_filter(
        state.inner(),
        &root,
        &id,
        &mut visited,
        &mut report,
        &state.flow_cancel,
        &state.rec_stdin,
    )?;
    if payloads.is_empty() && report.skipped.is_empty() && report.errors.is_empty() {
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

    // マテリアライズ（DB と fs のみ・長時間プロセスなし → 1回の短ロックでまとめて書く）。
    with_db(state.inner(), |p| {
    let mut out_i = 0usize;
    for pl in &payloads {
        // 結果の置き場と名前: prefix＋連番→「prefix連番.ext」（空き番号まで進める）、
        // prefixのみ（連番空欄）→「prefix.ext」固定名で毎回上書き（コンパイラ等）、
        // prefixなし→従来の「入力名_フィルタ名.ext」。out_dir指定なら外部フォルダへ。
        let dest = match (&prefix, term_spec.seq) {
            (Some(px), Some(_)) => loop {
                let cand = out_root.join(format!("{px}{seq:03}.{}", pl.ext));
                seq += 1;
                if !cand.exists() {
                    break cand;
                }
            },
            (Some(px), None) => out_root.join(format!("{px}.{}", pl.ext)),
            (None, _) => {
                unique_file_path(&out_root, &format!("{}_{safe_flabel}.{}", pl.stem, pl.ext))
            }
        };
        if let Err(e) = std::fs::write(&dest, &pl.bytes) {
            report.errors.push(format!("{}: 書き出し失敗 ({e})", pl.stem));
            continue;
        }
        let hash = match cas::store_bytes(&p.cas_root, &pl.bytes) {
            Ok(h) => h,
            Err(e) => {
                report.errors.push(format!("{}: CAS保存失敗 ({e})", pl.stem));
                continue;
            }
        };
        if let Err(e) =
            db::insert_real_body(&p.conn, &hash, &pl.mime, pl.bytes.len() as u64, Some(&pl.ext))
        {
            report.errors.push(format!("{}: {e}", pl.stem));
            continue;
        }
        let out_label = dest
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string();
        let dest_str = dest.to_string_lossy().into_owned();

        // 同じパスを指す生きたカードが既にあれば（固定名上書きの再実行）、
        // 新カードは作らず中身を差し替える — 位置と矢印は維持、緑シグナルで再誕生。
        let existing: Option<String> = p
            .conn
            .query_row(
                "SELECT id FROM virtual_bodies
                 WHERE deleted_at IS NULL AND (committed_path = ?1 OR origin_path = ?1)",
                rusqlite::params![dest_str],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        if let Some(vid) = existing {
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET real_id = ?1, committed_mtime = ?2 WHERE id = ?3",
                rusqlite::params![
                    hash,
                    if external_out { None } else { file_mtime_ms(&dest) },
                    vid
                ],
            );
            // フィルタ→結果の flow 矢印が無ければ張る（重複矢印は作らない）
            let has_rel: bool = p
                .conn
                .query_row(
                    "SELECT 1 FROM relations
                     WHERE from_id = ?1 AND to_id = ?2 AND kind = 'flow' AND deleted_at IS NULL",
                    rusqlite::params![id, vid],
                    |_| Ok(true),
                )
                .optional()
                .unwrap_or(None)
                .unwrap_or(false);
            if !has_rel {
                let _ = p.conn.execute(
                    "INSERT INTO relations (from_id, to_id, kind, created_at)
                     VALUES (?1, ?2, 'flow', ?3)",
                    rusqlite::params![id, vid, db::now_ms()],
                );
            }
            report.updated.push(vid);
            report.outputs.push(format!("{out_label}（上書き）"));
            continue;
        }

        let new_vid = uuid();
        let nx = fx + 190.0;
        let ny = fy + (out_i as f64) * 170.0;
        // 出力フォルダ指定時は外部ファイルへの窓（origin_path の仮身）、
        // 指定なしはプロジェクト直下のコミット済み実身（従来どおり）
        let (committed_path, committed_mtime, origin_path) = if external_out {
            (None, None, Some(dest_str))
        } else {
            (Some(dest_str), file_mtime_ms(&dest), None)
        };
        if let Err(e) = p.conn.execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at,
                 committed_path, committed_mtime, origin_path, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'file')",
            rusqlite::params![
                new_vid,
                hash,
                out_label,
                nx,
                ny,
                db::now_ms(),
                committed_path,
                committed_mtime,
                origin_path
            ],
        ) {
            report.errors.push(format!("{}: {e}", pl.stem));
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

    // 連番の永続化: 使った分だけ進めてカードに書き戻す
    // （未知フィールドを保持するため serde_json::Value 経由で seq だけ更新）
    if prefix.is_some() && term_spec.seq.unwrap_or(0) != seq {
        if let Ok(mut v) =
            serde_json::from_str::<serde_json::Value>(fref.as_deref().unwrap_or("{}"))
        {
            v["seq"] = seq.into();
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2",
                rusqlite::params![v.to_string(), id],
            );
        }
    }

    // 実行ログを memo に記録（最後の実行が常に見える — 確定方針4）。
    // v1.3.9: 先頭の ▶ は実行ボタンと紛らわしいので「✓ 前回」に変更（これは結果表示）。
    let mut log = format!("✓ 前回: 出力{}", report.outputs.len());
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
    Ok(())
    })?;

    Ok(report)
}

/// フロー実行の中断（UIの中断ボタン）。run_flow が外部コマンドでストールしていても
/// このコマンドは DB の Mutex を取らないため即座に通り、監視ループがツリーごと kill する。
#[tauri::command(async)]
fn break_flow(state: State<AppState>) {
    state
        .flow_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // 画像生成中ならデーモン側の生成も止める（GPU を握り続けないように）。best-effort。
    flow::t2i_post_cancel();
}

// --- v1.4.1 AIチャット（builtin:"ai" mode:"chat"）---------------------------
// 履歴は <root>/.texos/chats/<cardId>.json（source of truth）。system は履歴に
// 入れず、毎リクエストで spec.system から前置する。応答はストリーミングで
// "ai-chat-token" / "ai-chat-done" イベントにより UI へ逐次配信。

#[derive(Serialize, Deserialize, Clone)]
struct ChatMsg {
    role: String,
    content: String,
}

#[derive(Serialize, Clone)]
struct ChatTokenEvent {
    id: String,
    delta: String,
}

#[derive(Serialize, Clone)]
struct ChatDoneEvent {
    id: String,
    error: Option<String>,
}

fn chats_dir(root: &Path) -> PathBuf {
    texos_dir(root).join("chats")
}

fn chat_file(root: &Path, id: &str) -> PathBuf {
    // id は uuid（安全文字）だが念のためファイル名に使えない文字を落とす。
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    chats_dir(root).join(format!("{safe}.json"))
}

fn load_chat_history(root: &Path, id: &str) -> Vec<ChatMsg> {
    std::fs::read(chat_file(root, id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_chat_history(root: &Path, id: &str, msgs: &[ChatMsg]) -> Result<(), String> {
    std::fs::create_dir_all(chats_dir(root)).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(msgs).map_err(|e| e.to_string())?;
    std::fs::write(chat_file(root, id), json).map_err(|e| e.to_string())
}

/// 会話履歴から人が読む Markdown 実身ログカードを生成/更新する（毎ターン自動）。
/// 固定名「<ラベル>のログ.md」へ上書き → 既存カードがあれば中身差し替え（位置・矢印維持）、
/// 無ければ AI カードの隣に作って flow 矢印を張る（run_flow の出力カードと同じ流儀）。
fn upsert_chat_log_card(
    p: &Project,
    ai_id: &str,
    ai_label: &str,
    log_name: Option<&str>,
    history: &[ChatMsg],
) -> Result<(), String> {
    // 1) transcript Markdown（**You:** / **AI:**）
    let mut md = format!("# {ai_label}\n\n");
    for m in history {
        let who = match m.role.as_str() {
            "user" => "You",
            "assistant" => "AI",
            other => other,
        };
        md.push_str(&format!("**{who}:** {}\n\n", m.content));
    }
    let bytes = md.into_bytes();

    // 2) 出力ファイル名: log_name 指定があればそれ（.md を補う）、無ければ「<ラベル>のログ.md」。
    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                c if c.is_control() => '_',
                c => c,
            })
            .collect()
    };
    let fname = match log_name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => {
            let mut n = sanitize(name);
            if !n.to_lowercase().ends_with(".md") {
                n.push_str(".md");
            }
            n
        }
        None => format!("{}のログ.md", sanitize(ai_label).chars().take(24).collect::<String>()),
    };
    let dest = p.root.join(&fname);
    std::fs::write(&dest, &bytes).map_err(|e| format!("ログ書き出し失敗: {e}"))?;
    let dest_str = dest.to_string_lossy().into_owned();

    // 3) CAS + real_body
    let hash = cas::store_bytes(&p.cas_root, &bytes).map_err(|e| e.to_string())?;
    db::insert_real_body(&p.conn, &hash, "text/markdown", bytes.len() as u64, Some("md"))
        .map_err(|e| e.to_string())?;

    // 4) 既存ログカードがあれば中身差し替え、無ければ作成
    let existing: Option<String> = p
        .conn
        .query_row(
            "SELECT id FROM virtual_bodies
             WHERE deleted_at IS NULL AND (committed_path = ?1 OR origin_path = ?1)",
            rusqlite::params![dest_str],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);

    let log_id = if let Some(vid) = existing {
        p.conn
            .execute(
                "UPDATE virtual_bodies SET real_id = ?1, committed_mtime = ?2 WHERE id = ?3",
                rusqlite::params![hash, file_mtime_ms(&dest), vid],
            )
            .map_err(|e| e.to_string())?;
        vid
    } else {
        let (fx, fy): (f64, f64) = p
            .conn
            .query_row(
                "SELECT pos_x, pos_y FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![ai_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0.0, 0.0));
        let vid = uuid();
        p.conn
            .execute(
                "INSERT INTO virtual_bodies
                    (id, real_id, label, pos_x, pos_y, created_at,
                     committed_path, committed_mtime, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'file')",
                rusqlite::params![
                    vid,
                    hash,
                    fname,
                    fx + 230.0,
                    fy,
                    db::now_ms(),
                    dest_str,
                    file_mtime_ms(&dest),
                ],
            )
            .map_err(|e| e.to_string())?;
        vid
    };

    // 5) AI → ログ の flow 矢印（無ければ張る）
    let has_rel: bool = p
        .conn
        .query_row(
            "SELECT 1 FROM relations
             WHERE from_id = ?1 AND to_id = ?2 AND kind = 'flow' AND deleted_at IS NULL",
            rusqlite::params![ai_id, log_id],
            |_| Ok(true),
        )
        .optional()
        .unwrap_or(None)
        .unwrap_or(false);
    if !has_rel {
        let _ = p.conn.execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at) VALUES (?1, ?2, 'flow', ?3)",
            rusqlite::params![ai_id, log_id, db::now_ms()],
        );
    }
    Ok(())
}

/// v1.4.4 マルチコンテキスト: AIカードへ flow 矢印で繋がった上流カードの内容を
/// 接続順（rel.created_at）に集める。データカードは現在値を読み、フィルタカードは
/// 評価して結果を文脈にする（URL→取得→テキスト等のパイプライン結果も使える）。
/// bytes+mime を保持して返すのでマルチモーダル（画像→image_url）への拡張は
/// メッセージ組立側の分岐追加だけで済む。
fn collect_context(
    state: &AppState,
    root: &Path,
    ai_id: &str,
    cancel: &std::sync::atomic::AtomicBool,
    stdin_slot: &flow::StdinSlot,
) -> Vec<FlowPayload> {
    // 短ロック: 接続カードを取得（best-effort・失敗は空）。評価・読みはロック外。
    let inputs: Vec<(String, String, String, String, Option<String>, Option<String>)> =
        with_db(state, |p| {
            let mut stmt = p
                .conn
                .prepare(
                    "SELECT v.id, v.real_id, v.label, COALESCE(v.mime_type, r.mime_type), COALESCE(v.ext, r.ext), v.kind
                     FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                     JOIN real_bodies r ON r.id = v.real_id
                     WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                     ORDER BY rel.created_at",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![ai_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })
                .map_err(|e| e.to_string())?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    let mut report = FlowReport {
        created: vec![],
        updated: vec![],
        outputs: vec![],
        skipped: vec![],
        errors: vec![],
    };
    let mut visited = std::collections::HashSet::new();
    for (vid, real_id, vlabel, mime, ext, kind) in &inputs {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if kind.as_deref() == Some("filter") {
            if let Ok(v) = eval_filter(state, root, vid, &mut visited, &mut report, cancel, stdin_slot) {
                out.extend(v);
            }
        } else if real_id.starts_with("drop:") {
            continue;
        } else {
            // 短ロック: 窓の今を解決。ファイル読みはロック外。
            let path = match with_db(state, |p| {
                refresh_origin_snapshot(p, vid);
                Ok(resolve_view_path(p, vid, real_id))
            }) {
                Ok(path) => path,
                Err(_) => continue,
            };
            if let Ok(d) = std::fs::read(&path) {
                let stem = std::path::Path::new(vlabel)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(vlabel)
                    .to_string();
                out.push(FlowPayload {
                    bytes: d,
                    ext: ext.clone().unwrap_or_else(|| "bin".into()),
                    mime: mime.clone(),
                    stem,
                });
            }
        }
    }
    out
}

/// 画像か（mime と拡張子の両方で判定 — 取り込み元で mime が欠ける場合に備える）。
fn is_image(mime: &str, ext: &str) -> bool {
    mime.starts_with("image/")
        || matches!(
            ext.to_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
        )
}

/// 文脈に使えるテキストか（マルチモーダルは次段。今はテキストのみ注入）。
fn is_textual(mime: &str, ext: &str) -> bool {
    if mime.starts_with("text/") || mime == "application/json" {
        return true;
    }
    matches!(
        ext.to_lowercase().as_str(),
        "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "log"
            | "html" | "py" | "js" | "ts" | "rs" | "c" | "cpp" | "h" | "css"
    )
}

/// 収集した文脈から system 資料メッセージを組み立てる（接続順・各資料を上限で切る）。
/// テキスト以外（画像等）は今は除外し、件数だけ末尾に注記する（マルチモーダルは次段）。
fn build_context_message(context: &[FlowPayload]) -> Option<String> {
    const PER_ITEM_CAP: usize = 8000; // 1資料あたりの文字数上限（コンテキスト溢れ対策）
    let mut texts: Vec<(String, String)> = Vec::new();
    let mut nontext = 0usize;
    for it in context {
        if is_textual(&it.mime, &it.ext) {
            let mut text = String::from_utf8_lossy(&it.bytes).into_owned();
            if text.chars().count() > PER_ITEM_CAP {
                text = text.chars().take(PER_ITEM_CAP).collect::<String>() + "\n…(長いため以下略)";
            }
            texts.push((it.stem.clone(), text));
        } else if !is_image(&it.mime, &it.ext) {
            // 画像は user 発話に添付するのでここでは数えない（音声/PDF 等のみ未対応注記）。
            nontext += 1;
        }
    }
    if texts.is_empty() {
        return None;
    }
    let mut msg = String::from(
        "以下の資料を参考に、ユーザーの質問へ日本語で答えてください。\
         資料に記載が無い事柄は推測せず「資料には記載がありません」と述べてください。\n\n",
    );
    for (i, (label, text)) in texts.iter().enumerate() {
        msg.push_str(&format!("=== 資料{}: {} ===\n{}\n\n", i + 1, label, text));
    }
    if nontext > 0 {
        msg.push_str(&format!(
            "（このほかテキスト以外の入力が{nontext}件接続されていますが、現在は未対応です）\n"
        ));
    }
    Some(msg)
}

/// 1ターン送信。ユーザー発話を履歴に積み、ストリーミングで応答を生成して
/// トークンをイベント配信し、完了後に履歴へ保存する。完成本文を返す。
#[tauri::command(async)]
fn ai_chat_send(
    window: tauri::WebviewWindow,
    state: State<AppState>,
    id: String,
    message: String,
) -> Result<String, String> {
    // 1) カードの spec/label/root を短時間ロックで取得（ストリーム中・文脈評価中は握らない）。
    let (spec, root, card_label) = with_db(state.inner(), |p| {
        let (fref, label): (Option<String>, String) = p
            .conn
            .query_row(
                "SELECT filter_ref, label FROM virtual_bodies
                 WHERE id = ?1 AND kind = 'filter' AND deleted_at IS NULL",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| "AIカードが見つかりません".to_string())?;
        let spec = flow::parse_spec(fref.as_deref().unwrap_or("{}"))?;
        if spec.builtin.as_deref() != Some("ai") {
            return Err("このカードはAIカードではありません".into());
        }
        Ok((spec, p.root.clone(), label))
    })?;
    // v1.4.4 マルチコンテキスト: 接続カードを毎ターン収集（編集が即反映）。v1.4.11: ロック外で
    // 評価する（上流にフィルタ連鎖があっても DB を握らない＝他操作が固まらない）。
    let context = collect_context(
        state.inner(),
        &root,
        &id,
        &std::sync::atomic::AtomicBool::new(false),
        &state.rec_stdin,
    );

    let user = message.trim().to_string();
    if user.is_empty() {
        return Err("メッセージが空です".into());
    }

    let mut history = load_chat_history(&root, &id);
    history.push(ChatMsg {
        role: "user".into(),
        content: user,
    });

    // リクエスト用 messages（system → 接続文脈 → 会話履歴 の順）
    let mut messages: Vec<serde_json::Value> = Vec::new();
    if let Some(sys) = spec
        .system
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        messages.push(serde_json::json!({ "role": "system", "content": sys }));
    }
    // v1.4.4 接続カードの内容を資料として注入（接続順・テキストのみ・画像は次段）。
    if let Some(ctx) = build_context_message(&context) {
        messages.push(serde_json::json!({ "role": "system", "content": ctx }));
    }
    for m in &history {
        messages.push(serde_json::json!({ "role": m.role, "content": m.content }));
    }

    // v1.4.5 マルチモーダル: 接続された画像カードを現在のユーザー発話に添付する。
    // provider で形式が違う（Ollama=メッセージの images:[bare base64] / OpenAI準拠=
    // content を [text, image_url(data URI)] の配列に）。ビジョン対応モデル（gemma3 等）が必要。
    let images: Vec<(String, String)> = context
        .iter()
        .filter(|it| is_image(&it.mime, &it.ext))
        .map(|it| {
            (
                base64::engine::general_purpose::STANDARD.encode(&it.bytes),
                it.mime.clone(),
            )
        })
        .collect();
    if !images.is_empty() {
        let provider = spec.provider.as_deref().unwrap_or("ollama");
        if let Some(last) = messages.last_mut() {
            if last.get("role").and_then(|v| v.as_str()) == Some("user") {
                if provider == "openai" {
                    let text = last
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut parts = vec![serde_json::json!({ "type": "text", "text": text })];
                    for (b64, mime) in &images {
                        parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{mime};base64,{b64}") }
                        }));
                    }
                    last["content"] = serde_json::Value::Array(parts);
                } else {
                    let imgs: Vec<serde_json::Value> =
                        images.iter().map(|(b64, _)| serde_json::json!(b64)).collect();
                    last["images"] = serde_json::Value::Array(imgs);
                }
            }
        }
    }

    state
        .chat_cancel
        .store(false, std::sync::atomic::Ordering::Relaxed);

    // ストリーミングは tokio 文脈外の素のOSスレッドで（run_ai と同じ理由）。
    let app = window.app_handle().clone();
    let result = std::thread::scope(|s| {
        s.spawn(|| {
            let mut on_token = |tok: &str| {
                let _ = app.emit(
                    "ai-chat-token",
                    ChatTokenEvent {
                        id: id.clone(),
                        delta: tok.to_string(),
                    },
                );
            };
            flow::run_ai_chat_stream(&spec, &messages, &state.chat_cancel, &mut on_token)
        })
        .join()
        .map_err(|_| "AI実行スレッドが異常終了しました".to_string())?
    });

    match result {
        Ok(answer) => {
            history.push(ChatMsg {
                role: "assistant".into(),
                content: answer.clone(),
            });
            let _ = save_chat_history(&root, &id, &history);
            // MD実身ログを毎ターン自動更新（固定名上書き＝位置と矢印を保ち緑シグナル再誕生）。
            {
                let guard = state.current.lock().unwrap();
                if let Some(p) = guard.as_ref() {
                    if let Err(e) =
                        upsert_chat_log_card(p, &id, &card_label, spec.log_name.as_deref(), &history)
                    {
                        eprintln!("チャットログ更新失敗: {e}");
                    }
                }
            }
            let _ = app.emit(
                "ai-chat-done",
                ChatDoneEvent {
                    id: id.clone(),
                    error: None,
                },
            );
            Ok(answer)
        }
        Err(mut e) => {
            // 画像添付時のエラーはモデル非対応が多い → ヒントを添える。
            if !images.is_empty() && (e.contains("400") || e.to_lowercase().contains("image")) {
                e.push_str("\n（ヒント: 画像を見るにはビジョン対応モデルが必要です。例: gemma3:latest。qwen等のテキスト専用モデルは画像を受け付けません）");
            }
            // 失敗時はユーザー発話を保存しない（次回リトライで二重に積まれない）
            let _ = app.emit(
                "ai-chat-done",
                ChatDoneEvent {
                    id: id.clone(),
                    error: Some(e.clone()),
                },
            );
            Err(e)
        }
    }
}

/// チャット履歴の読み出し（パネル描画用）。
#[tauri::command]
fn ai_chat_history(state: State<AppState>, id: String) -> Result<Vec<ChatMsg>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    Ok(load_chat_history(&p.root, &id))
}

#[derive(Serialize)]
struct ContextLabel {
    label: String,
    /// "image"（添付対象）/ "text"（資料注入）/ "other"（未対応）。📎表示の診断用。
    kind: String,
}

/// v1.4.4 接続中の文脈カード（接続順）。パネルの「📎資料」表示用。
/// mime/ext から種別を付けて返す（画像が image として認識されているか目視できる）。
#[tauri::command]
fn ai_context_labels(state: State<AppState>, id: String) -> Result<Vec<ContextLabel>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut stmt = p
        .conn
        .prepare(
            "SELECT v.label, COALESCE(v.mime_type, r.mime_type, ''), COALESCE(v.ext, r.ext, '')
             FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
             JOIN real_bodies r ON r.id = v.real_id
             WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
             ORDER BY rel.created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .filter_map(|r| r.ok())
        .map(|(label, mime, ext)| {
            let kind = if is_image(&mime, &ext) {
                "image"
            } else if is_textual(&mime, &ext) {
                "text"
            } else {
                "other"
            };
            ContextLabel {
                label,
                kind: kind.into(),
            }
        })
        .collect())
}

/// v1.4.2 設定ダイアログ用: 接続先のモデル一覧を取得（Ollama /api/tags 等）。
/// プロジェクト非依存（接続先だけで決まる）なので State を取らない。
#[tauri::command(async)]
fn ai_list_models(
    provider: Option<String>,
    endpoint: Option<String>,
) -> Result<Vec<String>, String> {
    flow::list_models(
        provider.as_deref().unwrap_or("ollama"),
        endpoint.as_deref().unwrap_or(""),
    )
}

// --- v1.4.6 Ollama セットアップ案内（未導入PC向け）-----------------------------

#[derive(Serialize, Clone)]
struct OllamaStatus {
    /// サーバ（localhost:11434 等）が応答するか＝すぐ使える
    running: bool,
    /// 実行ファイルが見つかるか（起動するだけで使える）
    installed: bool,
    /// winget があるか（自動インストール可否）
    can_winget: bool,
}

fn ollama_base(endpoint: Option<&str>) -> String {
    endpoint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("http://localhost:11434")
        .trim_end_matches('/')
        .to_string()
}

/// Ollama 実行ファイルを探す（既定インストール先 → 環境変数PATH）。
fn ollama_binary() -> Option<PathBuf> {
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

fn has_winget() -> bool {
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
fn ollama_running(base: &str) -> bool {
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
fn ai_ollama_status(endpoint: Option<String>) -> OllamaStatus {
    let base = ollama_base(endpoint.as_deref());
    OllamaStatus {
        running: ollama_running(&base),
        installed: ollama_binary().is_some(),
        can_winget: has_winget(),
    }
}

/// winget で Ollama を自動インストール（Windows）。長時間ブロックする。
#[tauri::command(async)]
fn ai_install_ollama() -> Result<(), String> {
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
fn ai_start_ollama() -> Result<(), String> {
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
fn ai_pull_model(
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
#[tauri::command(async)]
fn ai_chat_cancel(state: State<AppState>) {
    state
        .chat_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

/// 会話をリセット（履歴ファイルを削除）。
#[tauri::command]
fn ai_chat_clear(state: State<AppState>, id: String) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let f = chat_file(&p.root, &id);
    if f.exists() {
        std::fs::remove_file(&f).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// --- v1.4.8 Whisper 自動導入（音声→テキスト・A案: app_data 共有）-------------------
// whisper.cpp の CLI とモデルを %APPDATA%\com.yoshi.texos\whisper\ に置き、
// Whisper カードは絶対パスで参照する（巨大モデルをプロジェクト間で重複させない）。
// バイナリは安定版にピン留め（重大な不具合があれば上げる）。HTTP は reqwest::blocking を
// tokio 文脈外スレッドで呼ぶ（[[project_texos_ai_card]] の落とし穴回避と同じ作法）。

/// 安定版にピン留め（ggml-org/whisper.cpp リリース）。
const WHISPER_TAG: &str = "v1.9.1";

fn whisper_root(app_data: &Path) -> PathBuf {
    app_data.join("whisper")
}
fn whisper_bin_dir(app_data: &Path) -> PathBuf {
    whisper_root(app_data).join("bin")
}
fn whisper_models_dir(app_data: &Path) -> PathBuf {
    whisper_root(app_data).join("models")
}
fn whisper_cli_path(app_data: &Path) -> PathBuf {
    whisper_bin_dir(app_data).join("whisper-cli.exe")
}

/// ffmpeg が PATH（または WinGet Links）にあるか。フィルタ実行は name="ffmpeg.exe" を
/// .texos\filters→PATH で解決するため、ここでも PATH を基本に判定する。
fn ffmpeg_available() -> bool {
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
fn list_whisper_models(app_data: &Path) -> Vec<String> {
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
struct WhisperStatus {
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
fn whisper_status(state: State<AppState>) -> WhisperStatus {
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
fn whisper_install_ffmpeg() -> Result<(), String> {
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
fn whisper_install(window: tauri::WebviewWindow, state: State<AppState>) -> Result<(), String> {
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
fn whisper_download_model(
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
fn whisper_set_model(state: State<AppState>, model: String) -> Result<(), String> {
    let mut cfg = read_config(&state.app_data);
    cfg.whisper_model = Some(model.trim().to_string());
    write_config(&state.app_data, &cfg);
    Ok(())
}

// --- v1.4.9 子プロセス stdin チャネル（汎用）＋ 録音デバイス列挙 ---------------------
// A の最小版: 実行中フィルタ（record モード）の stdin へ文字列を送る道。録音停止は "q"。
// 将来は対話プログラムへの入力やキー転送（キャンバス内ターミナル）へ拡張できる。
// 暴走プログラムの停止（CTRL_BREAK シグナル＋Job Object）は別チャネル＝#4 で実装予定。

/// 実行中の子プロセスの stdin に文字列（+改行）を送る。録音の正常停止は text="q"。
#[tauri::command(async)]
fn flow_send_stdin(state: State<AppState>, text: String) -> Result<(), String> {
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
fn list_audio_devices() -> Result<Vec<String>, String> {
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

const DROP_MIME: &str = "application/vnd.texos.drop";

/// ドロップポイント（データ入口カード）を作成。中身は空（sentinel）で始まり、
/// ファイルを投入するたびに「最後に投入したファイルへの窓」になる仮身。
#[tauri::command]
fn create_drop_point(x: f64, y: f64, state: State<AppState>) -> Result<String, String> {
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

// ---------------------------------------------------------------------------
// v1.3.11 ランチャーカード（一般プログラムの登録・起動＝アイデアメモ #2 の実用最小版）
// フィルタ（同期・タイムアウト・stdout捕捉）と違い、GUIアプリを detached 起動し待たない。
// 時間制限なし（暴走停止は当面 Windows タスクマネージャ任せ。将来 Stop を装備）。
// {in} 接続があればそのファイルを引数で開く。保存後の新規ファイルは import_new_project_files で回収。
// ---------------------------------------------------------------------------

const LAUNCHER_MIME: &str = "application/vnd.texos.launcher";

/// ランチャーカードを作成（kind='launcher'、定義は filter_ref の JSON を流用）。
/// spec 例: 一般プログラム {"name":"app.exe","path":"C:\\...\\app.exe","type":"launcher"}
///          ターミナル     {"name":"cmd","type":"launcher"} / {"name":"powershell",...}
#[tauri::command]
fn create_launcher(
    label: String,
    spec: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    flow::parse_spec(&spec)?; // 保存前に検証
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES ('launcher:sentinel', ?1, 0, ?2)",
            rusqlite::params![LAUNCHER_MIME, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
             VALUES (?1, 'launcher:sentinel', ?2, ?3, ?4, ?5, 'launcher', ?6)",
            rusqlite::params![vid, label, x, y, db::now_ms(), spec],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// ランチャーを起動。接続された入力（flow 矢印）の「生きた実ファイル」を引数で開く。
/// GUI アプリは detached 起動し待たない（時間制限なし）。
#[tauri::command(async)]
fn run_launcher(id: String, state: State<AppState>) -> Result<(), String> {
    let (spec, root, inputs): (flow::FilterSpec, PathBuf, Vec<PathBuf>) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let fref: String = p
            .conn
            .query_row(
                "SELECT COALESCE(filter_ref, '{}') FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let spec = flow::parse_spec(&fref)?;
        // 入力: このランチャーへ入る flow 矢印（接続順）。編集が永続するよう
        // committed_path（実身の実ファイル）優先。無ければ窓の今を解決。
        let mut paths: Vec<PathBuf> = Vec::new();
        {
            let mut stmt = p
                .conn
                .prepare(
                    "SELECT v.id, v.real_id, v.committed_path
                     FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                     WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                     ORDER BY rel.created_at",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (vid, real_id, committed) = row.map_err(|e| e.to_string())?;
                let path = committed
                    .map(PathBuf::from)
                    .filter(|c| c.is_file())
                    .unwrap_or_else(|| resolve_view_path(p, &vid, &real_id));
                paths.push(path);
            }
        }
        (spec, p.root.clone(), paths)
    };

    let name = spec.name.as_deref().unwrap_or("").trim().to_lowercase();
    let root_disp = root.display().to_string();

    // 起動するコマンドライン（cmd /C "<...>" で実行）。`start` で新プロセスを切り離す。
    let cmdline = if name == "cmd" || name == "cmd.exe" {
        format!("start \"\" cmd /K cd /d \"{root_disp}\"")
    } else if name == "powershell" || name == "powershell.exe" || name == "pwsh" {
        format!("start \"\" powershell -NoExit -Command \"Set-Location -LiteralPath '{root_disp}'\"")
    } else {
        let exe = flow::resolve_program(
            spec.name.as_deref().unwrap_or("").trim(),
            spec.path.as_deref(),
            &filters_dir(&root),
        )
        .ok_or("登録されたプログラムが見つかりません（パスを確認してください）")?;
        let mut s = format!("start \"\" \"{}\"", exe.display());
        for inp in &inputs {
            s.push_str(&format!(" \"{}\"", inp.display()));
        }
        s
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // run_flow と同じ定石: cmd /C "<cmdline>" を raw_arg で素のまま渡す。
        // CREATE_NO_WINDOW で親 cmd のコンソールを出さない（start が本体の窓を出す）。
        std::process::Command::new("cmd")
            .raw_arg(format!("/C \"{cmdline}\""))
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| format!("起動に失敗しました: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        let _ = cmdline;
        let exe = flow::resolve_program(
            spec.name.as_deref().unwrap_or("").trim(),
            spec.path.as_deref(),
            &filters_dir(&root),
        )
        .ok_or("登録されたプログラムが見つかりません")?;
        std::process::Command::new(exe)
            .args(inputs.iter().map(|p| p.as_os_str()))
            .spawn()
            .map_err(|e| format!("起動に失敗しました: {e}"))?;
    }
    Ok(())
}

/// v1.3.11 対話シェル（フィルタの shell モード）: cmd/PowerShell を detached で開き、
/// 一時スクリプト（cd → 一行目）を実行して対話を継続（cmd /K / powershell -NoExit）。
/// 引用符の入れ子を避けるため一時 .bat/.ps1 に書き出してから起動する。
fn launch_interactive_shell(shell: &str, firstline: &str, root: &Path) -> Result<(), String> {
    let dir = temp_view_dir().join("shell");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let root_disp = root.display().to_string();
    let cmdline = if shell.eq_ignore_ascii_case("powershell") || shell.eq_ignore_ascii_case("pwsh") {
        let script = dir.join(format!("{}.ps1", uuid()));
        let body = format!(
            "Set-Location -LiteralPath '{}'\r\n{}\r\n",
            root_disp.replace('\'', "''"),
            firstline
        );
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        format!(
            "start \"\" powershell -NoExit -ExecutionPolicy Bypass -File \"{}\"",
            script.display()
        )
    } else {
        let script = dir.join(format!("{}.bat", uuid()));
        let body = format!("@echo off\r\ncd /d \"{}\"\r\n{}\r\n", root_disp, firstline);
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        format!("start \"\" cmd /K \"{}\"", script.display())
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .raw_arg(format!("/C \"{cmdline}\""))
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| format!("シェルの起動に失敗しました: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        let _ = cmdline;
    }
    Ok(())
}

/// v1.3.11: プロジェクトフォルダ直下の「未登録ファイル」を実身として取り込む。
/// ランチャーで起動したアプリが新規保存したファイルを TexoS に回収する用途（保存→新しい実身）。
/// .texos 配下とサブフォルダは対象外（フラット運用の原則）。新規カードを (x,y) から並べる。
#[tauri::command]
fn import_new_project_files(
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<Vec<String>, String> {
    // 1) 未登録ファイルを収集（ここだけロック）。import_file が内部で再ロックするため、
    //    収集後にロックを解放してから取り込む（再ロック＝デッドロック回避）。
    let candidates: Vec<String> = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let mut cands = Vec::new();
        let entries = std::fs::read_dir(&p.root).map_err(|e| e.to_string())?;
        for ent in entries.flatten() {
            let path = ent.path();
            if !path.is_file() {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            let known: bool = p
                .conn
                .query_row(
                    "SELECT 1 FROM virtual_bodies WHERE committed_path = ?1 AND deleted_at IS NULL",
                    rusqlite::params![path_str],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .is_some();
            if !known {
                cands.push(path_str);
            }
        }
        cands
    };

    // 2) ロック解放後に取り込む（import_file が in_project 判定で実身として登録）。
    let mut created = Vec::new();
    for (i, path_str) in candidates.into_iter().enumerate() {
        let cx = x + (i % 5) as f64 * 170.0;
        let cy = y + (i / 5) as f64 * 140.0;
        if let Ok(vid) = import_file(path_str, String::new(), cx, cy, state.clone()) {
            created.push(vid);
        }
    }
    Ok(created)
}

/// ドロップポイントへファイルを投入: CASスナップショット＋窓の付け替え。
/// （取り込み = import_file と同じ意味論。カードは増えず、入口の中身が入れ替わる）
#[tauri::command]
fn drop_into_point(id: String, path: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let src = std::path::Path::new(&path);
    if !src.is_file() {
        return Err("ファイルではありません（フォルダは投入できません）".into());
    }
    let mime = mime_from_path(src);
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    let hash = cas::store_file(&p.cas_root, src).map_err(|e| e.to_string())?;
    let size = src.metadata().map_err(|e| e.to_string())?.len();
    db::insert_real_body(&p.conn, &hash, &mime, size, ext.as_deref())
        .map_err(|e| e.to_string())?;
    let n = p
        .conn
        .execute(
            "UPDATE virtual_bodies SET real_id = ?1, origin_path = ?2
             WHERE id = ?3 AND kind = 'drop' AND deleted_at IS NULL",
            rusqlite::params![hash, path, id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("ドロップポイントが見つかりません".into());
    }
    Ok(())
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

/// v1.3.6: 仮身の ext/mime をバックフィルする（開くたび修復）。空ファイル等で実身を
/// 共有して別形式に潰れた既存データ（ビットマップ.bmp が ods など）を、ラベルの拡張子
/// から本来の型に直す。ext が既にある行・拡張子の無い行・URL/フォルダ/プロジェクト/
/// フィルタ等は対象外（ファイルカードのみ）。
fn backfill_vbody_ext(conn: &Connection) {
    let rows: Vec<(String, String)> = match conn.prepare(
        "SELECT id, label FROM virtual_bodies
         WHERE ext IS NULL AND label IS NOT NULL
           AND url_ref IS NULL AND folder_ref IS NULL AND project_ref IS NULL
           AND (kind IS NULL OR kind IN ('file', 'text'))",
    ) {
        Ok(mut stmt) => stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default(),
        Err(_) => return,
    };
    for (id, label) in rows {
        // ラベル末尾の拡張子を取り出す（"Word 文書.docx" → "docx"）。
        let ext = match label.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() && !e.is_empty() => e.to_lowercase(),
            _ => continue,
        };
        if let Some(mime) = mime_for_ext(&ext) {
            let _ = conn.execute(
                "UPDATE virtual_bodies SET ext = ?1, mime_type = ?2 WHERE id = ?3",
                rusqlite::params![ext, mime, id],
            );
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
// --- v1.5 Text-To-Image（builtin:"t2i"）: 常駐 diffusers デーモンの管理 ----------------
// Whisper/Ollama と同じ流儀: 共有 app-data 配下にモデル/スクリプトを置き、status コマンド＋
// 進捗イベント（"t2i-model"）でセットアップを案内する。デーモン本体（t2i_server.py）は
// ビルドに埋め込み、起動時に app-data へ書き出す（リリースでも resources 設定不要）。

/// デーモン本体を実行ファイルへ埋め込む（src-tauri/src → ../../python）。
const T2I_SERVER_PY: &str = include_str!("../../python/t2i_server.py");

/// 期待するデーモンの版（t2i_server.py の SERVER_VERSION と一致させること）。
/// 稼働中デーモンの版がこれと違えば「古い」と見なして起動し直す（再ビルド反映）。
const T2I_SERVER_VERSION: &str = "7";

/// 既知モデル（friendly name → 表示名）。t2i_server.py の MODELS と対応させること。
/// (name, label, gated, hf_url)。gated=true は HF 認証（HF_TOKEN）が必要。
/// hf_url はライセンス承諾ページ（ユーザーが開いて Agree する先）。
const T2I_KNOWN_MODELS: &[(&str, &str, bool, &str)] = &[
    ("sd1.5", "Stable Diffusion 1.5（軽量・高速・~2GB）", false,
     "https://huggingface.co/stable-diffusion-v1-5/stable-diffusion-v1-5"),
    ("dreamshaper", "Dreamshaper 8（SD1.5系・高品質・~2GB）", false,
     "https://huggingface.co/Lykon/dreamshaper-8"),
    ("flux-lite", "FLUX.1 Lite 8B（オープン・高品質・~26GB）", false,
     "https://huggingface.co/Freepik/flux.1-lite-8B"),
    ("flux-schnell", "FLUX.1 schnell（公式・要HF認証・~34GB）", true,
     "https://huggingface.co/black-forest-labs/FLUX.1-schnell"),
    ("flux-dev", "FLUX.1 dev（公式・非商用・要HF認証・~34GB）", true,
     "https://huggingface.co/black-forest-labs/FLUX.1-dev"),
];

fn t2i_root(app_data: &Path) -> PathBuf {
    app_data.join("t2i")
}
fn t2i_models_dir(app_data: &Path) -> PathBuf {
    t2i_root(app_data).join("models")
}
fn t2i_server_script(app_data: &Path) -> PathBuf {
    t2i_root(app_data).join("t2i_server.py")
}

/// config の HF トークンをデーモンが読むファイル（<home>/hf_token）へ書き出す（無ければ削除）。
/// デーモンは pull のたびにこのファイルを読むので、これだけで再起動なしにトークンが効く。
fn sync_t2i_token_file(app_data: &Path) {
    let f = t2i_root(app_data).join("hf_token");
    match read_config(app_data).t2i_hf_token.filter(|t| !t.trim().is_empty()) {
        Some(tok) => {
            let _ = std::fs::create_dir_all(t2i_root(app_data));
            let _ = std::fs::write(&f, tok.trim());
        }
        None => {
            let _ = std::fs::remove_file(&f);
        }
    }
}

/// 取得済みモデル（diffusers 構造＝model_index.json がある）を列挙。
/// モデルフォルダ配下に `.incomplete`（HF の DL 途中マーカー）があるか（再帰）。
fn t2i_has_incomplete(dir: &Path) -> bool {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if t2i_has_incomplete(&p) {
                    return true;
                }
            } else if e
                .file_name()
                .to_str()
                .map(|n| n.ends_with(".incomplete"))
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// diffusers フォルダ d が**完全**か（daemon の _diffusers_complete と同判定）。
/// model_index.json＋重みコンポーネントの重み一式＋シャード・`.incomplete` 無し。
fn t2i_dir_complete(d: &Path) -> bool {
    let idx = d.join("model_index.json");
    if !idx.is_file() || t2i_has_incomplete(d) {
        return false;
    }
    let mi: serde_json::Value = match std::fs::read_to_string(&idx)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
    {
        Some(v) => v,
        None => return false,
    };
    let obj = match mi.as_object() {
        Some(o) => o,
        None => return false,
    };
    const NO_WEIGHT: &[&str] = &[
        "scheduler", "tokenizer", "tokenizer_2", "tokenizer_3",
        "feature_extractor", "safety_checker", "image_encoder",
    ];
    for (comp, val) in obj {
        if comp.starts_with('_') || NO_WEIGHT.contains(&comp.as_str()) {
            continue;
        }
        // 値は [library, class]。library が null（未使用）なら対象外。
        match val.as_array() {
            Some(a) if a.len() == 2 && !a[0].is_null() => {}
            _ => continue,
        }
        let sub = d.join(comp);
        if !sub.is_dir() {
            return false;
        }
        let mut has_weight = false;
        let mut shard_idxs: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&sub) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.ends_with(".index.json") {
                    shard_idxs.push(e.path());
                } else if n.ends_with(".safetensors") || n.ends_with(".bin") {
                    has_weight = true;
                }
            }
        }
        if !shard_idxs.is_empty() {
            for ix in shard_idxs {
                let v: serde_json::Value = match std::fs::read_to_string(&ix)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                {
                    Some(v) => v,
                    None => return false,
                };
                if let Some(wm) = v.get("weight_map").and_then(|w| w.as_object()) {
                    for sh in wm.values().filter_map(|s| s.as_str()) {
                        if !sub.join(sh).is_file() {
                            return false;
                        }
                    }
                }
            }
        } else if !has_weight {
            return false;
        }
    }
    true
}

/// フォルダ d が単一ファイルモデル（model_index.json 無し・チェックポイント1つ）か。
fn t2i_dir_has_single_checkpoint(d: &Path) -> bool {
    if d.join("model_index.json").is_file() {
        return false;
    }
    let mut count = 0;
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if n.ends_with(".safetensors") || n.ends_with(".ckpt") {
                    count += 1;
                }
            }
        }
    }
    count == 1
}

/// 取得済み（ロード可能）モデル名。完全な diffusers / 単一ファイル（直置き or フォルダ内）。
/// daemon の list_available と一致させる（部分DLは含めない＝再取得ボタンが出る）。
fn list_t2i_models(app_data: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(t2i_models_dir(app_data)) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if t2i_dir_complete(&p) || t2i_dir_has_single_checkpoint(&p) {
                    out.push(name);
                }
            } else if name.ends_with(".safetensors") || name.ends_with(".ckpt") {
                // 直置き単一ファイル: 名前＝拡張子抜き
                out.push(name.rsplitn(2, '.').nth(1).unwrap_or(&name).to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 部分ダウンロード（中断/失敗）のモデル名: フォルダに model_index.json はあるが不完全。
/// 「不完全（続きから再取得）」を UI に出すため。
fn t2i_partial_models(app_data: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(t2i_models_dir(app_data)) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("model_index.json").is_file() && !t2i_dir_complete(&p) {
                out.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

/// torch/diffusers が使える Python を探す。戻り値 (実行ファイルパス, 依存が揃っているか, CUDA可否)。
/// 「システム Python 優先」: PATH の python / py / python3 を順に試し、sys.executable を得る。
/// 依存チェックと同時に torch.cuda.is_available() も取得（サーバー未起動でも GPU 判定できる）。
fn detect_python() -> (Option<String>, bool, bool) {
    use std::process::Command;
    for cand in ["python", "py", "python3"] {
        let mut c = Command::new(cand);
        c.args(["-c", "import sys; sys.stdout.write(sys.executable)"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        let out = match c.output() {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            continue;
        }
        let mut d = Command::new(&path);
        d.args([
            "-c",
            "import torch,diffusers,transformers,safetensors,PIL,huggingface_hub; \
             import sys; sys.stdout.write('1' if torch.cuda.is_available() else '0')",
        ]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            d.creation_flags(0x0800_0000);
        }
        match d.output() {
            Ok(o) if o.status.success() => {
                let cuda = String::from_utf8_lossy(&o.stdout).trim() == "1";
                return (Some(path), true, cuda);
            }
            // Python はあるが依存が無い
            _ => return (Some(path), false, false),
        }
    }
    (None, false, false)
}

/// デーモンへ GET（tokio 文脈外スレッドで blocking 実行）。
fn t2i_http_get(path: &str, timeout_ms: u64) -> Result<serde_json::Value, String> {
    let url = format!(
        "http://127.0.0.1:{}/{}",
        flow::T2I_PORT,
        path.trim_start_matches('/')
    );
    std::thread::scope(|s| {
        s.spawn(|| {
            let c = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_millis(timeout_ms))
                .build()
                .map_err(|e| e.to_string())?;
            c.get(&url)
                .send()
                .map_err(|e| e.to_string())?
                .json::<serde_json::Value>()
                .map_err(|e| e.to_string())
        })
        .join()
        .map_err(|_| "HTTPスレッドが異常終了".to_string())?
    })
}

fn t2i_running() -> bool {
    t2i_http_get("health", 800).is_ok()
}

/// 稼働中デーモンが報告する版（取得できなければ空）。古い版の検出に使う。
fn t2i_daemon_version() -> String {
    t2i_http_get("health", 1000)
        .ok()
        .and_then(|v| v["version"].as_str().map(String::from))
        .unwrap_or_default()
}

/// 稼働中デーモンが「取得済み」と認識しているモデル名。取得できなければ空。
fn t2i_daemon_models() -> Vec<String> {
    t2i_http_get("models", 2000)
        .ok()
        .and_then(|v| {
            v.get("available").and_then(|a| a.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// T2I_PORT を Listen しているプロセスを止める（孤児デーモンや古いデーモンの掃除）。
/// 自分の子だけでなく、前セッションの取り残しも確実に終わらせるため port 基準で kill。
#[cfg(windows)]
fn t2i_kill_on_port() {
    use std::os::windows::process::CommandExt;
    let ps = format!(
        "Get-NetTCPConnection -LocalPort {} -State Listen -ErrorAction SilentlyContinue | \
         ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }}",
        flow::T2I_PORT
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .creation_flags(0x0800_0000)
        .status();
}
#[cfg(not(windows))]
fn t2i_kill_on_port() {}

/// デーモンを起動して health が立つまで待つ。
/// 既に稼働中でも、ディスク上の取得済みモデルをすべて認識していなければ「古いデーモン」と
/// 見なして再起動する（モデルをDLしたのにデーモンが見ない不一致の自己修復）。
/// 子プロセスは state.t2i_server に保持して停止できるようにする。
fn t2i_ensure_server(app_data: &Path, state: &AppState) -> Result<(), String> {
    // 稼働中/新規どちらでも、デーモンが読むトークンファイルを最新化しておく
    // （pull のたびに読む方式なので、これだけで再起動なしにトークンが効く）。
    sync_t2i_token_file(app_data);
    if t2i_running() {
        // 版が一致し、ディスクの取得済みモデルを全部認識していれば、そのまま使う。
        let disk = list_t2i_models(app_data);
        let seen = t2i_daemon_models();
        let version_ok = t2i_daemon_version() == T2I_SERVER_VERSION;
        if version_ok && disk.iter().all(|m| seen.contains(m)) {
            return Ok(());
        }
        // 古い版／取得済みを見ていない → 止めて最新スクリプトで起動し直す。
        {
            let mut g = state.t2i_server.lock().unwrap();
            if let Some(mut old) = g.take() {
                let _ = old.kill();
                let _ = old.wait();
            }
        }
        t2i_kill_on_port();
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
    let (py, deps_ok, _cuda) = detect_python();
    let py = py.ok_or(
        "Python が見つかりません。システムに Python（+torch/diffusers）を導入してください",
    )?;
    if !deps_ok {
        return Err(
            "Python に torch / diffusers が見つかりません。`pip install torch diffusers transformers accelerate safetensors` を導入してください"
                .into(),
        );
    }
    let root = t2i_root(app_data);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let script = t2i_server_script(app_data);
    std::fs::write(&script, T2I_SERVER_PY).map_err(|e| format!("サーバースクリプトの書き出しに失敗: {e}"))?;

    let log = std::fs::File::create(root.join("server.log")).ok();
    let mut c = std::process::Command::new(&py);
    c.arg(&script)
        .args(["--port", &flow::T2I_PORT.to_string()])
        .args(["--home", &root.to_string_lossy()]);
    // ゲート付き FLUX 公式モデル用の HF トークンを環境変数で渡す（設定されていれば）。
    if let Some(tok) = read_config(app_data).t2i_hf_token.filter(|t| !t.trim().is_empty()) {
        c.env("HF_TOKEN", tok);
    }
    if let Some(f) = log {
        c.stderr(std::process::Stdio::from(f));
    }
    c.stdout(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = c.spawn().map_err(|e| format!("画像生成サーバーの起動に失敗: {e}"))?;
    // 旧プロセスがいれば置換（先に殺す）。
    {
        let mut g = state.t2i_server.lock().unwrap();
        if let Some(mut old) = g.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        *g = Some(child);
    }
    // health を最大 ~30 秒待つ（torch の import に時間がかかる）。
    for _ in 0..60 {
        if t2i_running() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err("画像生成サーバーが起動しませんでした（t2i/server.log を確認してください）".into())
}

#[derive(Serialize, Clone)]
struct T2iKnownModel {
    name: String,
    label: String,
    /// HF 認証（HF_TOKEN）が必要なゲート付きモデルか
    gated: bool,
    /// ライセンス承諾ページ URL（gated のときに UI でコピー提示する）
    hf_url: String,
}

#[derive(Serialize, Clone)]
struct T2iStatus {
    /// 使える Python が見つかったか
    python_ok: bool,
    /// その Python の絶対パス
    python_path: String,
    /// torch/diffusers 等が揃っているか
    deps_ok: bool,
    /// デーモンが応答するか（=すぐ生成できる）
    server_running: bool,
    /// CUDA(GPU) が使えるか（デーモン稼働時のみ判明）
    gpu: bool,
    /// 取得済みモデル名
    models: Vec<String>,
    /// 既知モデル（DL候補）
    known: Vec<T2iKnownModel>,
    /// 現在選択中のモデル（既定 sd1.5）
    current_model: String,
    /// HF トークンが設定済みか（ゲート付きモデル取得の可否）
    hf_token_set: bool,
    /// 部分DL（中断/失敗）のモデル名。「不完全（続きから再取得）」表示用。
    partial: Vec<String>,
}

#[tauri::command(async)]
fn t2i_status(state: State<AppState>) -> T2iStatus {
    let app_data = state.app_data.clone();
    let (py, deps_ok, cuda) = detect_python();
    let running = t2i_running();
    // GPU: サーバー稼働中は health の cuda を、未起動なら Python 直問い合わせの結果を使う。
    let gpu = if running {
        t2i_http_get("health", 1500)
            .ok()
            .and_then(|v| v["cuda"].as_bool())
            .unwrap_or(cuda)
    } else {
        cuda
    };
    let current = read_config(&app_data)
        .t2i_model
        .unwrap_or_else(|| "sd1.5".to_string());
    T2iStatus {
        python_ok: py.is_some(),
        python_path: py.unwrap_or_default(),
        deps_ok,
        server_running: running,
        gpu,
        models: list_t2i_models(&app_data),
        known: T2I_KNOWN_MODELS
            .iter()
            .map(|(n, l, gated, url)| T2iKnownModel {
                name: n.to_string(),
                label: l.to_string(),
                gated: *gated,
                hf_url: url.to_string(),
            })
            .collect(),
        current_model: current,
        hf_token_set: read_config(&app_data)
            .t2i_hf_token
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false),
        partial: t2i_partial_models(&app_data),
    }
}

/// デーモンを起動する（モデルのロードはまだしない＝最初の生成で読む）。
#[tauri::command(async)]
fn t2i_start_server(state: State<AppState>) -> Result<(), String> {
    let app_data = state.app_data.clone();
    t2i_ensure_server(&app_data, state.inner())
}

/// デーモンを停止する（VRAM を解放）。
#[tauri::command(async)]
fn t2i_stop_server(state: State<AppState>) -> Result<(), String> {
    let mut g = state.t2i_server.lock().unwrap();
    if let Some(mut child) = g.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// モデルを取得（共有 app-data へ DL）。進捗は "t2i-model"（{model,status,percent}）で配信。
/// デーモンが diffusers/huggingface_hub でダウンロードするので、先にサーバーを起動する。
#[tauri::command(async)]
fn t2i_download_model(
    window: tauri::WebviewWindow,
    state: State<AppState>,
    model: String,
) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let model = if model.trim().is_empty() {
        "sd1.5".to_string()
    } else {
        model.trim().to_string()
    };
    t2i_ensure_server(&app_data, state.inner())?;
    let app = window.app_handle().clone();
    let url = format!("http://127.0.0.1:{}/pull", flow::T2I_PORT);
    std::thread::scope(|s| {
        s.spawn(|| {
            use std::io::BufRead;
            let client = reqwest::blocking::Client::builder()
                .build()
                .map_err(|e| format!("HTTPクライアント生成に失敗: {e}"))?;
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "name": model }))
                .send()
                .map_err(|e| format!("ダウンロードを開始できません: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("モデル取得に失敗 ({})", resp.status()));
            }
            for line in std::io::BufReader::new(resp).lines() {
                let line = line.map_err(|e| format!("進捗の読み取りに失敗: {e}"))?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(err) = v["error"].as_str() {
                        return Err(err.to_string());
                    }
                    if v["done"].as_bool().unwrap_or(false) {
                        break;
                    }
                    let _ = app.emit(
                        "t2i-model",
                        serde_json::json!({
                            "model": model,
                            "status": v["status"].as_str().unwrap_or(""),
                            "percent": v["percent"].as_u64().unwrap_or(0),
                        }),
                    );
                }
            }
            // 既定モデルとして記録。
            let mut cfg = read_config(&app_data);
            cfg.t2i_model = Some(model.clone());
            write_config(&app_data, &cfg);
            let _ = app.emit(
                "t2i-model",
                serde_json::json!({ "model": model, "status": "完了", "percent": 100 }),
            );
            Ok::<(), String>(())
        })
        .join()
        .map_err(|_| "ダウンロードスレッドが異常終了しました".to_string())?
    })
}

/// 既定モデルを切り替え、デーモンに warm ロードさせる（取得済みのものから）。
#[tauri::command(async)]
fn t2i_set_model(state: State<AppState>, model: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("モデル名が空です".into());
    }
    let mut cfg = read_config(&app_data);
    cfg.t2i_model = Some(model.clone());
    write_config(&app_data, &cfg);
    // サーバーが動いていれば warm ロード（重いので best-effort・失敗は致命ではない）。
    if t2i_running() {
        let url = format!("http://127.0.0.1:{}/load", flow::T2I_PORT);
        std::thread::scope(|s| {
            s.spawn(|| {
                let c = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .map_err(|e| e.to_string())?;
                let r = c
                    .post(&url)
                    .json(&serde_json::json!({ "name": model }))
                    .send()
                    .map_err(|e| e.to_string())?;
                if !r.status().is_success() {
                    return Err(format!("モデルのロードに失敗 ({})", r.status()));
                }
                Ok::<(), String>(())
            })
            .join()
            .map_err(|_| "ロードスレッドが異常終了".to_string())?
        })?;
    }
    Ok(())
}

/// HF トークンを保存（ゲート付き FLUX 公式モデル用）。空文字でクリア。
/// デーモンは pull のたびに <home>/hf_token を読むので、ファイルを書くだけで再起動不要で効く。
#[tauri::command(async)]
fn t2i_set_hf_token(state: State<AppState>, token: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let mut cfg = read_config(&app_data);
    let t = token.trim().to_string();
    cfg.t2i_hf_token = if t.is_empty() { None } else { Some(t) };
    write_config(&app_data, &cfg);
    sync_t2i_token_file(&app_data); // デーモンが読むファイルへ反映（再起動不要）
    Ok(())
}

/// 手動配置用: モデルの配置先フォルダ <models>/<model>/ を作って Explorer で開く。
/// ユーザーが HF から diffusers 構造（model_index.json＋各サブフォルダ）を手で置けるようにする
/// （自動DLが失敗するゲートモデルの保険）。デーモンは完全性チェックで自動検出する。
#[tauri::command(async)]
fn t2i_reveal_model_dir(state: State<AppState>, model: String) -> Result<String, String> {
    let app_data = state.app_data.clone();
    let m = model.trim();
    let dir = if m.is_empty() {
        t2i_models_dir(&app_data)
    } else {
        t2i_models_dir(&app_data).join(m)
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&p).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&p).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&p).spawn();
    }
    Ok(p)
}

/// モデルのローカルデータを削除（部分DL/壊れたモデルを破棄して取り直す用）。
/// フォルダ <models>/<model>/ と直置き <models>/<model>.safetensors|.ckpt の両方を消す。
#[tauri::command(async)]
fn t2i_delete_model(state: State<AppState>, model: String) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let m = model.trim();
    if m.is_empty() {
        return Err("モデル名が空です".into());
    }
    let base = t2i_models_dir(&app_data);
    let dir = base.join(m);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("削除に失敗: {e}"))?;
    }
    for ext in ["safetensors", "ckpt"] {
        let f = base.join(format!("{m}.{ext}"));
        if f.is_file() {
            let _ = std::fs::remove_file(&f);
        }
    }
    Ok(())
}

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
                flow_cancel: std::sync::atomic::AtomicBool::new(false),
                chat_cancel: std::sync::atomic::AtomicBool::new(false),
                rec_stdin: std::sync::Mutex::new(None),
                flow_running: std::sync::atomic::AtomicBool::new(false),
                t2i_server: std::sync::Mutex::new(None),
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
            open_origin_project,
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
            rename_real,
            duplicate_virtual,
            commit_object,
            commit_all,
            relink_object,
            relink_folder,
            uncommit_object,
            make_real,
            open_with_default,
            open_with_chooser,
            open_url_with_chooser,
            reveal_in_dir,
            view_temp,
            materialize_view,
            convert_office_to_pdf,
            create_note,
            list_notes,
            update_note,
            move_note,
            delete_note,
            set_note_style,
            create_note_linked,
            commit_note_to_file,
            read_text_file,
            read_view_text,
            write_text_file,
            paste_clipboard,
            copy_objects_to_clipboard,
            copy_image_to_clipboard,
            create_text_object,
            create_launcher,
            run_launcher,
            import_new_project_files,
            create_filter,
            update_filter,
            run_flow,
            break_flow,
            ai_chat_send,
            ai_chat_history,
            ai_context_labels,
            ai_chat_cancel,
            ai_chat_clear,
            ai_list_models,
            ai_ollama_status,
            ai_install_ollama,
            ai_start_ollama,
            ai_pull_model,
            whisper_status,
            whisper_install_ffmpeg,
            whisper_install,
            whisper_download_model,
            whisper_set_model,
            t2i_status,
            t2i_start_server,
            t2i_stop_server,
            t2i_download_model,
            t2i_set_model,
            t2i_set_hf_token,
            t2i_reveal_model_dir,
            t2i_delete_model,
            flow_send_stdin,
            list_audio_devices,
            check_filter_program,
            list_filter_programs,
            import_filter_program,
            remove_filter_program,
            restore_filter_program,
            list_filter_library,
            save_filter_library,
            delete_filter_library,
            list_launcher_library,
            save_launcher_library,
            delete_launcher_library,
            list_installed_apps,
            create_drop_point,
            drop_into_point,
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

#[cfg(test)]
mod text_decode_tests {
    use super::decode_text_bytes;

    #[test]
    fn plain_utf8_is_passthrough() {
        let (text, enc) = decode_text_bytes("あいうABC".as_bytes());
        assert_eq!(text, "あいうABC");
        assert_eq!(enc, "UTF-8");
    }

    #[test]
    fn shift_jis_is_detected_and_decoded() {
        // Shift_JIS でエンコードした日本語（read_to_string なら壊れていた入力）。
        let (bytes, _, had_err) = encoding_rs::SHIFT_JIS.encode("日本語のテキスト");
        assert!(!had_err);
        let (text, enc) = decode_text_bytes(&bytes);
        assert_eq!(text, "日本語のテキスト");
        assert_eq!(enc, "Shift_JIS");
    }

    #[test]
    fn utf16le_bom_is_honored() {
        // encoding_rs は UTF-16 への encode を持たないので UTF-16LE バイトを手で組む。
        let s = "BOM付きUTF-16";
        let mut with_bom = vec![0xFF, 0xFE];
        for u in s.encode_utf16() {
            with_bom.extend_from_slice(&u.to_le_bytes());
        }
        let (text, enc) = decode_text_bytes(&with_bom);
        assert_eq!(text, "BOM付きUTF-16");
        assert_eq!(enc, "UTF-16LE");
    }
}
