// ============================================================================
// v1.8.9 AIカード: モデル能力の検出・キャッシュ・送信判定
// ----------------------------------------------------------------------------
// 設計メモ: docs/ai-card-model-capabilities-spec.md
//
// ローカルにあるモデルを「機能の有無」（tools / thinking / vision / embedding）で
// 見分け、カードの設定と実行に使う。精度・品質は扱わない（capabilities から
// 分からない）。
//
//   取得元        Ollama   … GET /api/tags（name・digest）＋ POST /api/show（capabilities 等）
//                 LM Studio … GET /api/v1/models（1回で能力・学習上限・思考の選択肢まで揃う）
//                 OpenAI互換（汎用）… 取得 API が無い → 全項目 unknown
//   キャッシュ    <app_data>/ai_model_cache.json（マシン単位。プロジェクトは持ち運ぶので
//                 載せない — 持ち運んだ先にモデルが無いのは許容）。キーは
//                 provider + endpoint + name、値に digest。digest が同じなら /api/show を
//                 呼び直さない。Ollama のバージョンが変わったら全件取り直す
//                 （新しい Ollama で capabilities が取れるようになる場合があるため）。
//   更新時機      設定ダイアログを開いたとき・「↻取得」、および実行時の1モデル確認
//                 （/api/tags を1回引いて digest を確かめ、変わっていたときだけ /api/show）。
//                 実行ログの digest を「そのとき実際に使った中身」にするため。
//
// 仕様書からの変更点（理由は各所のコメント）:
//   - ModelCapabilities に completion を足した（埋め込み専用モデルを「対話不可」と出すため）
//   - ThinkingMode に boolean_or_levels を足した（Ollama 0.34 は思考対応モデルなら
//     真偽値もレベル文字列も受ける — 実機確認済み。仕様書の「gpt-oss だけレベル」は古い）
//   - キャッシュのキーに endpoint を足した（ローカルと遠隔の Ollama を併用したとき、
//     「一覧に無いモデルを消す」処理が他方のキャッシュを消さないように）
// ============================================================================
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 能力の有無。「無し（no）」と「不明（unknown）」を区別する — unknown を no 扱いにして
/// 選択肢から消すと、古い Ollama では何も選べなくなる。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

impl Tri {
    fn of(list: Option<&[String]>, key: &str) -> Tri {
        match list {
            None => Tri::Unknown,
            Some(l) if l.iter().any(|c| c == key) => Tri::Yes,
            Some(_) => Tri::No,
        }
    }
    fn from_bool(b: bool) -> Tri {
        if b { Tri::Yes } else { Tri::No }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ModelCapabilities {
    /// 対話（文章生成）ができるか。埋め込み専用モデルは no。
    pub completion: Tri,
    pub tools: Tri,
    pub thinking: Tri,
    pub vision: Tri,
    pub embedding: Tri,
}

impl ModelCapabilities {
    pub fn unknown() -> Self {
        ModelCapabilities {
            completion: Tri::Unknown,
            tools: Tri::Unknown,
            thinking: Tri::Unknown,
            vision: Tri::Unknown,
            embedding: Tri::Unknown,
        }
    }
}

/// 思考（think / reasoning）に何を送れるか。
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThinkingMode {
    /// 思考しない（送ると Ollama は 400 を返す）。
    None,
    /// 真偽値だけ（on / off）。
    Boolean,
    /// レベルだけ（gpt-oss 系: 思考を止められず、真偽値も選択肢に無い）。
    Levels { levels: Vec<String> },
    /// 真偽値もレベルも受ける（Ollama 0.34 の思考対応モデル一般）。
    BooleanOrLevels { levels: Vec<String> },
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelInfo {
    /// "ollama" | "lmstudio" | "openai"
    pub provider: String,
    /// 正規化済みのエンドポイント（末尾 / なし）。
    pub endpoint: String,
    pub name: String,
    /// 中身の識別子。Ollama は /api/tags の digest。LM Studio は digest を返さないので
    /// `<selected_variant>#<size_bytes>` で代用する。汎用 OpenAI互換は空。
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    /// 学習上限のコンテキスト長。実行時の num_ctx とは別物（Ollama の既定 num_ctx は
    /// これよりずっと小さい）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trained_context_length: Option<u64>,
    pub capabilities: ModelCapabilities,
    /// サーバが返した能力の生の並び（audio / insert など判定表に無いものも残す）。
    /// None＝サーバが能力を返さなかった（→ 全項目 unknown）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_capabilities: Option<Vec<String>>,
    pub thinking_mode: ThinkingMode,
    /// 設定ダイアログの「思考」に出してよい値（off/on/low/medium/high の部分集合）。
    /// UI 側で判定表を二重に持たないよう、ここで決めて渡す。
    pub think_choices: Vec<String>,
    /// ISO 8601
    pub fetched_at: String,
}

// ---------------------------------------------------------------------------
// 判定表（モデル系統ごとの例外はここにだけ書く）
// ---------------------------------------------------------------------------

const LEVELS: [&str; 3] = ["low", "medium", "high"];

fn levels_vec() -> Vec<String> {
    LEVELS.iter().map(|s| s.to_string()).collect()
}

/// gpt-oss 系か。名前と architecture の両方で見る（別名で取り込まれた gpt-oss にも効くように）。
fn is_gpt_oss(name: &str, arch: Option<&str>) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("gpt-oss")
        || n.contains("gpt_oss")
        || matches!(arch.map(|a| a.to_ascii_lowercase()).as_deref(), Some("gptoss" | "gpt-oss"))
}

/// Ollama の thinking 判定。
/// - thinking=no → none
/// - gpt-oss 系 → レベルのみ（思考を止められない。LM Studio の allowed_options は
///   low/medium/high のみ。Ollama 0.34.4 + gpt-oss-safeguard:20b 実機: think=false でも
///   思考し、true は medium 相当、レベルは効く＝思考量 low 23 / medium 137 / high 239 字）
/// - それ以外の thinking=yes → 真偽値もレベルも可（Ollama 0.34.4 で gemma4 / qwen35 系に
///   "low"/"high" を送って通ることを確認）
/// - unknown → unknown
pub(crate) fn ollama_thinking_mode(thinking: Tri, name: &str, arch: Option<&str>) -> ThinkingMode {
    match thinking {
        Tri::No => ThinkingMode::None,
        Tri::Unknown => ThinkingMode::Unknown,
        Tri::Yes if is_gpt_oss(name, arch) => ThinkingMode::Levels { levels: levels_vec() },
        Tri::Yes => ThinkingMode::BooleanOrLevels { levels: levels_vec() },
    }
}

/// LM Studio の reasoning.allowed_options から判定。キーが無い＝思考しないモデル。
pub(crate) fn lmstudio_thinking_mode(allowed: Option<&[String]>) -> ThinkingMode {
    let Some(opts) = allowed else { return ThinkingMode::None };
    let levels: Vec<String> = LEVELS
        .iter()
        .filter(|l| opts.iter().any(|o| o == *l))
        .map(|s| s.to_string())
        .collect();
    let has_bool = opts.iter().any(|o| o == "on" || o == "off");
    match (levels.is_empty(), has_bool) {
        (true, true) => ThinkingMode::Boolean,
        (false, true) => ThinkingMode::BooleanOrLevels { levels },
        (false, false) => ThinkingMode::Levels { levels },
        (true, false) => ThinkingMode::Unknown, // xhigh だけ等、TexoS の選択肢に写せない
    }
}

/// 設定ダイアログに出す思考の選択肢。
/// レベル専用（gpt-oss 系）の "off" は「使わない」ではなく「指定しない＝サーバ既定の深さ」
/// の意味（止められないので）— 表示名は UI 側で差し替える。"on"（使う）は出さない。
pub(crate) fn think_choices(mode: &ThinkingMode) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    match mode {
        ThinkingMode::None => v.push("off".into()),
        ThinkingMode::Boolean => v.extend(["off".into(), "on".into()]),
        ThinkingMode::Levels { levels } => {
            v.push("off".into());
            v.extend(levels.iter().cloned());
        }
        ThinkingMode::BooleanOrLevels { levels } => {
            v.extend(["off".into(), "on".into()]);
            v.extend(levels.iter().cloned());
        }
        ThinkingMode::Unknown => {
            v.extend(["off", "on", "low", "medium", "high"].iter().map(|s| s.to_string()))
        }
    }
    v
}

/// 実際に送る思考パラメータの決定結果。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ThinkPlan {
    /// Ollama /api/chat の think（true / "low" 等）。None＝キーを送らない。
    pub ollama_think: Option<serde_json::Value>,
    /// OpenAI互換（LM Studio・汎用は送る設定のとき）の reasoning_effort。None＝送らない。
    pub reasoning_effort: Option<String>,
    /// 設定値どおりに送らなかった理由（実行ログに残す）。
    pub note: Option<String>,
}

