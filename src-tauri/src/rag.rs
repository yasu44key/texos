// V1.13 P1 RAG（builtin:"rag_index" / "rag_search"）— docs/V1.13_エージェントカード計画.md §5。
//
// 📚 索引: フォルダのテキスト・接続したカード → 切り分け → 埋め込み → `.texos/rag/<名前>.sqlite`。
// 🔎 検索: 問い → 埋め込み → 近いチャンクの上位 k 件を出典つき Markdown に。
//
// - 保存は SQLite（rusqlite は導入済み）にベクトルを BLOB で入れ、総当たりの内積（正規化済み＝
//   コサイン類似度）。数万チャンクまでは十分速く、依存が増えない。足りなくなったら sqlite-vec へ。
// - 作り直しは差分だけ（文書ごとに本文の sha256 を覚える）。モデルか切り分けの長さが変わったら全部。
// - 索引カードの出力（索引レポート.md）の1行目に `<!-- texos-rag-index: 名前 -->` を書く。
//   検索カードにつなぐと「この索引を使う」の意味になり、検索の前に差分の作り直しが走る
//   （問いとして読まない）。
// - HTTP（reqwest::blocking）は tokio 文脈を持たない素の OS スレッドで（flow.rs run_ai と同じ理由）。
use crate::*;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const MARKER_HEAD: &str = "<!-- texos-rag-index: ";
const MARKER_TAIL: &str = " -->";
const DEFAULT_INDEX: &str = "索引";
pub(crate) const DEFAULT_MODEL: &str = "bge-m3";
const DEFAULT_CHUNK: usize = 500;
const MIN_CHUNK: usize = 100;
const MAX_CHUNK: usize = 4000;
/// 前のチャンクの末尾をこの文字数まで次の頭に重ねる（文の途中で切れた文脈を拾う）。
const OVERLAP: usize = 80;
const DEFAULT_EXTS: &str = "txt,md,markdown,csv,log,pdf,docx";
/// ヒューズ: 1回の索引で読む文書の数・1ファイルの大きさ。
const MAX_FILES: usize = 2000;
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// 1回の HTTP で埋め込む数。
const BATCH: usize = 16;
const DEFAULT_TOP_K: usize = 6;
const MAX_TOP_K: usize = 30;
/// フォルダ走査で降りないディレクトリ（`.` で始まるものも降りない）。
const SKIP_DIRS: &[&str] = &["node_modules", "target", "__pycache__", "venv", ".venv"];

// --- 名前と置き場所 -------------------------------------------------------------

/// 索引の名前（ファイル名に使えない文字は `_`・空は「索引」）。
pub(crate) fn index_name(raw: Option<&str>) -> String {
    let s: String = raw
        .unwrap_or("")
        .trim()
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let s = s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if s.is_empty() {
        DEFAULT_INDEX.into()
    } else {
        s
    }
}

pub(crate) fn index_path(root: &Path, name: &str) -> PathBuf {
    texos_dir(root).join("rag").join(format!("{name}.sqlite"))
}

fn marker(name: &str) -> String {
    format!("{MARKER_HEAD}{name}{MARKER_TAIL}")
}

/// 索引レポートの1行目から索引の名前を読む（索引レポートでなければ None）。
pub(crate) fn marker_of(text: &str) -> Option<String> {
    let first = text.trim_start_matches('\u{feff}').lines().next()?.trim();
    let name = first.strip_prefix(MARKER_HEAD)?.strip_suffix(MARKER_TAIL)?;
    Some(index_name(Some(name)))
}

fn chunk_size(spec: &flow::FilterSpec) -> usize {
    spec.chunk_chars
        .map(|n| (n as usize).clamp(MIN_CHUNK, MAX_CHUNK))
        .unwrap_or(DEFAULT_CHUNK)
}

fn top_k(spec: &flow::FilterSpec) -> usize {
    spec.limit
        .map(|n| (n as usize).clamp(1, MAX_TOP_K))
        .unwrap_or(DEFAULT_TOP_K)
}

// --- 切り分け ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Chunk {
    /// このチャンクが始まる行（1始まり・重ねた分は数えない＝新しく入った最初の文の行）。
    pub line: usize,
    pub text: String,
}

/// 文の単位（行の中を 。！？!? で切る）。行末の文には改行を付ける。
struct Unit {
    line: usize,
    text: String,
    len: usize,
    /// 空行（段落の切れ目）。
    para: bool,
}

fn units_of(text: &str) -> Vec<Unit> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        if raw.trim().is_empty() {
            out.push(Unit { line, text: "\n".into(), len: 1, para: true });
            continue;
        }
        let mut cur = String::new();
        for c in raw.chars() {
            cur.push(c);
            if matches!(c, '。' | '！' | '？' | '!' | '?' | '．') {
                let len = cur.chars().count();
                out.push(Unit { line, text: std::mem::take(&mut cur), len, para: false });
            }
        }
        cur.push('\n');
        let len = cur.chars().count();
        out.push(Unit { line, text: cur, len, para: false });
    }
    out
}

