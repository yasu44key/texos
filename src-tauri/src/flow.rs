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
    /// 外部コマンドの実行上限秒（未指定は60秒）。暴走・ストール対策。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    // --- v1.5 フィルタ設定ダイアログ（新形式）。name があれば新形式として解釈 ---
    /// プログラムのファイル名（例: sp2tab.exe）
    #[serde(default)]
    pub name: Option<String>,
    /// プログラムの場所。空=「.texos/filters → 環境変数PATH」で name を探す。
    /// 絶対パス（ファイル選択ダイアログ由来）は最優先
    #[serde(default)]
    pub path: Option<String>,
    /// 実行形式: "filter"（< {in} > {out}）| "command"（引数で {in} {out}）| "generator"（入力不要）
    #[serde(default, rename = "type")]
    pub filter_type: Option<String>,
    /// 想定する入力拡張子（空=制限なし）。合わない入力はスキップ報告
    #[serde(default)]
    pub in_exts: Option<Vec<String>>,
    /// 出力先フォルダ。空=プロジェクト直下にコミット済み実身／指定=そこへ書き出して窓（仮身）
    #[serde(default)]
    pub out_dir: Option<String>,
    /// 出力名の接頭辞（空=従来の「入力名_フィルタ名」命名）
    #[serde(default)]
    pub prefix: Option<String>,
    /// 接頭辞使用時の次の連番。実行のたびにカードへ書き戻す
    #[serde(default)]
    pub seq: Option<u32>,
    /// ユーザーメモ
    #[serde(default)]
    pub note: Option<String>,
    /// v1.3.11 対話シェル: "cmd" | "powershell"。設定されていれば run_flow は捕捉せず
    /// 対話ウィンドウを detached で開き、cmd（一行目のコマンド・{in}可）を実行して継続する。
    #[serde(default)]
    pub shell: Option<String>,
}