/// カード設定（spec.think）と能力から、送る値を1か所で決める。
///
/// 原則（仕様 4.5・決定 (b)）: 利用者が選んだ値を送る。ただし能力が no のときは送らない
/// （Ollama は 400 を返す）。unknown のときは設定どおり送り、非対応エラーが返っても
/// 自動リトライしない（リトライで条件が変わると、ログ上の条件と実際が食い違うため）。
///
/// `send_generic`（v1.9.2・カード設定 send_reasoning）: 汎用 OpenAI互換へ reasoning_effort を
/// 送るか。汎用サーバは能力を返さず、未知の値で 400 を返すものもあるので、既定は送らない。
pub(crate) fn plan_think(
    provider: &str,
    requested: Option<&str>,
    info: Option<&ModelInfo>,
    send_generic: bool,
) -> ThinkPlan {
    #[derive(PartialEq)]
    enum Req {
        Off,
        On,
        Level(String),
    }
    let req = match requested.map(str::trim).unwrap_or("") {
        "on" | "true" => Req::On,
        l @ ("low" | "medium" | "high") => Req::Level(l.to_string()),
        _ => Req::Off, // "" / off / false / 未知の値（従来どおり送らない）
    };
    let thinking = info.map(|i| i.capabilities.thinking).unwrap_or(Tri::Unknown);
    let mode = info.map(|i| i.thinking_mode.clone()).unwrap_or(ThinkingMode::Unknown);
    let only_levels = matches!(mode, ThinkingMode::Levels { .. });
    let mut plan = ThinkPlan::default();

    // v1.9.2 汎用 OpenAI互換: 送る設定のときだけ。値は OpenAI 仕様の範囲に限る
    // （"none" は gpt-5.1 以降・Strata 等が解する。"使う"＝サーバ既定に任せて送らない）。
    if provider == "openai" {
        if !send_generic {
            if req != Req::Off {
                plan.note = Some("OpenAI互換（汎用）サーバには思考の指定を送りません（「思考の指定を送る」がオフ）".into());
            }
            return plan;
        }
        match req {
            Req::Off => plan.reasoning_effort = Some("none".into()),
            Req::On => plan.note = Some("「使う」は深さをサーバ既定に任せます（reasoning_effort は送りません）".into()),
            Req::Level(l) => plan.reasoning_effort = Some(l),
        }
        return plan;
    }

    if req == Req::Off {
        if only_levels {
            plan.note = Some("このモデル（gpt-oss 系）は思考を止められません。サーバ既定の深さで思考します".into());
        }
        return plan;
    }
    if thinking == Tri::No {
        plan.note = Some("モデルが思考に非対応（capabilities）のため、思考の指定を送りませんでした".into());
        return plan;
    }

    if crate::flow::is_openai_compat(provider) {
        // ここに来るのは LM Studio（汎用 openai は上で処理済み）。
        let accepts = |l: &str| match &mode {
            ThinkingMode::Levels { levels } | ThinkingMode::BooleanOrLevels { levels } => {
                levels.iter().any(|x| x == l)
            }
            ThinkingMode::Unknown => true,
            _ => false,
        };
        match req {
            Req::Level(l) if accepts(&l) => plan.reasoning_effort = Some(l),
            Req::Level(_) => {
                plan.note = Some("このモデルは深さの指定に非対応です（思考の有無は LM Studio 側の設定に従います）".into())
            }
            Req::On if only_levels => {
                plan.reasoning_effort = Some("medium".into());
                plan.note = Some("このモデルは深さで指定するため「使う」を medium として送りました".into());
            }
            _ => plan.note = Some("思考の使う／使わないは LM Studio 側の設定に従います（API からは送りません）".into()),
        }
        return plan;
    }

    // Ollama ネイティブ
    match req {
        Req::On if only_levels => {
            plan.ollama_think = Some(serde_json::json!("medium"));
            plan.note = Some("このモデル（gpt-oss 系）は深さで指定するため「使う」を medium として送りました".into());
        }
        Req::On => plan.ollama_think = Some(serde_json::json!(true)),
        Req::Level(_) if mode == ThinkingMode::Boolean => {
            plan.ollama_think = Some(serde_json::json!(true));
            plan.note = Some("このモデルは深さの指定に非対応のため「使う」として送りました".into());
        }
        Req::Level(l) => plan.ollama_think = Some(serde_json::json!(l)),
        Req::Off => unreachable!(),
    }
    plan
}

