// --- v1.12 思考役 — docs/V1.12_声の窓口と思考役計画.md §4・§5.1 ---------------------------
// 声の窓口（音声カード）が「じっくり考える」と決めた問いを、裏で常駐するローカルの思考役へ回す。
// 頭の順番は Strata → LM Studio（どちらも OpenAI 互換の /v1/chat/completions で話す）。
// Claude へは自動では落とさない（ローカル完結が基本・Claude は人が明示したときだけ）。
//
// GPU は1枚なので、LM Studio は Strata が止まっているときだけ使う。LM Studio は
// **読み込み済みのモデルだけ**を使う（未読み込みのモデル名を送ると数十GB を勝手に載せうる）。
//
// lib.rs の glob 再エクスポートには載せない（`crate::thinker::` で参照）。

use std::time::Duration;

pub(crate) const DEFAULT_STRATA: &str = "http://127.0.0.1:8080";
pub(crate) const DEFAULT_LMSTUDIO: &str = "http://127.0.0.1:1234";
/// 1回の思考の上限（出力トークン）。Strata 実測 毎秒約60 → 本文で1分強＋思考。
const MAX_TOKENS: u64 = 4000;

/// 思考役の頭。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Head {
    Strata { ep: String, model: String },
    LmStudio { ep: String, model: String },
}

impl Head {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Head::Strata { .. } => "Strata",
            Head::LmStudio { .. } => "LM Studio",
        }
    }
    fn ep(&self) -> &str {
        match self {
            Head::Strata { ep, .. } | Head::LmStudio { ep, .. } => ep,
        }
    }
    fn model(&self) -> &str {
        match self {
            Head::Strata { model, .. } | Head::LmStudio { model, .. } => model,
        }
    }
}

/// 窓口のカード設定から読む思考役の設定。
#[derive(Clone, Debug)]
pub(crate) struct Conf {
    /// "auto"（Strata → LM Studio）/ "strata" / "lmstudio" / "off"。
    pub backend: String,
    pub strata: String,
    pub lmstudio: String,
    /// LM Studio で使うモデル（空＝読み込み済みのうち最初の LLM）。
    pub lmstudio_model: String,
    /// Strata の思考の深さ（reasoning_effort: low / medium / high）。
    pub effort: String,
}

impl Conf {
    pub(crate) fn enabled(&self) -> bool {
        self.backend != "off"
    }
}

/// Strata の混み具合（/metrics の live）。
#[derive(Clone, Debug, Default, serde::Serialize, PartialEq)]
pub(crate) struct Status {
    /// 使う頭（"Strata" / "LM Studio" / ""＝どちらも使えない）。
    pub head: String,
    /// Strata: idle / reading / generating。LM Studio: ready。使えない: down。
    pub state: String,
    /// 順番待ちの件数（Strata のみ）。
    pub queued: u64,
}

fn client(timeout: Duration) -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(timeout)
        .build()
        .ok()
}

fn get_json(url: &str) -> Option<serde_json::Value> {
    let r = client(Duration::from_secs(4))?.get(url).send().ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().ok()
}

/// Strata: /health が loaded ならそのモデル名。
fn probe_strata(ep: &str) -> Option<Head> {
    let v = get_json(&format!("{ep}/health"))?;
    if v.get("loaded").and_then(|b| b.as_bool()) != Some(true) {
        return None;
    }
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("strata").to_string();
    Some(Head::Strata { ep: ep.to_string(), model })
}

/// LM Studio の /api/v0/models から使うモデルを選ぶ。指定があれば、それが読み込み済みのときだけ。
/// 指定が無ければ読み込み済みの LLM（vlm 含む）の最初。埋め込みモデルは除く。
pub(crate) fn pick_lmstudio_model(models: &serde_json::Value, want: &str) -> Option<String> {
    let list = models.get("data").and_then(|d| d.as_array())?;
    let loaded = list.iter().filter(|m| {
        m.get("state").and_then(|s| s.as_str()) == Some("loaded")
            && matches!(m.get("type").and_then(|t| t.as_str()), Some("llm") | Some("vlm"))
    });
    let ids: Vec<String> = loaded.filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect();
    if want.is_empty() {
        ids.into_iter().next()
    } else {
        ids.into_iter().find(|i| i == want)
    }
}