/// 外部プログラムの解決: 絶対パス → <project>/.texos/filters → 環境変数PATH の順
/// （v1.5 確定方針: 明示パスが見つからなくても後段へフォールバックして探す）。
pub fn resolve_program(
    name: &str,
    path: Option<&str>,
    filters_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if let Some(p) = path.map(str::trim).filter(|p| !p.is_empty()) {
        let pb = std::path::PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
        // フォルダ指定なら name を結合して試す
        let joined = pb.join(name);
        if joined.is_file() {
            return Some(joined);
        }
    }
    let bundled = filters_dir.join(name);
    if bundled.is_file() {
        return Some(bundled);
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
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

/// 外部コマンドの既定タイムアウト（FilterSpec.timeout_secs で上書き可）。
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// 暴走プログラムが出力を書き続けたときの強制終了しきい値。
const MAX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;

/// プロセスをツリーごと止める。Windows は cmd /C 経由で起動しているため
/// child.kill() では cmd だけが死に実体の孫プロセスが残る — taskkill /T で根こそぎ。
#[cfg(windows)]
fn kill_tree(child: &mut std::process::Child) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &child.id().to_string()])
        .creation_flags(0x0800_0000)
        .status();
    let _ = child.wait();
}
#[cfg(not(windows))]
fn kill_tree(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// CMD が「集約実行」（全入力を集めて1回実行）を要求しているか。
/// `{in*}`（全入力一括）または `{in2}` 以降の位置指定があれば集約。
/// `{in}`/`{in1}` のみなら従来の「入力ごとに実行」（ファンアウト）。
pub fn cmd_is_aggregate(cmd: &str) -> bool {
    if cmd.contains("{in*}") {
        return true;
    }
    regex::Regex::new(r"\{in([2-9]|[1-9]\d+)\}")
        .map(|re| re.is_match(cmd))
        .unwrap_or(false)
}

/// 外部コマンドの実行（逃げ道）: プレースホルダを一時ファイルパスに置換してシェル実行。
/// 一時ファイルは読み取り後すぐ削除する（TEMP孤児を作らない方針）。
///
/// 入力プレースホルダ（v1.5 複数入力 — gcc の分割ソースや画像合成）:
/// - `{in}`  = 最初の入力（従来互換。`{in1}` と同義）
/// - `{inN}` = 接続順の N 番目（1始まり）。ユーザーが `"{in1}"` とクォートして書く
/// - `{in*}` = 全入力。各ファイルを個別にクォートして空白区切りに展開（裸で書く）
///
/// 安全弁（v1.4 実機指摘: フィルタ型プログラムの誤指定で TexoS ごとストール）:
/// - stdin は常に閉じる — 標準入力待ちのプログラムは即 EOF を受けて終わる
/// - タイムアウト・出力サイズ上限・`cancel`（UIの中断ボタン）でツリーごと強制終了
/// - stderr はファイルに受けて、失敗時のエラーメッセージに含める
pub fn run_external(
    cmdline: &str,
    inputs: &[(&[u8], &str)],
    out_ext: &str,
    timeout_secs: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>, String> {
    use std::sync::atomic::Ordering;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join("texos_flow");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out_path = dir.join(format!("out_{nanos}.{out_ext}"));
    let err_path = dir.join(format!("err_{nanos}.txt"));
    let mut in_paths = Vec::new();
    for (i, (bytes, ext)) in inputs.iter().enumerate() {
        let p = dir.join(format!("in_{nanos}_{}.{}", i + 1, ext));
        if let Err(e) = std::fs::write(&p, bytes) {
            for q in &in_paths {
                let _ = std::fs::remove_file(q);
            }
            return Err(e.to_string());
        }
        in_paths.push(p);
    }
    let cleanup_inputs = |paths: &[std::path::PathBuf]| {
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    };
    let err_file = match std::fs::File::create(&err_path) {
        Ok(f) => f,
        Err(e) => {
            cleanup_inputs(&in_paths);
            return Err(e.to_string());
        }
    };
    // プレースホルダ展開: {in*} → 全入力（各個クォート）/ {inN} → N番目 / {in} → 1番目
    let mut cmd = cmdline.replace("{out}", &out_path.to_string_lossy());
    if cmd.contains("{in*}") {
        let joined = in_paths
            .iter()
            .map(|p| format!("\"{}\"", p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        cmd = cmd.replace("{in*}", &joined);
    }
    for (i, p) in in_paths.iter().enumerate() {
        cmd = cmd.replace(&format!("{{in{}}}", i + 1), &p.to_string_lossy());
    }
    if let Some(first) = in_paths.first() {
        cmd = cmd.replace("{in}", &first.to_string_lossy());
    }
    // 未解決の入力プレースホルダ = 入力矢印が足りない（集約実行は本数が意味を持つ）
    if let Ok(re) = regex::Regex::new(r"\{in\d*\}") {
        if let Some(m) = re.find(&cmd) {
            cleanup_inputs(&in_paths);
            let _ = std::fs::remove_file(&err_path);
            return Err(format!(
                "{} に対応する入力がありません（接続されている入力: {}本）",
                m.as_str(),
                in_paths.len()
            ));
        }
    }

    #[cfg(windows)]
    let spawned = {
        use std::os::windows::process::CommandExt;
        // コマンドライン全体を raw_arg で「素のまま」cmd に渡す。
        // std の自動クォート（\" エスケープ）は cmd.exe の解釈と非互換で、
        // 引用符を含むコマンドが exit 1 で壊れる（v1.4 実機指摘）。
        // さらに全体を "..." で包む — cmd /C は先頭が引用符のとき最初と最後の
        // 引用符を剥がすため、内側のクォートを無傷で残す定石。
        std::process::Command::new("cmd")
            .raw_arg(format!("/C \"{cmd}\""))
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW（コンソールを出さない）
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(err_file)
            .spawn()
    };
    #[cfg(not(windows))]
    let spawned = std::process::Command::new("sh")
        .args(["-c", &cmd])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(err_file)
        .spawn();

    // v1.3.11: timeout_secs==0 は「無制限」（開発者モード）。None は既定60秒。
    // 無制限でも中断ボタン（cancel）と出力上限は効く＝ハンドルを失わない。
    let timeout: Option<std::time::Duration> = match timeout_secs {
        Some(0) => None,
        Some(s) => Some(std::time::Duration::from_secs(s)),
        None => Some(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS)),
    };
    let started = std::time::Instant::now();
    let status: Result<std::process::ExitStatus, String> = match spawned {
        Err(e) => Err(format!("コマンド起動に失敗: {e}")),
        Ok(mut child) => loop {
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) => {}
                Err(e) => {
                    kill_tree(&mut child);
                    break Err(format!("プロセス監視に失敗: {e}"));
                }
            }
            if cancel.load(Ordering::Relaxed) {
                kill_tree(&mut child);
                break Err("中断されました".into());
            }
            if let Some(to) = timeout {
                if started.elapsed() > to {
                    kill_tree(&mut child);
                    break Err(format!(
                        "タイムアウト（{}秒）のため強制終了しました。標準入力待ちのフィルタ型プログラムを引数型で指定していないか確認してください",
                        to.as_secs()
                    ));
                }
            }
            if std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0) > MAX_OUTPUT_BYTES {
                kill_tree(&mut child);
                break Err("出力が上限（512MB）を超えたため強制終了しました".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        },
    };

    // stderr の先頭だけ拾ってエラーメッセージに添える（長大ログは切る）
    let stderr_snip = || -> String {
        match std::fs::read(&err_path) {
            Ok(b) if !b.is_empty() => {
                let s = String::from_utf8_lossy(&b);
                let head: String = s.trim().chars().take(500).collect();
                format!("\n{head}")
            }
            _ => String::new(),
        }
    };
    let result = match status {
        Err(e) => Err(e),
        Ok(s) if !s.success() => Err(format!(
            "コマンドが失敗しました (exit {:?}){}",
            s.code(),
            stderr_snip()
        )),
        Ok(_) => std::fs::read(&out_path)
            .map_err(|_| "コマンドは成功しましたが {out} に出力がありません".to_string()),
    };
    cleanup_inputs(&in_paths);
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
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

    fn no_cancel() -> std::sync::atomic::AtomicBool {
        std::sync::atomic::AtomicBool::new(false)
    }

    /// プログラム解決の順序: 明示パス → .texos/filters → 環境変数PATH。
    #[cfg(windows)]
    #[test]
    fn resolve_program_search_order() {
        let tmp = std::env::temp_dir().join(format!("texos_rp_{}", std::process::id()));
        let filters = tmp.join("filters");
        std::fs::create_dir_all(&filters).unwrap();
        let bundled = filters.join("prog.exe");
        std::fs::write(&bundled, b"x").unwrap();
        // filters 同梱が見つかる
        assert_eq!(resolve_program("prog.exe", None, &filters), Some(bundled.clone()));
        // 明示パス（ファイル）が最優先
        let explicit = tmp.join("explicit.exe");
        std::fs::write(&explicit, b"x").unwrap();
        assert_eq!(
            resolve_program("prog.exe", Some(explicit.to_str().unwrap()), &filters),
            Some(explicit)
        );
        // 明示パスがフォルダなら name を結合
        assert_eq!(
            resolve_program("prog.exe", Some(filters.to_str().unwrap()), &filters),
            Some(bundled)
        );
        // どこにも無ければ環境変数 PATH へフォールバック
        assert!(resolve_program("cmd.exe", None, &filters).is_some());
        assert!(resolve_program("texos_no_such_prog.exe", None, &filters).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn one(bytes: &'static [u8]) -> Vec<(&'static [u8], &'static str)> {
        vec![(bytes, "txt")]
    }

    /// 引用符付きパスのコマンドが cmd /C 経由で動くこと（raw_arg 修正の回帰テスト）。
    #[cfg(windows)]
    #[test]
    fn external_with_quoted_paths_runs() {
        let out =
            run_external("copy \"{in}\" \"{out}\"", &one(b"hello"), "txt", None, &no_cancel())
                .unwrap();
        assert_eq!(out, b"hello");
    }

    /// 標準入出力フィルタの形（`prog < {in} > {out}`）がリダイレクトで動くこと。
    #[cfg(windows)]
    #[test]
    fn external_stdio_redirection_runs() {
        let out = run_external(
            "sort < \"{in}\" > \"{out}\"",
            &one(b"b\r\na\r\n"),
            "txt",
            None,
            &no_cancel(),
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&out).starts_with('a'));
    }

    /// フィルタ型プログラムを引数型で誤指定（入力リダイレクトなし）してもストール
    /// しないこと — stdin が閉じているので sort は即 EOF を受けて空出力で終わる。
    #[cfg(windows)]
    #[test]
    fn external_stdin_is_closed_so_filter_programs_do_not_stall() {
        let out =
            run_external("sort > \"{out}\"", &one(b"ignored"), "txt", Some(10), &no_cancel())
                .unwrap();
        assert!(out.is_empty() || out == b"\r\n");
    }

    /// 終わらないコマンドはタイムアウトでツリーごと強制終了されること。
    #[cfg(windows)]
    #[test]
    fn external_times_out_and_kills() {
        let started = std::time::Instant::now();
        let err = run_external(
            "ping -n 30 127.0.0.1 > \"{out}\"",
            &one(b""),
            "txt",
            Some(1),
            &no_cancel(),
        )
        .unwrap_err();
        assert!(err.contains("タイムアウト"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    /// cancel フラグ（UIの中断ボタン）で実行中のコマンドが止まること。
    #[cfg(windows)]
    #[test]
    fn external_cancel_breaks_running_command() {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c2 = cancel.clone();
        let killer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            c2.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let err = run_external(
            "ping -n 30 127.0.0.1 > \"{out}\"",
            &one(b""),
            "txt",
            Some(60),
            &cancel,
        )
        .unwrap_err();
        killer.join().unwrap();
        assert!(err.contains("中断"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    /// 集約モードの判定: {in*} か {in2} 以降があれば集約、{in}/{in1} だけなら個別。
    #[test]
    fn aggregate_detection() {
        assert!(cmd_is_aggregate("{in*} -o \"{out}\""));
        assert!(cmd_is_aggregate("\"{in1}\" \"{in2}\" -composite \"{out}\""));
        assert!(!cmd_is_aggregate("\"{in}\" \"{out}\""));
        assert!(!cmd_is_aggregate("< \"{in1}\" > \"{out}\""));
    }

    /// 位置指定 {in1}+{in2} で2入力を連結できること（接続順が保たれる）。
    #[cfg(windows)]
    #[test]
    fn external_positional_two_inputs() {
        let inputs: Vec<(&[u8], &str)> = vec![(b"foo", "txt"), (b"bar", "txt")];
        let out = run_external(
            "copy \"{in1}\"+\"{in2}\" \"{out}\"",
            &inputs,
            "txt",
            None,
            &no_cancel(),
        )
        .unwrap();
        assert!(out.starts_with(b"foobar"), "{:?}", String::from_utf8_lossy(&out));
    }

    /// {in*} が全入力のクォート済みパス列に展開されること。
    #[cfg(windows)]
    #[test]
    fn external_in_star_expands_all_inputs() {
        let inputs: Vec<(&[u8], &str)> = vec![(b"a", "txt"), (b"b", "txt")];
        let out = run_external("echo {in*} > \"{out}\"", &inputs, "txt", None, &no_cancel())
            .unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("_1.txt") && s.contains("_2.txt"), "{s}");
    }

    /// 入力本数より大きい {inN} は実行前にエラーになること。
    #[test]
    fn external_missing_positional_input_is_an_error() {
        let inputs: Vec<(&[u8], &str)> = vec![(b"only", "txt")];
        let err = run_external(
            "copy \"{in2}\" \"{out}\"",
            &inputs,
            "txt",
            None,
            &no_cancel(),
        )
        .unwrap_err();
        assert!(err.contains("{in2}"), "{err}");
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
