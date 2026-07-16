// v1.7 P1: web_fetch ビルトイン（V1.7_情報収集経路計画.md §3 P1）。
// 「Web を実身化する」— URL の本文を Markdown 実身としてキャンバスに取り込む。
// WebView は埋め込まない。ページは消えるが実身は残る（水泡に帰さない）。
//
// §4 安全最小セットはこのモジュールの共通関数（fetch_url）に集約し、
// P3 web_feed / P4 web_capture もここを共用する（個別実装にしない）。

use crate::flow::FilterSpec;
use crate::AppState;
use rusqlite::OptionalExtension;
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// --- §4 安全最小セットの既定値（web_* 系で共用）---
/// タイムアウト（接続〜本文読み切りまで全体）。
pub const WEB_TIMEOUT_SECS: u64 = 30;
/// 取得サイズ上限（超過は明確なエラー。CAS 肥大対策 §7-3 の1件上限を兼ねる）。
pub const WEB_MAX_BYTES: usize = 20 * 1024 * 1024;
/// リダイレクト上限。
pub const WEB_MAX_REDIRECTS: usize = 5;
/// 同一ホストへの最小間隔（一括更新確認・複数 URL 時に直列＋待機）。
pub const WEB_HOST_INTERVAL: Duration = Duration::from_secs(2);
/// User-Agent（身元を偽らない）。
pub const WEB_USER_AGENT: &str = "TexoS/1.7 (+local thinking tool)";

/// fetch_url の結果。ヘッダのうちキャッシュモデル（v0.1 §2.3・P2 更新マーク）が
/// 使うものだけを持ち帰る。
pub struct WebResponse {
    pub bytes: Vec<u8>,
    /// リダイレクト解決後の URL。
    pub final_url: String,
    /// Content-Type ヘッダ生値（"text/html; charset=UTF-8" 等。無ければ空）。
    pub content_type: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// run_web_fetch の結果。NodeRun の 1 出力に対応する。
#[derive(Debug)]
pub struct WebFetchOut {
    pub bytes: Vec<u8>,
    pub ext: String,
    pub mime: String,
    /// カード名にする語幹（ページタイトル等）。None = 呼び出し側の従来命名。
    pub stem: Option<String>,
    /// virtual_bodies.web_meta に保存する JSON
    /// {source_url, final_url, fetched_at, etag, last_modified}。
    pub meta: String,
}

/// プライベート・特殊アドレスか（§4-5: localhost / 127.* / 192.168.* / 10.* 等への
/// fetch は既定で拒否 — t2i デーモン等のローカルポートへの誤爆・SSRF 系の芽を摘む）。
fn ip_is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // CGNAT 100.64.0.0/10（Tailscale 等の内部網）
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7（ULA）・fe80::/10（リンクローカル）
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4 射影は v4 側の判定に委譲
                || v6.to_ipv4_mapped().map(|m| ip_is_private(&IpAddr::V4(m))).unwrap_or(false)
        }
    }
}

/// 同一ホストへの最小間隔（§4-3）。プロセス内のグローバル台帳で「このホストに次に
/// 出てよい時刻」を予約してから待つ（並行フェッチでも直列化される）。
fn throttle_host(host: &str) {
    static LAST: Mutex<Option<std::collections::HashMap<String, Instant>>> = Mutex::new(None);
    let wait_until = {
        let mut guard = LAST.lock().unwrap();
        let map = guard.get_or_insert_with(std::collections::HashMap::new);
        let now = Instant::now();
        let slot = match map.get(host) {
            Some(prev) if *prev + WEB_HOST_INTERVAL > now => *prev + WEB_HOST_INTERVAL,
            _ => now,
        };
        map.insert(host.to_string(), slot);
        slot
    };
    let now = Instant::now();
    if wait_until > now {
        std::thread::sleep(wait_until - now);
    }
}

/// 条件付き GET の材料（v0.1 §2.3 キャッシュモデル・P2「更新を確認」）。
/// 取得実身の web_meta に記録した etag / last_modified をそのまま持ってくる。
pub struct FetchCond {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// §4 安全最小セットを同梱した GET（web_* 系の共通入口）。
/// reqwest::blocking は tokio 文脈で drop するとパニックするため、呼び出し側
/// （run_web_fetch）が素の OS スレッドに包む — run_ai と同じ流儀。
pub fn fetch_url(url_s: &str, allow_private: bool) -> Result<WebResponse, String> {
    fetch_url_cond(url_s, allow_private, None)?
        .ok_or_else(|| "サーバが 304 を返しました（条件なしの取得では想定外）".into())
}

/// fetch_url の条件付き版: cond の ETag / Last-Modified を If-None-Match /
/// If-Modified-Since として送り、304 Not Modified なら Ok(None)（＝変化なし・
/// 本文は転送されない）。P2「更新を確認」の一次判定に使う。
pub fn fetch_url_cond(
    url_s: &str,
    allow_private: bool,
    cond: Option<&FetchCond>,
) -> Result<Option<WebResponse>, String> {
    let url = reqwest::Url::parse(url_s).map_err(|e| format!("URL が不正です: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("http/https 以外の URL は取得できません: {}", url.scheme()));
    }
    if !allow_private {
        url_check_public(&url)?;
    }
    if let Some(host) = url.host_str() {
        throttle_host(host);
    }
    // リダイレクトも§4に従う: 上限5・各ホップでプライベート宛を遮断。
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > WEB_MAX_REDIRECTS {
            return attempt.error(format!("リダイレクトが{WEB_MAX_REDIRECTS}回を超えました"));
        }
        if !allow_private {
            if let Err(e) = url_check_public(attempt.url()) {
                return attempt.error(e);
            }
        }
        attempt.follow()
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(WEB_TIMEOUT_SECS))
        .redirect(policy)
        .user_agent(WEB_USER_AGENT)
        .build()
        .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))?;
    let mut req = client.get(url);
    if let Some(c) = cond {
        if let Some(etag) = c.etag.as_deref().filter(|s| !s.is_empty()) {
            req = req.header("if-none-match", etag);
        }
        if let Some(lm) = c.last_modified.as_deref().filter(|s| !s.is_empty()) {
            req = req.header("if-modified-since", lm);
        }
    }
    let resp = req.send().map_err(|e| format!("取得に失敗しました: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", resp.url()));
    }
    let final_url = resp.url().to_string();
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    };
    let content_type = header("content-type").unwrap_or_default();
    let etag = header("etag");
    let last_modified = header("last-modified");
    // サイズ上限: Content-Length を当てにせず、読みながら上限+1 で打ち切って判定。
    let mut bytes = Vec::new();
    resp.take(WEB_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("本文の読み込みに失敗しました: {e}"))?;
    if bytes.len() > WEB_MAX_BYTES {
        return Err(format!(
            "サイズが上限 {}MB を超えています: {final_url}",
            WEB_MAX_BYTES / (1024 * 1024)
        ));
    }
    Ok(Some(WebResponse { bytes, final_url, content_type, etag, last_modified }))
}

/// URL のホストがプライベート宛でないことを確認する（リテラル IP → 直接判定 /
/// ドメイン名 → DNS 解決して全 IP 判定）。DNS リバインディング等への完全防御ではなく
/// 「ローカル思考ツールの誤爆防止」が目的（§4-5）。
fn url_check_public(url: &reqwest::Url) -> Result<(), String> {
    let refuse = |what: &str| {
        Err(format!(
            "プライベートアドレス（{what}）への取得は既定で拒否します（誤爆・SSRF防止。\
             開発者向けにはフィルタ設定の JSON 編集で \"allow_private\": true）"
        ))
    };
    match url.host() {
        None => Err("URL にホストがありません".into()),
        Some(url::Host::Ipv4(ip)) => {
            if ip_is_private(&IpAddr::V4(ip)) { refuse(&ip.to_string()) } else { Ok(()) }
        }
        Some(url::Host::Ipv6(ip)) => {
            if ip_is_private(&IpAddr::V6(ip)) { refuse(&ip.to_string()) } else { Ok(()) }
        }
        Some(url::Host::Domain(d)) => {
            if d.eq_ignore_ascii_case("localhost") || d.to_ascii_lowercase().ends_with(".localhost") {
                return refuse(d);
            }
            let port = url.port_or_known_default().unwrap_or(443);
            let addrs = (d, port)
                .to_socket_addrs()
                .map_err(|e| format!("ホスト名を解決できません: {d} ({e})"))?;
            for a in addrs {
                if ip_is_private(&a.ip()) {
                    return refuse(&format!("{d} → {}", a.ip()));
                }
            }
            Ok(())
        }
    }
}

