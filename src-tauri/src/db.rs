use rusqlite::{params, Connection, Result};
use std::path::Path;

pub fn open(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS real_bodies (
            id          TEXT PRIMARY KEY,   -- SHA-256 hex
            mime_type   TEXT NOT NULL,
            size        INTEGER NOT NULL,
            created_at  INTEGER NOT NULL,   -- unix ms
            codec_ref   TEXT,               -- SHA-256 of Wasm codec (nullable)
            tags        TEXT NOT NULL DEFAULT '[]', -- JSON array
            ext         TEXT                -- original file extension (no dot)
        );

        CREATE TABLE IF NOT EXISTS virtual_bodies (
            id          TEXT PRIMARY KEY,   -- random UUID
            real_id     TEXT NOT NULL REFERENCES real_bodies(id),
            label       TEXT NOT NULL DEFAULT '',
            pos_x       REAL NOT NULL DEFAULT 0,
            pos_y       REAL NOT NULL DEFAULT 0,
            desktop_id  TEXT,               -- NULL = root desktop
            created_at  INTEGER NOT NULL,
            -- cache metadata for external real bodies
            etag        TEXT,
            last_modified TEXT,
            -- once committed, the editable copy lives here (project folder)
            committed_path TEXT,
            -- mtime (unix ms) of the committed file at commit time, to detect edits
            committed_mtime INTEGER,
            -- if set, this card is a reference to another TexoS project (folder path)
            project_ref TEXT,
            -- in-canvas expansion state (preview/editor open)
            expanded INTEGER NOT NULL DEFAULT 0,
            -- if set, this card is a web URL link
            url_ref TEXT,
            -- optional manual comment shown on the card
            memo TEXT,
            -- stacking order on the canvas (higher = front)
            z_order INTEGER NOT NULL DEFAULT 0,
            -- if set, this card is a link to an OS folder (not a TexoS project)
            folder_ref TEXT,
            -- where the file was imported from (借用元; informational only)
            origin_path TEXT
        );

        CREATE TABLE IF NOT EXISTS desktops (
            id          TEXT PRIMARY KEY,   -- virtual_body id of this desktop
            name        TEXT NOT NULL,
            created_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS diff_stack (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            virtual_id  TEXT NOT NULL REFERENCES virtual_bodies(id),
            seq         INTEGER NOT NULL,
            op          TEXT NOT NULL,      -- JSON: {op, ...params}
            created_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS relations (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            from_id     TEXT NOT NULL,      -- virtual_body id
            to_id       TEXT NOT NULL,      -- virtual_body id
            kind        TEXT NOT NULL DEFAULT 'link',  -- link | sequence | group
            created_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS notes (
            id          TEXT PRIMARY KEY,
            text        TEXT NOT NULL DEFAULT '',
            pos_x       REAL NOT NULL DEFAULT 0,
            pos_y       REAL NOT NULL DEFAULT 0,
            created_at  INTEGER NOT NULL,
            width       REAL NOT NULL DEFAULT 220,
            font_size   REAL NOT NULL DEFAULT 13,
            bold        INTEGER NOT NULL DEFAULT 0,
            file_ref    TEXT          -- if set, note mirrors this text file
        );

        -- v1.3-②③: simple background shapes (角丸四角/楕円/太矢印).
        -- Shapes sit BEHIND cards and act as visual thought-groups: dragging a
        -- shape moves the cards on it; clicking selects them.
        CREATE TABLE IF NOT EXISTS shapes (
            id          TEXT PRIMARY KEY,
            kind        TEXT NOT NULL DEFAULT 'rect',  -- rect | ellipse | arrow
            pos_x       REAL NOT NULL DEFAULT 0,
            pos_y       REAL NOT NULL DEFAULT 0,
            width       REAL NOT NULL DEFAULT 320,
            height      REAL NOT NULL DEFAULT 220,
            rotation    REAL NOT NULL DEFAULT 0,       -- degrees (arrow direction)
            fill        TEXT NOT NULL DEFAULT '#4a90d9',
            opacity     REAL NOT NULL DEFAULT 0.2,     -- 元から半透明 (overlaps blend)
            z_order     INTEGER NOT NULL DEFAULT 0,    -- among shapes only
            label       TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_vb_desktop ON virtual_bodies(desktop_id);
        CREATE INDEX IF NOT EXISTS idx_diff_virtual ON diff_stack(virtual_id, seq);
        CREATE INDEX IF NOT EXISTS idx_rel_from ON relations(from_id);
    ",
    )?;

    // Best-effort migration for DBs created before committed_path existed.
    // Ignore the error if the column is already present.
    let _ = conn.execute(
        "ALTER TABLE virtual_bodies ADD COLUMN committed_path TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE virtual_bodies ADD COLUMN committed_mtime INTEGER",
        [],
    );
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN project_ref TEXT", []);
    let _ = conn.execute("ALTER TABLE real_bodies ADD COLUMN ext TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN width REAL NOT NULL DEFAULT 220",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN font_size REAL NOT NULL DEFAULT 13",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN bold INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN file_ref TEXT", []);
    // v1.4.16 付箋の充実: 付箋の色（NULL = テーマ既定の黄）と先頭行を見出しにする
    // ときの見出しフォントサイズ（NULL = 見出しなし＝全体を font_size で表示）。
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN color TEXT", []);
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN title_size REAL", []);
    let _ = conn.execute(
        "ALTER TABLE virtual_bodies ADD COLUMN expanded INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN url_ref TEXT", []);
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN memo TEXT", []);
    // v1.2: z-order, folder links, and the original import location (借用元)
    let _ = conn.execute(
        "ALTER TABLE virtual_bodies ADD COLUMN z_order INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN folder_ref TEXT", []);
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN origin_path TEXT", []);
    // v1.3.1: per-object width of the in-canvas text editor panel
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN panel_w REAL", []);
    // v1.3.2: soft delete — 削除は deleted_at を立てるだけ（復活の呪文 Ctrl+Z 用）。
    // 物理削除（行とtrashの掃除）はメンテナンスツール「データの正規化」の仕事。
    // カードだけでなくコメント・図形・矢印リンクも同じ方式で復活できる。
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN deleted_at INTEGER", []);
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN deleted_at INTEGER", []);
    let _ = conn.execute("ALTER TABLE shapes ADD COLUMN deleted_at INTEGER", []);
    let _ = conn.execute("ALTER TABLE relations ADD COLUMN deleted_at INTEGER", []);

    // v1.4: カード統一モデル §1 — 変種を kind で明示する（カード統一モデル検討結果.md）。
    // filter_ref は kind='filter' のフィルタ定義 JSON（オートフロー⑤）。
    // 既存行は ref 列から推定してバックフィル（repair_mime_types と同じ「開くたび修復」方式）。
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN kind TEXT", []);
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN filter_ref TEXT", []);
    // v1.3.6: 拡張子/MIME は「そのファイルの見え方」＝仮身の属性。空ファイル等は内容
    // ハッシュが同一で実身(real_bodies)を共有するため、実身の ext/mime だけだと別名・
    // 別形式のファイルが同じ型に潰れる。仮身側にも持たせ、表示/オープン/コミットで優先する。
    // NULL の行は従来どおり実身へフォールバック（COALESCE(v.*, r.*)）。
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN ext TEXT", []);
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN mime_type TEXT", []);
    // v1.3.10 窓モデル: 内部の仮身が「どの実身カードの窓か」を ID（不変）で指す。
    // window_of があるカードだけが内部レーンで解決される（外部=origin_path は従来どおり凍結）。
    let _ = conn.execute("ALTER TABLE virtual_bodies ADD COLUMN window_of TEXT", []);
    let _ = conn.execute(
        "UPDATE virtual_bodies SET kind='project' WHERE kind IS NULL AND project_ref IS NOT NULL",
        [],
    );
    let _ = conn.execute(
        "UPDATE virtual_bodies SET kind='url' WHERE kind IS NULL AND url_ref IS NOT NULL",
        [],
    );
    let _ = conn.execute(
        "UPDATE virtual_bodies SET kind='folder' WHERE kind IS NULL AND folder_ref IS NOT NULL",
        [],
    );
    let _ = conn.execute("UPDATE virtual_bodies SET kind='file' WHERE kind IS NULL", []);

    Ok(())
}

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn insert_real_body(
    conn: &Connection,
    id: &str,
    mime_type: &str,
    size: u64,
    ext: Option<&str>,
) -> Result<()> {
    // If the content was imported before, the row exists; still backfill ext
    // when it is currently missing (e.g. imported before the ext column existed).
    conn.execute(
        "INSERT INTO real_bodies (id, mime_type, size, created_at, ext)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET ext = COALESCE(real_bodies.ext, excluded.ext)",
        params![id, mime_type, size as i64, now_ms(), ext],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole migration batch (incl. v1.3 shapes) must run on a fresh DB,
    /// and twice in a row (IF NOT EXISTS / best-effort ALTER idempotency).
    #[test]
    fn migrate_fresh_and_reopen() {
        let dir = std::env::temp_dir().join(format!("texos_db_test_{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("texos.db");
        {
            let conn = open(&db_path).expect("first open");
            conn.execute(
                "INSERT INTO shapes (id, kind, pos_x, pos_y, width, height, created_at)
                 VALUES ('s1', 'rect', 1.0, 2.0, 320.0, 220.0, ?1)",
                params![now_ms()],
            )
            .expect("insert shape");
            let (fill, opacity): (String, f64) = conn
                .query_row(
                    "SELECT fill, opacity FROM shapes WHERE id = 's1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("select shape defaults");
            assert_eq!(fill, "#4a90d9");
            assert!((opacity - 0.2).abs() < 1e-9);
        }
        let _ = open(&db_path).expect("re-open (idempotent migrate)");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub fn insert_virtual_body(
    conn: &Connection,
    id: &str,
    real_id: &str,
    label: &str,
    x: f64,
    y: f64,
    desktop_id: Option<&str>,
    project_ref: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO virtual_bodies
            (id, real_id, label, pos_x, pos_y, desktop_id, created_at, project_ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, real_id, label, x, y, desktop_id, now_ms(), project_ref],
    )?;
    Ok(())
}
