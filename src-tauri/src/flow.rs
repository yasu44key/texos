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
    // --- v1.4.0 AIフィルタ（builtin:"ai"）。デュアルプロバイダ設計 ---
    /// "ollama"（既定・ネイティブ /api/chat）| "openai"（OpenAI準拠 /v1/chat/completions）
    #[serde(default)]
    pub provider: Option<String>,
    /// API ベースURL。空=provider既定（ollama: http://localhost:11434 / openai: http://localhost:1234/v1）
    #[serde(default)]
    pub endpoint: Option<String>,
    /// モデル名（例: qwen2.5:3b）
    #[serde(default)]
    pub model: Option<String>,
    /// システムプロンプト（空可）
    #[serde(default)]
    pub system: Option<String>,
    /// 生成温度（未指定はモデル既定にまかせる）
    #[serde(default)]
    pub temperature: Option<f64>,
    /// v1.4.0 md_codeblock: 抽出するコードブロックの言語（空=最初のブロック）。
    #[serde(default)]
    pub lang: Option<String>,
    /// v1.4.1 AI動作モード: "transform"（一発変換・既定）| "chat"（対話パネル）。
    #[serde(default)]
    pub mode: Option<String>,
    /// v1.4.5 チャットのMDログ保存名（空=「<ラベル>のログ.md」）。カード毎に変えれば
    /// 同名カードでもログが衝突しない。
    #[serde(default)]
    pub log_name: Option<String>,
    /// v1.4.9 録音モード（ジェネレータ）: true なら stdin をパイプで開いたまま起動し、
    /// 子プロセスへ制御文字列（ffmpeg の 'q' 等）を送れるようにする（汎用 stdin チャネル）。
    /// 録音は自分で終わらない（-t 不要）ので「停止」で 'q' を送って正常終了させ WAV を確定する。
    #[serde(default)]
    pub record: bool,
}

/// v1.4.10 リファクタリング（§4.2 ノード種別の体系化）: いままで eval_filter / run_flow に
/// 散在していた「このカードはどう実行するか」の判定（`filter_type=="generator"`・`builtin`
/// 有無・`builtin=="ai"`・`name`/`cmd` 有無）を1つの列挙に集約する。FilterSpec の各フィールドは
/// そのまま（serde 後方互換を維持）。判別だけをここへ寄せ、execute_node が一本道で分岐できるようにする。
///
/// 注意（挙動不変の要）: 対話シェル（`shell`）は終端カードのときだけ run_flow が特別扱いする
/// 概念で、eval_filter の連鎖評価では一切見ない。そのため NodeKind には Shell を含めない
/// （= execute_node 側の分類は従来の eval_filter と完全一致）。シェル判定は run_flow が
/// `FilterSpec::is_shell()` で行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// 入力を取らず1回実行してファイルを生む外部プログラム（`type:"generator"`）。
    /// `record` 指定なら stdin チャネルを開いたまま起動する（録音の停止制御）。
    Generator,
    /// builtin:"ai" — ネットワーク経由の LLM 変換（同期 blocking・別経路）。
    Ai,
    /// その他の内蔵フィルタ（text_grep / text_replace / img_* / md_codeblock …）。
    Builtin,
    /// 外部コマンド／プログラム（{in}/{out} 展開）。集約か個別かは is_aggregate() で。
    External,
    /// builtin も cmd も name も無い不正な定義。
    Invalid,
}

impl FilterSpec {
    /// このカードの実行種別を判定する。判定順序は現行 eval_filter の優先順位を保つ:
    /// generator（`type`）→ builtin（`ai` を含む）→ external（`name`/`cmd`）→ invalid。
    pub fn node_kind(&self) -> NodeKind {
        if self.filter_type.as_deref() == Some("generator") {
            return NodeKind::Generator;
        }
        if let Some(b) = self.builtin.as_deref() {
            return if b == "ai" { NodeKind::Ai } else { NodeKind::Builtin };
        }
        if self.name.is_some() || self.cmd.is_some() {
            return NodeKind::External;
        }
        NodeKind::Invalid
    }