// ---------------------------------------------------------------------------
// 実行ログ（トレース・会話履歴）に残す実行条件
// ---------------------------------------------------------------------------

/// 実際に送ったパラメータ。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SentParams {
    /// tools を付けたか（AIカードにツール使用の機能はまだ無いので常に false）。
    pub tools: bool,
    /// Ollama の think に送った値（None＝送っていない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<serde_json::Value>,
    /// LM Studio の reasoning_effort（None＝送っていない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_gpu: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// v1.9.2 追加パラメータ（本体トップレベルへマージしたもの）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Map<String, serde_json::Value>>,
}

/// AI 実行1回分の再現条件（仕様 4.6）。同じ名前でも `ollama pull` で中身が入れ替わるので
/// 名前だけでなく digest と能力のスナップショットまで残す。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AiRunRecord {
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    /// 実行時点の digest（取得できなければ None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// 実行時点の能力（取得できなければ None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_mode: Option<ThinkingMode>,
    /// カードに設定されていた思考の値（spec.think）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_setting: Option<String>,
    pub sent: SentParams,
    /// 設定どおりに送らなかった理由・能力を取れなかった理由など。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// 応答に思考（thinking / reasoning / <think>）が含まれていたか。
    pub thinking_returned: bool,
}

impl AiRunRecord {
    /// トレース（Markdown）に出す行。
    pub(crate) fn lines(&self) -> Vec<(String, String)> {
        let tri = |t: Tri| match t {
            Tri::Yes => "あり",
            Tri::No => "なし",
            Tri::Unknown => "不明",
        };
        let mut v = vec![(
            "モデル".to_string(),
            format!("{} ／ {}（{}）", self.model, self.provider, self.endpoint),
        )];
        v.push(("digest".into(), self.digest.clone().unwrap_or_else(|| "不明".into())));
        if let Some(c) = &self.capabilities {
            v.push((
                "能力（機能の有無）".into(),
                format!(
                    "対話 {}・ツール {}・思考 {}・画像入力 {}・埋め込み {}",
                    tri(c.completion),
                    tri(c.tools),
                    tri(c.thinking),
                    tri(c.vision),
                    tri(c.embedding)
                ),
            ));
        } else {
            v.push(("能力（機能の有無）".into(), "取得できず（全項目 不明）".into()));
        }
        let mut sent = vec![format!("tools {}", if self.sent.tools { "あり" } else { "なし" })];
        sent.push(match (&self.sent.think, &self.sent.reasoning_effort) {
            (Some(t), _) => format!("think {t}"),
            (None, Some(r)) => format!("reasoning_effort \"{r}\""),
            (None, None) => "思考指定なし".into(),
        });
        if let Some(n) = self.sent.num_ctx {
            sent.push(format!("num_ctx {n}"));
        }
        if let Some(n) = self.sent.num_gpu {
            sent.push(format!("num_gpu {n}"));
        }
        if let Some(t) = self.sent.temperature {
            sent.push(format!("temperature {t}"));
        }
        if let Some(m) = &self.sent.extra {
            sent.push(format!("追加 {}", serde_json::Value::Object(m.clone())));
        }
        v.push(("送信パラメータ".into(), sent.join("・")));
        v.push((
            "応答の思考".into(),
            if self.thinking_returned { "含まれていた（保存はしない）" } else { "なし" }.into(),
        ));
        for n in &self.notes {
            v.push(("注記".into(), n.clone()));
        }
        v
    }
}

// ---------------------------------------------------------------------------
// キャッシュ
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct ServerCache {
    /// Ollama の /api/version。変わったら全件取り直す。
    #[serde(default)]
    server_version: Option<String>,
    #[serde(default)]
    models: HashMap<String, ModelInfo>,
}

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    /// キー: "<provider>|<endpoint>"
    #[serde(default)]
    servers: HashMap<String, ServerCache>,
}