/// テキストを size 文字前後のチャンクに切る。段落（空行）の切れ目を優先し、長すぎる文は
/// 文字数で割る。新しいチャンクの頭には前のチャンクの末尾（overlap 文字まで）を重ねる。
pub(crate) fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<Chunk> {
    // 長すぎる文を先に割っておく（1単位が size を超えないように）。
    let mut units: Vec<Unit> = Vec::new();
    for u in units_of(text) {
        if u.len <= size {
            units.push(u);
            continue;
        }
        let chars: Vec<char> = u.text.chars().collect();
        for piece in chars.chunks(size) {
            let t: String = piece.iter().collect();
            units.push(Unit { line: u.line, len: piece.len(), text: t, para: false });
        }
    }

    let mut chunks = Vec::new();
    let mut cur: Vec<usize> = Vec::new(); // units の添字
    let mut cur_len = 0usize;
    let mut fresh: Option<usize> = None; // このチャンクで新しく入った最初の単位

    let flush = |cur: &[usize], fresh: Option<usize>, chunks: &mut Vec<Chunk>| {
        let Some(f) = fresh else { return };
        let body: String = cur.iter().map(|&i| units[i].text.as_str()).collect();
        let body = body.trim().to_string();
        if !body.is_empty() {
            chunks.push(Chunk { line: units[f].line, text: body });
        }
    };

    for i in 0..units.len() {
        let u = &units[i];
        let over = cur_len + u.len > size;
        // 段落の切れ目で、もう半分以上たまっていればそこで切る。
        let para_break = u.para && cur_len >= size / 2;
        if (over || para_break) && fresh.is_some() {
            flush(&cur, fresh, &mut chunks);
            // 末尾を重ねる（ただしチャンク全体は重ねない）。
            let mut keep: Vec<usize> = Vec::new();
            let mut kept = 0usize;
            for &j in cur.iter().rev() {
                if kept + units[j].len > overlap || keep.len() + 1 >= cur.len() {
                    break;
                }
                kept += units[j].len;
                keep.push(j);
            }
            keep.reverse();
            cur = keep;
            cur_len = kept;
            fresh = None;
        }
        if u.para && fresh.is_none() && cur.is_empty() {
            continue; // チャンクの頭の空行は捨てる
        }
        if fresh.is_none() && !u.para {
            fresh = Some(i);
        }
        cur.push(i);
        cur_len += u.len;
    }
    flush(&cur, fresh, &mut chunks);
    chunks
}

// --- 埋め込み ------------------------------------------------------------------

/// 埋め込みの呼び先。provider="ollama"（既定・/api/embed）か OpenAI 互換（/embeddings）。
pub(crate) struct Embedder {
    provider: String,
    endpoint: String,
    pub model: String,
}

impl Embedder {
    pub(crate) fn from_spec(spec: &flow::FilterSpec, model: Option<&str>) -> Self {
        let model = model
            .or(spec.model.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_MODEL)
            .to_string();
        Embedder {
            provider: spec.provider.clone().unwrap_or_else(|| "ollama".into()),
            endpoint: flow::effective_ai_endpoint(spec),
            model,
        }
    }

    pub(crate) fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        std::thread::scope(|s| {
            s.spawn(|| self.embed_blocking(texts))
                .join()
                .map_err(|_| "埋め込みのスレッドが異常終了しました".to_string())?
        })
    }

    fn embed_blocking(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))?;
        let compat = flow::is_openai_compat(&self.provider);
        let url = if compat {
            format!("{}/embeddings", self.endpoint.trim_end_matches('/'))
        } else {
            format!("{}/api/embed", ollama_base(Some(&self.endpoint)))
        };
        let body = serde_json::json!({ "model": self.model, "input": texts });
        let resp = client.post(&url).json(&body).send().map_err(|e| {
            if e.is_connect() {
                format!("埋め込みサーバーにつながりません（{url}）— Ollama 等が起動しているか確かめてください")
            } else {
                format!("埋め込みの要求に失敗しました: {e}")
            }
        })?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("埋め込みの応答を読めません（HTTP {status}）: {e}"))?;
        if !status.is_success() {
            let msg = v
                .get("error")
                .map(|e| e.get("message").unwrap_or(e))
                .and_then(|e| e.as_str())
                .unwrap_or("")
                .to_string();
            if status.as_u16() == 404 || msg.contains("not found") {
                return Err(format!(
                    "埋め込みモデル「{m}」が見つかりません。Ollama なら `ollama pull {m}` で入れてください（{msg}）",
                    m = self.model
                ));
            }
            return Err(format!("埋め込みに失敗しました（HTTP {status}）: {msg}"));
        }
        let vecs: Vec<Vec<f32>> = if compat {
            v.get("data")
                .and_then(|d| d.as_array())
                .ok_or("埋め込みの応答に data がありません")?
                .iter()
                .map(|d| to_vec(d.get("embedding")))
                .collect::<Result<_, _>>()?
        } else {
            v.get("embeddings")
                .and_then(|d| d.as_array())
                .ok_or("埋め込みの応答に embeddings がありません")?
                .iter()
                .map(|d| to_vec(Some(d)))
                .collect::<Result<_, _>>()?
        };
        if vecs.len() != texts.len() {
            return Err(format!("埋め込みの数が合いません（{} 件送って {} 件）", texts.len(), vecs.len()));
        }
        Ok(vecs.into_iter().map(normalize).collect())
    }
}