    /// 終端カードを対話シェルとして開くか（run_flow 専用。連鎖評価では使わない）。
    pub fn is_shell(&self) -> bool {
        self.shell.is_some()
    }

    /// External が集約実行（全入力を集めて1回）か。{in*} か {in2} 以降があれば集約、
    /// {in}/{in1} のみなら入力ごとに実行（ファンアウト）。集約は External のときだけ意味を持つ。
    pub fn is_aggregate(&self) -> bool {
        self.node_kind() == NodeKind::External
            && cmd_is_aggregate(self.cmd.as_deref().unwrap_or(""))
    }
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
        "md_codeblock" => {
            // v1.4.0: Markdown のフェンス付きコードブロック(```lang ... ```)から中身を取り出す。
            // AI出力(.md)を gcc 等のコンパイラへ渡す前処理。lang 指定で言語を絞り込み、
            // 無指定なら最初のブロック。複数ブロックは最初の1つを採用（Phase1）。
            let text = String::from_utf8_lossy(input);
            let want = spec
                .lang
                .as_deref()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty());
            let mut in_block = false;
            let mut cur_lang = String::new();
            let mut cur: Vec<&str> = Vec::new();
            let mut found: Option<(String, String)> = None;
            for line in text.lines() {
                let t = line.trim_start();
                if !in_block {
                    if let Some(rest) = t.strip_prefix("```") {
                        in_block = true;
                        cur_lang = rest.trim().to_lowercase();
                        cur.clear();
                    }
                } else if t.starts_with("```") {
                    in_block = false;
                    let ok = want.as_ref().map(|w| &cur_lang == w).unwrap_or(true);
                    if ok && found.is_none() {
                        found = Some((cur_lang.clone(), cur.join("\n")));
                    }
                } else {
                    cur.push(line);
                }
            }
            let (lang, mut code) = found.ok_or_else(|| match &want {
                Some(w) => format!("`{w}` のコードブロックが見つかりません"),
                None => "コードブロック(```...```)が見つかりません".to_string(),
            })?;
            if !code.is_empty() && !code.ends_with('\n') {
                code.push('\n'); // 末尾改行（コンパイラ・POSIXツールが好む）
            }
            // 拡張子: output_ext 指定 > 言語からの推定 > 言語トークン > txt
            let ext = spec
                .output_ext
                .clone()
                .map(|e| e.trim().trim_start_matches('.').to_string())
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| lang_to_ext(&lang));
            Ok((code.into_bytes(), ext, "text/plain".into()))
        }
        other => Err(format!("未知の内蔵フィルタ: {other}")),
    }
}

/// md_codeblock: フェンスの言語トークンから出力拡張子を推定（未知はトークンをそのまま）。
fn lang_to_ext(lang: &str) -> String {
    match lang {
        "c" => "c",
        "cpp" | "c++" | "cxx" | "cc" => "cpp",
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "ts",
        "go" | "golang" => "go",
        "java" => "java",
        "kotlin" | "kt" => "kt",
        "ruby" | "rb" => "rb",
        "html" => "html",
        "css" => "css",
        "json" => "json",
        "xml" => "xml",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        "sh" | "bash" | "shell" | "zsh" => "sh",
        "powershell" | "ps1" => "ps1",
        "bat" | "batch" | "cmd" => "bat",
        "" => "txt",
        other => other,
    }
    .to_string()
}

/// AI応答の既定タイムアウト（生成は外部コマンドより長くかかるため別値）。
const AI_DEFAULT_TIMEOUT_SECS: u64 = 180;

