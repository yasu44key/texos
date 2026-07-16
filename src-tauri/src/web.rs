// v1.7 P1: web_fetch ビルトイン（V1.7_情報収集経路計画.md §3 P1）。
// 「Web を実身化する」— URL の本文を Markdown 実身としてキャンバスに取り込む。
// WebView は埋め込まない。ページは消えるが実身は残る（水泡に帰さない）。
//
// §4 安全最小セットはこのモジュールの共通関数（fetch_url）に集約し、
// P3 web_feed / P4 web_capture もここを共用する（個別実装にしない）。

use crate::flow::FilterSpec;
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

/// §4 安全最小セットを同梱した GET（web_* 系の共通入口）。
/// reqwest::blocking は tokio 文脈で drop するとパニックするため、呼び出し側
/// （run_web_fetch）が素の OS スレッドに包む — run_ai と同じ流儀。
pub fn fetch_url(url_s: &str, allow_private: bool) -> Result<WebResponse, String> {
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
    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("取得に失敗しました: {e}"))?;
    let status = resp.status();
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
    Ok(WebResponse { bytes, final_url, content_type, etag, last_modified })
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
fn sanitize_stem(s: &str) -> Option<String> {
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
    let (mime, header_charset) = split_content_type(&resp.content_type);
    let meta = build_meta(&url, &resp);

    // --- text/html 以外: raw のまま実身化（§3 P1・変換もしない＝§7-4）---
    if !(mime == "text/html" || mime == "application/xhtml+xml") {
        // 拡張子: 汎用 MIME（text/plain 等）は URL 側の拡張子を優先（csv が text/plain で
        // 配られる等）。それ以外は MIME の正引きを優先し、無ければ URL 拡張子 → bin。
        let url_ext = ext_from_url(&resp.final_url).or_else(|| ext_from_url(&url));
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
        return Ok(WebFetchOut {
            bytes: resp.bytes,
            ext,
            mime: out_mime,
            stem: stem_from_url(&resp.final_url).or_else(|| stem_from_url(&url)),
            meta,
        });
    }

    // --- HTML: UTF-8 化 → 本文抽出 → Markdown ---
    let html = match decode_html(&resp.bytes, header_charset.as_deref()) {
        Some(h) => h,
        None => {
            // 文字コード判定不能 → raw の .html のまま実身化（後段 text_convert で直す）。
            return Ok(WebFetchOut {
                bytes: resp.bytes,
                ext: "html".into(),
                mime: "text/html".into(),
                stem: stem_from_url(&resp.final_url).or_else(|| stem_from_url(&url)),
                meta,
            });
        }
    };
    let (title, md) = html_to_markdown(&html, &resp.final_url);
    let stem = title
        .as_deref()
        .and_then(sanitize_stem)
        .or_else(|| stem_from_url(&resp.final_url))
        .or_else(|| stem_from_url(&url));
    let body = match (&title, md.starts_with('#')) {
        // タイトルが本文側に無ければ見出しとして先頭に付ける（単体で読める md にする）。
        (Some(t), false) if !t.trim().is_empty() => format!("# {}\n\n{md}", t.trim()),
        _ => md,
    };
    Ok(WebFetchOut {
        bytes: body.into_bytes(),
        ext: "md".into(),
        mime: "text/markdown".into(),
        stem,
        meta,
    })
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
}