fn probe_lmstudio(ep: &str, want: &str) -> Result<Head, String> {
    let v = get_json(&format!("{ep}/api/v0/models")).ok_or("LM Studio のサーバーが止まっています")?;
    match pick_lmstudio_model(&v, want) {
        Some(model) => Ok(Head::LmStudio { ep: ep.to_string(), model }),
        None if want.is_empty() => Err("LM Studio にモデルが読み込まれていません".into()),
        None => Err(format!("LM Studio に {want} が読み込まれていません")),
    }
}

/// いま使える頭（順番: Strata → LM Studio）。どちらも使えなければ理由。
pub(crate) fn pick(conf: &Conf) -> Result<Head, String> {
    let try_strata = matches!(conf.backend.as_str(), "auto" | "strata");
    let try_lm = matches!(conf.backend.as_str(), "auto" | "lmstudio");
    if try_strata {
        if let Some(h) = probe_strata(&conf.strata) {
            return Ok(h);
        }
    }
    if try_lm {
        return probe_lmstudio(&conf.lmstudio, &conf.lmstudio_model).map_err(|e| {
            if try_strata { format!("Strata が止まっていて、{e}") } else { e }
        });
    }
    if try_strata {
        return Err("Strata が止まっています".into());
    }
    Err("思考役が設定されていません".into())
}

/// 状態パネル用。Strata なら /metrics の live を読む。
pub(crate) fn status(conf: &Conf) -> Status {
    match pick(conf) {
        Ok(Head::Strata { ep, .. }) => {
            let live = get_json(&format!("{ep}/metrics")).and_then(|v| v.get("live").cloned());
            let s = |k: &str| live.as_ref().and_then(|l| l.get(k)).cloned();
            Status {
                head: "Strata".into(),
                state: s("state").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "idle".into()),
                queued: s("queued").and_then(|v| v.as_u64()).unwrap_or(0),
            }
        }
        Ok(Head::LmStudio { .. }) => Status { head: "LM Studio".into(), state: "ready".into(), queued: 0 },
        Err(_) => Status { head: String::new(), state: "down".into(), queued: 0 },
    }
}

const SYSTEM: &str = "あなたは TexoS の思考役です。音声アシスタント（窓口）から、じっくり考える必要のある問いを\
受け取ります。答えは窓口がユーザーに声で伝え、全文はファイルに残します。\n\n\
答え方:\n\
1. 最初の段落は、声で読み上げる要約を日本語で1〜3文。Markdown・箇条書き・記号・URL は使わない。\n\
2. 空行を1つ入れる。\n\
3. そのあとに詳しい本文（Markdown 可）。\n\
分からないことは分からないと書き、推測なら推測だと書く。";

/// 思考の結果。
pub(crate) struct Answer {
    pub head: &'static str,
    pub model: String,
    /// 声で伝える要約（最初の段落）。
    pub summary: String,
    /// 全文（要約を含む）。
    pub full: String,
    pub secs: f32,
}

/// 最初の段落（空行まで）を要約、全体を全文とする。段落が無ければ先頭 120 字。
pub(crate) fn split_answer(text: &str) -> (String, String) {
    let full = text.trim().to_string();
    let first = full.split("\n\n").next().unwrap_or("").trim();
    let summary = if first.is_empty() || first.chars().count() > 240 {
        let mut s: String = full.chars().take(120).collect();
        if full.chars().count() > 120 {
            s.push('…');
        }
        s
    } else {
        first.to_string()
    };
    (summary.replace('\n', " "), full)
}