fn to_vec(v: Option<&serde_json::Value>) -> Result<Vec<f32>, String> {
    v.and_then(|a| a.as_array())
        .ok_or_else(|| "埋め込みの形が違います".to_string())?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| "埋め込みに数値でない値があります".to_string()))
        .collect()
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn dot_blob(q: &[f32], blob: &[u8]) -> f32 {
    blob.chunks_exact(4)
        .zip(q)
        .map(|(b, x)| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * x)
        .sum()
}

// --- 索引 DB ---------------------------------------------------------------------

fn open_db(path: &Path) -> Result<Connection, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("索引の置き場所を作れません: {e}"))?;
    }
    let c = Connection::open(path).map_err(|e| format!("索引を開けません: {e}"))?;
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS docs(source TEXT PRIMARY KEY, hash TEXT NOT NULL,
             chunks INTEGER NOT NULL, updated_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS chunks(id INTEGER PRIMARY KEY, source TEXT NOT NULL,
             ord INTEGER NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, vec BLOB NOT NULL);
         CREATE INDEX IF NOT EXISTS chunks_source ON chunks(source);",
    )
    .map_err(|e| format!("索引の初期化に失敗: {e}"))?;
    Ok(c)
}

fn meta_get(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

fn meta_set(c: &Connection, key: &str, value: &str) -> Result<(), String> {
    c.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

// --- 本文の取り出し（PDF・Word）--------------------------------------------------

/// 中身が文字でない文書（本文を取り出してから索引する）。
fn is_binary_doc(ext: &str) -> bool {
    matches!(ext, "pdf" | "docx")
}

/// ファイルの中身を索引用のテキストにする。PDF・Word は本文を取り出し、ほかは文字コードを見分けて読む。
pub(crate) fn doc_text(ext: &str, bytes: &[u8]) -> Result<String, String> {
    match ext {
        "docx" => docx_text(bytes),
        "pdf" => pdf_text(bytes),
        _ => Ok(decode_text_bytes(bytes).0),
    }
}

/// Word（docx）: zip の中の word/document.xml から段落ごとに文字を拾う。
/// 段落（</w:p>）と改行（<w:br/>）は改行、タブ（<w:tab/>）はタブにする。
pub(crate) fn docx_text(bytes: &[u8]) -> Result<String, String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("Word ファイルとして開けません: {e}"))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .map_err(|_| "Word ファイルに本文（word/document.xml）がありません".to_string())?
        .read_to_string(&mut xml)
        .map_err(|e| format!("Word ファイルの本文を読めません: {e}"))?;
    let re = regex::Regex::new(r"(?s)<w:t(?:\s[^>]*)?>(.*?)</w:t>|</w:p>|<w:br\s*/>|<w:tab\s*/>").unwrap();
    let mut out = String::new();
    for c in re.captures_iter(&xml) {
        match c.get(1) {
            Some(t) => out.push_str(&xml_unescape(t.as_str())),
            None => {
                let m = c.get(0).unwrap().as_str();
                out.push(if m.starts_with("<w:tab") { '\t' } else { '\n' });
            }
        }
    }
    Ok(out)
}

fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let re = regex::Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|lt|gt|amp|quot|apos);").unwrap();
    re.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        match e {
            "lt" => "<".into(),
            "gt" => ">".into(),
            "amp" => "&".into(),
            "quot" => "\"".into(),
            "apos" => "'".into(),
            _ => {
                let n = if let Some(h) = e.strip_prefix("#x") {
                    u32::from_str_radix(h, 16).ok()
                } else {
                    e[1..].parse().ok()
                };
                n.and_then(char::from_u32).map(String::from).unwrap_or_default()
            }
        }
    })
    .into_owned()
}

/// PDF: pdf-extract（MIT）で本文を取り出す。壊れた PDF で panic することがあるので受け止める。
/// 文字が無い（スキャン画像だけの PDF）は理由つきのエラー（OCR が要る）。
pub(crate) fn pdf_text(bytes: &[u8]) -> Result<String, String> {
    let r = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes));
    let text = match r {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return Err(format!("PDF の本文を取り出せません: {e}")),
        Err(_) => return Err("PDF の本文を取り出せません（読めない形式でした）".into()),
    };
    if text.chars().filter(|c| !c.is_whitespace()).count() < 10 {
        return Err("PDF に文字がありません（スキャン画像だけの PDF は OCR が要ります）".into());
    }
    Ok(text)
}

// --- 文書を集める --------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Doc {
    /// 出典の名前（プロジェクト内は相対パス・外は絶対パス・カードは「名前.拡張子」）。
    pub source: String,
    pub text: String,
}

fn ext_list(spec: &flow::FilterSpec) -> Vec<String> {
    spec.exts
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_EXTS)
        .split([',', ' ', '、'])
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

