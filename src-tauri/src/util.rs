#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn file_mtime_ms(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_millis() as i64)
}

/// Prefer the original file extension; fall back to one derived from the MIME.
pub(crate) fn resolve_ext(stored: Option<&str>, mime: &str) -> String {
    match stored {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => ext_from_mime(mime).to_string(),
    }
}

pub(crate) fn ext_from_mime(mime: &str) -> &'static str {
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

pub(crate) fn ensure_ext(label: &str, ext: &str) -> String {
    let suffix = format!(".{}", ext.to_lowercase());
    if label.to_lowercase().ends_with(&suffix) {
        label.to_string()
    } else {
        format!("{}.{}", label, ext)
    }
}

pub(crate) fn unique_path(dir: &Path, filename: &str) -> PathBuf {
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
pub(crate) fn mime_for_ext(ext: &str) -> Option<&'static str> {
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

pub(crate) fn mime_from_path(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .and_then(|e| mime_for_ext(&e))
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// v1.3-⑧ repair: rows imported when the extension map was smaller carry a
/// wrong MIME (e.g. .docx as text/plain → opened as text). Re-derive from ext.
pub(crate) fn repair_mime_types(conn: &Connection) {
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
pub(crate) fn backfill_vbody_ext(conn: &Connection) {
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

pub(crate) fn uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", t ^ (t >> 17) ^ (t << 3))
}