/// 問いを思考役に渡して答えを待つ（呼び出し側のスレッドで同期に待つ・長くて数分）。
/// context は窓口の会話の抜粋（「それ」「さっきの件」を解くため）。
pub(crate) fn think(conf: &Conf, question: &str, context: &str) -> Result<Answer, String> {
    let head = pick(conf)?;
    let t0 = std::time::Instant::now();
    let mut user = String::new();
    if !context.trim().is_empty() {
        user.push_str("これまでの会話（抜粋）:\n");
        user.push_str(context.trim());
        user.push_str("\n\n");
    }
    user.push_str("考えてほしい問い:\n");
    user.push_str(question.trim());
    let mut body = serde_json::json!({
        "model": head.model(),
        "stream": false,
        "max_tokens": MAX_TOKENS,
        "messages": [
            { "role": "system", "content": SYSTEM },
            { "role": "user", "content": user }
        ]
    });
    if matches!(head, Head::Strata { .. }) {
        body["reasoning_effort"] = serde_json::Value::String(conf.effort.clone());
    }
    let c = client(Duration::from_secs(900)).ok_or("通信の準備に失敗しました")?;
    let resp = c
        .post(format!("{}/v1/chat/completions", head.ep()))
        .json(&body)
        .send()
        .map_err(|e| format!("{} に接続できません: {e}", head.name()))?;
    if !resp.status().is_success() {
        let code = resp.status();
        let msg = resp.text().unwrap_or_default();
        return Err(format!("{} がエラーを返しました（{code}）{}", head.name(), msg.chars().take(120).collect::<String>()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("{} の応答を読めません: {e}", head.name()))?;
    let text = v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("");
    // 思考モデルが本文に混ぜた <think>…</think> は落とす（AI カードと同じ扱い）。
    let text = strip_think(text);
    if text.trim().is_empty() {
        return Err(format!("{} の答えが空でした（出力の上限に達した可能性）", head.name()));
    }
    let (summary, full) = split_answer(&text);
    Ok(Answer { head: head.name(), model: head.model().to_string(), summary, full, secs: t0.elapsed().as_secs_f32() })
}

fn strip_think(s: &str) -> String {
    let mut out = s.to_string();
    while let (Some(a), Some(b)) = (out.find("<think>"), out.find("</think>")) {
        if b < a {
            break;
        }
        out.replace_range(a..b + "</think>".len(), "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lmstudio_uses_loaded_llm_only() {
        let v = serde_json::json!({ "data": [
            { "id": "big", "type": "llm", "state": "not-loaded" },
            { "id": "emb", "type": "embeddings", "state": "loaded" },
            { "id": "qwen", "type": "vlm", "state": "loaded" },
        ]});
        assert_eq!(pick_lmstudio_model(&v, ""), Some("qwen".into()));
        assert_eq!(pick_lmstudio_model(&v, "qwen"), Some("qwen".into()));
        // 未読み込みの指定は選ばない（勝手に数十GB を載せない）。
        assert_eq!(pick_lmstudio_model(&v, "big"), None);
        let none = serde_json::json!({ "data": [ { "id": "big", "type": "llm", "state": "not-loaded" } ] });
        assert_eq!(pick_lmstudio_model(&none, ""), None);
    }

    #[test]
    fn answer_splits_summary_and_body() {
        let (s, f) = split_answer("要点は二つです。速さと確実さ。\n\n## 詳しく\n- 速さ\n- 確実さ");
        assert_eq!(s, "要点は二つです。速さと確実さ。");
        assert!(f.contains("## 詳しく"));
        let (s, _) = split_answer("段落なしで一気に書いた長い答え");
        assert_eq!(s, "段落なしで一気に書いた長い答え");
    }

    #[test]
    fn think_tags_are_dropped() {
        assert_eq!(strip_think("<think>考え中</think>答え"), "答え");
        assert_eq!(strip_think("答えだけ"), "答えだけ");
    }

    /// 実機の思考役に当てる（cargo test --lib thinker_live -- --ignored --nocapture）。
    #[test]
    #[ignore]
    fn thinker_live() {
        let conf = Conf {
            backend: "auto".into(),
            strata: DEFAULT_STRATA.into(),
            lmstudio: DEFAULT_LMSTUDIO.into(),
            lmstudio_model: String::new(),
            effort: "low".into(),
        };
        println!("status = {:?}", status(&conf));
        let lm = Conf { backend: "lmstudio".into(), ..conf.clone() };
        println!("lmstudio only = {:?}", pick(&lm).map(|h| h.name()));
        let down = Conf { strata: "http://127.0.0.1:9".into(), ..conf.clone() };
        println!("strata down = {:?}", pick(&down).map(|h| h.name()));
        if std::env::var("THINKER_LIVE_PICK_ONLY").is_ok() {
            return;
        }
        let a = think(&conf, "音声アシスタントの返事を速く感じさせる工夫を3つ挙げて", "").expect("思考役");
        println!("{} / {} / {:.1}s\n要約: {}\n---\n{}", a.head, a.model, a.secs, a.summary, a.full);
    }
}