/// 入力テキストから URL を取り出す（1行=1URL・P1 は先頭の1本。複数行の展開は P3 の
/// ビルトイン複数出力とセットで解禁する）。
pub fn url_from_text(input: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(input);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or("入力テキストが空です（1行目に URL を書いてください）")?;
    if line.starts_with("http://") || line.starts_with("https://") {
        Ok(line.to_string())
    } else {
        Err(format!("入力の1行目が URL ではありません: {line}"))
    }
}

/// spec のパラメータ欄 URL（空白のみは無いのと同じ）。
pub fn spec_url(spec: &FilterSpec) -> Option<&str> {
    spec.url.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Content-Type ヘッダを (mime小文字, charset) に分解する。
fn split_content_type(raw: &str) -> (String, Option<String>) {
    let mut parts = raw.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let charset = parts
        .filter_map(|p| p.trim().strip_prefix("charset="))
        .map(|v| v.trim_matches('"').trim().to_string())
        .find(|v| !v.is_empty());
    (mime, charset)
}

/// HTML バイト列を UTF-8 文字列へ（§7-4: 実身データを作る瞬間に一度だけ変換する）。
/// 判定順: BOM → HTTP ヘッダ charset → meta charset（先頭2KBを ASCII 走査）→ UTF-8 厳密。
/// どれでも決められなければ None（= raw のまま実身化し、後段 text_convert で直す）。
fn decode_html(bytes: &[u8], header_charset: Option<&str>) -> Option<String> {
    if let Some((enc, _len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (s, _, malformed) = enc.decode(bytes);
        if !malformed {
            return Some(s.into_owned());
        }
    }
    let label = header_charset.map(str::to_string).or_else(|| sniff_meta_charset(bytes));
    if let Some(l) = label {
        if let Some(enc) = encoding_rs::Encoding::for_label(l.as_bytes()) {
            let (s, _, malformed) = enc.decode(bytes);
            if !malformed {
                return Some(s.into_owned());
            }
        }
    }
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

/// 先頭 2KB から <meta charset=...> / http-equiv Content-Type の charset を拾う。
fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let ascii: String = head.iter().map(|&b| if b < 128 { b as char } else { ' ' }).collect();
    let re = regex::Regex::new(r#"(?i)charset\s*=\s*["']?\s*([a-z0-9_\-]+)"#).ok()?;
    re.captures(&ascii).map(|c| c[1].to_string())
}

/// カード名（＝ファイル名の語幹）に使えるようタイトルを整形する。
/// （P2: flow_run の URLカード入力の stem 整形にも使う＝ラベルに URL が入っていても安全）
pub(crate) fn sanitize_stem(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(48)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() { None } else { Some(cleaned) }
}

/// URL からフォールバックの語幹を作る（パス末尾のファイル名語幹 → ホスト名）。
fn stem_from_url(url_s: &str) -> Option<String> {
    let url = reqwest::Url::parse(url_s).ok()?;
    let seg = url
        .path_segments()
        .and_then(|s| s.filter(|p| !p.is_empty()).next_back().map(str::to_string));
    let base = match seg {
        Some(f) => std::path::Path::new(&f)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or(f),
        None => url.host_str()?.to_string(),
    };
    sanitize_stem(&base)
}

/// MIME → 拡張子（util::mime_for_ext の逆引き。よく使う直リンク型だけ）。
fn ext_for_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "text/css" => "css",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "application/zip" => "zip",
        _ => return None,
    })
}

/// URL パス末尾の拡張子（1〜8 文字の英数字のみ。split_output_name と同じ寛容基準）。
fn ext_from_url(url_s: &str) -> Option<String> {
    let url = reqwest::Url::parse(url_s).ok()?;
    let last = url.path_segments()?.filter(|p| !p.is_empty()).next_back()?.to_string();
    let (_, ext) = crate::flow::split_output_name(&last);
    ext.map(|e| e.to_ascii_lowercase())
}

/// web_meta JSON（P2 更新マークの条件付き GET がここの etag/last_modified を読む）。
fn build_meta(source_url: &str, r: &WebResponse) -> String {
    serde_json::json!({
        "source_url": source_url,
        "final_url": r.final_url,
        "fetched_at": crate::db::now_ms(),
        "etag": r.etag,
        "last_modified": r.last_modified,
    })
    .to_string()
}

/// builtin:"web_fetch" 本体: URL → 本文 Markdown 実身（タイトル=語幹）。
/// - 入力両対応: `input`（テキスト実身の1行目の URL）が優先、無ければ spec.url。
/// - text/html 以外は **raw のまま実身化**（画像 / csv / json 直リンクが全部この1カードで済む）。
/// - 本文抽出（readability 相当）失敗時は全文 HTML→MD へフォールバック。
/// run_ai と同じ理由で reqwest::blocking を素の OS スレッドに包む。
pub fn run_web_fetch(spec: &FilterSpec, input: Option<&[u8]>) -> Result<WebFetchOut, String> {
    std::thread::scope(|s| {
        s.spawn(|| run_web_fetch_blocking(spec, input))
            .join()
            .map_err(|_| "Web取得スレッドが異常終了しました".to_string())?
    })
}

fn run_web_fetch_blocking(spec: &FilterSpec, input: Option<&[u8]>) -> Result<WebFetchOut, String> {
    let url = match input {
        Some(bytes) => url_from_text(bytes)?,
        None => spec_url(spec)
            .ok_or("URL がありません。パラメータ欄に URL を入力するか、URL を書いたテキストカードを接続してください")?
            .to_string(),
    };
    let resp = fetch_url(&url, spec.allow_private)?;
    Ok(response_to_out(&url, resp))
}

/// 取得レスポンス → 実身の形（HTML なら本文抽出→ Markdown・それ以外は raw）。
/// 変換は決定的なので、P2「更新を確認」は同じ変換を通した結果のハッシュを
/// 取得実身の CAS ハッシュと比べれば「内容が変わったか」を判定できる。
pub fn response_to_out(source_url: &str, resp: WebResponse) -> WebFetchOut {
    let (mime, header_charset) = split_content_type(&resp.content_type);
    let meta = build_meta(source_url, &resp);

    // --- text/html 以外: raw のまま実身化（§3 P1・変換もしない＝§7-4）---
    if !(mime == "text/html" || mime == "application/xhtml+xml") {
        // 拡張子: 汎用 MIME（text/plain 等）は URL 側の拡張子を優先（csv が text/plain で
        // 配られる等）。それ以外は MIME の正引きを優先し、無ければ URL 拡張子 → bin。
        let url_ext = ext_from_url(&resp.final_url).or_else(|| ext_from_url(source_url));
        let generic = mime.is_empty() || mime == "text/plain" || mime == "application/octet-stream";
        let ext = if generic {
            url_ext.clone().or_else(|| ext_for_mime(&mime).map(str::to_string))
        } else {
            ext_for_mime(&mime).map(str::to_string).or(url_ext.clone())
        }
        .unwrap_or_else(|| "bin".into());
        // MIME が空・汎用なら拡張子から補正（表示・接続型チェックが働くように）。
        let out_mime = if mime.is_empty() || (generic && url_ext.is_some()) {
            crate::util::mime_for_ext(&ext).unwrap_or("application/octet-stream").to_string()
        } else {
            mime
        };
        return WebFetchOut {
            bytes: resp.bytes,
            ext,
            mime: out_mime,
            stem: stem_from_url(&resp.final_url).or_else(|| stem_from_url(source_url)),
            meta,
        };
    }

    // --- HTML: UTF-8 化 → 本文抽出 → Markdown ---
    let html = match decode_html(&resp.bytes, header_charset.as_deref()) {
        Some(h) => h,
        None => {
            // 文字コード判定不能 → raw の .html のまま実身化（後段 text_convert で直す）。
            return WebFetchOut {
                bytes: resp.bytes,
                ext: "html".into(),
                mime: "text/html".into(),
                stem: stem_from_url(&resp.final_url).or_else(|| stem_from_url(source_url)),
                meta,
            };
        }
    };
    let (title, md) = html_to_markdown(&html, &resp.final_url);
    let stem = title
        .as_deref()
        .and_then(sanitize_stem)
        .or_else(|| stem_from_url(&resp.final_url))
        .or_else(|| stem_from_url(source_url));
    let body = match (&title, md.starts_with('#')) {
        // タイトルが本文側に無ければ見出しとして先頭に付ける（単体で読める md にする）。
        (Some(t), false) if !t.trim().is_empty() => format!("# {}\n\n{md}", t.trim()),
        _ => md,
    };
    WebFetchOut {
        bytes: body.into_bytes(),
        ext: "md".into(),
        mime: "text/markdown".into(),
        stem,
        meta,
    }
}

