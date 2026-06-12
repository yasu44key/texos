// v1.4 オートフローMVP（⑤）: フィルタ実行の純ロジック。
// 「UNIX CUI テキストフィルタパイプラインのGUI再来」の最小単位 —
// バイト列を受けてバイト列を返す。DB・キャンバスへの配線は lib.rs の run_flow。
use serde::Deserialize;

/// virtual_bodies.filter_ref に保存される JSON（V1.3後半_オートフロー設計.md 確定方針2）。
#[derive(Deserialize, Clone)]
pub struct FilterSpec {
    /// 内蔵フィルタ名: text_grep | text_replace | img_resize | img_convert
    #[serde(default)]
    pub builtin: Option<String>,
    /// 外部コマンド（{in}/{out} プレースホルダ）。builtin が無いときに使う逃げ道
    #[serde(default)]
    pub cmd: Option<String>,
    /// 受け入れMIME: "*/*" | "text/*" | "image/*" 等（接続時型チェック。front と同一規則）
    #[serde(default)]
    pub accept_mime: Option<String>,
    /// 出力拡張子（外部コマンド用。内蔵フィルタは自動決定）
    #[serde(default)]
    pub output_ext: Option<String>,
    // --- 内蔵フィルタの引数 ---
    #[serde(default)]
    pub pattern: Option<String>, // text_grep / text_replace（正規表現）
    #[serde(default)]
    pub replacement: Option<String>, // text_replace
    #[serde(default)]
    pub width: Option<u32>, // img_resize（高さはアスペクト維持）
    #[serde(default)]
    pub format: Option<String>, // img_convert: png | jpeg | webp | bmp | gif
}

pub fn parse_spec(json: &str) -> Result<FilterSpec, String> {
    serde_json::from_str(json).map_err(|e| format!("フィルタ定義(JSON)が不正です: {e}"))
}

/// 接続時型チェック。front 側 model.ts の mimeMatches と同じ規則を保つこと:
/// 空・"*/*" は全受け入れ / "text/*" は text/ 系＋JSON / その他は前方一致 or 完全一致。
pub fn mime_matches(accept: &str, mime: &str) -> bool {
    if accept.is_empty() || accept == "*/*" {
        return true;
    }
    if let Some(prefix) = accept.strip_suffix("/*") {
        if prefix == "text" && mime == "application/json" {
            return true;
        }
        return mime.starts_with(&format!("{prefix}/"));
    }
    accept == mime
}