static CACHE_PATH: OnceLock<PathBuf> = OnceLock::new();
static CACHE: Mutex<Option<CacheFile>> = Mutex::new(None);

/// 起動時に app_data を教える（未設定＝テスト等ではメモリのみ）。
pub(crate) fn init_cache(app_data: &Path) {
    let _ = CACHE_PATH.set(app_data.join("ai_model_cache.json"));
}

fn with_cache<R>(f: impl FnOnce(&mut CacheFile) -> R) -> R {
    let mut g = CACHE.lock().unwrap();
    if g.is_none() {
        let loaded = CACHE_PATH
            .get()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        *g = Some(loaded);
    }
    f(g.as_mut().unwrap())
}

fn save_cache() {
    let Some(path) = CACHE_PATH.get() else { return };
    let g = CACHE.lock().unwrap();
    if let Some(c) = g.as_ref() {
        if let Ok(s) = serde_json::to_vec_pretty(c) {
            let _ = std::fs::write(path, s);
        }
    }
}

fn server_key(provider: &str, endpoint: &str) -> String {
    format!("{provider}|{endpoint}")
}

pub(crate) fn normalize_endpoint(provider: &str, endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    if !e.is_empty() {
        return e.to_string();
    }
    if crate::flow::is_openai_compat(provider) {
        "http://localhost:1234/v1".into()
    } else {
        "http://localhost:11434".into()
    }
}

/// LM Studio のネイティブ API のベース（/v1 を落とす）。
fn lmstudio_base(endpoint: &str) -> String {
    endpoint.trim_end_matches('/').trim_end_matches("/v1").to_string()
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))
}

// ---------------------------------------------------------------------------
// Ollama
// ---------------------------------------------------------------------------

/// /api/tags → (name, digest) の並び。
fn ollama_tags(c: &reqwest::blocking::Client, ep: &str) -> Result<Vec<(String, String)>, String> {
    let resp = c
        .get(format!("{ep}/api/tags"))
        .send()
        .map_err(|e| format!("Ollama に接続できません（{ep}）: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("モデル一覧の取得に失敗 ({})", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("一覧の解析に失敗: {e}"))?;
    Ok(v["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some((m["name"].as_str()?.to_string(), m["digest"].as_str().unwrap_or("").to_string()))
                })
                .collect()
        })
        .unwrap_or_default())
}

fn ollama_version(c: &reqwest::blocking::Client, ep: &str) -> Option<String> {
    let v: serde_json::Value = c.get(format!("{ep}/api/version")).send().ok()?.json().ok()?;
    v["version"].as_str().map(String::from)
}