/// HTML → (タイトル, Markdown)。本文抽出（dom_smoothie）が成功すればその記事部分を、
/// 失敗すれば全文 HTML を htmd で変換する（script/style 等は落とす）。
fn html_to_markdown(html: &str, base_url: &str) -> (Option<String>, String) {
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "noscript", "iframe", "head"])
        .build();
    // 本文抽出（readability 相当）。パース失敗・空記事は全文へフォールバック。
    if let Ok(mut r) = dom_smoothie::Readability::new(html, Some(base_url), None) {
        if let Ok(article) = r.parse() {
            let title = Some(article.title.clone()).filter(|t| !t.trim().is_empty());
            if let Ok(md) = converter.convert(&article.content) {
                if !md.trim().is_empty() {
                    return (title, md);
                }
            }
        }
    }
    let title = sniff_title(html);
    let md = converter.convert(html).unwrap_or_else(|_| html.to_string());
    (title, md)
}

/// <title> をゆるく拾う（フォールバック経路用）。
fn sniff_title(html: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok()?;
    let t = re.captures(html)?.get(1)?.as_str();
    let t = t.replace(['\n', '\r', '\t'], " ");
    let t = t.trim();
    if t.is_empty() { None } else { Some(html_unescape(t)) }
}

/// タイトル用の最小限のエンティティ解決（&amp; 等。本文は htmd が解決する）。
fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

// ---------------------------------------------------------------------------
// v1.7 P2: URLカードの仮身昇格 — 更新マークと取得系譜（計画 §3 P2・§7-3 決着）。
// 「更新を確認」= 条件付きGET → 同じ変換 → ハッシュ比較 → **破棄**（マークを付けるだけで
// 実身は作らない）。実身が増えるのはユーザーが「再取得」を選んだ時だけ＝明示コミットと同型。
// ---------------------------------------------------------------------------

/// URLカードの web_meta（チェック状態）。取得実身の web_meta（source_url/etag/…）とは
/// 別用途で同じ列に載る: {checked_at, update: bool}。
fn write_check_mark(conn: &rusqlite::Connection, id: &str, updated: bool) {
    let meta =
        serde_json::json!({ "checked_at": crate::db::now_ms(), "update": updated }).to_string();
    let _ = conn.execute(
        "UPDATE virtual_bodies SET web_meta = ?1 WHERE id = ?2",
        rusqlite::params![meta, id],
    );
}

/// 最新の取得系譜（fetched エッジの先）の比較材料。
struct Baseline {
    /// 取得実身カードの id（変化なしのとき web_meta の ETag を最新化する先）。
    vid: String,
    /// 取得実身の CAS ハッシュ（= real_id。同じ変換を通した結果と比べる）。
    real_id: String,
    cond: FetchCond,
}

/// URLカードの URL と最新ベースラインを読む（プロジェクトロック中に呼ぶ）。
fn url_card_base(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(String, Option<Baseline>), String> {
    let url: Option<String> = conn
        .query_row(
            "SELECT url_ref FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let url = url.ok_or("URLカードではありません")?;
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT v.id, v.real_id, v.web_meta FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.to_id AND v.deleted_at IS NULL
             WHERE rel.from_id = ?1 AND rel.kind = 'fetched' AND rel.deleted_at IS NULL
             ORDER BY rel.created_at DESC LIMIT 1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let baseline = row.map(|(vid, real_id, meta)| {
        let v: serde_json::Value = meta
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or(serde_json::Value::Null);
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        Baseline {
            vid,
            real_id,
            cond: FetchCond { etag: s("etag"), last_modified: s("last_modified") },
        }
    });
    Ok((url, baseline))
}

/// 条件付きGET→同じ変換→ハッシュ比較。true = 内容が変わっている。取得物は破棄（§7-3）。
/// reqwest::blocking を使うので素の OS スレッドで（run_web_fetch と同じ流儀）。
fn fetch_and_compare(url: &str, base: &Baseline) -> Result<bool, String> {
    std::thread::scope(|s| {
        s.spawn(|| match fetch_url_cond(url, false, Some(&base.cond))? {
            None => Ok(false), // 304 Not Modified
            Some(resp) => {
                let out = response_to_out(url, resp);
                Ok(crate::cas::hash_bytes(&out.bytes) != base.real_id)
            }
        })
        .join()
        .map_err(|_| "Web取得スレッドが異常終了しました".to_string())?
    })
}

/// 「更新を確認」の結果（フロントの結果表示用）。
#[derive(serde::Serialize)]
pub struct WebCheckOut {
    pub updated: bool,
    pub message: String,
}

/// URLカード右クリック「更新を確認」。変化があればカードに更新マーク（強制反映なし）。
#[tauri::command(async)]
pub(crate) fn web_check_update(
    id: String,
    state: tauri::State<AppState>,
) -> Result<WebCheckOut, String> {
    // ロックは読み書きの瞬間だけ（ネットワーク待ち 30s で UI の他操作を塞がない）。
    let (url, baseline) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        url_card_base(&p.conn, &id)?
    };
    let base = baseline.ok_or(
        "このURLはまだ実身化されていません。「再取得（実身を作る）」か、web_fetch フィルタに接続して取得してください",
    )?;
    let updated = fetch_and_compare(&url, &base)?;
    {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        write_check_mark(&p.conn, &id, updated);
    }
    Ok(WebCheckOut {
        updated,
        message: if updated {
            "ページに更新があります。実身にするには「再取得」を選んでください（前の実身は残ります）".into()
        } else {
            "変化はありません（取得済みの実身が最新です）".into()
        },
    })
}

/// 「再取得」の結果（フロントの結果表示用）。
#[derive(serde::Serialize)]
pub struct WebRefetchOut {
    /// 新しく作った実身カードの id。**変化なしのときは None＝実身は作らない**
    /// （§1-3: 内容が同一なら実身は増えない。実機FB 2026-07-17）。
    pub created: Option<String>,
    pub message: String,
}

/// URLカード右クリック「再取得」。取得→**内容が変わっていれば**新しい実身カードを作り
/// 系譜を更新する（旧実身は残る＝イミュータブル・履歴が自然に溜まる）。
/// 変化がなければ実身は作らず「変化なし」を返す（304 または変換後ハッシュ一致）。
/// 未取得なら初回取得になる。
#[tauri::command(async)]
pub(crate) fn web_refetch(
    id: String,
    state: tauri::State<AppState>,
) -> Result<WebRefetchOut, String> {
    let (url, x, y, baseline) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let (url, baseline) = url_card_base(&p.conn, &id)?;
        let (x, y): (f64, f64) = p
            .conn
            .query_row(
                "SELECT pos_x, pos_y FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        (url, x, y, baseline)
    };
    // ネットワークはロック外で（素の OS スレッド・run_web_fetch と同じ流儀）。
    // ベースラインがあれば条件付きGET＝変わっていなければ 304 で本文転送すら省ける。
    let cond = baseline.as_ref().map(|b| &b.cond);
    let resp = std::thread::scope(|s| {
        s.spawn(|| fetch_url_cond(&url, false, cond))
            .join()
            .map_err(|_| "Web取得スレッドが異常終了しました".to_string())?
    })?;
    let Some(resp) = resp else {
        // 304 Not Modified: 取得済みの実身が最新のまま。
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        write_check_mark(&p.conn, &id, false);
        return Ok(WebRefetchOut {
            created: None,
            message: "変化はありません（サーバ確認 304・実身は作りませんでした）".into(),
        });
    };
    let out = response_to_out(&url, resp);

    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // 変化なし判定（本命の比較・§7-3）: サーバが 304 を返さなくても、同じ変換を通した
    // 結果がベースラインと同一ハッシュなら実身は作らない。ETag/取得日時だけ最新化して
    // 次回の条件付きGET が効くようにする。
    if let Some(b) = &baseline {
        if crate::cas::hash_bytes(&out.bytes) == b.real_id {
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET web_meta = ?1 WHERE id = ?2",
                rusqlite::params![out.meta, b.vid],
            );
            write_check_mark(&p.conn, &id, false);
            return Ok(WebRefetchOut {
                created: None,
                message: "変化はありません（内容が同一・実身は作りませんでした）".into(),
            });
        }
    }
    // 実ファイル＋CAS＋カード（flow 終端の実身化と同じ形＝プロジェクト直下のコミット済み実身）。
    let stem = out.stem.clone().or_else(|| stem_from_url(&url)).unwrap_or_else(|| "web".into());
    let dest = crate::objects::unique_file_path(&p.root, &format!("{stem}.{}", out.ext));
    std::fs::write(&dest, &out.bytes).map_err(|e| format!("書き出し失敗 ({e})"))?;
    let hash = crate::cas::store_bytes(&p.cas_root, &out.bytes).map_err(|e| e.to_string())?;
    crate::db::insert_real_body(&p.conn, &hash, &out.mime, out.bytes.len() as u64, Some(&out.ext))
        .map_err(|e| e.to_string())?;
    let label = dest.file_name().and_then(|s| s.to_str()).unwrap_or("output").to_string();
    let dest_str = dest.to_string_lossy().into_owned();
    let new_vid = crate::util::uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at,
                 committed_path, committed_mtime, web_meta, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'file')",
            rusqlite::params![
                new_vid,
                hash,
                label,
                x + 190.0,
                y,
                crate::db::now_ms(),
                dest_str,
                crate::util::file_mtime_ms(&dest),
                out.meta
            ],
        )
        .map_err(|e| e.to_string())?;
    record_fetch_lineage(&p.conn, &id, &new_vid);
    Ok(WebRefetchOut {
        created: Some(new_vid),
        message: format!("新しい実身「{label}」を作りました（前の実身は残っています）"),
    })
}