/// v1.4.0 AIフィルタ（builtin:"ai"）: 入力テキストをユーザープロンプトとしてLLMに送り、
/// 応答 Markdown を返す。provider="ollama"（既定・ネイティブ /api/chat）または
/// "openai"（OpenAI準拠 /v1/chat/completions）。
/// Phase 1 は単発変換（履歴なし）。
///
/// 重要: reqwest::blocking は内部で tokio ランタイムを生成・破棄する。run_flow は
/// #[tauri::command(async)] = tokio ワーカースレッド上で走るため、その非同期文脈の中で
/// blocking クライアントの内部ランタイムを drop すると tokio が
/// 「Cannot drop a runtime in a context where blocking is not allowed」でパニックし、
/// 握っている DB Mutex を poison させてアプリ全体を巻き込む。そこで HTTP は
/// tokio 文脈を持たない素の OS スレッドに逃がして実行する（scope で借用は安全）。
pub fn run_ai(spec: &FilterSpec, input: &[u8]) -> Result<(Vec<u8>, String, String), String> {
    std::thread::scope(|s| {
        s.spawn(|| run_ai_blocking(spec, input))
            .join()
            .map_err(|_| "AI実行スレッドが異常終了しました".to_string())?
    })
}

fn run_ai_blocking(spec: &FilterSpec, input: &[u8]) -> Result<(Vec<u8>, String, String), String> {
    let provider = spec.provider.as_deref().unwrap_or("ollama");
    let model = spec
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("AIフィルタにモデルが指定されていません")?;
    let user = String::from_utf8_lossy(input).into_owned();
    if user.trim().is_empty() {
        return Err("入力テキストが空です".into());
    }
    let system = spec
        .system
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // タイムアウト: Some(0)=無制限 / 未指定=180秒。
    let timeout = match spec.timeout_secs {
        Some(0) => None,
        Some(s) => Some(std::time::Duration::from_secs(s)),
        None => Some(std::time::Duration::from_secs(AI_DEFAULT_TIMEOUT_SECS)),
    };
    let mut cb = reqwest::blocking::Client::builder();
    if let Some(t) = timeout {
        cb = cb.timeout(t);
    }
    let client = cb
        .build()
        .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))?;

    let mut messages = Vec::new();
    if let Some(s) = system {
        messages.push(serde_json::json!({ "role": "system", "content": s }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": user }));

    let content = if provider == "openai" {
        let endpoint = spec
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("http://localhost:1234/v1")
            .trim_end_matches('/');
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });
        if let Some(t) = spec.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        let resp = client
            .post(format!("{endpoint}/chat/completions"))
            .json(&body)
            .send()
            .map_err(|e| ai_conn_err(provider, endpoint, &e))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("AI応答の解析に失敗: {e}"))?;
        if !status.is_success() {
            let msg = v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .unwrap_or("不明なエラー");
            return Err(format!("AI APIエラー ({status}): {msg}"));
        }
        v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("AI応答に content がありません")?
            .to_string()
    } else {
        let endpoint = spec
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("http://localhost:11434")
            .trim_end_matches('/');
        let mut options = serde_json::Map::new();
        if let Some(t) = spec.temperature {
            options.insert("temperature".into(), serde_json::json!(t));
        }
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
            // モデルをVRAMに5分常駐 → 連続実行でロード待ちを避ける（効率化）。
            "keep_alive": "5m",
            "options": serde_json::Value::Object(options),
        });
        let resp = client
            .post(format!("{endpoint}/api/chat"))
            .json(&body)
            .send()
            .map_err(|e| ai_conn_err(provider, endpoint, &e))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("AI応答の解析に失敗: {e}"))?;
        if !status.is_success() {
            // Ollama はエラーを {"error":"..."} で返す
            let msg = v["error"].as_str().unwrap_or("不明なエラー");
            return Err(format!("AI APIエラー ({status}): {msg}"));
        }
        v["message"]["content"]
            .as_str()
            .ok_or("AI応答に content がありません")?
            .to_string()
    };

    Ok((content.into_bytes(), "md".into(), "text/markdown".into()))
}