/// フォルダ（空欄＝プロジェクト直下）を下の階層まで走査して、対象の拡張子のテキストを読む。
/// 返り値の2つ目は注意書き（ヒューズ・大きすぎて読まなかったファイル）。
pub(crate) fn collect_folder(root: &Path, spec: &flow::FilterSpec) -> Result<(Vec<Doc>, Vec<String>), String> {
    let dir = resolve_project_path(root, spec.path.as_deref());
    if !dir.is_dir() {
        return Err(format!("フォルダがありません: {}", dir.display()));
    }
    let exts = ext_list(spec);
    let texos = texos_dir(root);
    let mut files: Vec<PathBuf> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let q = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.starts_with('~') {
                continue;
            }
            if q.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) && !path_inside(&q, &texos) {
                    stack.push(q);
                }
                continue;
            }
            let ext = q.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            if exts.iter().any(|x| *x == ext) {
                files.push(q);
            }
        }
    }
    files.sort();
    if files.len() > MAX_FILES {
        notes.push(format!("文書が多すぎるので先頭 {MAX_FILES} 件だけ読みました（全 {} 件）", files.len()));
        files.truncate(MAX_FILES);
    }
    let mut docs = Vec::new();
    for q in files {
        let size = std::fs::metadata(&q).map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            notes.push(format!("大きすぎるので読みませんでした: {}（{} MB）", q.display(), size / 1024 / 1024));
            continue;
        }
        let Ok(bytes) = std::fs::read(&q) else { continue };
        let source = match q.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => q.to_string_lossy().to_string(),
        };
        let ext = q.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        match doc_text(&ext, &bytes) {
            Ok(text) => docs.push(Doc { source, text }),
            Err(e) => notes.push(format!("{source}: {e}")),
        }
    }
    Ok((docs, notes))
}

// --- 索引を作る ------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct IndexStats {
    pub added: usize,
    pub updated: usize,
    pub same: usize,
    pub removed: usize,
    pub embedded_chunks: usize,
    pub total_docs: usize,
    pub total_chunks: usize,
    pub dim: usize,
    pub rebuilt: bool,
}