/// 取得系譜を「最新のみ」に張り替える（§7-3: 最新系譜のみ表示・過去は検索で辿る）:
/// URLカードから出る古い fetched エッジを soft-delete → target への1本を保証 →
/// 更新マークをクリア（いま取得したものが最新）。flow 終端（web_fetch 実行）と
/// 「再取得」の両方がここを通る。
pub(crate) fn record_fetch_lineage(conn: &rusqlite::Connection, url_card: &str, target: &str) {
    let now = crate::db::now_ms();
    let _ = conn.execute(
        "UPDATE relations SET deleted_at = ?1
         WHERE from_id = ?2 AND kind = 'fetched' AND to_id <> ?3 AND deleted_at IS NULL",
        rusqlite::params![now, url_card, target],
    );
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM relations
             WHERE from_id = ?1 AND to_id = ?2 AND kind = 'fetched' AND deleted_at IS NULL",
            rusqlite::params![url_card, target],
            |_| Ok(true),
        )
        .optional()
        .unwrap_or(None)
        .unwrap_or(false);
    if !exists {
        let _ = conn.execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at)
             VALUES (?1, ?2, 'fetched', ?3)",
            rusqlite::params![url_card, target, now],
        );
    }
    write_check_mark(conn, url_card, false);
}

/// 一括「このプロジェクトのURLカードの更新を確認」の結果。
#[derive(serde::Serialize)]
pub struct WebCheckAllOut {
    /// 確認した枚数（取得済み＝系譜を持つ URL カードだけが対象）。
    pub checked: usize,
    /// うち更新マークが付いた枚数。
    pub updated: usize,
    /// 系譜が無くスキップした URL カードの枚数。
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// キャンバスメニュー「URLカードの更新を確認」（明示操作なので pull 原則と矛盾しない）。
/// 直列に確認する（同一ホスト間隔 §4-3 は fetch_url 側の共通台帳が守る）。
#[tauri::command(async)]
pub(crate) fn web_check_update_all(
    state: tauri::State<AppState>,
) -> Result<WebCheckAllOut, String> {
    // 対象一覧はロック中に確定し、ネットワークはロック外で1枚ずつ。
    struct Target {
        id: String,
        label: String,
        url: String,
        baseline: Option<Baseline>,
    }
    let targets: Vec<Target> = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let ids: Vec<(String, String)> = {
            let mut stmt = p
                .conn
                .prepare(
                    "SELECT id, label FROM virtual_bodies
                     WHERE url_ref IS NOT NULL AND deleted_at IS NULL AND desktop_id IS NULL
                     ORDER BY created_at",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
        };
        ids.into_iter()
            .filter_map(|(id, label)| {
                let (url, baseline) = url_card_base(&p.conn, &id).ok()?;
                Some(Target { id, label, url, baseline })
            })
            .collect()
    };
    let mut out =
        WebCheckAllOut { checked: 0, updated: 0, skipped: 0, errors: Vec::new() };
    for t in targets {
        let Some(base) = t.baseline else {
            out.skipped += 1; // 未取得（系譜なし）は対象外＝比較のしようがない
            continue;
        };
        match fetch_and_compare(&t.url, &base) {
            Ok(updated) => {
                out.checked += 1;
                if updated {
                    out.updated += 1;
                }
                let guard = state.current.lock().unwrap();
                if let Some(p) = guard.as_ref() {
                    write_check_mark(&p.conn, &t.id, updated);
                }
            }
            Err(e) => out.errors.push(format!("{}: {e}", t.label)),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// v1.7 P3: web_feed — RSS/Atom → 記事カード群（計画 §3 P3・§7-2 決着）。
// 「概要＋実アドレスの2段構え」: カードは「タイトル＋概要＋記事リンク」の md で、
// 本文が要るときは記事 URL（メタに保持）を web_fetch で叩く。
// 1入力→N出力＝ビルトイン複数出力の解禁実例（NodeRun.outputs が受け皿）。
// 既読管理は meta の guid をマテリアライズ側（flow_run）が照合する＝差分 pull。
// ---------------------------------------------------------------------------

/// web_feed の既定取り込み件数（新しい方から。spec.limit で変更・0 = 全件）。
pub const WEB_FEED_DEFAULT_LIMIT: u32 = 10;

/// builtin:"web_feed" 本体: フィード URL → エントリごとの Markdown 実身（N 出力）。
/// 入力両対応は web_fetch と同型（テキスト/URL カード接続が優先・無ければ spec.url）。
/// reqwest::blocking を素の OS スレッドに包むのも同じ流儀。
pub fn run_web_feed(
    spec: &FilterSpec,
    input: Option<&[u8]>,
) -> Result<Vec<WebFetchOut>, String> {
    std::thread::scope(|s| {
        s.spawn(|| run_web_feed_blocking(spec, input))
            .join()
            .map_err(|_| "Web取得スレッドが異常終了しました".to_string())?
    })
}

fn run_web_feed_blocking(
    spec: &FilterSpec,
    input: Option<&[u8]>,
) -> Result<Vec<WebFetchOut>, String> {
    let url = match input {
        Some(bytes) => url_from_text(bytes)?,
        None => spec_url(spec)
            .ok_or("フィードURLがありません。パラメータ欄に URL を入力するか、URL を書いたテキストカードを接続してください")?
            .to_string(),
    };
    let resp = fetch_url(&url, spec.allow_private)?;
    feed_to_outs(&resp.final_url, &resp.bytes, spec.limit)
}

/// フィードのバイト列 → エントリごとの WebFetchOut（純関数・テスト対象）。
/// limit: None = 既定 WEB_FEED_DEFAULT_LIMIT・Some(0) = 全件。
fn feed_to_outs(
    feed_url: &str,
    bytes: &[u8],
    limit: Option<u32>,
) -> Result<Vec<WebFetchOut>, String> {
    let feed = feed_rs::parser::parse(bytes).map_err(|e| {
        format!("RSS/Atom フィードとして解釈できません: {e}（URL がフィード〔feed.xml / rss 等〕を指しているか確認してください）")
    })?;
    let feed_title = feed
        .title
        .as_ref()
        .map(|t| t.content.trim().to_string())
        .filter(|s| !s.is_empty());
    let take = match limit.unwrap_or(WEB_FEED_DEFAULT_LIMIT) {
        0 => usize::MAX,
        n => n as usize,
    };
    let now = crate::db::now_ms();
    // 概要（RSS description / Atom summary）は HTML が普通に混ざる → htmd で md 化。
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "noscript", "iframe"])
        .build();
    let mut outs = Vec::new();
    for (i, e) in feed.entries.iter().take(take).enumerate() {
        // 記事 URL: rel=alternate 優先 → 先頭リンク。無いエントリもあり得る（meta は null）。
        let link = e
            .links
            .iter()
            .find(|l| l.rel.as_deref() == Some("alternate"))
            .or_else(|| e.links.first())
            .map(|l| l.href.trim().to_string())
            .filter(|s| !s.is_empty());
        let title = e
            .title
            .as_ref()
            .map(|t| t.content.trim().to_string())
            .filter(|s| !s.is_empty());
        // 概要: summary → 無ければ content 本文（全文配信フィード）。HTML っぽければ md 化。
        let summary_raw = e
            .summary
            .as_ref()
            .map(|t| t.content.clone())
            .or_else(|| e.content.as_ref().and_then(|c| c.body.clone()))
            .unwrap_or_default();
        let summary = if summary_raw.contains('<') {
            converter.convert(&summary_raw).unwrap_or(summary_raw)
        } else {
            summary_raw
        };
        let summary = summary.trim();
        let published = e.published.or(e.updated);
        // 既読管理の鍵。feed-rs の Entry.id は必須（無い RSS でも link 等から補完される）。
        let guid = {
            let id = e.id.trim();
            if id.is_empty() { link.clone().unwrap_or_default() } else { id.to_string() }
        };

        // --- md 本文（単体で読めるカードにする）---
        let mut body = format!("# {}\n", title.as_deref().unwrap_or("(無題)"));
        let mut byline = Vec::new();
        if let Some(d) = published {
            byline.push(d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string());
        }
        if let Some(ft) = &feed_title {
            byline.push(ft.clone());
        }
        if !byline.is_empty() {
            body.push_str(&format!("\n{}\n", byline.join(" — ")));
        }
        if !summary.is_empty() {
            body.push_str(&format!("\n{summary}\n"));
        }
        if let Some(l) = &link {
            body.push_str(&format!("\n記事: {l}\n"));
        }

        let meta = serde_json::json!({
            "source_url": link,       // 記事 URL（web_fetch に渡す本文の在処）
            "feed_url": feed_url,     // どのフィード経由か（リダイレクト解決後）
            "guid": guid,             // 既読管理の鍵（flow_run が照合・差分 pull）
            "published": published.map(|d| d.to_rfc3339()),
            "fetched_at": now,
        })
        .to_string();
        let stem = title
            .as_deref()
            .and_then(sanitize_stem)
            .or_else(|| {
                feed_title.as_deref().and_then(|t| sanitize_stem(&format!("{t}_{}", i + 1)))
            })
            .unwrap_or_else(|| format!("feed_{}", i + 1));
        outs.push(WebFetchOut {
            bytes: body.into_bytes(),
            ext: "md".into(),
            mime: "text/markdown".into(),
            stem: Some(stem),
            meta,
        });
    }
    if outs.is_empty() {
        return Err("フィードにエントリがありません".into());
    }
    Ok(outs)
}

// ---------------------------------------------------------------------------
// v1.7 P5(b): to_url — URLカード化（テキスト→リンク・実機FB 2026-07-17）。
// テキスト全文を走査して見つかった URL を**全部**カード化する（1 URL = 1カード）。
// 「1本のリンクを保持する URL カード（P5(a) 受け皿）」と「リンクを量産するフィルタ」の
// 役割をはっきり分ける、というユーザー設計。
// ---------------------------------------------------------------------------

/// テキスト中の http(s):// URL を出現順に全部拾う（重複は最初の1つ・末尾の約物を落とす）。
/// 文字集合は実用 ASCII の URL 構成文字（空白・<>・引用符・括弧・全角文字で切る —
/// Markdown の `[題](URL)` や「URLです」の直結日本語で正しく止まる）。
pub fn scan_urls(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"https?://[0-9A-Za-z\-._~:/?#@!$&*+,;=%]+").unwrap()
    });
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        // 末尾の文末記号は URL の一部でないことがほとんど（「…html.」等）。
        let u = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']).to_string();
        if u.len() > "https://".len() && seen.insert(u.clone()) {
            out.push(u);
        }
    }
    out
}