/// v1.4.1 チャット: ストリーミング応答。messages は system を含む会話履歴全体。
/// on_token はトークン断片ごとに呼ばれる（UIへ逐次表示する）。完成した本文を返す。
/// 呼び出し側は tokio 文脈外の素のOSスレッドで実行すること（run_ai と同じ理由 —
/// reqwest::blocking の内部ランタイム drop パニックを避ける）。cancel で途中停止。
pub fn run_ai_chat_stream(
    spec: &FilterSpec,
    messages: &[serde_json::Value],
    cancel: &std::sync::atomic::AtomicBool,
    on_token: &mut dyn FnMut(&str),
) -> Result<String, String> {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    let provider = spec.provider.as_deref().unwrap_or("ollama");
    let model = spec
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("AIフィルタにモデルが指定されていません")?;
    // ストリーミングは長時間。timeout 指定があれば全体上限、無指定/0 は無制限（cancelで停止）。
    let client = {
        let mut cb = reqwest::blocking::Client::builder();
        if let Some(s) = spec.timeout_secs {
            if s > 0 {
                cb = cb.timeout(std::time::Duration::from_secs(s));
            }
        }
        cb.build()
            .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))?
    };

    let mut full = String::new();
    if provider == "openai" {
        // OpenAI準拠: SSE（"data: {...}" 行、終端は [DONE]）
        let endpoint = spec
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("http://localhost:1234/v1")
            .trim_end_matches('/');
        let mut body = serde_json::json!({
            "model": model, "messages": messages, "stream": true,
        });
        if let Some(t) = spec.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        let resp = client
            .post(format!("{endpoint}/chat/completions"))
            .json(&body)
            .send()
            .map_err(|e| ai_conn_err(provider, endpoint, &e))?;
        if !resp.status().is_success() {
            return Err(format!("AI APIエラー ({})", resp.status()));
        }
        for line in std::io::BufReader::new(resp).lines() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let line = line.map_err(|e| format!("ストリーム読み取り失敗: {e}"))?;
            let data = match line.trim().strip_prefix("data:") {
                Some(d) => d.trim(),
                None => continue,
            };
            if data == "[DONE]" {
                break;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(tok) = v["choices"][0]["delta"]["content"].as_str() {
                    full.push_str(tok);
                    on_token(tok);
                }
            }
        }
    } else {
        // Ollama ネイティブ: NDJSON（1行=1JSON、done:true で終端）
        let endpoint = spec
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("http://localhost:11434")
            .trim_end_matches('/');
        let mut options = serde_json::Map::new();
        if let Some(t) = spec.temperature {
            options.insert("temperature".into(), serde_json::json!(t));
        }
        let body = serde_json::json!({
            "model": model, "messages": messages, "stream": true,
            "keep_alive": "5m", "options": serde_json::Value::Object(options),
        });
        let resp = client
            .post(format!("{endpoint}/api/chat"))
            .json(&body)
            .send()
            .map_err(|e| ai_conn_err(provider, endpoint, &e))?;
        if !resp.status().is_success() {
            return Err(format!("AI APIエラー ({})", resp.status()));
        }
        for line in std::io::BufReader::new(resp).lines() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let line = line.map_err(|e| format!("ストリーム読み取り失敗: {e}"))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(err) = v["error"].as_str() {
                    return Err(format!("AI APIエラー: {err}"));
                }
                if let Some(tok) = v["message"]["content"].as_str() {
                    if !tok.is_empty() {
                        full.push_str(tok);
                        on_token(tok);
                    }
                }
                if v["done"].as_bool().unwrap_or(false) {
                    break;
                }
            }
        }
    }
    Ok(full)
}

/// v1.4.2 設定ダイアログのモデル一覧。Ollama=/api/tags（name）、OpenAI準拠=/v1/models（id）。
/// run_ai と同じく tokio 文脈外スレッドへ逃がして blocking 実行する。
pub fn list_models(provider: &str, endpoint: &str) -> Result<Vec<String>, String> {
    std::thread::scope(|s| {
        s.spawn(|| list_models_blocking(provider, endpoint))
            .join()
            .map_err(|_| "モデル取得スレッドが異常終了しました".to_string())?
    })
}