/// /api/show の応答から ModelInfo を作る（テスト可能な純関数）。
pub(crate) fn ollama_info_from_show(ep: &str, name: &str, digest: &str, v: &serde_json::Value) -> ModelInfo {
    let raw: Option<Vec<String>> = v["capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    let r = raw.as_deref();
    let caps = ModelCapabilities {
        completion: Tri::of(r, "completion"),
        tools: Tri::of(r, "tools"),
        thinking: Tri::of(r, "thinking"),
        vision: Tri::of(r, "vision"),
        embedding: Tri::of(r, "embedding"),
    };
    let mi = &v["model_info"];
    let arch = mi["general.architecture"].as_str();
    let ctx = arch.and_then(|a| mi[format!("{a}.context_length")].as_u64());
    let mode = ollama_thinking_mode(caps.thinking, name, arch);
    ModelInfo {
        provider: "ollama".into(),
        endpoint: ep.to_string(),
        name: name.to_string(),
        digest: digest.to_string(),
        parameter_size: v["details"]["parameter_size"].as_str().map(String::from),
        quantization: v["details"]["quantization_level"].as_str().map(String::from),
        trained_context_length: ctx,
        capabilities: caps,
        raw_capabilities: raw,
        think_choices: think_choices(&mode),
        thinking_mode: mode,
        fetched_at: now_iso(),
    }
}

/// /api/show を呼んだ回数（差分取得の検証用）。
static SHOW_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn ollama_show(c: &reqwest::blocking::Client, ep: &str, name: &str, digest: &str) -> Result<ModelInfo, String> {
    SHOW_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = c
        .post(format!("{ep}/api/show"))
        .json(&serde_json::json!({ "model": name }))
        .send()
        .map_err(|e| format!("/api/show に失敗: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("/api/show に失敗 ({})", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("/api/show の解析に失敗: {e}"))?;
    Ok(ollama_info_from_show(ep, name, digest, &v))
}

/// /api/show の同時実行数（モデルごとに1回呼ぶので、多数あってもサーバを詰まらせない）。
const SHOW_PARALLEL: usize = 4;

/// digest が変わった／新規のモデルだけ /api/show。結果は (name, info) の並び。
fn ollama_show_many(ep: &str, targets: &[(String, String)]) -> Vec<(String, Result<ModelInfo, String>)> {
    let mut out = Vec::new();
    for chunk in targets.chunks(SHOW_PARALLEL) {
        let res: Vec<_> = std::thread::scope(|s| {
            let hs: Vec<_> = chunk
                .iter()
                .map(|(n, d)| {
                    s.spawn(move || {
                        let r = client().and_then(|c| ollama_show(&c, ep, n, d));
                        (n.clone(), r)
                    })
                })
                .collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        out.extend(res);
    }
    out
}

fn ollama_catalog(ep: &str) -> Result<Vec<ModelInfo>, String> {
    let c = client()?;
    let tags = ollama_tags(&c, ep)?;
    let version = ollama_version(&c, ep);
    let key = server_key("ollama", ep);

    // 取り直す対象: 新規・digest 変化・（Ollama 更新時は）全件。
    let targets: Vec<(String, String)> = with_cache(|cf| {
        let sc = cf.servers.entry(key.clone()).or_default();
        if version.is_some() && sc.server_version != version {
            sc.models.clear();
            sc.server_version = version.clone();
        }
        tags.iter()
            .filter(|(n, d)| sc.models.get(n).map(|m| &m.digest != d).unwrap_or(true))
            .cloned()
            .collect()
    });
    let fetched = ollama_show_many(ep, &targets);

    let list = with_cache(|cf| {
        let sc = cf.servers.entry(key.clone()).or_default();
        for (n, r) in fetched {
            match r {
                Ok(info) => {
                    sc.models.insert(n, info);
                }
                Err(_) => {
                    sc.models.remove(&n);
                }
            }
        }
        // 削除されたモデル（/api/tags に無い）はキャッシュから除く。
        sc.models.retain(|n, _| tags.iter().any(|(t, _)| t == n));
        tags.iter()
            .map(|(n, d)| {
                sc.models.get(n).cloned().unwrap_or_else(|| unknown_info("ollama", ep, n, d))
            })
            .collect::<Vec<_>>()
    });
    save_cache();
    Ok(list)
}

fn unknown_info(provider: &str, ep: &str, name: &str, digest: &str) -> ModelInfo {
    ModelInfo {
        provider: provider.into(),
        endpoint: ep.into(),
        name: name.into(),
        digest: digest.into(),
        parameter_size: None,
        quantization: None,
        trained_context_length: None,
        capabilities: ModelCapabilities::unknown(),
        raw_capabilities: None,
        think_choices: think_choices(&ThinkingMode::Unknown),
        thinking_mode: ThinkingMode::Unknown,
        fetched_at: now_iso(),
    }
}

// ---------------------------------------------------------------------------
// LM Studio
// ---------------------------------------------------------------------------

/// /api/v1/models の1件から ModelInfo を作る（テスト可能な純関数）。
pub(crate) fn lmstudio_info_from_entry(ep: &str, m: &serde_json::Value) -> Option<ModelInfo> {
    let key = m["key"].as_str()?.to_string();
    let is_embedding = m["type"].as_str() == Some("embedding");
    let caps_v = &m["capabilities"];
    let allowed: Option<Vec<String>> = caps_v["reasoning"]["allowed_options"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    let mode = if is_embedding { ThinkingMode::None } else { lmstudio_thinking_mode(allowed.as_deref()) };
    let caps = ModelCapabilities {
        completion: Tri::from_bool(!is_embedding),
        tools: Tri::from_bool(caps_v["trained_for_tool_use"].as_bool().unwrap_or(false)),
        thinking: Tri::from_bool(allowed.is_some() && !is_embedding),
        vision: Tri::from_bool(caps_v["vision"].as_bool().unwrap_or(false)),
        embedding: Tri::from_bool(is_embedding),
    };
    let mut raw = vec![m["type"].as_str().unwrap_or("").to_string()];
    if caps.tools == Tri::Yes {
        raw.push("tool_use".into());
    }
    if caps.vision == Tri::Yes {
        raw.push("vision".into());
    }
    if let Some(a) = &allowed {
        raw.push(format!("reasoning[{}]", a.join(",")));
    }
    let variant = m["selected_variant"].as_str().unwrap_or(&key).to_string();
    let size = m["size_bytes"].as_u64().unwrap_or(0);
    Some(ModelInfo {
        provider: "lmstudio".into(),
        endpoint: ep.to_string(),
        name: key,
        digest: format!("{variant}#{size}"),
        parameter_size: m["params_string"].as_str().map(String::from),
        quantization: m["quantization"]["name"].as_str().map(String::from),
        trained_context_length: m["max_context_length"].as_u64(),
        capabilities: caps,
        raw_capabilities: Some(raw),
        think_choices: think_choices(&mode),
        thinking_mode: mode,
        fetched_at: now_iso(),
    })
}

/// LM Studio のモデル ID は "google/gemma-4-e4b" のほか、旧来の短い識別子や
/// "@q4_k_m" 付きの variant で書かれていることがある。どれでも同じモデルに当てる。
fn lmstudio_matches(info_key: &str, entry: &serde_json::Value, wanted: &str) -> bool {
    info_key == wanted
        || info_key.rsplit('/').next() == Some(wanted)
        || entry["variants"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some(wanted)))
            .unwrap_or(false)
}

fn lmstudio_entries(ep: &str) -> Result<Vec<serde_json::Value>, String> {
    let c = client()?;
    let base = lmstudio_base(ep);
    let resp = c
        .get(format!("{base}/api/v1/models"))
        .send()
        .map_err(|e| format!("LM Studio に接続できません（{base}）: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("LM Studio のモデル一覧（/api/v1/models）を取得できません ({})", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("一覧の解析に失敗: {e}"))?;
    Ok(v["models"].as_array().cloned().unwrap_or_default())
}

fn lmstudio_catalog(ep: &str) -> Result<Vec<ModelInfo>, String> {
    // 古い LM Studio で /api/v1/models が無ければ、OpenAI 準拠の一覧（能力は unknown）。
    let entries = match lmstudio_entries(ep) {
        Ok(e) => e,
        Err(_) => return openai_catalog("lmstudio", ep),
    };
    let list: Vec<ModelInfo> = entries.iter().filter_map(|m| lmstudio_info_from_entry(ep, m)).collect();
    let key = server_key("lmstudio", ep);
    with_cache(|cf| {
        let sc = cf.servers.entry(key).or_default();
        sc.models = list.iter().map(|m| (m.name.clone(), m.clone())).collect();
    });
    save_cache();
    Ok(list)
}

// ---------------------------------------------------------------------------
// OpenAI互換（汎用）: 能力を取る API が無い → unknown
// ---------------------------------------------------------------------------

fn openai_catalog(provider: &str, ep: &str) -> Result<Vec<ModelInfo>, String> {
    let names = crate::flow::list_models_blocking(provider, ep)?;
    Ok(names.iter().map(|n| unknown_info(provider, ep, n, "")).collect())
}

// ---------------------------------------------------------------------------
// 公開 API
// ---------------------------------------------------------------------------

fn catalog_blocking(provider: &str, endpoint: &str) -> Result<Vec<ModelInfo>, String> {
    let ep = normalize_endpoint(provider, endpoint);
    let mut list = match provider {
        "lmstudio" => lmstudio_catalog(&ep)?,
        p if crate::flow::is_openai_compat(p) => openai_catalog(p, &ep)?,
        _ => ollama_catalog(&ep)?,
    };
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

/// 設定ダイアログ用: モデル一覧と能力。開いたとき・「↻取得」で呼ぶ。
#[tauri::command(async)]
pub(crate) fn ai_model_catalog(provider: Option<String>, endpoint: Option<String>) -> Result<Vec<ModelInfo>, String> {
    let provider = provider.unwrap_or_else(|| "ollama".into());
    let endpoint = endpoint.unwrap_or_default();
    // reqwest::blocking は tokio 文脈の外で（run_ai と同じ理由）。
    std::thread::scope(|s| {
        s.spawn(|| catalog_blocking(&provider, &endpoint))
            .join()
            .map_err(|_| "モデル取得スレッドが異常終了しました".to_string())?
    })
}

/// 実行時の1モデル確認（tokio 文脈外のスレッドから呼ぶこと）。
/// Ollama は /api/tags を1回だけ引き、digest がキャッシュと違う（または未取得）ときだけ
/// そのモデルの /api/show を呼ぶ。LM Studio は /api/v1/models を1回。
/// 失敗しても実行は止めない（能力不明＝設定どおり送る）— Err は理由（ログの注記用）。
pub(crate) fn ensure_model_info(provider: &str, endpoint: &str, model: &str) -> Result<ModelInfo, String> {
    let ep = normalize_endpoint(provider, endpoint);
    match provider {
        "lmstudio" => {
            let entries = lmstudio_entries(&ep)?;
            let hit = entries.iter().find_map(|m| {
                let info = lmstudio_info_from_entry(&ep, m)?;
                lmstudio_matches(&info.name, m, model).then_some(info)
            });
            let info = hit.ok_or_else(|| format!("LM Studio の一覧に「{model}」が見つかりません"))?;
            with_cache(|cf| {
                cf.servers
                    .entry(server_key("lmstudio", &ep))
                    .or_default()
                    .models
                    .insert(info.name.clone(), info.clone());
            });
            save_cache();
            Ok(info)
        }
        p if crate::flow::is_openai_compat(p) => {
            Err("OpenAI互換（汎用）サーバからは能力を取得できません".into())
        }
        _ => {
            let c = client()?;
            let tags = ollama_tags(&c, &ep)?;
            // Ollama は "gemma3" を "gemma3:latest" として扱う。
            let full = if model.contains(':') { model.to_string() } else { format!("{model}:latest") };
            let (name, digest) = tags
                .into_iter()
                .find(|(n, _)| n == model || *n == full)
                .ok_or_else(|| format!("Ollama の一覧に「{model}」が見つかりません"))?;
            let key = server_key("ollama", &ep);
            let cached = with_cache(|cf| {
                cf.servers
                    .get(&key)
                    .and_then(|sc| sc.models.get(&name))
                    .filter(|m| m.digest == digest)
                    .cloned()
            });
            if let Some(m) = cached {
                return Ok(m);
            }
            let info = ollama_show(&c, &ep, &name, &digest)?;
            with_cache(|cf| {
                cf.servers.entry(key).or_default().models.insert(name, info.clone());
            });
            save_cache();
            Ok(info)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(provider: &str, mode: ThinkingMode, thinking: Tri) -> ModelInfo {
        let mut m = unknown_info(provider, "ep", "m", "d");
        m.capabilities.thinking = thinking;
        m.think_choices = think_choices(&mode);
        m.thinking_mode = mode;
        m
    }

    #[test]
    fn capabilities_missing_means_unknown_not_no() {
        let v = serde_json::json!({ "details": {}, "model_info": {} });
        let m = ollama_info_from_show("ep", "old:latest", "d1", &v);
        assert_eq!(m.capabilities, ModelCapabilities::unknown());
        assert_eq!(m.thinking_mode, ThinkingMode::Unknown);
        assert_eq!(m.think_choices.len(), 5, "不明なら選択肢は絞らない");
    }

    #[test]
    fn ollama_show_is_parsed() {
        let v = serde_json::json!({
            "capabilities": ["completion", "vision", "audio", "tools", "thinking"],
            "details": { "parameter_size": "8.0B", "quantization_level": "Q4_K_M" },
            "model_info": { "general.architecture": "gemma4", "gemma4.context_length": 131072 }
        });
        let m = ollama_info_from_show("ep", "gemma4:latest", "c6eb", &v);
        assert_eq!(m.capabilities.thinking, Tri::Yes);
        assert_eq!(m.capabilities.vision, Tri::Yes);
        assert_eq!(m.capabilities.embedding, Tri::No);
        assert_eq!(m.trained_context_length, Some(131072));
        assert_eq!(m.parameter_size.as_deref(), Some("8.0B"));
        assert!(m.raw_capabilities.unwrap().contains(&"audio".to_string()), "判定表に無い能力も残す");
        assert!(matches!(m.thinking_mode, ThinkingMode::BooleanOrLevels { .. }));
    }

    #[test]
    fn gpt_oss_is_levels_only() {
        let m = ollama_thinking_mode(Tri::Yes, "gpt-oss:20b", Some("gptoss"));
        assert_eq!(m, ThinkingMode::Levels { levels: levels_vec() });
        assert_eq!(think_choices(&m), vec!["off", "low", "medium", "high"], "「使う」は出さない（off＝指定しない）");
        assert_eq!(ollama_thinking_mode(Tri::No, "gpt-oss:20b", None), ThinkingMode::None);
    }

    #[test]
    fn plan_ollama_sends_user_choice_but_not_to_unsupported() {
        let yes = info("ollama", ThinkingMode::BooleanOrLevels { levels: levels_vec() }, Tri::Yes);
        assert_eq!(plan_think("ollama", Some("high"), Some(&yes), false).ollama_think, Some(serde_json::json!("high")));
        assert_eq!(plan_think("ollama", Some("on"), Some(&yes), false).ollama_think, Some(serde_json::json!(true)));
        assert_eq!(plan_think("ollama", Some("off"), Some(&yes), false), ThinkPlan::default());

        let no = info("ollama", ThinkingMode::None, Tri::No);
        let p = plan_think("ollama", Some("on"), Some(&no), false);
        assert_eq!(p.ollama_think, None, "非対応モデルに送ると 400 になる");
        assert!(p.note.is_some());

        // 能力不明（取得失敗・古い Ollama）は設定どおり送る。
        assert_eq!(plan_think("ollama", Some("low"), None, false).ollama_think, Some(serde_json::json!("low")));
    }

    #[test]
    fn plan_gpt_oss_maps_on_to_medium() {
        let g = info("ollama", ThinkingMode::Levels { levels: levels_vec() }, Tri::Yes);
        let p = plan_think("ollama", Some("on"), Some(&g), false);
        assert_eq!(p.ollama_think, Some(serde_json::json!("medium")));
        assert!(p.note.is_some());
        assert_eq!(plan_think("ollama", Some("low"), Some(&g), false).ollama_think, Some(serde_json::json!("low")));
        let off = plan_think("ollama", Some("off"), Some(&g), false);
        assert_eq!(off.ollama_think, None);
        assert!(off.note.is_some(), "止められないことを記録する");
    }

    #[test]
    fn plan_lmstudio_uses_reasoning_effort_for_levels_only() {
        let g = info("lmstudio", ThinkingMode::Levels { levels: levels_vec() }, Tri::Yes);
        assert_eq!(plan_think("lmstudio", Some("high"), Some(&g), false).reasoning_effort.as_deref(), Some("high"));
        assert_eq!(plan_think("lmstudio", Some("on"), Some(&g), false).reasoning_effort.as_deref(), Some("medium"));

        let b = info("lmstudio", ThinkingMode::Boolean, Tri::Yes);
        let p = plan_think("lmstudio", Some("on"), Some(&b), false);
        assert_eq!(p.reasoning_effort, None, "on/off は LM Studio 側の設定に従う");
        assert!(p.note.is_some());
        assert_eq!(plan_think("lmstudio", Some("high"), Some(&b), false).reasoning_effort, None);
        assert_eq!(plan_think("lmstudio", Some("high"), Some(&b), false).ollama_think, None);

        // LM Studio は send_reasoning に左右されない（能力で決まる）。
        assert_eq!(plan_think("lmstudio", Some("on"), Some(&b), true).reasoning_effort, None);
    }

    #[test]
    fn plan_generic_openai_sends_only_when_enabled() {
        // オフ（既定）: 何も送らない（従来どおり）。指定していたら注記を残す。
        let p = plan_think("openai", Some("high"), None, false);
        assert_eq!((p.ollama_think, p.reasoning_effort), (None, None));
        assert!(p.note.is_some());
        assert_eq!(plan_think("openai", Some("off"), None, false), ThinkPlan::default());

        // オン: 浅く/中/深く→low/medium/high、使わない→none、使う→送らない（サーバ既定）。
        let effort = |t: Option<&str>| plan_think("openai", t, None, true).reasoning_effort;
        assert_eq!(effort(Some("low")).as_deref(), Some("low"));
        assert_eq!(effort(Some("medium")).as_deref(), Some("medium"));
        assert_eq!(effort(Some("high")).as_deref(), Some("high"));
        assert_eq!(effort(Some("off")).as_deref(), Some("none"));
        assert_eq!(effort(None).as_deref(), Some("none"), "未設定＝使わない");
        let on = plan_think("openai", Some("on"), None, true);
        assert_eq!(on.reasoning_effort, None);
        assert!(on.note.is_some());
        // 未知の値は「使わない」扱い＝none（任意の文字列をそのまま送って 400 にしない）。
        assert_eq!(effort(Some("xhigh")).as_deref(), Some("none"));
        assert_eq!(plan_think("openai", Some("high"), None, true).ollama_think, None);
    }

    #[test]
    fn lmstudio_entry_is_parsed() {
        let e = serde_json::json!({
            "type": "llm", "key": "openai/gpt-oss-20b", "params_string": "20B",
            "quantization": { "name": "MXFP4" }, "size_bytes": 12109664245u64,
            "max_context_length": 131072,
            "capabilities": { "vision": false, "trained_for_tool_use": true,
                "reasoning": { "allowed_options": ["low", "medium", "high"], "default": "low" } },
            "variants": ["openai/gpt-oss-20b@mxfp4"], "selected_variant": "openai/gpt-oss-20b@mxfp4"
        });
        let m = lmstudio_info_from_entry("http://localhost:1234/v1", &e).unwrap();
        assert_eq!(m.capabilities.tools, Tri::Yes);
        assert_eq!(m.capabilities.vision, Tri::No);
        assert_eq!(m.thinking_mode, ThinkingMode::Levels { levels: levels_vec() });
        assert_eq!(m.digest, "openai/gpt-oss-20b@mxfp4#12109664245");
        assert!(lmstudio_matches(&m.name, &e, "gpt-oss-20b"));
        assert!(lmstudio_matches(&m.name, &e, "openai/gpt-oss-20b@mxfp4"));

        let emb = serde_json::json!({ "type": "embedding", "key": "nomic", "max_context_length": 2048 });
        let m = lmstudio_info_from_entry("ep", &emb).unwrap();
        assert_eq!(m.capabilities.completion, Tri::No, "埋め込み専用は対話不可");
        assert_eq!(m.capabilities.embedding, Tri::Yes);

        let none = serde_json::json!({ "type": "llm", "key": "q", "capabilities": { "vision": false } });
        assert_eq!(lmstudio_info_from_entry("ep", &none).unwrap().thinking_mode, ThinkingMode::None);
        let mixed = lmstudio_thinking_mode(Some(&["off".into(), "low".into(), "xhigh".into(), "on".into()]));
        assert_eq!(mixed, ThinkingMode::BooleanOrLevels { levels: vec!["low".into()] });
    }

    // 実機検証（ライブの Ollama と LM Studio が要る）。
    // 実行: `cargo test --lib -- --ignored ai_models_live --nocapture`
    #[test]
    #[ignore = "ライブOllama/LM Studioが必要"]
    fn ai_models_live() {
        use std::sync::atomic::Ordering;
        // 受け入れ条件1・5: 全モデルの能力が取れる／2回目は /api/show を呼ばない。
        let before = SHOW_CALLS.load(Ordering::Relaxed);
        let list = catalog_blocking("ollama", "").unwrap();
        let first = SHOW_CALLS.load(Ordering::Relaxed) - before;
        for m in &list {
            eprintln!("{:45} {:?} {:?} ctx={:?}", m.name, m.capabilities, m.think_choices, m.trained_context_length);
        }
        assert!(!list.is_empty());
        assert_eq!(first, list.len(), "初回は全件 /api/show");
        let before = SHOW_CALLS.load(Ordering::Relaxed);
        let again = catalog_blocking("ollama", "").unwrap();
        assert_eq!(SHOW_CALLS.load(Ordering::Relaxed) - before, 0, "digest が同じなら呼び直さない");
        assert_eq!(again.len(), list.len());
        // digest を偽って1件だけ差し替える＝pull で中身が変わった状況 → その1件だけ取り直す。
        with_cache(|cf| {
            let sc = cf.servers.get_mut(&server_key("ollama", "http://localhost:11434")).unwrap();
            sc.models.values_mut().next().unwrap().digest = "stale".into();
        });
        let before = SHOW_CALLS.load(Ordering::Relaxed);
        catalog_blocking("ollama", "").unwrap();
        assert_eq!(SHOW_CALLS.load(Ordering::Relaxed) - before, 1, "digest 差分のモデルだけ");

        // 受け入れ条件3相当（思考）: 非対応モデルには think を送らず、実行が通る。
        let g3 = ensure_model_info("ollama", "", "gemma3:latest").unwrap();
        assert_eq!(g3.capabilities.thinking, Tri::No);
        let spec: crate::flow::FilterSpec =
            serde_json::from_str(r#"{"builtin":"ai","model":"gemma3:latest","think":"on"}"#).unwrap();
        let mut rec = None;
        let out = crate::flow::run_ai_rec(&spec, "1+1は？数字だけ".as_bytes(), &mut rec).unwrap();
        let rec = rec.unwrap();
        eprintln!("gemma3: {} / {:?}", String::from_utf8_lossy(&out.0), rec);
        assert_eq!(rec.sent.think, None);
        assert_eq!(rec.digest.as_deref(), Some(g3.digest.as_str()));
        assert!(!rec.notes.is_empty());

        // 受け入れ条件4: gpt-oss（LM Studio）にはレベル文字列が送られ、思考が返る。
        let lm = catalog_blocking("lmstudio", "").unwrap();
        let g = lm.iter().find(|m| m.name == "openai/gpt-oss-20b").expect("LM Studio に gpt-oss-20b");
        assert_eq!(g.thinking_mode, ThinkingMode::Levels { levels: levels_vec() });
        let spec: crate::flow::FilterSpec = serde_json::from_str(
            r#"{"builtin":"ai","provider":"lmstudio","model":"openai/gpt-oss-20b","think":"on","timeout_secs":0}"#,
        )
        .unwrap();
        let mut rec = None;
        let out = crate::flow::run_ai_rec(&spec, "17*23は？答えだけ".as_bytes(), &mut rec).unwrap();
        let rec = rec.unwrap();
        eprintln!("gpt-oss: {} / {:?}", String::from_utf8_lossy(&out.0), rec);
        assert_eq!(rec.sent.reasoning_effort.as_deref(), Some("medium"), "「使う」は medium");
        assert!(rec.thinking_returned);
        assert!(!String::from_utf8_lossy(&out.0).contains("<think>"));
    }

    #[test]
    fn endpoint_normalization() {
        assert_eq!(normalize_endpoint("ollama", ""), "http://localhost:11434");
        assert_eq!(normalize_endpoint("lmstudio", " http://h:1234/v1/ "), "http://h:1234/v1");
        assert_eq!(lmstudio_base("http://h:1234/v1"), "http://h:1234");
    }
}