/// 索引を差分で作り直す。embed は差し替え可能（テストでは決まった関数を渡す）。
/// 文書ごとに確定（commit）するので、途中で中断しても済んだ分は残る。
pub(crate) fn update_index(
    db_path: &Path,
    model: &str,
    size: usize,
    docs: &[Doc],
    cancel: &AtomicBool,
    embed: &dyn Fn(&[String]) -> Result<Vec<Vec<f32>>, String>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<IndexStats, String> {
    let mut c = open_db(db_path)?;
    let mut st = IndexStats::default();
    // モデルか切り分けの長さが変わったら全部作り直す（ベクトルを混ぜない）。
    let same_settings = meta_get(&c, "model").as_deref() == Some(model)
        && meta_get(&c, "chunk_chars").as_deref() == Some(size.to_string().as_str());
    if !same_settings {
        let had: i64 = c.query_row("SELECT COUNT(*) FROM docs", [], |r| r.get(0)).unwrap_or(0);
        st.rebuilt = had > 0;
        c.execute_batch("DELETE FROM chunks; DELETE FROM docs;").map_err(|e| e.to_string())?;
        meta_set(&c, "model", model)?;
        meta_set(&c, "chunk_chars", &size.to_string())?;
        let _ = c.execute("DELETE FROM meta WHERE key = 'dim'", []);
    }
    let mut dim: Option<usize> = meta_get(&c, "dim").and_then(|s| s.parse().ok());

    // 消えた文書を落とす。
    let current: std::collections::HashSet<&str> = docs.iter().map(|d| d.source.as_str()).collect();
    let known: Vec<String> = {
        let mut stmt = c.prepare("SELECT source FROM docs").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };
    for s in known.iter().filter(|s| !current.contains(s.as_str())) {
        c.execute("DELETE FROM chunks WHERE source = ?1", params![s]).map_err(|e| e.to_string())?;
        c.execute("DELETE FROM docs WHERE source = ?1", params![s]).map_err(|e| e.to_string())?;
        st.removed += 1;
    }

    for (n, d) in docs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("中断されました（ここまでの分は索引に残っています）".into());
        }
        progress(n, docs.len());
        let hash = sha256_hex(&d.text);
        let old: Option<String> = c
            .query_row("SELECT hash FROM docs WHERE source = ?1", params![d.source], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        if old.as_deref() == Some(hash.as_str()) {
            st.same += 1;
            continue;
        }
        let chunks = chunk_text(&d.text, size, OVERLAP);
        let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
        for batch in chunks.chunks(BATCH) {
            if cancel.load(Ordering::Relaxed) {
                return Err("中断されました（ここまでの分は索引に残っています）".into());
            }
            let texts: Vec<String> = batch.iter().map(|ch| ch.text.clone()).collect();
            let vs = embed(&texts)?;
            for v in &vs {
                match dim {
                    None => dim = Some(v.len()),
                    Some(k) if k != v.len() => {
                        return Err(format!("埋め込みの次元が揃いません（{k} と {}）", v.len()));
                    }
                    _ => {}
                }
            }
            vecs.extend(vs);
        }
        let tx = c.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM chunks WHERE source = ?1", params![d.source]).map_err(|e| e.to_string())?;
        for (i, (ch, v)) in chunks.iter().zip(&vecs).enumerate() {
            tx.execute(
                "INSERT INTO chunks(source, ord, line, text, vec) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![d.source, i as i64, ch.line as i64, ch.text, to_blob(v)],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "INSERT INTO docs(source, hash, chunks, updated_at) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(source) DO UPDATE SET hash = excluded.hash, chunks = excluded.chunks,
                 updated_at = excluded.updated_at",
            params![d.source, hash, chunks.len() as i64, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
        if let Some(k) = dim {
            tx.execute(
                "INSERT INTO meta(key, value) VALUES('dim', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k.to_string()],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        st.embedded_chunks += chunks.len();
        if old.is_some() {
            st.updated += 1;
        } else {
            st.added += 1;
        }
    }
    progress(docs.len(), docs.len());
    st.total_docs = c.query_row("SELECT COUNT(*) FROM docs", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize;
    st.total_chunks = c.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize;
    st.dim = dim.unwrap_or(0);
    Ok(st)
}

fn index_report(name: &str, model: &str, root: &Path, db_path: &Path, st: &IndexStats, notes: &[String]) -> String {
    let rel = db_path.strip_prefix(root).unwrap_or(db_path).to_string_lossy().replace('\\', "/");
    let mut md = format!("{}\n# 索引「{name}」\n\n", marker(name));
    md.push_str(&format!(
        "文書 {} 件・チャンク {} を索引しました（新規 {}・更新 {}・そのまま {}・削除 {}）。\n\n",
        st.total_docs, st.total_chunks, st.added, st.updated, st.same, st.removed
    ));
    md.push_str(&format!("- 埋め込みモデル: {model}（{} 次元）\n", st.dim));
    md.push_str(&format!("- 今回埋め込んだチャンク: {}\n", st.embedded_chunks));
    md.push_str(&format!("- 保存先: {rel}\n"));
    if st.rebuilt {
        md.push_str("- モデルか切り分けの長さが変わったので、全部作り直しました\n");
    }
    if !notes.is_empty() {
        md.push_str("\n## 注意\n\n");
        for n in notes {
            md.push_str(&format!("- {n}\n"));
        }
    }
    md.push_str("\nこのカードを 🔎 検索カードにつなぐと、この索引から探します。\n");
    md
}

// --- 検索 ------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Hit {
    pub source: String,
    pub line: usize,
    pub text: String,
    pub score: f32,
}

/// 索引から問いに近いチャンクを上位 k 件（総当たりの内積）。
pub(crate) fn search_index(
    db_path: &Path,
    query_vec: &[f32],
    k: usize,
) -> Result<Vec<Hit>, String> {
    let c = open_db(db_path)?;
    let mut stmt = c.prepare("SELECT id, vec FROM chunks").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut scored: Vec<(f32, i64)> = Vec::new();
    for row in rows.flatten() {
        let (id, blob) = row;
        if blob.len() != query_vec.len() * 4 {
            continue; // 次元違い（ありえないが壊れた行は飛ばす）
        }
        scored.push((dot_blob(query_vec, &blob), id));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    let mut hits = Vec::new();
    for (score, id) in scored {
        let (source, line, text): (String, i64, String) = c
            .query_row("SELECT source, line, text FROM chunks WHERE id = ?1", params![id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(|e| e.to_string())?;
        hits.push(Hit { source, line: line as usize, text, score });
    }
    Ok(hits)
}

fn search_report(name: &str, model: &str, total: usize, query: &str, hits: &[Hit]) -> String {
    let q1: String = query.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(40).collect();
    let mut md = format!("# 「{q1}」に近い箇所\n\n");
    md.push_str(&format!(
        "索引「{name}」の {total} チャンクから上位 {} 件（{model}）。\n\n",
        hits.len()
    ));
    for (i, h) in hits.iter().enumerate() {
        md.push_str(&format!(
            "## {}. {}（{} 行目〜・類似度 {:.2}）\n\n{}\n\n",
            i + 1,
            h.source,
            h.line,
            h.score,
            h.text
        ));
    }
    md
}

// --- カードとしての実行（flow_run.rs eval_filter から）-----------------------------------

fn payload(text: String, stem: String) -> FlowPayload {
    FlowPayload { bytes: text.into_bytes(), ext: "md".into(), mime: "text/markdown".into(), stem, meta: None }
}

fn out_stem(spec: &flow::FilterSpec, default: &str) -> String {
    spec.prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| flow::split_output_name(s).0.to_string())
        .unwrap_or_else(|| default.to_string())
}

fn is_text(pl: &FlowPayload) -> bool {
    pl.mime.starts_with("text/") || matches!(pl.ext.as_str(), "md" | "txt" | "csv" | "json")
}

/// 📚 索引カード。接続したカード（テキスト）とフォルダ（パラメータ欄・空欄で入力も無ければ
/// プロジェクト直下）を読んで、差分で索引を作り直す。
pub(crate) fn run_rag_index(
    app: &tauri::AppHandle,
    p: &Project,
    fid: &str,
    spec: &flow::FilterSpec,
    collected: &[FlowPayload],
    job: &Job,
) -> EvalOut {
    let name = index_name(spec.index.as_deref());
    let model = Embedder::from_spec(spec, None);
    let mut notes: Vec<String> = Vec::new();
    // 入力のカード（索引レポートは読まない＝索引を索引しない）。PDF・Word は本文を取り出す。
    let mut docs: Vec<Doc> = Vec::new();
    let mut skipped_bin = 0usize;
    for pl in collected {
        let source = format!("{}.{}", pl.stem, pl.ext);
        let ext = pl.ext.to_ascii_lowercase();
        if !is_text(pl) && !is_binary_doc(&ext) {
            skipped_bin += 1;
            continue;
        }
        match doc_text(&ext, &pl.bytes) {
            Ok(text) if marker_of(&text).is_none() => docs.push(Doc { source, text }),
            Ok(_) => {}
            Err(e) => notes.push(format!("{source}: {e}")),
        }
    }
    if skipped_bin > 0 {
        notes.push(format!("文書でない入力 {skipped_bin} 件は読みませんでした"));
    }
    let want_folder = spec.path.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) || docs.is_empty();
    if want_folder {
        match collect_folder(&p.root, spec) {
            Ok((fd, n)) => {
                notes.extend(n);
                // 同じ出典名ならカードの方を残す（後から入れた方で上書きしない）。
                let have: std::collections::HashSet<String> = docs.iter().map(|d| d.source.clone()).collect();
                docs.extend(fd.into_iter().filter(|d| !have.contains(&d.source)));
            }
            Err(e) => return EvalOut::failed(vec![e], None, vec![]),
        }
    }
    if docs.is_empty() {
        return EvalOut::failed(
            vec![format!(
                "索引にする文書がありません（拡張子 {} のファイルを置くか、テキストのカードをつないでください）",
                ext_list(spec).join(",")
            )],
            None,
            vec![],
        );
    }
    let db_path = index_path(&p.root, &name);
    let size = chunk_size(spec);
    let embed = |texts: &[String]| model.embed(texts);
    let mut progress = |n: usize, total: usize| {
        let pct = if total == 0 { 100 } else { (n * 100 / total) as u32 };
        emit_job_progress(app, job, Some(&format!("索引「{name}」を作成中（{n}/{total}）")), Some(pct));
    };
    match update_index(&db_path, &model.model, size, &docs, &job.cancel, &embed, &mut progress) {
        Ok(st) => {
            job.trace.note(
                fid,
                "索引",
                &format!("{name}: 文書 {}・チャンク {}・今回 {}", st.total_docs, st.total_chunks, st.embedded_chunks),
            );
            let md = index_report(&name, &model.model, &p.root, &db_path, &st, &notes);
            EvalOut::data(vec![payload(md, out_stem(spec, "索引レポート"))], None)
        }
        Err(e) => EvalOut::failed(vec![e], None, vec![]),
    }
}

/// 🔎 検索カード。入力のうち索引レポートは「どの索引か」、それ以外は問い。
/// 問いが無ければパラメータ欄の問い。ヒット0件は None（呼び出し側が no-data にする）。
pub(crate) fn run_rag_search(
    p: &Project,
    spec: &flow::FilterSpec,
    collected: &[FlowPayload],
    report: &mut FlowReport,
) -> EvalOut {
    let mut name: Option<String> = None;
    let mut queries: Vec<String> = Vec::new();
    for pl in collected.iter().filter(|pl| is_text(pl)) {
        let t = decode_text_bytes(&pl.bytes).0;
        match marker_of(&t) {
            Some(n) => name = name.or(Some(n)),
            None if !t.trim().is_empty() => queries.push(t.trim().to_string()),
            None => {}
        }
    }
    let name = name.unwrap_or_else(|| index_name(spec.index.as_deref()));
    let query = if queries.is_empty() {
        spec.query.as_deref().map(str::trim).unwrap_or("").to_string()
    } else {
        queries.join("\n\n")
    };
    if query.is_empty() {
        return EvalOut::failed(
            vec!["問いがありません（パラメータ欄に書くか、問いのカードをつないでください）".into()],
            None,
            vec![],
        );
    }
    let db_path = index_path(&p.root, &name);
    if !db_path.is_file() {
        return EvalOut::failed(
            vec![format!("索引「{name}」がまだありません — 先に 📚 索引カードを実行してください")],
            None,
            vec![],
        );
    }
    let model = match open_db(&db_path).map(|c| meta_get(&c, "model")) {
        Ok(Some(m)) => m,
        Ok(None) => {
            return EvalOut::failed(vec![format!("索引「{name}」は空です — 索引カードを実行し直してください")], None, vec![])
        }
        Err(e) => return EvalOut::failed(vec![e], None, vec![]),
    };
    // 問いは索引と同じモデルで埋め込む（違うモデルのベクトルは比べられない）。
    let emb = Embedder::from_spec(spec, Some(&model));
    let qv = match emb.embed(&[query.clone()]) {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        Ok(_) => return EvalOut::failed(vec!["問いを埋め込めませんでした".into()], None, vec![]),
        Err(e) => return EvalOut::failed(vec![e], None, vec![]),
    };
    let total: usize = open_db(&db_path)
        .ok()
        .and_then(|c| c.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0)).ok())
        .unwrap_or(0) as usize;
    match search_index(&db_path, &qv, top_k(spec)) {
        Ok(hits) if hits.is_empty() => {
            report.skipped.push(format!("索引「{name}」にチャンクがありません — 下流を止めました"));
            EvalOut::nodata_out()
        }
        Ok(hits) => {
            let md = search_report(&name, &model, total, &query, &hits);
            EvalOut::data(vec![payload(md, out_stem(spec, "検索結果"))], None)
        }
        Err(e) => EvalOut::failed(vec![e], None, vec![]),
    }
}

/// preflight 用: 入力なしで成り立つか（索引＝フォルダを読む・検索＝パラメータ欄に問い）。
pub(crate) fn runs_without_input(spec: &flow::FilterSpec) -> bool {
    match spec.builtin.as_deref() {
        Some("rag_index") => true,
        Some("rag_search") => spec.query.as_deref().is_some_and(|q| !q.trim().is_empty()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_markers() {
        assert_eq!(index_name(None), "索引");
        assert_eq!(index_name(Some("  ")), "索引");
        assert_eq!(index_name(Some("社内/規程:2026")), "社内_規程_2026");
        let m = marker("議事録");
        assert_eq!(marker_of(&format!("{m}\n# 索引")), Some("議事録".into()));
        assert_eq!(marker_of("# ただの文書"), None);
    }

    #[test]
    fn chunks_respect_size_overlap_and_lines() {
        let text = "一文目です。二文目です。\n\n三行目の文。\n".repeat(30);
        let cs = chunk_text(&text, 100, 20);
        assert!(cs.len() > 3);
        for c in &cs {
            // 重ねた分（最大 20）を足しても size を大きくは超えない
            assert!(c.text.chars().count() <= 100 + 20, "{}", c.text.chars().count());
            assert!(!c.text.trim().is_empty());
        }
        // 行番号は増えていく
        assert!(cs.windows(2).all(|w| w[0].line <= w[1].line));
        assert_eq!(cs[0].line, 1);
        // 2つ目のチャンクの頭は1つ目の末尾と重なる
        let tail: String = cs[0].text.chars().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
        assert!(cs[1].text.contains(&tail));
    }

    #[test]
    fn long_sentence_is_split() {
        let text = "あ".repeat(1050);
        let cs = chunk_text(&text, 500, 0);
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].text.chars().count(), 500);
    }

    #[test]
    fn short_text_is_one_chunk() {
        let cs = chunk_text("短い文書。", 500, 80);
        assert_eq!(cs, vec![Chunk { line: 1, text: "短い文書。".into() }]);
        assert!(chunk_text("\n\n  \n", 500, 80).is_empty());
    }

    // 文字の出現でベクトルを作る決まった埋め込み（テスト用）。
    fn fake_embed(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; 64];
                for c in t.chars() {
                    v[(c as usize) % 64] += 1.0;
                }
                normalize(v)
            })
            .collect())
    }

    #[test]
    fn index_is_incremental_and_searchable() {
        let dir = std::env::temp_dir().join(format!("texos_rag_{}", db::now_ms()));
        let db_path = dir.join("rag").join("t.sqlite");
        let cancel = AtomicBool::new(false);
        let mut docs = vec![
            Doc { source: "猫.md".into(), text: "猫は丸くなって眠る。猫は魚が好き。".into() },
            Doc { source: "車.md".into(), text: "エンジンとタイヤとハンドル。ガソリンで走る。".into() },
        ];
        let st = update_index(&db_path, "fake", 200, &docs, &cancel, &fake_embed, &mut |_, _| {}).unwrap();
        assert_eq!((st.added, st.updated, st.same, st.removed), (2, 0, 0, 0));
        assert_eq!(st.total_docs, 2);

        // 変えていない文書は埋め込み直さない
        let st = update_index(&db_path, "fake", 200, &docs, &cancel, &fake_embed, &mut |_, _| {}).unwrap();
        assert_eq!((st.added, st.updated, st.same, st.embedded_chunks), (0, 0, 2, 0));

        // 1つ変え、1つ消す
        docs[0].text.push_str("猫はよく鳴く。");
        docs.pop();
        let st = update_index(&db_path, "fake", 200, &docs, &cancel, &fake_embed, &mut |_, _| {}).unwrap();
        assert_eq!((st.updated, st.removed, st.total_docs), (1, 1, 1));

        // 設定が変わると全部作り直し
        let st = update_index(&db_path, "fake2", 200, &docs, &cancel, &fake_embed, &mut |_, _| {}).unwrap();
        assert!(st.rebuilt);
        assert_eq!(st.added, 1);

        // 検索
        docs.push(Doc { source: "車.md".into(), text: "エンジンとタイヤとハンドル。ガソリンで走る。".into() });
        update_index(&db_path, "fake2", 200, &docs, &cancel, &fake_embed, &mut |_, _| {}).unwrap();
        let q = fake_embed(&["猫が好きな魚".into()]).unwrap().remove(0);
        let hits = search_index(&db_path, &q, 1).unwrap();
        assert_eq!(hits[0].source, "猫.md");
        let md = search_report("t", "fake2", 2, "猫が好きな魚", &hits);
        assert!(md.contains("## 1. 猫.md（1 行目〜"));

        // 中断
        cancel.store(true, Ordering::Relaxed);
        docs[0].text.push_str("追記。");
        assert!(update_index(&db_path, "fake2", 200, &docs, &cancel, &fake_embed, &mut |_, _| {}).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_scan_filters_and_skips() {
        let root = std::env::temp_dir().join(format!("texos_rag_dir_{}", db::now_ms()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".texos").join("rag")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("a.md"), "あ").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), "い").unwrap();
        std::fs::write(root.join("c.png"), [0u8, 1, 2]).unwrap();
        std::fs::write(root.join("~d.md"), "ロック").unwrap();
        std::fs::write(root.join(".texos").join("e.md"), "内部").unwrap();
        std::fs::write(root.join("node_modules").join("f.md"), "依存").unwrap();
        let spec = flow::parse_spec(r#"{"builtin":"rag_index"}"#).unwrap();
        let (docs, _) = collect_folder(&root, &spec).unwrap();
        let names: Vec<&str> = docs.iter().map(|d| d.source.as_str()).collect();
        assert_eq!(names, vec!["a.md", "sub/b.txt"]);
        let spec = flow::parse_spec(r#"{"builtin":"rag_index","exts":"txt"}"#).unwrap();
        let (docs, _) = collect_folder(&root, &spec).unwrap();
        assert_eq!(docs.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn make_docx(document_xml: &str) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default();
            z.start_file("word/document.xml", opt).unwrap();
            z.write_all(document_xml.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn docx_paragraphs_tabs_and_entities() {
        let xml = r#"<?xml version="1.0"?><w:document><w:body>
            <w:p><w:r><w:t>見出し</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Ctrl </w:t></w:r><w:r><w:tab/><w:t>&lt;保存&gt; &amp; 終了&#x3002;</w:t></w:r></w:p>
            <w:p><w:r><w:t>一行目</w:t><w:br/><w:t>二行目</w:t></w:r></w:p>
        </w:body></w:document>"#;
        let t = docx_text(&make_docx(xml)).unwrap();
        assert_eq!(t, "見出し\nCtrl \t<保存> & 終了。\n一行目\n二行目\n");
        assert!(docx_text(b"not a zip").is_err());
        assert_eq!(doc_text("docx", &make_docx(xml)).unwrap(), t);
    }

    #[test]
    fn broken_pdf_is_an_error_not_a_panic() {
        assert!(pdf_text(b"%PDF-1.4 broken").is_err());
        assert!(pdf_text(b"").is_err());
    }

    /// 実機: 手元の PDF / docx から本文を取り出せるか（RAG_LIVE_FILE）。
    /// `cargo test --lib rag_extract_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn rag_extract_live() {
        let Ok(f) = std::env::var("RAG_LIVE_FILE") else { return };
        let path = Path::new(&f);
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        let t = std::time::Instant::now();
        match doc_text(&ext, &std::fs::read(path).unwrap()) {
            Ok(s) => {
                let head: String = s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
                let chunks = chunk_text(&s, DEFAULT_CHUNK, OVERLAP).len();
                println!("{f}: {} 文字・{chunks} チャンク（{:?}）先頭: {head}", s.chars().count(), t.elapsed());
            }
            Err(e) => println!("{f}: エラー {e}"),
        }
    }

    /// 実機: Ollama に埋め込みモデルが入っていること（RAG_LIVE_MODEL・既定 bge-m3）。
    /// `cargo test --lib rag_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn rag_live() {
        let model = std::env::var("RAG_LIVE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        let spec = flow::parse_spec(&format!(r#"{{"builtin":"rag_index","model":"{model}"}}"#)).unwrap();
        let emb = Embedder::from_spec(&spec, None);
        let dir = std::env::temp_dir().join(format!("texos_rag_live_{}", db::now_ms()));
        let db_path = dir.join("live.sqlite");
        // RAG_LIVE_DIR（フォルダ）と RAG_LIVE_QUERY（問い）があれば、そのフォルダを索引して問う。
        if let (Ok(src), Ok(q)) = (std::env::var("RAG_LIVE_DIR"), std::env::var("RAG_LIVE_QUERY")) {
            let (docs, _) = collect_folder(Path::new(&src), &spec).unwrap();
            let t = std::time::Instant::now();
            let st = update_index(&db_path, &emb.model, 500, &docs, &AtomicBool::new(false), &|x| emb.embed(x), &mut |_, _| {}).unwrap();
            println!("索引: 文書 {}・チャンク {}（{:?}）", st.total_docs, st.total_chunks, t.elapsed());
            let qv = emb.embed(&[q.clone()]).unwrap().remove(0);
            for h in search_index(&db_path, &qv, 3).unwrap() {
                let head: String = h.text.chars().take(80).collect();
                println!("{}:{}({:.2}) {}", h.source, h.line, h.score, head.replace('\n', " "));
            }
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let docs = vec![
            Doc { source: "経費.md".into(), text: "出張の交通費は領収書を添えて月末までに申請する。宿泊費は一泊一万二千円まで。".into() },
            Doc { source: "休暇.md".into(), text: "有給休暇は入社半年後に十日付与される。申請は三日前までに上長へ。".into() },
            Doc { source: "献立.md".into(), text: "月曜はカレー、火曜は焼き魚、水曜は麻婆豆腐。".into() },
        ];
        let t = std::time::Instant::now();
        let st = update_index(&db_path, &emb.model, 500, &docs, &AtomicBool::new(false), &|x| emb.embed(x), &mut |_, _| {}).unwrap();
        println!("索引: {st:?}（{:?}）", t.elapsed());
        for q in ["ホテル代の上限はいくら？", "休みを取るにはいつまでに言えばいい？", "水曜の夕食"] {
            let qv = emb.embed(&[q.to_string()]).unwrap().remove(0);
            let hits = search_index(&db_path, &qv, 3).unwrap();
            println!("{q} → {}", hits.iter().map(|h| format!("{}({:.2})", h.source, h.score)).collect::<Vec<_>>().join(" "));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