fn list_models_blocking(provider: &str, endpoint: &str) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))?;
    let ep = endpoint.trim().trim_end_matches('/');
    let (url, is_openai) = if provider == "openai" {
        let base = if ep.is_empty() { "http://localhost:1234/v1" } else { ep };
        (format!("{base}/models"), true)
    } else {
        let base = if ep.is_empty() { "http://localhost:11434" } else { ep };
        (format!("{base}/api/tags"), false)
    };
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| ai_conn_err(provider, &url, &e))?;
    if !resp.status().is_success() {
        return Err(format!("モデル一覧の取得に失敗 ({})", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("一覧の解析に失敗: {e}"))?;
    let mut out: Vec<String> = if is_openai {
        v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    out.sort();
    out.dedup();
    Ok(out)
}

/// AI接続失敗を日本語で分かりやすく（Ollama未起動が最頻なので最優先で案内）。
fn ai_conn_err(provider: &str, endpoint: &str, e: &reqwest::Error) -> String {
    if e.is_connect() {
        format!("{provider} に接続できません（{endpoint}）。Ollama が起動しているか確認してください")
    } else if e.is_timeout() {
        format!("AI応答がタイムアウトしました（{endpoint}）。制限時間を延ばすか軽いモデルを試してください")
    } else {
        format!("AIリクエストに失敗（{endpoint}）: {e}")
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

/// v1.4.10 リファクタリング（§4.1）: 外部プロセスを最後まで走らせた結果を「捨てずに」返す型。
/// 従来 run_external_cwd は exit code を握りつぶし、stderr も失敗時しか使っていなかった。
/// 将来の exit code 分岐（§4.3 成功時/失敗時の矢印）を載せられるよう、完走したプロセスの
/// 終了コード・stderr 抜粋・出力バイト列をそのまま持ち上げる。成否ポリシー（非0→エラー /
/// 出力なし→エラー）は into_bytes() に閉じ込め、従来契約（run_external_cwd）はその薄い適用。
#[derive(Debug, Clone)]
pub struct ExternalOutput {
    /// プロセス終了コード（Unix でシグナル終了した場合は None）。
    pub exit_code: Option<i32>,
    /// stderr の先頭 ~500 字（trim 済み。無ければ空文字）。
    pub stderr: String,
    /// {out} を読み出せた場合のみ Some（プログラムが出力を書かなければ None）。
    pub output: Option<Vec<u8>>,
}

impl ExternalOutput {
    /// 終了コード 0 を成功とみなす（std の ExitStatus::success と等価）。
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// 従来の run_external 契約（成功→出力バイト列 / 失敗→日本語エラー）へ落とす。
    /// エラーメッセージは旧 run_external_cwd と同一文面を保つ（回帰の安全網）。
    pub fn into_bytes(self) -> Result<Vec<u8>, String> {
        if !self.success() {
            let tail = if self.stderr.is_empty() {
                String::new()
            } else {
                format!("\n{}", self.stderr)
            };
            return Err(format!(
                "コマンドが失敗しました (exit {:?}){}",
                self.exit_code, tail
            ));
        }
        self.output
            .ok_or_else(|| "コマンドは成功しましたが {out} に出力がありません".to_string())
    }
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
    run_external_cwd(cmdline, inputs, out_ext, timeout_secs, cancel, None, None)
}

/// v1.4.9: 子プロセスへ制御を送る汎用 stdin チャネル。`Some(slot)` を渡すと stdin を
/// パイプで開いたまま起動し、その書き込み端を `slot` に置く（別コマンド flow_send_stdin が
/// 'q' 等を書ける）。`None` なら従来どおり stdin を閉じる（標準入力待ちのストール防止）。
pub type StdinSlot = std::sync::Mutex<Option<std::process::ChildStdin>>;

/// v1.4.2: 作業ディレクトリ(cwd)を指定できる版。cwd を渡すとコマンドはそこを
/// 「ここ」として実行する → 相対名や既定名の副産物（gcc の a.exe 等）が cwd に
/// 落ちる。lib.rs はプロジェクトルートを渡し、副産物がプロジェクト内に収まるようにする。
/// {in}/{out} は絶対パスなので正規の入出力には影響しない。
pub fn run_external_cwd(
    cmdline: &str,
    inputs: &[(&[u8], &str)],
    out_ext: &str,
    timeout_secs: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
    cwd: Option<&std::path::Path>,
    stdin_slot: Option<&StdinSlot>,
) -> Result<Vec<u8>, String> {
    run_external_full(cmdline, inputs, out_ext, timeout_secs, cancel, cwd, stdin_slot)?.into_bytes()
}

/// v1.4.10（§4.1）: run_external_cwd の中身。完走したプロセスの exit code・stderr・出力を
/// ExternalOutput としてそのまま返す（成否ポリシーを適用しない）。Err になるのは起動失敗・
/// タイムアウト・中断・出力上限超過・監視失敗といった「そもそも完走しなかった」場合のみ。
/// 従来契約が要るところは run_external_cwd（= 本関数 + into_bytes）を使う。
pub fn run_external_full(
    cmdline: &str,
    inputs: &[(&[u8], &str)],
    out_ext: &str,
    timeout_secs: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
    cwd: Option<&std::path::Path>,
    stdin_slot: Option<&StdinSlot>,
) -> Result<ExternalOutput, String> {
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
    // {out} → 出力ファイル（拡張子つき）/ {out_noext} → 拡張子を除いたパス
    // （whisper.cpp の `-of <ベース名>` のように、ツール側が拡張子を自分で付ける場合に使う。
    //  output_ext を txt にしておけば whisper が書く <base>.txt が {out} と一致する）。
    // {out_noext} を先に置換する（{out} は "{out_noext}" の部分文字列にならないが順序を明示）。
    let out_noext = out_path.with_extension("");
    let mut cmd = cmdline.replace("{out_noext}", &out_noext.to_string_lossy());
    cmd = cmd.replace("{out}", &out_path.to_string_lossy());
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

    // v1.4.9: 録音など制御を送りたいときは stdin をパイプで開く。それ以外は閉じる。
    let stdin_cfg = || {
        if stdin_slot.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        }
    };
    #[cfg(windows)]
    let spawned = {
        use std::os::windows::process::CommandExt;
        // コマンドライン全体を raw_arg で「素のまま」cmd に渡す。
        // std の自動クォート（\" エスケープ）は cmd.exe の解釈と非互換で、
        // 引用符を含むコマンドが exit 1 で壊れる（v1.4 実機指摘）。
        // さらに全体を "..." で包む — cmd /C は先頭が引用符のとき最初と最後の
        // 引用符を剥がすため、内側のクォートを無傷で残す定石。
        let mut c = std::process::Command::new("cmd");
        c.raw_arg(format!("/C \"{cmd}\""))
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW（コンソールを出さない）
            .stdin(stdin_cfg())
            .stdout(std::process::Stdio::null())
            .stderr(err_file);
        if let Some(dir) = cwd {
            c.current_dir(dir); // 相対名/既定名の副産物をプロジェクト内に落とす
        }
        c.spawn()
    };
    #[cfg(not(windows))]
    let spawned = {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", &cmd])
            .stdin(stdin_cfg())
            .stdout(std::process::Stdio::null())
            .stderr(err_file);
        if let Some(dir) = cwd {
            c.current_dir(dir);
        }
        c.spawn()
    };

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
            // v1.4.9: stdin の書き込み端を共有スロットへ（別コマンドが 'q' 等を送れる）。
            // 取り出しは一度だけ（child.stdin は2回目以降 None）。
            if let Some(slot) = stdin_slot {
                if let Some(si) = child.stdin.take() {
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(si);
                    }
                }
            }
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

    // v1.4.9: プロセス終了後は stdin スロットを空に（以後の送信は「実行中なし」になる）。
    if let Some(slot) = stdin_slot {
        if let Ok(mut g) = slot.lock() {
            *g = None;
        }
    }

    // stderr の先頭だけ拾う（長大ログは切る）。先頭 ~500 字・trim 済み、無ければ空。
    let stderr_snip = || -> String {
        match std::fs::read(&err_path) {
            Ok(b) if !b.is_empty() => {
                let s = String::from_utf8_lossy(&b);
                s.trim().chars().take(500).collect()
            }
            _ => String::new(),
        }
    };
    // 完走したプロセスは exit code・stderr・出力をそのまま持ち上げる（成否判定はしない）。
    // Err は「完走しなかった」場合のみ（起動失敗・タイムアウト・中断・出力上限・監視失敗）。
    let outcome = match status {
        Err(e) => Err(e),
        Ok(s) => Ok(ExternalOutput {
            exit_code: s.code(),
            stderr: stderr_snip(),
            output: std::fs::read(&out_path).ok(),
        }),
    };
    cleanup_inputs(&in_paths);
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> FilterSpec {
        parse_spec(json).unwrap()
    }

    // v1.4.0 AIフィルタの実機検証。ライブの Ollama(localhost:11434) と qwen2.5:3b が要るので
    // 既定では無視。実行: `cargo test --lib -- --ignored ai_live`
    #[test]
    #[ignore = "ライブOllamaが必要"]
    fn ai_live_ollama_roundtrip() {
        let s = spec(
            r#"{"builtin":"ai","provider":"ollama","model":"qwen2.5:3b","system":"地名だけを一語で答えよ。"}"#,
        );
        let (out, ext, mime) = run_ai(&s, "日本の首都は？".as_bytes()).unwrap();
        let text = String::from_utf8(out).unwrap();
        // run_ai の契約は「非空のMDを返す」こと。回答内容の正確さはモデル依存なので問わない。
        assert_eq!(ext, "md");
        assert_eq!(mime, "text/markdown");
        assert!(!text.trim().is_empty(), "空の応答が返った");
        eprintln!("AI応答: {text}");
    }

    // 回帰テスト: 本番(Tauri #[command(async)])と同じく tokio ランタイム文脈の中から
    // 同期 run_ai を呼ぶ。修正前はここで reqwest::blocking の内部ランタイム drop が
    // 「Cannot drop a runtime in a context where blocking is not allowed」でパニックして
    // アプリごと落ちた。thread::scope で素のOSスレッドに逃がす修正が効いていれば通る。
    #[test]
    #[ignore = "ライブOllamaが必要"]
    fn ai_inside_tokio_runtime_does_not_panic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(async {
            let s = spec(
                r#"{"builtin":"ai","provider":"ollama","model":"qwen2.5:3b","system":"一語で答えよ。"}"#,
            );
            run_ai(&s, "日本の首都は？".as_bytes())
        });
        let (out, ext, mime) = res.expect("tokio文脈内でrun_aiが失敗");
        assert_eq!(ext, "md");
        assert_eq!(mime, "text/markdown");
        assert!(!out.is_empty());
    }

    #[test]
    #[ignore = "ライブOllamaが必要"]
    fn ai_chat_stream_live() {
        use std::sync::atomic::AtomicBool;
        let s = spec(r#"{"builtin":"ai","provider":"ollama","model":"qwen2.5:3b"}"#);
        let msgs = vec![serde_json::json!({"role":"user","content":"1から5まで数えてください。"})];
        let cancel = AtomicBool::new(false);
        let mut tokens = 0usize;
        let mut acc = String::new();
        let full = run_ai_chat_stream(&s, &msgs, &cancel, &mut |t| {
            tokens += 1;
            acc.push_str(t);
        })
        .unwrap();
        eprintln!("tokens={tokens} full={full}");
        assert!(tokens > 1, "ストリームが断片で届いていない (tokens={tokens})");
        assert_eq!(full, acc, "返却本文と逐次累積が一致");
        assert!(!full.trim().is_empty());
    }

    #[test]
    fn md_codeblock_extracts_fenced_code() {
        let md = "説明文\n```c\n#include <stdio.h>\nint main(){return 0;}\n```\n末尾の解説。";
        let s = spec(r#"{"builtin":"md_codeblock","lang":"c"}"#);
        let (out, ext, mime) = run_builtin(&s, "md_codeblock", md.as_bytes()).unwrap();
        let code = String::from_utf8(out).unwrap();
        assert_eq!(ext, "c");
        assert_eq!(mime, "text/plain");
        assert!(code.starts_with("#include <stdio.h>"));
        assert!(!code.contains("```"), "フェンスが残っている");
        assert!(!code.contains("説明文"), "ブロック外の文が混入");
        assert!(code.ends_with('\n'), "末尾改行が無い");
    }

    #[test]
    fn md_codeblock_first_block_when_no_lang() {
        let md = "```py\nprint(1)\n```\n```js\nconsole.log(2)\n```";
        let s = spec(r#"{"builtin":"md_codeblock"}"#);
        let (out, ext, _) = run_builtin(&s, "md_codeblock", md.as_bytes()).unwrap();
        assert_eq!(ext, "py"); // 言語トークン py → 拡張子 py
        assert_eq!(String::from_utf8(out).unwrap(), "print(1)\n");
    }

    #[test]
    fn md_codeblock_missing_block_is_error() {
        let s = spec(r#"{"builtin":"md_codeblock","lang":"rust"}"#);
        assert!(run_builtin(&s, "md_codeblock", b"plain text only").is_err());
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

    /// {out_noext} が拡張子を除いたパスに展開され、ツールが付ける `.txt` が {out} と
    /// 一致して読み戻せること（whisper.cpp の `-of <ベース名>` 想定の回帰テスト）。
    #[cfg(windows)]
    #[test]
    fn out_noext_placeholder_strips_extension() {
        let out = run_external(
            "copy \"{in}\" \"{out_noext}.txt\"",
            &one(b"hello"),
            "txt",
            None,
            &no_cancel(),
        )
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

    /// §4.2 ノード種別判定が現行 eval_filter の分類と一致すること。
    #[test]
    fn node_kind_classification() {
        assert_eq!(spec(r#"{"type":"generator","name":"rec.exe"}"#).node_kind(), NodeKind::Generator);
        assert_eq!(spec(r#"{"builtin":"ai","model":"x"}"#).node_kind(), NodeKind::Ai);
        assert_eq!(spec(r#"{"builtin":"text_grep"}"#).node_kind(), NodeKind::Builtin);
        assert_eq!(spec(r#"{"name":"sp2tab.exe"}"#).node_kind(), NodeKind::External);
        assert_eq!(spec(r#"{"cmd":"sort < {in} > {out}"}"#).node_kind(), NodeKind::External);
        assert_eq!(spec(r#"{}"#).node_kind(), NodeKind::Invalid);
        // generator は builtin/name より優先（eval_filter の早期 return と同順）。
        assert_eq!(
            spec(r#"{"type":"generator","builtin":"text_grep","name":"x"}"#).node_kind(),
            NodeKind::Generator
        );
    }

    /// 集約判定は External のときだけ true（builtin に {in*} があっても集約ではない）。
    #[test]
    fn is_aggregate_only_for_external() {
        assert!(spec(r#"{"cmd":"{in*} -o {out}"}"#).is_aggregate());
        assert!(spec(r#"{"name":"convert","cmd":"\"{in1}\" \"{in2}\" {out}"}"#).is_aggregate());
        assert!(!spec(r#"{"cmd":"\"{in}\" {out}"}"#).is_aggregate());
        assert!(!spec(r#"{"builtin":"text_grep","cmd":"{in*}"}"#).is_aggregate());
    }

    /// shell 判定（run_flow 終端の対話シェル）。
    #[test]
    fn is_shell_detection() {
        assert!(spec(r#"{"shell":"cmd"}"#).is_shell());
        assert!(!spec(r#"{"cmd":"sort"}"#).is_shell());
    }

    /// §4.1 run_external_full が非0終了コードを「捨てずに」持ち上げること。
    #[cfg(windows)]
    #[test]
    fn external_full_captures_nonzero_exit_code() {
        let o = run_external_full("exit 3", &one(b""), "txt", Some(10), &no_cancel(), None, None)
            .unwrap();
        assert_eq!(o.exit_code, Some(3));
        assert!(!o.success());
        // 出力が無いので従来契約（into_bytes）は exit を含むエラーへ落ちる。
        assert!(o.into_bytes().unwrap_err().contains("exit"));
    }

    /// 成功時は exit 0 と出力バイト列をそのまま持つこと。
    #[cfg(windows)]
    #[test]
    fn external_full_captures_success_and_output() {
        let o = run_external_full(
            "copy \"{in}\" \"{out}\"",
            &one(b"hi"),
            "txt",
            None,
            &no_cancel(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(o.exit_code, Some(0));
        assert!(o.success());
        assert_eq!(o.output.as_deref(), Some(&b"hi"[..]));
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