/// 内蔵フィルタの実行: 入力バイト列 → (出力バイト列, 出力拡張子, 出力MIME)。
pub fn run_builtin(
    spec: &FilterSpec,
    name: &str,
    input: &[u8],
) -> Result<(Vec<u8>, String, String), String> {
    match name {
        "text_grep" => {
            let pat = spec.pattern.as_deref().unwrap_or("");
            let re = regex::Regex::new(pat).map_err(|e| format!("正規表現が不正です: {e}"))?;
            let text = String::from_utf8_lossy(input);
            let out: String = text
                .lines()
                .filter(|l| re.is_match(l))
                .collect::<Vec<_>>()
                .join("\n");
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_replace" => {
            let pat = spec.pattern.as_deref().unwrap_or("");
            let re = regex::Regex::new(pat).map_err(|e| format!("正規表現が不正です: {e}"))?;
            let rep = spec.replacement.as_deref().unwrap_or("");
            let text = String::from_utf8_lossy(input);
            let out = re.replace_all(&text, rep).into_owned();
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "img_resize" => {
            // 幅指定・アスペクト維持。出力はPNGに統一（劣化なし・確実にエンコード可能）。
            let w = spec.width.unwrap_or(800).max(1);
            let img = image::load_from_memory(input)
                .map_err(|e| format!("画像を読めません: {e}"))?;
            let resized = img.resize(w, u32::MAX, image::imageops::FilterType::Lanczos3);
            let mut buf = std::io::Cursor::new(Vec::new());
            resized
                .write_to(&mut buf, image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            Ok((buf.into_inner(), "png".into(), "image/png".into()))
        }
        "img_convert" => {
            let fmt_name = spec.format.as_deref().unwrap_or("png").to_lowercase();
            let (fmt, ext, mime) = match fmt_name.as_str() {
                "png" => (image::ImageFormat::Png, "png", "image/png"),
                "jpeg" | "jpg" => (image::ImageFormat::Jpeg, "jpg", "image/jpeg"),
                "webp" => (image::ImageFormat::WebP, "webp", "image/webp"),
                "bmp" => (image::ImageFormat::Bmp, "bmp", "image/bmp"),
                "gif" => (image::ImageFormat::Gif, "gif", "image/gif"),
                other => return Err(format!("未対応の出力形式: {other}")),
            };
            let img = image::load_from_memory(input)
                .map_err(|e| format!("画像を読めません: {e}"))?;
            let mut buf = std::io::Cursor::new(Vec::new());
            if fmt == image::ImageFormat::Jpeg {
                // JPEG はアルファ非対応 → RGB に落としてからエンコード
                image::DynamicImage::ImageRgb8(img.to_rgb8())
                    .write_to(&mut buf, fmt)
                    .map_err(|e| e.to_string())?;
            } else {
                img.write_to(&mut buf, fmt).map_err(|e| e.to_string())?;
            }
            Ok((buf.into_inner(), ext.into(), mime.into()))
        }
        other => Err(format!("未知の内蔵フィルタ: {other}")),
    }
}

/// 外部コマンドの実行（逃げ道）: {in}/{out} を一時ファイルパスに置換してシェル実行。
/// 一時ファイルは読み取り後すぐ削除する（TEMP孤児を作らない方針）。
pub fn run_external(
    cmdline: &str,
    input: &[u8],
    in_ext: &str,
    out_ext: &str,
) -> Result<Vec<u8>, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join("texos_flow");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let in_path = dir.join(format!("in_{nanos}.{in_ext}"));
    let out_path = dir.join(format!("out_{nanos}.{out_ext}"));
    std::fs::write(&in_path, input).map_err(|e| e.to_string())?;
    let cmd = cmdline
        .replace("{in}", &in_path.to_string_lossy())
        .replace("{out}", &out_path.to_string_lossy());

    #[cfg(windows)]
    let status = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/C", &cmd])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW（コンソールを出さない）
            .status()
    };
    #[cfg(not(windows))]
    let status = std::process::Command::new("sh").args(["-c", &cmd]).status();

    let result = match status {
        Err(e) => Err(format!("コマンド起動に失敗: {e}")),
        Ok(s) if !s.success() => Err(format!("コマンドが失敗しました (exit {:?})", s.code())),
        Ok(_) => std::fs::read(&out_path)
            .map_err(|_| "コマンドは成功しましたが {out} に出力がありません".to_string()),
    };
    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> FilterSpec {
        parse_spec(json).unwrap()
    }

    #[test]
    fn mime_matching_rules() {
        assert!(mime_matches("*/*", "application/pdf"));
        assert!(mime_matches("", "image/png"));
        assert!(mime_matches("text/*", "text/plain"));
        assert!(mime_matches("text/*", "application/json")); // JSONはテキスト扱い
        assert!(!mime_matches("text/*", "image/png"));
        assert!(mime_matches("image/*", "image/webp"));
        assert!(mime_matches("application/pdf", "application/pdf"));
        assert!(!mime_matches("application/pdf", "application/zip"));
    }

    #[test]
    fn text_grep_keeps_matching_lines() {
        let s = spec(r#"{"builtin":"text_grep","pattern":"foo"}"#);
        let (out, ext, mime) = run_builtin(&s, "text_grep", b"foo bar\nbaz\nfoofoo").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "foo bar\nfoofoo");
        assert_eq!(ext, "txt");
        assert_eq!(mime, "text/plain");
    }

    #[test]
    fn text_replace_uses_regex() {
        let s = spec(r#"{"builtin":"text_replace","pattern":"a+","replacement":"X"}"#);
        let (out, _, _) = run_builtin(&s, "text_replace", b"aaa b aa").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "X b X");
    }

    #[test]
    fn invalid_regex_is_a_clean_error() {
        let s = spec(r#"{"builtin":"text_grep","pattern":"("}"#);
        assert!(run_builtin(&s, "text_grep", b"x").is_err());
    }

    #[test]
    fn img_convert_roundtrip() {
        // 4x4 PNG を作って BMP へ変換 → デコードできること
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let s = spec(r#"{"builtin":"img_convert","format":"bmp"}"#);
        let (out, ext, mime) = run_builtin(&s, "img_convert", png.get_ref()).unwrap();
        assert_eq!(ext, "bmp");
        assert_eq!(mime, "image/bmp");
        assert!(image::load_from_memory(&out).is_ok());
    }

    #[test]
    fn img_resize_constrains_width() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(100, 50));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let s = spec(r#"{"builtin":"img_resize","width":10}"#);
        let (out, _, _) = run_builtin(&s, "img_resize", png.get_ref()).unwrap();
        let resized = image::load_from_memory(&out).unwrap();
        assert_eq!(resized.width(), 10);
        assert_eq!(resized.height(), 5); // アスペクト維持
    }
}