/// builtin:"to_url" 本体: テキスト → URLカード化ペイロード群（1入力→N出力）。
/// ペイロードは text/plain の URL 文字列 — **途中に置けば**下流（web_fetch 等）へ
/// そのまま流れ、**終端なら** meta の url_card マーカーを見たマテリアライザが
/// URL カードとして着地させる（実ファイル・CAS なし）。
pub fn run_to_url(input: &[u8]) -> Result<Vec<WebFetchOut>, String> {
    let text = String::from_utf8_lossy(input);
    let urls = scan_urls(&text);
    if urls.is_empty() {
        return Err("テキストに URL（http:// / https://）が見つかりません".into());
    }
    Ok(urls
        .into_iter()
        .map(|u| WebFetchOut {
            bytes: u.into_bytes(),
            ext: "txt".into(),
            mime: "text/plain".into(),
            stem: None, // 入力カードの語幹を引き継ぐ（記事タイトル等がそのままカード名に）
            meta: r#"{"url_card":true}"#.into(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// v1.7 P4: web_capture — ページのスクリーンショット実身（L2 OSアダプタ・計画 §3 P4）。
// OS 搭載/汎用の Chromium 系ブラウザ（Windows=Edge 標準搭載）をヘッドレスで呼ぶ —
// ブラウザエンジンは同梱しない（§1-5 配布ミニマム）。web_fetch の本文 md と対で
// 「テキストと見た目」の両方が実身化できる。
//
// 実機検証（2026-07-17・Edge 150 / Windows 11）:
// - **--user-data-dir が必須**。無指定だと既存の Edge プロセスに合流して起動側が
//   即終了し、スクリーンショットが出力されない（exit=0 なのにファイル無し）。
// - 専用プロファイルなら安定（cold 7.4s / warm 2.8s）。同一プロファイルの並行起動は
//   2本目が1本目に合流して同じ症状になるため、プロセス内で直列化する（CAPTURE_LOCK）。
// - JS ページは --virtual-time-budget で描画を待てる（Yahoo!ニュース完全描画を確認）。
// ---------------------------------------------------------------------------

/// キャプチャの既定ビューポート。縦長既定＝「ページの保存」用途（設定 width/height で変更可）。
pub const WEB_CAPTURE_DEFAULT_WIDTH: u32 = 1280;
pub const WEB_CAPTURE_DEFAULT_HEIGHT: u32 = 1600;
/// JS 描画の仮想時間予算（ms）。レンダラの仮想時計を進めるので実時間はこれより短いことが多い。
const WEB_CAPTURE_TIME_BUDGET_MS: u32 = 8000;

/// ヘッドレス実行できる Chromium 系ブラウザを探す（L2: OS 差はこの関数に閉じ込める）。
/// Windows = Edge（標準搭載）→ Chrome。macOS/Linux = Chrome/Chromium/Edge を検出。
/// 未検出は導入ヒント付きの明確なエラー（speak の espeak と同じ扱い）。
pub fn find_headless_browser() -> Result<std::path::PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_default();
        let pf = std::env::var("ProgramFiles").unwrap_or_default();
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let candidates = [
            format!(r"{pf86}\Microsoft\Edge\Application\msedge.exe"),
            format!(r"{pf}\Microsoft\Edge\Application\msedge.exe"),
            format!(r"{pf}\Google\Chrome\Application\chrome.exe"),
            format!(r"{pf86}\Google\Chrome\Application\chrome.exe"),
            format!(r"{local}\Google\Chrome\Application\chrome.exe"),
        ];
        for c in candidates {
            let p = std::path::PathBuf::from(&c);
            if !c.is_empty() && p.is_file() {
                return Ok(p);
            }
        }
        Err("ヘッドレス実行できるブラウザが見つかりません。💡 Windows には Microsoft Edge が標準搭載のはずです（標準の場所に msedge.exe がありません）。Edge か Google Chrome をインストールしてください".into())
    }
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ];
        for c in candidates {
            let p = std::path::PathBuf::from(c);
            if p.is_file() {
                return Ok(p);
            }
        }
        Err("ヘッドレス実行できるブラウザが見つかりません。💡 Google Chrome / Chromium / Microsoft Edge のいずれかをインストールしてください".into())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge"];
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                for n in names {
                    let p = dir.join(n);
                    if p.is_file() {
                        return Ok(p);
                    }
                }
            }
        }
        Err("ヘッドレス実行できるブラウザが見つかりません。💡 chromium / google-chrome をインストールしてください（例: sudo apt install chromium-browser）".into())
    }
}

/// builtin:"web_capture" 本体: URL → ページのスクリーンショット PNG 実身。
/// 入力両対応（テキスト/URL カード接続が優先・無ければ spec.url）は web_fetch と同型。
/// cancel が立つとブラウザを kill して中断する（外部プロセスなので途中で切れる）。
pub fn run_web_capture(
    spec: &FilterSpec,
    input: Option<&[u8]>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<WebFetchOut, String> {
    let url_s = match input {
        Some(bytes) => url_from_text(bytes)?,
        None => spec_url(spec)
            .ok_or("URL がありません。パラメータ欄に URL を入力するか、URL を書いたテキストカードを接続してください")?
            .to_string(),
    };
    // §4 と同じ入口チェック。ネットに出るのはブラウザだが、行き先の検査は同じ基準で行う。
    let url = reqwest::Url::parse(&url_s).map_err(|e| format!("URL が不正です: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("http/https 以外の URL は取得できません: {}", url.scheme()));
    }
    if !spec.allow_private {
        url_check_public(&url)?;
    }
    if let Some(host) = url.host_str() {
        throttle_host(host);
    }
    let browser = find_headless_browser()?;
    let w = spec.width.unwrap_or(WEB_CAPTURE_DEFAULT_WIDTH).clamp(320, 4096);
    let h = spec.height.unwrap_or(WEB_CAPTURE_DEFAULT_HEIGHT).clamp(320, 8192);
    // 同一プロファイルの並行起動は2本目が合流して出力ゼロになる（実機検証）→ 直列化。
    static CAPTURE_LOCK: Mutex<()> = Mutex::new(());
    let _serial = CAPTURE_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir()
        .join(format!("texos_webcap_{}_{}.png", std::process::id(), crate::db::now_ms()));
    // プロファイルは使い回す（warm start 2.8s）。temp なので OS の掃除に任せてよい。
    let profile = std::env::temp_dir().join("texos_webcap_profile");
    let mut cmd = std::process::Command::new(&browser);
    cmd.arg("--headless")
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg("--no-first-run")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--virtual-time-budget={WEB_CAPTURE_TIME_BUDGET_MS}"))
        .arg(format!("--screenshot={}", tmp.display()))
        .arg(format!("--window-size={w},{h}"))
        .arg(url.as_str())
        // stderr を piped にしない: 初期プロセスが切り離した子がハンドルを持ち続けると
        // read が固まる（下記の分離挙動）。診断は終了コードとファイル有無で足りる。
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("ブラウザを起動できません（{}）: {e}", browser.display()))?;
    // 完了条件は「プロセス終了」ではなく**ファイルの出現＋サイズ安定**（実機検証 2026-07-17）:
    // Rust（CreateProcess 直）から起動すると初期 msedge が即 exit 0 で戻り、切り離された
    // 子プロセスが数秒後にスクリーンショットを書き上げる。PowerShell Start-Process 経由では
    // 初期プロセスが完走まで生きる — 起動元により分離挙動が変わるため、どちらでも成立する
    // 条件で待つ。タイムアウト（§4-2 と同じ 30s）と中断はポーリングで見る（100ms 刻み）。
    let deadline = Instant::now() + Duration::from_secs(WEB_TIMEOUT_SECS);
    let mut status: Option<std::process::ExitStatus> = None;
    loop {
        if status.is_none() {
            status = child.try_wait().map_err(|e| e.to_string())?;
        }
        // ファイルが現れ、200ms サイズが変わらなければ書き上がりとみなす。
        if let Ok(m) = std::fs::metadata(&tmp) {
            let sz = m.len();
            if sz > 0 {
                std::thread::sleep(Duration::from_millis(200));
                if std::fs::metadata(&tmp).map(|m2| m2.len() == sz).unwrap_or(false) {
                    break;
                }
                continue;
            }
        }
        let cancelled =
            cancel.map(|c| c.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(false);
        if cancelled || Instant::now() >= deadline {
            if status.is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = std::fs::remove_file(&tmp);
            return Err(if cancelled {
                "中断されました".into()
            } else if let Some(st) = status {
                format!(
                    "スクリーンショットが出力されませんでした（ブラウザ終了コード {:?}）。URL とネットワークを確認してください: {url_s}",
                    st.code()
                )
            } else {
                format!("キャプチャが {WEB_TIMEOUT_SECS} 秒以内に終わりませんでした: {url_s}")
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // 初期プロセスがまだ生きていれば軽く待って畳む（スクリーンショットは確保済み）。
    if status.is_none() {
        let grace = Instant::now() + Duration::from_secs(3);
        while Instant::now() < grace {
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    let bytes = std::fs::read(&tmp)
        .map_err(|e| format!("スクリーンショットを読み込めませんでした: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if bytes.is_empty() {
        return Err(format!("スクリーンショットが空でした: {url_s}"));
    }
    if bytes.len() > WEB_MAX_BYTES {
        return Err(format!(
            "サイズが上限 {}MB を超えています: {url_s}",
            WEB_MAX_BYTES / (1024 * 1024)
        ));
    }
    // capture:true = 取得系譜（fetched）と「更新を確認」の対象外 — 比較基準は本文変換
    // （md）のハッシュであり、png とは比較が成立しないため（link_fetch_source が見る）。
    let meta = serde_json::json!({
        "source_url": url_s,
        "fetched_at": crate::db::now_ms(),
        "capture": true,
    })
    .to_string();
    Ok(WebFetchOut {
        bytes,
        ext: "png".into(),
        mime: "image/png".into(),
        stem: stem_from_url(&url_s),
        meta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> FilterSpec {
        crate::flow::parse_spec(json).unwrap()
    }

    /// §4-5: プライベート・特殊アドレスの判定。
    #[test]
    fn private_ip_detection() {
        for s in ["127.0.0.1", "10.1.2.3", "192.168.0.10", "172.16.5.5", "169.254.1.1",
                  "100.100.1.1", "0.0.0.0", "::1", "fe80::1", "fc00::1"] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(ip_is_private(&ip), "{s} はプライベート扱いのはず");
        }
        for s in ["93.184.216.34", "8.8.8.8", "2606:2800:220:1:248:1893:25c8:1946"] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!ip_is_private(&ip), "{s} はパブリックのはず");
        }
    }

    /// URL 段階の遮断（リテラル IP・localhost・スキーム）。DNS を引かないものだけ。
    #[test]
    fn url_level_blocking() {
        let bad = ["http://127.0.0.1:8080/", "http://192.168.1.1/", "http://localhost:11434/",
                   "http://[::1]/", "http://sub.localhost/"];
        for u in bad {
            let url = reqwest::Url::parse(u).unwrap();
            assert!(url_check_public(&url).is_err(), "{u} は拒否のはず");
        }
        // fetch_url はスキームも見る
        assert!(fetch_url("ftp://example.com/x", false).is_err());
        assert!(fetch_url("file:///C:/x", false).is_err());
    }

    /// 入力テキストからの URL 取り出し（1行目・空行スキップ・非URLはエラー）。
    #[test]
    fn url_from_text_first_line() {
        assert_eq!(url_from_text("https://example.com/a\n2行目".as_bytes()).unwrap(),
                   "https://example.com/a");
        assert_eq!(url_from_text(b"\n  \nhttp://example.com \n").unwrap(),
                   "http://example.com");
        assert!(url_from_text(b"").is_err());
        assert!(url_from_text("メモ書き".as_bytes()).is_err());
    }

    /// spec_url は空白のみを無いのと同じに扱う。
    #[test]
    fn spec_url_trims() {
        assert_eq!(spec_url(&spec(r#"{"builtin":"web_fetch","url":" https://a.jp "}"#)),
                   Some("https://a.jp"));
        assert_eq!(spec_url(&spec(r#"{"builtin":"web_fetch","url":"  "}"#)), None);
        assert_eq!(spec_url(&spec(r#"{"builtin":"web_fetch"}"#)), None);
    }

    /// Content-Type の分解と charset。
    #[test]
    fn content_type_split() {
        assert_eq!(split_content_type("text/html; charset=UTF-8"),
                   ("text/html".into(), Some("UTF-8".into())));
        assert_eq!(split_content_type("TEXT/HTML"), ("text/html".into(), None));
        assert_eq!(split_content_type(""), ("".into(), None));
        assert_eq!(split_content_type(r#"text/html; charset="Shift_JIS""#).1,
                   Some("Shift_JIS".into()));
    }

    /// §7-4: BOM → ヘッダ → meta → UTF-8 厳密の順で一度だけ UTF-8 化。判定不能は None。
    #[test]
    fn decode_html_priority() {
        // ヘッダ charset（Shift_JIS）
        let sjis = encoding_rs::SHIFT_JIS.encode("<p>日本語</p>").0.into_owned();
        assert!(decode_html(&sjis, Some("shift_jis")).unwrap().contains("日本語"));
        // meta charset 走査
        let head = b"<meta charset=\"shift_jis\">".to_vec();
        let mut doc = head.clone();
        doc.extend_from_slice(&encoding_rs::SHIFT_JIS.encode("<p>日本語</p>").0);
        assert!(decode_html(&doc, None).unwrap().contains("日本語"));
        // UTF-8 厳密
        assert!(decode_html("<p>日本語</p>".as_bytes(), None).unwrap().contains("日本語"));
        // 判定不能（ヘッダ・meta 無しの SJIS は UTF-8 として不正）→ None = raw 実身化
        assert!(decode_html(&sjis, None).is_none());
    }

    /// タイトル整形とフォールバック語幹。
    #[test]
    fn stems() {
        assert_eq!(sanitize_stem("A/B: C?"), Some("A_B_ C_".into()));
        assert_eq!(sanitize_stem("  "), None);
        assert_eq!(stem_from_url("https://ex.jp/docs/guide.html"), Some("guide".into()));
        assert_eq!(stem_from_url("https://ex.jp/"), Some("ex.jp".into()));
        assert_eq!(ext_from_url("https://ex.jp/a/b.CSV?x=1"), Some("csv".into()));
        assert_eq!(ext_from_url("https://ex.jp/a/b"), None);
    }

    /// html→md: 本文抽出＋タイトル。抽出に足る本文が無い断片は全文フォールバック。
    #[test]
    fn html_to_md_paths() {
        let page = r#"<html><head><title>記事のタイトル</title></head><body>
            <nav>メニュー</nav>
            <article><h1>記事のタイトル</h1><p>本文の段落です。リンクは
            <a href="/rel">相対</a>。</p><p>二段落目。十分な長さの本文をここに書いて
            readability の判定に掛かるようにする。あいうえおかきくけこ。</p></article>
            <footer>フッタ</footer></body></html>"#;
        let (title, md) = html_to_markdown(page, "https://ex.jp/post/1");
        assert_eq!(title.as_deref(), Some("記事のタイトル"));
        assert!(md.contains("本文の段落"), "md: {md}");
        // 断片 HTML でも md にはなる（フォールバック経路）
        let (t2, md2) = html_to_markdown("<p>だけ</p>", "https://ex.jp/");
        assert!(t2.is_none());
        assert!(md2.contains("だけ"));
        // <title> フォールバックの実体
        assert_eq!(sniff_title("<TITLE> A &amp; B </TITLE>").as_deref(), Some("A & B"));
    }

    /// run_web_fetch: URL 無しの案内エラー（ネットに出ない失敗経路）。
    #[test]
    fn missing_url_is_clear_error() {
        let s = spec(r#"{"builtin":"web_fetch"}"#);
        let e = run_web_fetch(&s, None).unwrap_err();
        assert!(e.contains("URL がありません"), "{e}");
    }

    /// run_web_fetch: プライベート宛はネットに出る前に拒否（実 HTTP なしで通るテスト）。
    #[test]
    fn private_target_refused() {
        let s = spec(r#"{"builtin":"web_fetch","url":"http://127.0.0.1:7860/"}"#);
        let e = run_web_fetch(&s, None).unwrap_err();
        assert!(e.contains("プライベートアドレス"), "{e}");
    }

    /// 実機: ネットワークに出る統合テスト（html→md・タイトル語幹・メタ記録）。
    /// 実行: `cargo test --lib -- --ignored web_live`
    #[test]
    #[ignore = "実機: ネットワーク接続が必要"]
    fn web_live_fetch_example_com() {
        let s = spec(r#"{"builtin":"web_fetch","url":"https://example.com/"}"#);
        let out = run_web_fetch(&s, None).unwrap();
        assert_eq!(out.ext, "md");
        assert_eq!(out.mime, "text/markdown");
        let md = String::from_utf8(out.bytes).unwrap();
        assert!(md.contains("Example Domain"), "md: {md}");
        assert!(out.stem.as_deref().unwrap_or("").contains("Example"), "stem: {:?}", out.stem);
        let meta: serde_json::Value = serde_json::from_str(&out.meta).unwrap();
        assert_eq!(meta["source_url"], "https://example.com/");
        assert!(meta["fetched_at"].as_i64().unwrap() > 0);
    }

    /// 同一ホスト間隔: 2回目の予約が2秒後スロットになる（実待機はしない検証）。
    #[test]
    fn throttle_reserves_slot() {
        let t0 = Instant::now();
        throttle_host("throttle-test.example");
        throttle_host("throttle-test.example");
        // 2回目は最初のスロット+2秒まで待つ＝合計でおよそ2秒以上経過している。
        assert!(t0.elapsed() >= WEB_HOST_INTERVAL, "2回目の同一ホストは間隔を空けるはず");
    }

    /// v1.7 P2: 取得系譜の張り替え（最新のみ・§7-3）とベースライン読み・更新マークの
    /// ライフサイクル（張り替えでクリア）。
    #[test]
    fn lineage_and_check_mark() {
        let dir =
            std::env::temp_dir().join(format!("texos_web_p2_test_{}", crate::db::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::open(&dir.join("texos.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO real_bodies (id, mime_type, size, created_at)
               VALUES ('url:sentinel', 'application/vnd.texos.url', 0, 1),
                      ('hash1', 'text/markdown', 10, 1),
                      ('hash2', 'text/markdown', 12, 1);
             INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, url_ref, kind)
               VALUES ('u1', 'url:sentinel', 'Example', 0, 0, 1, 'https://ex.jp/', 'url');
             INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, web_meta, kind)
               VALUES ('f1', 'hash1', 'Example.md', 0, 0, 2,
                       '{\"source_url\":\"https://ex.jp/\",\"etag\":\"W/x\",\"last_modified\":\"LM\"}', 'file');
             INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind)
               VALUES ('f2', 'hash2', 'Example2.md', 0, 0, 3, 'file');",
        )
        .unwrap();
        // 系譜なし: URL は読めるがベースラインは None（=「更新を確認」は案内エラーになる）
        let (url, base) = url_card_base(&conn, "u1").unwrap();
        assert_eq!(url, "https://ex.jp/");
        assert!(base.is_none());
        // URLカードでない id はエラー
        assert!(url_card_base(&conn, "f1").is_err());
        // f1 へ系譜 → ベースライン = f1 の CAS ハッシュ＋web_meta の etag/last_modified
        record_fetch_lineage(&conn, "u1", "f1");
        let b = url_card_base(&conn, "u1").unwrap().1.unwrap();
        assert_eq!(b.real_id, "hash1");
        assert_eq!(b.cond.etag.as_deref(), Some("W/x"));
        assert_eq!(b.cond.last_modified.as_deref(), Some("LM"));
        // 更新マーク → f2 へ張り替え → 旧エッジは soft-delete・生きた系譜は1本・マーククリア
        write_check_mark(&conn, "u1", true);
        record_fetch_lineage(&conn, "u1", "f2");
        assert_eq!(url_card_base(&conn, "u1").unwrap().1.unwrap().real_id, "hash2");
        let live: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relations
                 WHERE from_id = 'u1' AND kind = 'fetched' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, 1, "最新系譜のみ生存（§7-3）");
        let meta: String = conn
            .query_row("SELECT web_meta FROM virtual_bodies WHERE id = 'u1'", [], |r| r.get(0))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(v["update"], false, "張り替えで更新マークはクリア");
        // 同じ target への再実行は重複エッジを作らない（冪等）
        record_fetch_lineage(&conn, "u1", "f2");
        let live2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relations
                 WHERE from_id = 'u1' AND kind = 'fetched' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live2, 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- v1.7 P3: web_feed ---

    const RSS_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
  <title>テストブログ</title>
  <link>https://ex.jp/blog/</link>
  <item>
    <title>新しい記事: A/B テスト</title>
    <link>https://ex.jp/blog/a</link>
    <guid>tag:ex.jp,2026:a</guid>
    <pubDate>Thu, 16 Jul 2026 10:00:00 GMT</pubDate>
    <description>&lt;p&gt;概要の&lt;b&gt;本文&lt;/b&gt;です。&lt;/p&gt;</description>
  </item>
  <item>
    <title>二本目</title>
    <link>https://ex.jp/blog/b</link>
    <description>プレーンな概要</description>
  </item>
</channel></rss>"#;

    const ATOM_SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atomフィード</title>
  <id>urn:uuid:feed</id>
  <updated>2026-07-16T10:00:00Z</updated>
  <entry>
    <title>Atom記事</title>
    <id>urn:uuid:entry-1</id>
    <link rel="alternate" href="https://ex.jp/atom/1"/>
    <updated>2026-07-16T10:00:00Z</updated>
    <summary>要約テキスト</summary>
  </entry>
</feed>"#;

    /// RSS: タイトル＝語幹・概要の md 化・記事リンクと guid がメタに載る・件数制限。
    #[test]
    fn feed_rss_entries() {
        let outs = feed_to_outs("https://ex.jp/feed.xml", RSS_SAMPLE.as_bytes(), Some(0)).unwrap();
        assert_eq!(outs.len(), 2);
        let o = &outs[0];
        assert_eq!(o.ext, "md");
        assert_eq!(o.mime, "text/markdown");
        // 語幹はタイトル（ファイル名に使えない文字は整形）
        assert_eq!(o.stem.as_deref(), Some("新しい記事_ A_B テスト"));
        let body = String::from_utf8(o.bytes.clone()).unwrap();
        assert!(body.starts_with("# 新しい記事: A/B テスト"), "body: {body}");
        assert!(body.contains("**本文**"), "HTML 概要は md 化: {body}");
        assert!(body.contains("テストブログ"), "出典（フィード名）: {body}");
        assert!(body.contains("記事: https://ex.jp/blog/a"), "実アドレス: {body}");
        let meta: serde_json::Value = serde_json::from_str(&o.meta).unwrap();
        assert_eq!(meta["source_url"], "https://ex.jp/blog/a");
        assert_eq!(meta["feed_url"], "https://ex.jp/feed.xml");
        assert_eq!(meta["guid"], "tag:ex.jp,2026:a");
        assert!(meta["published"].as_str().unwrap().starts_with("2026-07-16"));
        // 2本目: guid 無し → link で補完される（feed-rs の id 生成込みで空にはならない）
        let m2: serde_json::Value = serde_json::from_str(&outs[1].meta).unwrap();
        assert!(!m2["guid"].as_str().unwrap().is_empty());
        // 件数制限: 新しい方（先頭）から
        let one = feed_to_outs("https://ex.jp/feed.xml", RSS_SAMPLE.as_bytes(), Some(1)).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].stem, outs[0].stem);
    }

    /// Atom: rel=alternate リンク・summary・id。
    #[test]
    fn feed_atom_entries() {
        let outs = feed_to_outs("https://ex.jp/atom.xml", ATOM_SAMPLE.as_bytes(), None).unwrap();
        assert_eq!(outs.len(), 1);
        let meta: serde_json::Value = serde_json::from_str(&outs[0].meta).unwrap();
        assert_eq!(meta["source_url"], "https://ex.jp/atom/1");
        assert_eq!(meta["guid"], "urn:uuid:entry-1");
        let body = String::from_utf8(outs[0].bytes.clone()).unwrap();
        assert!(body.contains("要約テキスト"), "body: {body}");
    }

    /// フィードでないもの・空フィードは明確なエラー。
    #[test]
    fn feed_errors_are_clear() {
        let e = feed_to_outs("https://ex.jp/", b"<html><body>not a feed</body></html>", None)
            .unwrap_err();
        assert!(e.contains("フィードとして解釈できません"), "{e}");
        let empty = r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <title>空</title></channel></rss>"#;
        let e2 = feed_to_outs("https://ex.jp/", empty.as_bytes(), None).unwrap_err();
        assert!(e2.contains("エントリがありません"), "{e2}");
    }

    /// run_web_feed: URL 無しの案内エラー・プライベート宛拒否（ネットに出ない失敗経路）。
    #[test]
    fn feed_missing_url_and_private() {
        let s = spec(r#"{"builtin":"web_feed"}"#);
        let e = run_web_feed(&s, None).unwrap_err();
        assert!(e.contains("フィードURLがありません"), "{e}");
        let s = spec(r#"{"builtin":"web_feed","url":"http://127.0.0.1:8080/feed"}"#);
        let e = run_web_feed(&s, None).unwrap_err();
        assert!(e.contains("プライベートアドレス"), "{e}");
    }

    // --- v1.7 P5(b): to_url（URLカード化） ---

    /// URL 走査: 出現順・重複除去・末尾約物トリム・Markdown/日本語直結で正しく切れる。
    #[test]
    fn scan_urls_variants() {
        let text = "本文 https://a.jp/x?q=1&r=2 と [題](https://b.jp/y) 。\n\
                    記事: https://a.jp/x?q=1&r=2\n\
                    直結https://c.jp/zです。末尾 https://d.jp/w.html.";
        let urls = scan_urls(text);
        assert_eq!(
            urls,
            vec![
                "https://a.jp/x?q=1&r=2",
                "https://b.jp/y",
                "https://c.jp/z",
                "https://d.jp/w.html",
            ]
        );
        assert!(scan_urls("URL なしのテキスト").is_empty());
        assert!(scan_urls("https:// だけ（中身なし）").is_empty());
    }

    /// run_to_url: 1入力→N出力・ペイロードは text/plain の URL・url_card マーカー付き。
    /// URL が無いテキストは明確なエラー。
    #[test]
    fn to_url_payloads() {
        let outs = run_to_url("記事: https://ex.jp/a\nおまけ https://ex.jp/b".as_bytes()).unwrap();
        assert_eq!(outs.len(), 2);
        assert_eq!(String::from_utf8(outs[0].bytes.clone()).unwrap(), "https://ex.jp/a");
        assert_eq!(outs[0].mime, "text/plain");
        assert!(outs[0].stem.is_none(), "語幹は入力カードを引き継ぐ");
        let m: serde_json::Value = serde_json::from_str(&outs[0].meta).unwrap();
        assert_eq!(m["url_card"], true);
        let e = run_to_url("リンクなし".as_bytes()).unwrap_err();
        assert!(e.contains("URL"), "{e}");
    }

    // --- v1.7 P4: web_capture ---

    /// run_web_capture: URL 無し・不正スキーム・プライベート宛は**ブラウザ起動前に**
    /// 拒否される（ネットにもプロセスにも出ない失敗経路）。
    #[test]
    fn capture_missing_url_and_private() {
        let s = spec(r#"{"builtin":"web_capture"}"#);
        let e = run_web_capture(&s, None, None).unwrap_err();
        assert!(e.contains("URL がありません"), "{e}");
        let s = spec(r#"{"builtin":"web_capture","url":"http://127.0.0.1:7860/"}"#);
        let e = run_web_capture(&s, None, None).unwrap_err();
        assert!(e.contains("プライベートアドレス"), "{e}");
        let s = spec(r#"{"builtin":"web_capture","url":"file:///C:/x"}"#);
        let e = run_web_capture(&s, None, None).unwrap_err();
        assert!(e.contains("http/https 以外"), "{e}");
    }

    /// ブラウザ検出: 見つかれば実在パス・見つからなければ導入ヒント付きのエラー。
    #[test]
    fn browser_detection_is_actionable() {
        match find_headless_browser() {
            Ok(p) => assert!(p.is_file(), "検出パスが実在しない: {}", p.display()),
            Err(e) => assert!(e.contains("💡"), "導入ヒントが無い: {e}"),
        }
    }

    /// 実機: ヘッドレスブラウザで実キャプチャ（PNG マジック・メタ・語幹）。
    /// 実行: `cargo test --lib -- --ignored web_live`
    #[test]
    #[ignore = "実機: ブラウザとネットワーク接続が必要"]
    fn web_live_capture_example_com() {
        let s = spec(r#"{"builtin":"web_capture","url":"https://example.com/","width":800,"height":600}"#);
        let out = run_web_capture(&s, None, None).unwrap();
        assert_eq!(out.ext, "png");
        assert_eq!(out.mime, "image/png");
        assert!(out.bytes.starts_with(&[0x89, b'P', b'N', b'G']), "PNG マジック不一致");
        let meta: serde_json::Value = serde_json::from_str(&out.meta).unwrap();
        assert_eq!(meta["source_url"], "https://example.com/");
        assert_eq!(meta["capture"], true);
        assert!(out.stem.as_deref().unwrap_or("").contains("example"), "stem: {:?}", out.stem);
    }

    /// 実機: ネットワークに出る統合テスト（実在フィード）。
    /// 実行: `cargo test --lib -- --ignored web_live`
    #[test]
    #[ignore = "実機: ネットワーク接続が必要"]
    fn web_live_feed_rust_blog() {
        let s = spec(r#"{"builtin":"web_feed","url":"https://blog.rust-lang.org/feed.xml","limit":3}"#);
        let outs = run_web_feed(&s, None).unwrap();
        assert!(!outs.is_empty() && outs.len() <= 3, "len: {}", outs.len());
        for o in &outs {
            assert_eq!(o.mime, "text/markdown");
            let meta: serde_json::Value = serde_json::from_str(&o.meta).unwrap();
            assert!(!meta["guid"].as_str().unwrap_or("").is_empty());
        }
    }

    /// 実機: 条件付き GET（ETag/Last-Modified → 304 は Ok(None)）。
    /// 実行: `cargo test --lib -- --ignored web_live`
    #[test]
    #[ignore = "実機: ネットワーク接続が必要"]
    fn web_live_conditional_get_304() {
        let first = fetch_url("https://example.com/", false).unwrap();
        let cond = FetchCond { etag: first.etag.clone(), last_modified: first.last_modified.clone() };
        if cond.etag.is_none() && cond.last_modified.is_none() {
            return; // 検証材料が無いサーバなら何も主張しない
        }
        let again = fetch_url_cond("https://example.com/", false, Some(&cond)).unwrap();
        assert!(again.is_none(), "同じ条件なら 304 = Ok(None) のはず");
    }
}
