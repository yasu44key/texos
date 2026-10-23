// --- v1.10 音声反射カード（op:"voice"）— docs/V1.10_音声反射カード計画.md ------------------
// TexoS に反射神経を入れる最初の1枚。カードが稼働中（paused=false）の間だけ、音声プロセス
// （python/voice_server.py）を子プロセスとして持つ。マイクと話者の持ち主はその1つ。
//
// 反射弓（§3.2）: 発話 → スーパーバイザー語 / req 矢印の言い回しと照合 → 発火（run_flow）
// → 完了・失敗を声で報告。反射弓に LLM は挟まない。効果は「発火」と「声で報告」だけ
// （構造は書き換えない＝オペ層 §1-2 権能の限定）。
//
// 生死はスケジューラ（ops.rs の 1 秒 tick）から voice_tick で突き合わせる: 現在のプロジェクトに
// 稼働中の音声カードがあれば起動、無ければ（停止・削除・プロジェクト切替）止める。
// アプリが落ちたら子の stdin が閉じ、音声プロセスは自分で終わる（孤児にならない）。
//
// lib.rs の glob 再エクスポートには載せない（`crate::voice::` で参照）。

use crate::*;
use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};

/// デーモン本体はビルドに埋め込み、起動時に app_data/voice へ書き出す（t2i と同じ流儀）。
const VOICE_SERVER_PY: &str = include_str!("../../python/voice_server.py");

/// 管理 env 名（runtime/envs/voice）。
pub(crate) const VOICE_ENV: &str = "voice";

/// 既定値（spec に無いとき）。§5-1: 発火回数は無制限・暴走止めは min_interval だけ。
const DEFAULT_MIN_INTERVAL: u64 = 1;
const DEFAULT_MAX_FIRES: u64 = 0;
const DEFAULT_SAY_OUT: &str = "音声/発話.txt";
/// 人が読む会話の記録（受け皿と同じフォルダ・追記）。機械用の JSONL は .texos/logs/voice/。
const LOG_MD_NAME: &str = "発話ログ.md";
const LOG_JSONL_NAME: &str = "発話ログ.jsonl";
const DEFAULT_HUSH: &[&str] = &["止めて", "ストップ", "静かに"];
const DEFAULT_CANCEL: &[&str] = &["全部止めて", "中止して"];
/// 会話履歴を消す言葉（VoiceAssistantAgent の command() と同じ・LLM を通さない）。
const DEFAULT_RESET: &[&str] = &["リセット", "会話をリセット", "話題をリセット", "最初からやり直して"];
/// LLM に渡す会話履歴の上限（文字数・VoiceAssistantAgent の context() と同じ）。完全な記録は発話ログ。
const HISTORY_CHARS: usize = 6000;
/// 異常終了・起動失敗のあと、同じ設定での再起動を待つ秒数（tick ごとの再起動ループ防止）。
const RETRY_SECS: u64 = 15;

struct VoiceProc {
    card: String,
    root: String,
    sig: String,
    gen: u64,
    child: Child,
    stdin: ChildStdin,
}

#[derive(Clone)]
struct Pending {
    /// 中身を待っているコマンド（矢印の先のカード名）。
    label: String,
    /// ここまでの中身（発話ごと）。
    parts: Vec<String>,
    at: Instant,
}

/// 聞き取り中のまま放置されたら捨てる（中身は発話.txt に残っている）。
const PENDING_IDLE: Duration = Duration::from_secs(600);

#[derive(Default)]
struct VoiceRt {
    proc: Option<VoiceProc>,
    gen: u64,
    /// (card|sig, 時刻): この組での起動を RETRY_SECS 待つ。
    backoff: Option<(String, Instant)>,
    /// カードごとの最終発火（min_interval）。
    last_fire: HashMap<String, Instant>,
    /// 音声から発火した終端カード（「全部止めて」の対象）。
    fired: HashSet<String>,
    /// カードごとの会話履歴（role, content）。VoiceAssistantAgent と同じく LLM に毎回渡す。
    /// 音声プロセスの起動（＝稼働開始）で新しい会話になる。「リセット」で消す。
    history: HashMap<String, Vec<(String, String)>>,
    /// カードごとの「聞き取り中」（「メモして」→ 話す → 「以上」）。中身はコードが溜める
    /// （AI に毎回まとめ直させると、届くたびに実行してしまった・2026-10-11 実測）。
    pending: HashMap<String, Pending>,
    /// 発話ログ（人が読む md）の置き場と、いま処理中の発話の出どころ（mic / text / card）。
    log_ctx: HashMap<String, (PathBuf, &'static str)>,
    say_seq: u64,
}

fn rt() -> &'static Mutex<VoiceRt> {
    static RT: OnceLock<Mutex<VoiceRt>> = OnceLock::new();
    RT.get_or_init(|| Mutex::new(VoiceRt::default()))
}

/// 録音カードへのマイク譲渡（§5-4）: 譲渡中の数と、音声プロセスがマイクを閉じたか。
static MIC_YIELDS: AtomicUsize = AtomicUsize::new(0);
/// 外からの入力（voice_inject）が反射弓を回すためのアプリハンドル（最初の tick で控える）。
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static MIC_CLOSED: AtomicBool = AtomicBool::new(true);

fn voice_home(app_data: &Path) -> PathBuf {
    app_data.join("voice")
}

// ---------------------------------------------------------------------------
// spec の読み出し（音声カード固有のキーは OpSpec.extra に温存されている）
// ---------------------------------------------------------------------------

fn extra_str(spec: &OpSpec, key: &str) -> Option<String> {
    spec.extra
        .get(key)
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.trim().to_string()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
}

fn extra_f64(spec: &OpSpec, key: &str, default: f64) -> f64 {
    spec.extra
        .get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
        .filter(|f| *f > 0.0)
        .unwrap_or(default)
}

/// 文字列配列（ダイアログの asList）。キーが無ければ既定、明示の空配列は「なし」。
fn extra_list(spec: &OpSpec, key: &str, default: &[&str]) -> Vec<String> {
    match spec.extra.get(key) {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(serde_json::Value::String(s)) => s
            .split([',', '、'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => default.iter().map(|s| s.to_string()).collect(),
    }
}

/// 音声プロセスの起動引数（これが変わったら起動し直す）。
fn proc_args(spec: &OpSpec, hotwords: &str) -> Vec<String> {
    let mut v = vec![
        "--whisper-model".to_string(),
        extra_str(spec, "whisper_model").unwrap_or_else(|| "base".into()),
        "--threshold".into(),
        extra_f64(spec, "threshold", 0.012).to_string(),
        "--interrupt-threshold".into(),
        extra_f64(spec, "interrupt_threshold", 0.04).to_string(),
        "--silence".into(),
        extra_f64(spec, "silence", 1.2).to_string(),
        "--max-seconds".into(),
        extra_f64(spec, "max_seconds", 30.0).to_string(),
        "--report-gap".into(),
        extra_f64(spec, "report_gap", 1.0).to_string(),
    ];
    if let Some(d) = extra_str(spec, "device") {
        v.push("--device".into());
        v.push(d);
    }
    if !hotwords.is_empty() {
        v.push("--hotwords".into());
        v.push(hotwords.to_string());
    }
    v
}

// ---------------------------------------------------------------------------
// 発話の照合（反射弓の中心。LLM なし・閉じた語彙）
// ---------------------------------------------------------------------------

/// 照合用の正規化: 空白と句読点を除く（VoiceAssistantAgent の command() と同じ流儀）。
fn is_noise(c: char) -> bool {
    c.is_whitespace() || "、。！？!?,.・…「」『』".contains(c)
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| !is_noise(*c)).collect()
}

/// 照合用の文字の畳み込み: カタカナ→ひらがな（「メモ」と「めも」を同じに見る）。
fn fold(c: char) -> char {
    match c {
        '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
        _ => c,
    }
}

/// 言い回しの字と字の間に挟まってよい字数（「メモ“を”して」「英訳“を”して」）。
const MAX_GAP: usize = 2;
/// 挟まる字の合計の上限（離れすぎた偶然の一致を拾わない）。
const MAX_GAP_TOTAL: usize = 3;

/// 発話の中で言い回しが現れる位置（原文の文字位置 [start, end) と、挟まった字数）。
/// 空白・句読点は無視し、カタカナ/ひらがなを同一視し、字の間に助詞などが少し挟まるのを許す。
/// 挟まりが少ない一致を優先する（完全一致＝0）。
fn locate(text: &str, phrase: &str) -> Option<(usize, usize, usize)> {
    let kept: Vec<(usize, char)> = text
        .chars()
        .enumerate()
        .filter(|(_, c)| !is_noise(*c))
        .map(|(i, c)| (i, fold(c)))
        .collect();
    let p: Vec<char> = phrase.chars().filter(|c| !is_noise(*c)).map(fold).collect();
    if p.is_empty() || p.len() > kept.len() {
        return None;
    }
    // 1字の言い回しは挟まりを許さない（偶然の一致だらけになる）。
    let max_gap = if p.len() < 2 { 0 } else { MAX_GAP };
    let mut best: Option<(usize, usize, usize)> = None;
    for start in 0..kept.len() {
        if kept[start].1 != p[0] {
            continue;
        }
        let (mut k, mut gap_total, mut ok) = (start, 0usize, true);
        for &pc in &p[1..] {
            let mut found = None;
            for g in 0..=max_gap {
                let j = k + 1 + g;
                if j < kept.len() && kept[j].1 == pc {
                    found = Some((j, g));
                    break;
                }
            }
            match found {
                Some((j, g)) if gap_total + g <= MAX_GAP_TOTAL => {
                    k = j;
                    gap_total += g;
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && best.map(|b| gap_total < b.2).unwrap_or(true) {
            best = Some((kept[start].0, kept[k].0 + 1, gap_total));
            if gap_total == 0 {
                break;
            }
        }
    }
    best
}

/// 発話から言い回しを取り除いた残り（＝中身）。原文の句読点は中身の内側では保つ。
/// 言い回しが見つからなければ None。
fn strip_phrase(text: &str, phrase: &str) -> Option<String> {
    let (s_idx, e_idx, _) = locate(text, phrase)?;
    let chars: Vec<char> = text.chars().collect();
    let rest: String = chars[..s_idx].iter().chain(chars[e_idx..].iter()).collect();
    Some(rest.trim_matches(is_noise).to_string())
}

/// スーパーバイザー語の一致: 全体一致、または語の後ろに丁寧さ・促しが付いただけ
/// （「止めてください」「ストップして」）。長い文の途中の「止めて」は拾わない。
const POLITE_TAILS: &[&str] = &[
    "ください", "下さい", "くれ", "ちょうだい", "して", "よ", "ね", "お願い", "おねがい",
    "お願いします", "おねがいします",
];

fn folded(s: &str) -> String {
    normalize(s).chars().map(fold).collect()
}

fn supervisor_hit(text: &str, word: &str) -> bool {
    let (t, w) = (folded(text), folded(word));
    if w.is_empty() {
        return false;
    }
    t == w
        || t.strip_prefix(w.as_str())
            .map(|tail| POLITE_TAILS.iter().any(|p| folded(p) == tail))
            .unwrap_or(false)
}

/// req 矢印の先と、その言い回し（attrs.say＝文字列か配列。無ければカード名）。
struct Target {
    id: String,
    label: String,
    phrases: Vec<String>,
    /// カードのコメント（AI 振り分けでコマンドの説明として渡す）。
    memo: Option<String>,
}

fn phrases_of(label: &str, attrs: Option<&str>) -> Vec<String> {
    let from_attrs = attrs
        .and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok())
        .and_then(|v| v.get("say").cloned())
        .map(|say| match say {
            serde_json::Value::String(s) => vec![s],
            serde_json::Value::Array(a) => a
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => vec![],
        })
        .unwrap_or_default();
    let v: Vec<String> = from_attrs
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !normalize(s).is_empty())
        .collect();
    if v.is_empty() {
        vec![label.trim().to_string()]
    } else {
        v
    }
}

/// 一番長く一致した言い回しのターゲット（同じ長さなら挟まりの少ない方・さらに同点は矢印を引いた順）。
fn best_match<'a>(text: &str, targets: &'a [Target]) -> Option<(&'a Target, String)> {
    let mut best: Option<(&Target, String, usize, usize)> = None;
    for t in targets {
        for ph in &t.phrases {
            let Some((_, _, gap)) = locate(text, ph) else { continue };
            let len = normalize(ph).chars().count();
            let better = match &best {
                None => true,
                Some(b) => len > b.2 || (len == b.2 && gap < b.3),
            };
            if better {
                best = Some((t, ph.clone(), len, gap));
            }
        }
    }
    best.map(|(t, ph, _, _)| (t, ph))
}

/// Whisper に渡す語彙（hotwords）: コマンドの言い回しとスーパーバイザー語。
/// 「メモして」が「目もして」と聞き違えられるのを減らす（2026-10-11 実機）。
fn hotwords_of(spec: &OpSpec, targets: &[Target]) -> String {
    let mut words: Vec<String> = Vec::new();
    if extra_str(spec, "mode").as_deref() != Some("dictation") {
        for t in targets {
            words.extend(t.phrases.iter().cloned());
        }
    }
    words.extend(extra_list(spec, "hush_words", DEFAULT_HUSH));
    words.extend(extra_list(spec, "cancel_words", DEFAULT_CANCEL));
    words.extend(extra_list(spec, "reset_words", DEFAULT_RESET));
    let mut seen = HashSet::new();
    words.retain(|w| seen.insert(w.clone()));
    words.join(" ")
}

/// 音声カードから出る req 矢印の先（言い回しつき）。
fn targets_of(conn: &rusqlite::Connection, card: &str) -> Vec<Target> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT v.id, v.label, rel.attrs, v.memo FROM relations rel
         JOIN virtual_bodies v ON v.id = rel.to_id AND v.deleted_at IS NULL
         WHERE rel.from_id = ?1 AND rel.kind = 'req' AND rel.deleted_at IS NULL
           AND v.kind = 'filter'
         ORDER BY rel.created_at",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map(rusqlite::params![card], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    });
    match rows {
        Ok(rows) => rows
            .filter_map(|r| r.ok())
            .map(|(id, label, attrs, memo)| {
                // v1.12 ②: カードのコメントが無ければ、そのカードにつながる役割の付箋を説明に使う。
                let memo = memo.filter(|m| !m.trim().is_empty()).or_else(|| role_note_of(conn, &id));
                Target { phrases: phrases_of(&label, attrs.as_deref()), id, label, memo }
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// v1.12 ②: カードへ役割（role:"system"）の矢印でつながる付箋の本文。
fn role_note_of(conn: &rusqlite::Connection, card: &str) -> Option<String> {
    let mut stmt = conn
        .prepare(
            "SELECT v.note_text, rel.attrs FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
             WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL AND v.kind = 'note'
             ORDER BY rel.created_at",
        )
        .ok()?;
    let rows = stmt
        .query_map(rusqlite::params![card], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))
        .ok()?;
    for (text, attrs) in rows.flatten() {
        if flow::parse_edge_attrs(attrs.as_deref()).role.as_deref() == Some("system") {
            if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
                return Some(t);
            }
        }
    }
    None
}

/// 選択肢の説明: 冒頭の段落（空行まで）を1行にして最大 160 字。1行目がカード名と同じ見出しなら飛ばす
/// （役割の付箋は「プログラミング／自然言語を…」のように見出しから書かれることが多い）。
fn describe(label: &str, memo: &str) -> String {
    let mut lines: Vec<&str> = memo.lines().map(str::trim).collect();
    while lines.first().map(|l| l.is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    if lines.len() > 1 && normalize(lines[0]) == normalize(label) {
        lines.remove(0);
    }
    let para: Vec<&str> = lines.into_iter().take_while(|l| !l.is_empty()).collect();
    let one = para.join(" ");
    let mut out: String = one.chars().take(160).collect();
    if one.chars().count() > 160 {
        out.push('…');
    }
    out
}

// ---------------------------------------------------------------------------
// AI 振り分け（2026-10-11 ユーザー方針）: 文字照合は「メモ“を”して」「目もして」で外れる。
// 発話の全文をローカル LLM に渡し、コマンド（矢印の先のカード名）＋ none の閉じた選択肢から
// 1つ選ばせ、渡す中身も同時に返させる。enum の制約付き出力（format=JSON Schema）で、
// 選択肢の外は物理的に出ない。LLM が応答しなければ文字照合へ戻る。
// ---------------------------------------------------------------------------

/// v1.12: 応答用は gemma3（Strata と GPU を分け合うと gemma4 は載らない・V1.12 計画 §3.2）。
const DEFAULT_AI_MODEL: &str = "gemma3:latest";
const DEFAULT_AI_ENDPOINT: &str = "http://localhost:11434";
/// 振り分け用モデルを載せたままにする時間（反射の速さのため・稼働開始で先読みする）。
const AI_KEEP_ALIVE: &str = "30m";

/// 応答用 AI（窓口）の接続。v1.12: AI カードと同じく Ollama / LM Studio / OpenAI 互換（Strata など）を選べる
/// （2026-10-23 ユーザー: モデルの入れ替え・調整を AI カードと同じ使い勝手に）。
/// カード設定のキーは ai_ 接頭辞（ai_provider / ai_model / ai_endpoint / ai_think / ai_temperature / ai_extra_params）。
#[derive(Clone, Debug)]
struct FrontAi {
    /// "ollama" / "lmstudio" / "openai"。
    provider: String,
    /// Ollama は http://host:11434、OpenAI 互換は /v1 まで（AI カードと同じ流儀）。
    ep: String,
    model: String,
    /// "off"（既定）/ "on" / "low" / "medium" / "high"。
    think: String,
    temperature: f64,
    /// リクエスト本体のトップレベルへ足す（AI カードの extra_params と同じ）。
    extra: serde_json::Map<String, serde_json::Value>,
}

impl FrontAi {
    #[cfg(test)]
    fn ollama(model: &str) -> Self {
        FrontAi {
            provider: "ollama".into(),
            ep: DEFAULT_AI_ENDPOINT.into(),
            model: model.into(),
            think: "off".into(),
            temperature: 0.0,
            extra: Default::default(),
        }
    }
    fn is_ollama(&self) -> bool {
        !matches!(self.provider.as_str(), "lmstudio" | "openai")
    }
}

/// AI 振り分けの接続。classifier="phrase" なら None（文字照合だけ）。
fn ai_conf(spec: &OpSpec) -> Option<FrontAi> {
    if extra_str(spec, "classifier").as_deref() == Some("phrase") {
        return None;
    }
    let provider = extra_str(spec, "ai_provider").unwrap_or_else(|| "ollama".into());
    let default_ep = if matches!(provider.as_str(), "lmstudio" | "openai") { "http://localhost:1234/v1" } else { DEFAULT_AI_ENDPOINT };
    let ep = extra_str(spec, "ai_endpoint").unwrap_or_else(|| default_ep.into());
    let model = extra_str(spec, "ai_model").unwrap_or_else(|| DEFAULT_AI_MODEL.into());
    let temperature = spec.extra.get("ai_temperature").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let extra = spec.extra.get("ai_extra_params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    Some(FrontAi {
        provider,
        ep: ep.trim_end_matches('/').to_string(),
        model,
        think: extra_str(spec, "ai_think").unwrap_or_else(|| "off".into()),
        temperature,
        extra,
    })
}

/// v1.12 思考役の設定（カード設定から）。think_backend="off" なら「じっくり考える」を出さない。
fn thinker_conf(spec: &OpSpec) -> crate::thinker::Conf {
    crate::thinker::Conf {
        backend: extra_str(spec, "think_backend").unwrap_or_else(|| "auto".into()),
        strata: extra_str(spec, "strata_endpoint")
            .unwrap_or_else(|| crate::thinker::DEFAULT_STRATA.into())
            .trim_end_matches('/')
            .to_string(),
        lmstudio: extra_str(spec, "lmstudio_endpoint")
            .unwrap_or_else(|| crate::thinker::DEFAULT_LMSTUDIO.into())
            .trim_end_matches('/')
            .to_string(),
        lmstudio_model: extra_str(spec, "lmstudio_model").unwrap_or_default(),
        effort: extra_str(spec, "think_effort").unwrap_or_else(|| "medium".into()),
    }
}

/// 1ターンで AI に選ばせてよいもの。
#[derive(Clone, Copy)]
struct TurnOpts {
    /// 「じっくり考える」（思考役へ回す）を選択肢に入れる。
    think: bool,
    /// 組み込み行動（止める・中止・リセット）を選択肢に入れる。
    sys: bool,
}

/// 会話エージェントの指示（system）。コマンドの説明はカード名・言い方の例・カードのコメントから作る。
///
/// 聞き取り中の状態は system に入れる（2026-10-23 実測: 状態を発話の頭へ移して system を固定にすると、
/// Strata の会話キャッシュは効きやすくなるが、gemma3 が聞き取り中のルールと取り違えて
/// 「以上」の前に実行する・英訳を会話にする、が出た。既定の gemma3 の正確さを優先する）。
fn agent_prompt(targets: &[Target], pending: Option<(&str, &[String])>, opts: TurnOpts) -> String {
    let mut s = String::from(
        "あなたは TexoS の音声アシスタントです。入力はユーザーの発話を音声認識した文字列です。\
         音声認識は同じ読みの別の字に化けます（例: メモ→目も、英訳→駅役）。読みが近ければ\
         そのコマンドのことだとみなします。\n\n\
         毎回、次の順で決めます。\n\
         1. action（何をするか）\n\
         2. command（コマンド名。関係なければ なし）\n\
         3. content（コマンドに渡す本文。無ければ空）\n\
         4. reply（読み上げる返事。日本語で1〜2文。Markdown・箇条書き・記号は使わない）\n\n\
         コマンド:\n",
    );
    for t in targets {
        s.push_str(&format!("- {}", t.label));
        let others: Vec<&String> = t.phrases.iter().filter(|p| **p != t.label).collect();
        if !others.is_empty() {
            s.push_str(&format!("（言い方の例: {}）", others.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("、")));
        }
        if let Some(m) = &t.memo {
            let d = describe(&t.label, m);
            if !d.is_empty() {
                s.push_str(&format!(": {d}"));
            }
        }
        s.push('\n');
    }
    if targets.is_empty() {
        s.push_str("（なし）\n");
    }
    match pending {
        None => {
            s.push_str(&format!(
                "\naction の選び方:\n\
                 - {ACT_RUN}: 頼む言葉と中身が一度に来たとき（例: メモして、牛乳を買う）。command にコマンド名、\
                   content に中身（頼む言葉は除く・ユーザーが話した内容だけ）。\n\
                 - {ACT_WAIT}: 頼む言葉だけで中身がまだ無いとき（例: メモして）。command に待つコマンド名を入れ、\
                   reply で短く促す（例: どうぞ。終わったら以上と言ってください）。\n\
                 - {ACT_CHAT}: コマンドと関係ない話・質問・説明。自然に短く返事する。\n"
            ));
            if opts.think {
                s.push_str(&format!(
                    "- {ACT_THINK}: その場で短く答えるには重い問い（比べて選ぶ・設計や計画を考える・理由を詳しく説明する・\
                       長い文章を書く）や、ユーザーが「よく考えて」「ちゃんと調べて」と頼んだとき。command は なし。\
                       content に考えてほしい問いを、話の流れを補って1〜2文で書く。reply は「考えておきます」程度の短い返事。\n"
                ));
            }
        }
        Some((label, parts)) => {
            s.push_str(&format!(
                "\nいまの状態: ユーザーは「{label}」の中身を話している途中です。ここまでの中身:\n"
            ));
            if parts.is_empty() {
                s.push_str("（まだ無し）\n");
            }
            for part in parts {
                s.push_str(&format!("・{part}\n"));
            }
            s.push_str(&format!(
                "\naction の選び方:\n\
                 - {ACT_WAIT}: この発話が中身の続きのとき。command は「{label}」、content にこの発話の中身\
                   （えーと 等のつなぎ言葉は除く）。reply は空か「はい」程度。\n\
                 - {ACT_RUN}: ユーザーが言い終えた合図をしたとき（以上・終わり・メモ終了・それで全部 など）。\
                   command は「{label}」。この発話に中身も含まれていれば content に、無ければ空。\n\
                 - {ACT_CHAT}: 中身とは関係のない質問など。\n"
            ));
        }
    }
    if opts.sys {
        for (name, _, desc) in SYS_ACTIONS {
            s.push_str(&format!("- {name}: {desc}。reply は空でよい。\n"));
        }
    }
    s
}

/// 組み込みの行動（カードではなく音声カード自身が行う）。2026-10-11 ユーザー方針で、これも
/// 文字の一致ではなく AI が発話全体から分類する（文字判定は AI が応答しないときの予備）。
#[derive(Clone, Copy, Debug, PartialEq)]
enum SysAction {
    /// 読み上げを止め、溜まった報告を捨てる。
    Hush,
    /// 音声から起動した実行中のフローを中断する。
    Cancel,
    /// 会話履歴を消す。
    Reset,
}

/// AI に選ばせる行動（action の enum）。カードの実行・続きを待つ・ただの会話・組み込み行動。
/// 実測（2026-10-11）: 行動とカード名を1つの欄に混ぜると、返事では「止めますね」と言いながら
/// 欄は none のまま、が起きた。行動を独立の欄にし、**返事より先に**決めさせる。
const ACT_RUN: &str = "コマンドを実行";
const ACT_WAIT: &str = "続きを待つ";
const ACT_CHAT: &str = "会話";
/// v1.12: 思考役（Strata → LM Studio）へ回す。
const ACT_THINK: &str = "じっくり考える";
const SYS_ACTIONS: &[(&str, SysAction, &str)] = &[
    ("読み上げを止める", SysAction::Hush, "読み上げや返事を止めてほしい・黙ってほしい（止めて・ストップ・静かに など）"),
    ("実行を中止する", SysAction::Cancel, "実行中の処理そのものをやめてほしい（全部止めて・中止して・キャンセル など）"),
    ("会話をリセットする", SysAction::Reset, "これまでの会話を忘れて最初からにしてほしい（リセット・やり直して など）"),
];

/// v1.12: 思考役へ回す合図の言葉（既定）。gemma3 は「じっくり考える」をほとんど選ばず自分で短く
/// 答えてしまった（実測 2026-10-23）ので、会話と判定されてもこれらを含めば思考役へ回す。
const DEFAULT_THINK_WORDS: &[&str] = &["よく考えて", "じっくり", "詳しく", "比べて", "比較して", "調べて", "検討して"];

/// 会話と判定された発話でも、合図の言葉があれば「じっくり考える」に格上げする（中身は発話全文）。
fn promote_think(out: &mut AgentOut, text: &str, words: &[String]) {
    // 「実行」「待つ」でもコマンド名が無い（なし）なら実際には会話と同じ扱い（実測 2026-10-23: gemma3 が
    // 「…よく考えて教えて」を action=実行・command=なし で返した）。
    let chat_like = out.action == ACT_CHAT || ((out.action == ACT_RUN || out.action == ACT_WAIT) && out.cmd.is_none());
    if out.sys.is_some() || !chat_like {
        return;
    }
    let t = folded(text);
    if words.iter().any(|w| !w.is_empty() && t.contains(&folded(w))) {
        out.action = ACT_THINK.into();
        out.content = text.trim().to_string();
        out.reply = "考えておきます".into();
    }
}

/// v1.12: AI が組み込み行動を選んだときの裏付け。発話にそれらしい言葉が無ければ選び直させる
/// （実測 2026-10-23: gemma3 は「英語で要約して」「文字数を数えて、…」を「実行を中止する」にした。
/// 中止は実行中のフローを止めるので、取り違えの害が大きい）。
fn sys_backed(a: SysAction, text: &str) -> bool {
    let t = folded(text);
    let stems: &[&str] = match a {
        SysAction::Hush => &["止め", "とめ", "ストップ", "すとっぷ", "静か", "しずか", "黙", "だま", "やめ", "待って", "まって"],
        SysAction::Cancel => &["止め", "とめ", "中止", "ちゅうし", "キャンセル", "きゃんせる", "やめ", "ストップ", "すとっぷ"],
        SysAction::Reset => &["リセット", "りせっと", "やり直", "やりなお", "最初から", "さいしょから", "忘れ", "わすれ"],
    };
    stems.iter().any(|s| t.contains(&folded(s)))
}

/// 1ターンの結果: 読み上げる返事と、実行するコマンド（targets の添字・本文）か組み込み行動。
struct AgentOut {
    reply: String,
    /// ACT_RUN / ACT_WAIT / ACT_CHAT（組み込み行動のときは sys）。
    action: String,
    /// command が矢印の先のカード名なら、その targets の添字。
    cmd: Option<usize>,
    content: String,
    sys: Option<SysAction>,
}

/// 直近の会話履歴を文字数の上限に収める（古い方から落とす）。
fn bounded_history(h: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut len = 0;
    for m in h.iter().rev() {
        if !out.is_empty() && len + m.1.chars().count() > HISTORY_CHARS {
            break;
        }
        len += m.1.chars().count();
        out.push(m.clone());
    }
    out.reverse();
    out
}

/// 会話エージェントの1ターン（VoiceAssistantAgent の respond に、コマンドの選択を足したもの）。
/// command は enum の制約付き出力（矢印の先のカード名＋none）＝選択肢の外は出ない。
fn agent_turn(
    ai: &FrontAi,
    history: &[(String, String)],
    text: &str,
    targets: &[Target],
    pending: Option<(&str, &[String])>,
    opts: TurnOpts,
) -> Result<AgentOut, String> {
    let out = agent_turn_once(ai, history, text, targets, pending, opts)?;
    match out.sys {
        // 裏付けの無い組み込み行動は捨て、組み込み行動を外して選び直させる（§sys_backed）。
        Some(a) if opts.sys && !sys_backed(a, text) => {
            eprintln!("[voice] AI の {a:?} は発話に裏付けが無いので選び直し: {text}");
            agent_turn_once(ai, history, text, targets, pending, TurnOpts { sys: false, ..opts })
        }
        _ => Ok(out),
    }
}

fn agent_turn_once(
    ai: &FrontAi,
    history: &[(String, String)],
    text: &str,
    targets: &[Target],
    pending: Option<(&str, &[String])>,
    opts: TurnOpts,
) -> Result<AgentOut, String> {
    let mut labels: Vec<String> = Vec::new();
    for t in targets {
        if !labels.contains(&t.label) {
            labels.push(t.label.clone());
        }
    }
    let mut commands = labels.clone();
    commands.push("なし".into());
    let mut actions: Vec<String> = vec![ACT_RUN.into(), ACT_WAIT.into(), ACT_CHAT.into()];
    if opts.think && pending.is_none() {
        actions.push(ACT_THINK.into());
    }
    if opts.sys {
        actions.extend(SYS_ACTIONS.iter().map(|(n, _, _)| n.to_string()));
    }
    let mut messages =
        vec![serde_json::json!({ "role": "system", "content": agent_prompt(targets, pending, opts) })];
    for (role, content) in bounded_history(history) {
        messages.push(serde_json::json!({ "role": role, "content": content }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": text }));
    // 並び順＝生成順。行動を先に決めさせ、返事は最後（返事に引っ張られて欄を外さない）。
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": actions },
            "command": { "type": "string", "enum": commands },
            "content": { "type": "string" },
            "reply": { "type": "string" }
        },
        "required": ["action", "command", "content", "reply"],
        "additionalProperties": false
    });
    let thinking = ai.think != "off";
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(if thinking { 180 } else { 60 }))
        .build()
        .map_err(|e| e.to_string())?;
    let content = if ai.is_ollama() {
        let mut body = serde_json::json!({
            "model": ai.model,
            "stream": false,
            "keep_alive": AI_KEEP_ALIVE,
            // 既定は思考を切る（実測: 思考ありの gemma4 は format を外れた素の文を返すことがあった）。
            "think": match ai.think.as_str() { "off" => serde_json::json!(false), "on" => serde_json::json!(true), l => serde_json::json!(l) },
            "format": schema,
            "options": { "temperature": ai.temperature, "num_predict": if thinking { 4000 } else { 400 }, "num_ctx": 8192 },
            "messages": messages
        });
        merge_extra(&mut body, &ai.extra);
        let url = format!("{}/api/chat", ai.ep);
        let mut resp = client.post(&url).json(&body).send().map_err(|e| format!("AI に接続できません: {e}"))?;
        if !resp.status().is_success() {
            // think を知らない古いサーバ・モデル向けにもう一度（think 抜き）。
            if let Some(o) = body.as_object_mut() {
                o.remove("think");
            }
            resp = client.post(&url).json(&body).send().map_err(|e| format!("AI に接続できません: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("AI がエラーを返しました（{}）", resp.status()));
            }
        }
        let v: serde_json::Value = resp.json().map_err(|e| format!("AI の応答を読めません: {e}"))?;
        v.pointer("/message/content").and_then(|c| c.as_str()).unwrap_or("").to_string()
    } else {
        // OpenAI 互換（LM Studio / Strata など）: response_format の json_schema で形を縛る
        // （Strata は strict に従う・2026-10-23 実測）。
        let mut body = serde_json::json!({
            "model": ai.model,
            "stream": false,
            "temperature": ai.temperature,
            "max_tokens": if thinking { 4000 } else { 400 },
            "response_format": { "type": "json_schema",
                "json_schema": { "name": "voice_turn", "strict": true, "schema": schema } },
            "messages": messages
        });
        // 汎用 OpenAI 互換（Strata など）は思考の深さを reasoning_effort で（"on" はサーバ既定に任せて送らない）。
        // LM Studio には送らない。
        if ai.provider == "openai" && ai.think != "on" {
            let effort = if ai.think == "off" { "none" } else { ai.think.as_str() };
            body["reasoning_effort"] = serde_json::Value::String(effort.into());
        }
        merge_extra(&mut body, &ai.extra);
        let url = format!("{}/chat/completions", ai.ep);
        let mut resp = client.post(&url).json(&body).send().map_err(|e| format!("AI に接続できません: {e}"))?;
        if !resp.status().is_success() {
            // json_schema・reasoning_effort を知らないサーバ向けにもう一度（json_object・思考指定なし）。
            if let Some(o) = body.as_object_mut() {
                o.remove("reasoning_effort");
                o.insert("response_format".into(), serde_json::json!({ "type": "json_object" }));
            }
            resp = client.post(&url).json(&body).send().map_err(|e| format!("AI に接続できません: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("AI がエラーを返しました（{}）", resp.status()));
            }
        }
        let v: serde_json::Value = resp.json().map_err(|e| format!("AI の応答を読めません: {e}"))?;
        v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("").to_string()
    };
    let content = strip_think_tags(&content);
    let content = content.as_str();
    let a = parse_agent(content, &labels);
    let sys = SYS_ACTIONS.iter().find(|(n, _, _)| *n == a.action).map(|(_, x, _)| *x);
    let cmd = targets.iter().position(|t| t.label == a.command);
    Ok(AgentOut { reply: a.reply, action: a.action, cmd, content: a.content, sys })
}

/// 聞き取り中の1発話から溜める中身。
/// - 続き（ACT_WAIT）: 発話の原文そのもの（AI に言い直させると前の分まで繰り返して重複した・実測）。
/// - 終わりの合図（ACT_RUN）: AI が拾った中身のうち、まだ溜めていないものだけ（「…、以上」の「…」）。
fn pending_part(action: &str, text: &str, content: &str, parts: &[String]) -> String {
    if action == ACT_WAIT {
        return text.trim().to_string();
    }
    let c = content.trim();
    let n = normalize(c);
    if n.is_empty() {
        return String::new();
    }
    let joined = normalize(&parts.join(""));
    let overlaps = joined.contains(&n) || parts.iter().any(|p| !normalize(p).is_empty() && n.contains(&normalize(p)));
    if overlaps { String::new() } else { c.to_string() }
}

/// エージェントの応答（生の欄）。
#[derive(Debug, Default, PartialEq)]
struct AgentRaw {
    action: String,
    command: String,
    content: String,
    reply: String,
}

/// エージェントの応答を読む。JSON が基本。崩れていたら、本文にあるコマンド名を拾って実行扱い
/// （本文は空）、無ければ会話扱い。
fn parse_agent(content: &str, labels: &[String]) -> AgentRaw {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(content.trim()) {
        let f = |k: &str| v.get(k).and_then(|c| c.as_str()).unwrap_or("").trim().to_string();
        return AgentRaw { action: f("action"), command: f("command"), content: f("content"), reply: f("reply") };
    }
    let mut hits: Vec<&String> = labels.iter().filter(|l| content.contains(l.as_str())).collect();
    hits.sort_by_key(|l| std::cmp::Reverse(l.chars().count()));
    match hits.first() {
        Some(l) => AgentRaw { action: ACT_RUN.into(), command: (*l).clone(), ..Default::default() },
        None => AgentRaw { action: ACT_CHAT.into(), ..Default::default() },
    }
}

/// 追加パラメータ（ai_extra_params）を本体へ足す。管理しているキーは上書きさせない（AI カードと同じ）。
fn merge_extra(body: &mut serde_json::Value, extra: &serde_json::Map<String, serde_json::Value>) {
    const RESERVED: &[&str] =
        &["model", "messages", "stream", "temperature", "think", "reasoning_effort", "options", "format", "response_format"];
    if let Some(o) = body.as_object_mut() {
        for (k, v) in extra {
            if !RESERVED.contains(&k.as_str()) {
                o.insert(k.clone(), v.clone());
            }
        }
    }
}

/// 本文に混ざった <think>…</think> を落とす（思考モデルの OpenAI 互換サーバ向け）。
fn strip_think_tags(s: &str) -> String {
    let mut out = s.to_string();
    while let (Some(a), Some(b)) = (out.find("<think>"), out.find("</think>")) {
        if b < a {
            break;
        }
        out.replace_range(a..b + "</think>".len(), "");
    }
    out
}

/// 稼働開始時に振り分け用モデルを先に載せる（1発目の待ちを消す・失敗は黙って無視）。
fn warm_up(ep: String, model: String) {
    std::thread::spawn(move || {
        if let Ok(c) = reqwest::blocking::Client::builder().timeout(Duration::from_secs(120)).build() {
            let _ = c
                .post(format!("{ep}/api/generate"))
                .json(&serde_json::json!({ "model": model, "keep_alive": AI_KEEP_ALIVE }))
                .send();
        }
    });
}

// ---------------------------------------------------------------------------
// 音声プロセスとの通信
// ---------------------------------------------------------------------------

/// カード面の表示の控え（パネルを開き直したとき・画面を再読込したときに今の状態を出す）。
/// rt() とは別のロック（rt を握ったまま emit_event を呼ぶ箇所があるため）。
#[derive(Default, Clone, serde::Serialize)]
pub(crate) struct CardStatus {
    state: String,
    message: String,
    last: String,
    /// v1.12 思考役の状態の1行（thinker_line）。
    thinker: String,
}

/// 思考役の状態の表示（パネルと控えで同じ文言）。
fn thinker_line(v: &serde_json::Value) -> String {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let head = s("head");
    if head.is_empty() {
        return "🧠 思考役なし（Strata も LM Studio も使えません）".into();
    }
    let state = match s("state").as_str() {
        "idle" | "ready" => "待機中",
        "reading" => "読み込み中",
        "generating" => "生成中",
        other => return format!("🧠 {head}：{other}"),
    };
    let mut line = format!("🧠 {head}：{state}");
    if n("queued") > 0 {
        line.push_str(&format!("（{}件待ち）", n("queued")));
    }
    if n("busy") > 0 {
        line.push_str(&format!("・考え中 {}件", n("busy")));
    }
    line
}

fn status_cache() -> &'static Mutex<HashMap<String, CardStatus>> {
    static C: OnceLock<Mutex<HashMap<String, CardStatus>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn emit_event(app: &tauri::AppHandle, card: &str, mut v: serde_json::Value) {
    {
        let mut c = status_cache().lock().unwrap();
        let st = c.entry(card.to_string()).or_default();
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        match v.get("ev").and_then(|e| e.as_str()) {
            Some("state") => {
                st.state = s("state");
                st.message = s("message");
            }
            Some("error") => st.message = s("message"),
            Some("thinker") => st.thinker = thinker_line(&v),
            Some("utterance") => st.last = format!("「{}」", s("text")),
            Some("speaking") => st.last = format!("🔊 {}", s("text")),
            Some("reflex") => match s("kind").as_str() {
                "fire" => st.last = format!("→ {}（{}）", s("label"), s("content")),
                "classifying" => st.last = format!("「{}」→ 考え中…", s("text")),
                "reply" => st.last = format!("「{}」", s("text")),
                "gather" => {
                    let n = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
                    st.last = format!("📝 {}：聞き取り中（{n}件）— 「以上」で確定", s("label"));
                }
                "think" => st.last = format!("🧠 考え中：{}", s("text")),
                "thought" => st.last = format!("🧠 {}", s("text")),
                "reset" => st.last = "（会話をリセット）".into(),
                "nomatch" => st.last = format!("「{}」（該当なし）", s("text")),
                "hush" => st.last = "（読み上げ停止）".into(),
                "cancel" => st.last = "（実行を中止）".into(),
                _ => {}
            },
            _ => {}
        }
    }
    if let Some(o) = v.as_object_mut() {
        o.insert("card".into(), serde_json::Value::String(card.to_string()));
    }
    let _ = app.emit("voice:event", v);
}

/// 外からのテキストを、話しかけたのと同じ扱いで会話へ入れる（v1.10・2026-10-11 ユーザー要望）。
/// 入口は2つ: 状態パネルのテキスト欄（source="text"）と「会話へ送る」カード（source="card"）。
/// 文字起こしの後と同じ handle_utterance に入る＝脳は1つ・感覚器だけが増える。
/// card=None なら稼働中の会話コマンドへ（マイクは1つ＝稼働は1枚）。
pub(crate) fn voice_inject(card: Option<&str>, text: &str, source: &'static str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("送るテキストが空です".into());
    }
    let (c, root) = {
        let g = rt().lock().unwrap();
        let p = g.proc.as_ref().ok_or(
            "稼働中の会話コマンドがありません（音声カードの「▶ 稼働開始」を押してください）",
        )?;
        if card.map(|c| c != p.card).unwrap_or(false) {
            return Err("この音声カードは稼働していません".into());
        }
        (p.card.clone(), p.root.clone())
    };
    let app = APP.get().ok_or("アプリの準備ができていません")?.clone();
    // パネルに「聞こえた」ものとして出す（マイクでは音声プロセスが出す utterance と同じ形）。
    emit_event(&app, &c, serde_json::json!({ "ev": "utterance", "text": text, "source": source }));
    let t = text.to_string();
    std::thread::spawn(move || handle_utterance(&app, &c, &root, &t, source));
    Ok(())
}

/// 状態パネルのテキスト欄から。
#[tauri::command]
pub(crate) fn voice_send_text(card: String, text: String) -> Result<(), String> {
    voice_inject(Some(&card), &text, "text")
}

/// カード面の初期表示用（VoiceStatus のマウント時）。未稼働なら null。
#[tauri::command]
pub(crate) fn voice_status(card: String) -> Option<CardStatus> {
    status_cache().lock().unwrap().get(&card).cloned()
}

/// 環境構築の直後など、再起動待ち（RETRY_SECS）を解いて次の tick ですぐ起動させる。
#[tauri::command]
pub(crate) fn voice_retry() {
    rt().lock().unwrap().backoff = None;
}

/// 発話の受け皿カード（書き出し先ファイルのカード）を音声カードの右に置き、音声カードから
/// 「書き出し」の矢印を引く。下に人が読む発話ログ（発話ログ.md）のカードも置く。
/// ファイルが無ければ空で作る。既に同じファイルのカードがあればそれを返す（増やさない）。
/// 音声カードの作成直後と、カードの右クリック「発話の受け皿カードを置く」から呼ぶ。
#[tauri::command]
pub(crate) fn voice_ensure_inbox(id: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let (spec_json, x, y): (String, f64, f64) = p
        .conn
        .query_row(
            "SELECT filter_ref, pos_x, pos_y FROM virtual_bodies
             WHERE id = ?1 AND kind = 'op' AND deleted_at IS NULL",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("音声カードが見つかりません")?;
    let spec = parse_op_spec(&spec_json)?;
    let rel = extra_str(&spec, "say_out").unwrap_or_else(|| DEFAULT_SAY_OUT.to_string());
    let path = resolve_project_path(&p.root, Some(&rel));
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("書き出し先のフォルダを作れません: {e}"))?;
        }
        std::fs::write(&path, "").map_err(|e| format!("書き出し先のファイルを作れません: {e}"))?;
    }
    let path_s = path.to_string_lossy().into_owned();
    let existing: Option<String> = p
        .conn
        .query_row(
            // 相対保存（v1.9.1〜）と絶対保存（旧カード・ビルダー製）の両方を見る。
            "SELECT id FROM virtual_bodies WHERE committed_path IN (?1, ?2) AND deleted_at IS NULL
             ORDER BY created_at LIMIT 1",
            rusqlite::params![store_path(&p.root, &path_s), path_s],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let inbox = match existing {
        Some(v) => v,
        None => import_file_into(p, path_s, String::new(), x + 190.0, y)?,
    };
    // 音声カード → 受け皿 の矢印（「書き出し」）。これが受け皿を決める（inbox_path）。
    p.conn
        .execute(
            "INSERT INTO relations (from_id, to_id, kind, attrs, created_at)
             SELECT ?1, ?2, 'req', '{\"role\":\"inbox\"}', ?3
             WHERE NOT EXISTS (SELECT 1 FROM relations
                 WHERE from_id = ?1 AND to_id = ?2 AND kind = 'req' AND deleted_at IS NULL)",
            rusqlite::params![id, inbox, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    // 人が読む発話ログ（受け皿の隣・追記）のカードを受け皿の下に。
    if let Some(dir) = path.parent() {
        let md = dir.join(LOG_MD_NAME);
        if !md.exists() {
            std::fs::write(&md, "# 発話ログ\n\n").map_err(|e| format!("発話ログを作れません: {e}"))?;
        }
        let md_s = md.to_string_lossy().into_owned();
        let has: bool = p
            .conn
            .query_row(
                "SELECT 1 FROM virtual_bodies WHERE committed_path IN (?1, ?2) AND deleted_at IS NULL LIMIT 1",
                rusqlite::params![store_path(&p.root, &md_s), md_s],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .is_some();
        if !has {
            import_file_into(p, md_s, String::new(), x + 190.0, y + 160.0)?;
        }
    }
    Ok(inbox)
}

fn send_locked(rt: &mut VoiceRt, cmd: serde_json::Value) {
    if let Some(p) = rt.proc.as_mut() {
        let _ = writeln!(p.stdin, "{cmd}");
        let _ = p.stdin.flush();
    }
}

fn send(cmd: serde_json::Value) {
    send_locked(&mut rt().lock().unwrap(), cmd);
}

/// 報告を読み上げ待ち行列へ（priority: "urgent" | "normal" | "low"）。
/// card が今の音声プロセスの持ち主でなければ黙って捨てる（停止後に終わったフロー等）。
fn say(card: &str, text: &str, priority: &str) {
    let mut g = rt().lock().unwrap();
    if g.proc.as_ref().map(|p| p.card != card).unwrap_or(true) {
        return;
    }
    g.say_seq += 1;
    let id = format!("s{}", g.say_seq);
    send_locked(&mut g, serde_json::json!({ "cmd": "say", "id": id, "text": text, "priority": priority }));
}

fn stop_proc(app: &tauri::AppHandle, rt: &mut VoiceRt) {
    let Some(mut p) = rt.proc.take() else { return };
    let _ = writeln!(p.stdin, "{}", serde_json::json!({ "cmd": "stop" }));
    let _ = p.stdin.flush();
    drop(p.stdin);
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        match p.child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = p.child.kill();
                let _ = p.child.wait();
                break;
            }
        }
    }
    MIC_CLOSED.store(true, Ordering::SeqCst);
    emit_event(app, &p.card, serde_json::json!({ "ev": "state", "state": "stopped" }));
}

fn spawn_proc(
    app: &tauri::AppHandle,
    rt: &mut VoiceRt,
    root: &str,
    card: &str,
    sig: &str,
    args: &[String],
) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let app_data = state.app_data.clone();
    let py = env_python(&app_data, VOICE_ENV);
    if !py.is_file() {
        return Err("no_env".into());
    }
    let home = voice_home(&app_data);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let script = home.join("voice_server.py");
    std::fs::write(&script, VOICE_SERVER_PY)
        .map_err(|e| format!("音声プロセスの書き出しに失敗: {e}"))?;
    let mut c = std::process::Command::new(&py);
    c.arg(&script)
        .args(["--home", &home.to_string_lossy()])
        .args(args)
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    match std::fs::File::create(home.join("server.log")) {
        Ok(f) => c.stderr(std::process::Stdio::from(f)),
        Err(_) => c.stderr(std::process::Stdio::null()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = c.spawn().map_err(|e| format!("音声プロセスの起動に失敗: {e}"))?;
    let stdin = child.stdin.take().ok_or("音声プロセスの stdin を取れません")?;
    let stdout = child.stdout.take().ok_or("音声プロセスの stdout を取れません")?;
    rt.gen += 1;
    let gen = rt.gen;
    rt.history.remove(card); // 稼働開始＝新しい会話
    rt.pending.remove(card);
    rt.proc = Some(VoiceProc {
        card: card.to_string(),
        root: root.to_string(),
        sig: sig.to_string(),
        gen,
        child,
        stdin,
    });
    MIC_CLOSED.store(false, Ordering::SeqCst);
    // 録音カードが今まさにマイクを使っているなら、開いた直後に譲らせる。
    if MIC_YIELDS.load(Ordering::SeqCst) > 0 {
        send_locked(rt, serde_json::json!({ "cmd": "pause" }));
    }
    let (app2, card2, root2) = (app.clone(), card.to_string(), root.to_string());
    std::thread::spawn(move || read_events(app2, card2, root2, gen, stdout));
    Ok(())
}

/// 音声プロセスの stdout（JSON 行）を読み、画面へ中継し、発話を反射弓へ回す。
fn read_events(
    app: tauri::AppHandle,
    card: String,
    root: String,
    gen: u64,
    out: std::process::ChildStdout,
) {
    for line in std::io::BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match v.get("ev").and_then(|e| e.as_str()) {
            Some("state") => match v.get("state").and_then(|s| s.as_str()) {
                Some("paused") => MIC_CLOSED.store(true, Ordering::SeqCst),
                Some("listening") | Some("recording") => MIC_CLOSED.store(false, Ordering::SeqCst),
                _ => {}
            },
            Some("utterance") => {
                if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                    let (a, c, r, t) = (app.clone(), card.clone(), root.clone(), text.to_string());
                    std::thread::spawn(move || handle_utterance(&a, &c, &r, &t, "mic"));
                }
            }
            Some("error") => {
                eprintln!("[voice] {}", v.get("message").and_then(|m| m.as_str()).unwrap_or(""));
            }
            _ => {}
        }
        emit_event(&app, &card, v);
    }
    // EOF: 停止要求（stop_proc が先に proc を外す）なら gen が合わない。合うなら異常終了。
    let mut g = rt().lock().unwrap();
    if g.proc.as_ref().map(|p| p.gen == gen).unwrap_or(false) {
        if let Some(mut p) = g.proc.take() {
            let _ = p.child.wait();
            g.backoff = Some((format!("{}|{}", p.card, p.sig), Instant::now()));
        }
        MIC_CLOSED.store(true, Ordering::SeqCst);
        emit_event(
            &app,
            &card,
            serde_json::json!({ "ev": "state", "state": "error",
                "message": "音声プロセスが終了しました（詳細は app_data の voice/server.log）。設定を変えるか、しばらくすると再起動します" }),
        );
    }
}

// ---------------------------------------------------------------------------
// 生死の突き合わせ（ops.rs のスケジューラから毎秒）
// ---------------------------------------------------------------------------

/// 現在のプロジェクトの稼働中の音声カード（最初の1枚）＝ (root, id, spec)。
fn desired(app: &tauri::AppHandle) -> Option<(String, String, OpSpec, String)> {
    let state: State<AppState> = app.state();
    let guard = state.current.lock().ok()?;
    let p = guard.as_ref()?;
    let root = p.root.to_string_lossy().into_owned();
    let mut stmt = p
        .conn
        .prepare(
            "SELECT id, filter_ref FROM virtual_bodies
             WHERE kind = 'op' AND deleted_at IS NULL AND filter_ref IS NOT NULL
             ORDER BY created_at",
        )
        .ok()?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    let (id, s) = rows.into_iter().find_map(|(id, json)| {
        let s = parse_op_spec(&json).ok()?;
        (s.op == "voice" && !s.paused).then_some((id, s))
    })?;
    // 言い回し（矢印の先のカード名）が変わったら hotwords も変わる＝音声プロセスを起動し直す。
    let hot = hotwords_of(&s, &targets_of(&p.conn, &id));
    Some((root, id, s, hot))
}

pub(crate) fn voice_tick(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
    let want = desired(app);
    let mut g = rt().lock().unwrap();
    let Some((root, id, spec, hot)) = want else {
        stop_proc(app, &mut g);
        return;
    };
    // v1.12: 思考役の状態を見回る（状態パネルの「🧠 Strata：生成中（2件待ち）」）。
    {
        static LAST: Mutex<Option<Instant>> = Mutex::new(None);
        let mut last = LAST.lock().unwrap();
        if g.proc.is_some() && last.map(|t| t.elapsed() >= THINKER_POLL).unwrap_or(true) {
            *last = Some(Instant::now());
            emit_thinker(app, &id, &thinker_conf(&spec));
        }
    }
    let args = proc_args(&spec, &hot);
    let sig = args.join(" ");
    if let Some(p) = g.proc.as_mut() {
        let alive = matches!(p.child.try_wait(), Ok(None));
        if alive && p.card == id && p.root == root && p.sig == sig {
            return;
        }
    }
    stop_proc(app, &mut g);
    let key = format!("{id}|{sig}");
    if let Some((k, t)) = &g.backoff {
        if *k == key && t.elapsed() < Duration::from_secs(RETRY_SECS) {
            return;
        }
    }
    // 先に「起動中」を出す（後から来る音声プロセスの preparing を上書きしない順序）。
    emit_event(app, &id, serde_json::json!({ "ev": "state", "state": "starting" }));
    match spawn_proc(app, &mut g, &root, &id, &sig, &args) {
        Ok(()) => {
            g.backoff = None;
            if extra_str(&spec, "mode").as_deref() != Some("dictation") {
                // 先読みは Ollama だけ（OpenAI 互換は読み込み済みのモデルを使う）。
                if let Some(ai) = ai_conf(&spec).filter(|a| a.is_ollama()) {
                    warm_up(ai.ep, ai.model);
                }
            }
        }
        Err(e) => {
            g.backoff = Some((key, Instant::now()));
            let v = if e == "no_env" {
                serde_json::json!({ "ev": "state", "state": "no_env",
                    "message": "音声環境がありません。カードの「音声環境を構築」で導入します（数百MB）" })
            } else {
                serde_json::json!({ "ev": "state", "state": "error", "message": e })
            };
            emit_event(app, &id, v);
        }
    }
}

// ---------------------------------------------------------------------------
// 録音カードへのマイク譲渡（§5-4 録音カード優先）
// ---------------------------------------------------------------------------

/// 録音ジェネレータの実行中だけ持つ。最初の1つで音声プロセスにマイクを閉じさせ、
/// 最後の1つが落ちたら開き直させる。
pub(crate) struct MicYield;

pub(crate) fn voice_mic_yield() -> MicYield {
    if MIC_YIELDS.fetch_add(1, Ordering::SeqCst) == 0 {
        let running = {
            let mut g = rt().lock().unwrap();
            send_locked(&mut g, serde_json::json!({ "cmd": "pause" }));
            g.proc.is_some()
        };
        // 閉じ終わるまで少し待つ（ffmpeg が先にデバイスを開きにいかないように）。
        let deadline = Instant::now() + Duration::from_millis(1500);
        while running && !MIC_CLOSED.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    MicYield
}

impl Drop for MicYield {
    fn drop(&mut self) {
        if MIC_YIELDS.fetch_sub(1, Ordering::SeqCst) == 1 {
            send(serde_json::json!({ "cmd": "resume" }));
        }
    }
}

// ---------------------------------------------------------------------------
// 反射弓: 発話 → 照合 → 発火 → 報告
// ---------------------------------------------------------------------------

/// 発話ログ（§5-5 スーパーバイザーの声も含めて全部残す）。どちらもプロジェクト内・受け皿と同じフォルダ:
/// 人が読む 発話ログ.md と、機械が読む 発話ログ.jsonl（1行1件・time/role/text＋付帯情報）。
fn log_utterance(card: &str, root: &Path, role: &str, text: &str, extra: serde_json::Value) {
    // 人が読む md（受け皿の隣の 発話ログ.md）。
    {
        let src = rt().lock().unwrap().log_ctx.get(card).map(|c| c.1).unwrap_or("mic");
        let mark = match src {
            "text" => "⌨",
            "card" => "📄",
            _ => "🎙",
        };
        let g = |k: &str| extra.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut lines = vec![format!("- {} {mark}「{text}」", md_time())];
        let reply = g("reply");
        if !reply.is_empty() {
            lines.push(format!("  - 🤖 {reply}"));
        }
        match role {
            "supervisor" => lines.push(format!("  - ⏹ {}", g("action"))),
            "think" => lines.push(format!("  - 🧠 思考役へ：{}", g("question"))),
            "gather" if extra.get("start").is_some() => lines.push(format!("  - 📝 {}：聞き取り開始", g("target"))),
            "gather" => lines.push(format!("  - 📝 {}：追記", g("target"))),
            "command" | "dictation" => {
                let targets: Vec<String> = extra
                    .get("targets")
                    .and_then(|t| t.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if !targets.is_empty() {
                    lines.push(format!("  - ▶ {} ←「{}」", targets.join("・"), g("content")));
                }
                if !g("skipped").is_empty() {
                    lines.push(format!("  - （見送り: {}）", g("skipped")));
                }
            }
            _ => {}
        }
        md_append(card, &lines.join("\n"));
    }
    // 機械用の JSONL もプロジェクトの資料として 発話ログ.md の隣へ（2026-10-11 ユーザー:
    // .texos/logs はシステム側の領域＝プロジェクトの記録として見えない）。
    let md = rt().lock().unwrap().log_ctx.get(card).map(|c| c.0.clone());
    let jsonl = md
        .map(|m| m.with_extension("jsonl"))
        .unwrap_or_else(|| resolve_project_path(root, Some(DEFAULT_SAY_OUT)).with_file_name(LOG_JSONL_NAME));
    if let Some(dir) = jsonl.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let now = chrono::Local::now();
    let mut v = serde_json::json!({ "time": now.to_rfc3339(), "role": role, "text": text });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in e {
            o.insert(k.clone(), val.clone());
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jsonl)
    {
        let _ = writeln!(f, "{v}");
    }
}

/// 受け皿（発話の中身の書き出し先）: **音声カードからファイルカードへ引いた矢印の先**
/// （2026-10-11 実機FB: 何もつながっていないと無関係に見える）。矢印が無ければ設定の say_out。
fn inbox_path(conn: &rusqlite::Connection, root: &Path, card: &str, spec: &OpSpec) -> PathBuf {
    let by_arrow: Option<String> = conn
        .query_row(
            "SELECT v.committed_path FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.to_id AND v.deleted_at IS NULL
             WHERE rel.from_id = ?1 AND rel.kind = 'req' AND rel.deleted_at IS NULL
               AND v.committed_path IS NOT NULL AND COALESCE(v.kind, 'file') = 'file'
             ORDER BY rel.created_at LIMIT 1",
            rusqlite::params![card],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    match by_arrow {
        Some(p) => PathBuf::from(load_path(root, &p)),
        None => {
            let rel = extra_str(spec, "say_out").unwrap_or_else(|| DEFAULT_SAY_OUT.to_string());
            resolve_project_path(root, Some(&rel))
        }
    }
}

/// 中身を受け皿へ（一時ファイル→置き換え＝書きかけを読ませない）。
fn write_say_out(path: &Path, content: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("発話の書き出し先を作れません: {e}"))?;
    }
    let tmp = path.with_extension("tmp~");
    std::fs::write(&tmp, content).map_err(|e| format!("発話を書き出せません: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("発話を書き出せません: {e}"))?;
    Ok(())
}

/// 人が読む発話ログ（md）へ1行追記する。
fn md_append(card: &str, line: &str) {
    let Some((md, _)) = rt().lock().unwrap().log_ctx.get(card).cloned() else { return };
    if let Some(dir) = md.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let fresh = !md.exists() || std::fs::metadata(&md).map(|m| m.len() == 0).unwrap_or(true);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&md) {
        if fresh {
            let _ = writeln!(f, "# 発話ログ\n");
        }
        let _ = writeln!(f, "{line}");
    }
}

fn md_time() -> String {
    chrono::Local::now().format("%m/%d %H:%M:%S").to_string()
}

fn short(s: &str, n: usize) -> String {
    let first = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = first.chars().take(n).collect();
    if first.chars().count() > n {
        out.push('…');
    }
    out
}

/// 文字判定の予備（AI を使わない設定・口述モード・AI が応答しないとき）。
/// 「全部止めて」が「止めて」に食われないよう、中止を先に見る。
fn word_action(spec: &OpSpec, text: &str) -> Option<SysAction> {
    let hit = |key: &str, d: &[&str]| extra_list(spec, key, d).iter().any(|w| supervisor_hit(text, w));
    if hit("cancel_words", DEFAULT_CANCEL) {
        Some(SysAction::Cancel)
    } else if hit("hush_words", DEFAULT_HUSH) {
        Some(SysAction::Hush)
    } else if hit("reset_words", DEFAULT_RESET) {
        Some(SysAction::Reset)
    } else {
        None
    }
}

/// 組み込み行動を行う（発話ログには supervisor として残す）。
fn run_sys_action(app: &tauri::AppHandle, card: &str, root_p: &Path, text: &str, a: SysAction, via: &str) {
    match a {
        SysAction::Hush => {
            send(serde_json::json!({ "cmd": "hush" }));
            log_utterance(card, root_p, "supervisor", text, serde_json::json!({ "action": "hush", "via": via }));
            emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "hush", "text": text }));
        }
        SysAction::Cancel => {
            let cards: Vec<String> = rt().lock().unwrap().fired.iter().cloned().collect();
            let state: State<AppState> = app.state();
            let n = state.jobs.cancel_cards(&cards);
            send(serde_json::json!({ "cmd": "hush" }));
            log_utterance(card, root_p, "supervisor", text, serde_json::json!({ "action": "cancel", "jobs": n, "via": via }));
            emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "cancel", "text": text, "jobs": n }));
            say(card, if n > 0 { "止めました" } else { "止める実行はありません" }, "urgent");
        }
        SysAction::Reset => {
            rt().lock().unwrap().history.remove(card);
            log_utterance(card, root_p, "supervisor", text, serde_json::json!({ "action": "reset", "via": via }));
            emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "reset", "text": text }));
            say(card, "会話をリセットしました", "urgent");
        }
    }
}

/// v1.12 ③: 実行結果の読み上げ文（出力の冒頭）と、発話ログに残す1行（ファイル名＋冒頭）。
fn result_summary(root: &Path, r: &FlowReport) -> (String, String) {
    let ids: Vec<&String> = r.created.iter().chain(r.updated.iter()).collect();
    let Ok(p) = open_project_at(root) else { return (String::new(), String::new()) };
    for id in ids {
        let row: Option<(Option<String>, String)> = p
            .conn
            .query_row(
                "SELECT committed_path, label FROM virtual_bodies WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((Some(path), name)) = row else { continue };
        let full = PathBuf::from(load_path(root, &path));
        let text = std::fs::read(&full).ok().and_then(|b| String::from_utf8(b).ok());
        return match text {
            Some(t) => {
                let head = speakable_head(&t, 120);
                (head.clone(), format!("{name}：{}", short(&head, 80)))
            }
            None => (format!("{name}を作りました"), name),
        };
    }
    match r.outputs.first() {
        Some(o) => (format!("{o}を作りました"), o.clone()),
        None => (String::new(), String::new()),
    }
}

/// 読み上げ用の冒頭: コードブロック・見出し記号・箇条書き記号・強調を落とし、最初の段落を最大 n 字
/// （句点で切れるならそこで切る）。
fn speakable_head(text: &str, n: usize) -> String {
    let mut in_code = false;
    let mut para: Vec<String> = Vec::new();
    for raw in text.lines() {
        let l = raw.trim();
        if l.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if l.is_empty() || l.starts_with('|') || l.starts_with("---") {
            if !para.is_empty() {
                break;
            }
            continue;
        }
        // 見出しだけを読み上げても意味が無いので飛ばす（本文の最初の段落を読む）。
        if l.starts_with('#') {
            continue;
        }
        let l = l.trim_start_matches(['-', '*', '>', '・']).trim();
        let l = l.replace("**", "").replace('`', "");
        if !l.is_empty() {
            para.push(l);
        }
    }
    let one = para.join(" ");
    let cut: String = one.chars().take(n).collect();
    if one.chars().count() <= n {
        return cut;
    }
    match cut.rfind('。') {
        Some(i) if i > 0 => cut[..i + '。'.len_utf8()].to_string(),
        _ => format!("{cut}…"),
    }
}

/// v1.12 思考の結果の置き場（発話ログと同じフォルダ・追記）。
const THINK_MD_NAME: &str = "思考.md";
/// 思考役に頼んで答えを待っている件数（アプリ全体・窓口は1枚）。
static THINKING: AtomicUsize = AtomicUsize::new(0);

/// 思考役（Strata → LM Studio）に問いを渡し、答えが来たら全文を 思考.md へ、要約を声で報告する。
/// 待ちは別スレッド（窓口の会話は止めない＝裏で考えている間もおしゃべりは続く）。
fn start_think(
    app: &tauri::AppHandle,
    card: &str,
    root_p: &Path,
    conf: crate::thinker::Conf,
    question: String,
    hist: &[(String, String)],
) {
    // 「それ」「さっきの件」を解くための抜粋（直近 6 件・各 300 字まで）。丸ごとは渡さない（§4.3）。
    let context: String = hist
        .iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|(r, c)| {
            let who = if r == "user" { "ユーザー" } else { "窓口" };
            format!("{who}: {}", c.chars().take(300).collect::<String>())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (app, card, root_p) = (app.clone(), card.to_string(), root_p.to_path_buf());
    THINKING.fetch_add(1, Ordering::SeqCst);
    emit_thinker(&app, &card, &conf);
    std::thread::spawn(move || {
        let res = crate::thinker::think(&conf, &question, &context);
        THINKING.fetch_sub(1, Ordering::SeqCst);
        // 停止・切替の後に届いた答えも 思考.md には残す（考えた分は捨てない）。声は稼働中のときだけ。
        let live = rt().lock().unwrap().proc.as_ref().map(|p| p.card == card).unwrap_or(false);
        match res {
            Ok(a) => {
                let md = rt()
                    .lock()
                    .unwrap()
                    .log_ctx
                    .get(&card)
                    .map(|c| c.0.with_file_name(THINK_MD_NAME))
                    .unwrap_or_else(|| root_p.join(THINK_MD_NAME));
                let entry = format!(
                    "## {} {}\n\n{}\n\n— {}（{}・{:.0}秒）\n\n",
                    md_time(),
                    question,
                    a.full,
                    a.head,
                    a.model,
                    a.secs
                );
                if let Some(dir) = md.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let fresh = !md.exists();
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&md) {
                    if fresh {
                        let _ = writeln!(f, "# 思考\n");
                    }
                    let _ = write!(f, "{entry}");
                }
                md_append(&card, &format!("- {} 🧠 {}：{}", md_time(), a.head, short(&a.summary, 80)));
                // 窓口の会話にも要約を残す（続けて「それって…」と聞けるように）。
                rt().lock().unwrap().history.entry(card.clone()).or_default().push((
                    "assistant".into(),
                    format!("（思考役の答え: {question} → {}）", a.summary),
                ));
                emit_event(&app, &card, serde_json::json!({ "ev": "reflex", "kind": "thought",
                    "text": a.summary, "label": a.head }));
                if live {
                    say(&card, &format!("考えがまとまりました。{}", a.summary), "normal");
                }
            }
            Err(e) => {
                md_append(&card, &format!("- {} 🧠 ✗ {}", md_time(), short(&e, 80)));
                emit_event(&app, &card, serde_json::json!({ "ev": "reflex", "kind": "thought", "text": e, "error": true }));
                if live {
                    say(&card, &format!("考えられませんでした。{}", short(&e, 40)), "urgent");
                }
            }
        }
        emit_thinker(&app, &card, &conf);
    });
}

/// 状態パネルへ思考役の状態を送る（ネットワークを引くので呼び出し側は別スレッドで）。
fn emit_thinker(app: &tauri::AppHandle, card: &str, conf: &crate::thinker::Conf) {
    if !conf.enabled() {
        return;
    }
    let (app, card, conf) = (app.clone(), card.to_string(), conf.clone());
    std::thread::spawn(move || {
        let s = crate::thinker::status(&conf);
        emit_event(&app, &card, serde_json::json!({ "ev": "thinker", "head": s.head, "state": s.state,
            "queued": s.queued, "busy": THINKING.load(Ordering::SeqCst) }));
    });
}

/// 思考役の見回り間隔（voice_tick は 1 秒ごと）。
const THINKER_POLL: Duration = Duration::from_secs(5);

fn handle_utterance(app: &tauri::AppHandle, card: &str, root: &str, text: &str, source: &'static str) {
    let root_p = Path::new(root);
    // 停止・切替の後に届いた発話は捨てる。
    if rt().lock().unwrap().proc.as_ref().map(|p| p.card != card).unwrap_or(true) {
        return;
    }
    // 別接続で読む（DB の Mutex を長く握らない・ジョブと同じ流儀）。
    let p = match open_project_at(root_p) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[voice] プロジェクトを開けません: {e}");
            return;
        }
    };
    let spec_json: Option<String> = p
        .conn
        .query_row(
            "SELECT filter_ref FROM virtual_bodies WHERE id = ?1 AND kind = 'op' AND deleted_at IS NULL",
            rusqlite::params![card],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(mut spec) = spec_json.and_then(|j| parse_op_spec(&j).ok()) else { return };
    let inbox = inbox_path(&p.conn, root_p, card, &spec);
    let log_md = inbox.parent().map(|d| d.join(LOG_MD_NAME)).unwrap_or_else(|| root_p.join(LOG_MD_NAME));
    rt().lock().unwrap().log_ctx.insert(card.to_string(), (log_md, source));

    // 1) 組み込み行動（止める・中止・リセット）。AI と会話する設定では AI が発話全体から分類する
    //    （下の AI 会話の中）。AI を使わない設定と口述モードでは、ここで文字判定する。
    let dictation_mode = extra_str(&spec, "mode").as_deref() == Some("dictation");
    let use_ai = !dictation_mode && ai_conf(&spec).is_some();
    if !use_ai {
        if let Some(a) = word_action(&spec, text) {
            run_sys_action(app, card, root_p, text, a, "word");
            return;
        }
    }

    // 2) 矢印の先（コマンド）。
    let targets = targets_of(&p.conn, card);
    // 口述モード（2026-10-11 ユーザー提案）: 言い回しを照合せず、発話のたびに全文を書き出して
    // 矢印の先を全部発火する＝話した内容をそのまま AI カードやフィルタで自動処理する。
    let dictation = extra_str(&spec, "mode").as_deref() == Some("dictation");
    // AI が返事を読み上げたか（したなら「○○、始めます」の受け付けの返事は重ねない）。
    let mut replied = false;
    let (fire, content, phrase): (Vec<&Target>, String, Option<String>) = if dictation {
        if targets.is_empty() {
            log_utterance(card, root_p, "unmatched", text, serde_json::json!({ "mode": "dictation" }));
            return;
        }
        (targets.iter().collect(), text.to_string(), None)
    } else {
        // v1.12: AI と会話する設定なら矢印が無くても話せる（ふだんは質問に答えるだけ・V1.12 §1）。
        if targets.is_empty() && !use_ai {
            log_utterance(card, root_p, "unmatched", text, serde_json::json!({}));
            return;
        }
        // AI 会話（既定）: 履歴ごと渡し、返事はすぐ読み上げ、頼まれたらコマンドを選ぶ。
        // 応答が無ければ文字照合。
        let mut via = "phrase";
        let mut picked: Option<(&Target, String, Option<String>)> = None;
        let mut ai_ok = false;
        if let Some(ai) = ai_conf(&spec) {
            emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "classifying", "text": text }));
            let (hist, pending) = {
                let mut g = rt().lock().unwrap();
                if g.pending.get(card).map(|p| p.at.elapsed() > PENDING_IDLE).unwrap_or(false) {
                    g.pending.remove(card);
                }
                (g.history.get(card).cloned().unwrap_or_default(), g.pending.get(card).cloned())
            };
            let pend_arg = pending.as_ref().map(|p| (p.label.as_str(), p.parts.as_slice()));
            let tconf = thinker_conf(&spec);
            let opts = TurnOpts { think: tconf.enabled(), sys: true };
            match agent_turn(&ai, &hist, text, &targets, pend_arg, opts) {
                Ok(mut out) => {
                    if opts.think && pending.is_none() {
                        promote_think(&mut out, text, &extra_list(&spec, "think_words", DEFAULT_THINK_WORDS));
                    }
                    ai_ok = true;
                    via = "ai";
                    let remember = |assistant: String| {
                        let mut g = rt().lock().unwrap();
                        let h = g.history.entry(card.to_string()).or_default();
                        h.push(("user".into(), text.to_string()));
                        h.push(("assistant".into(), assistant));
                    };
                    if let Some(a) = out.sys {
                        // 組み込み行動は自分の短い文で知らせる（AI の返事は重ねない）。
                        if a != SysAction::Hush {
                            rt().lock().unwrap().pending.remove(card);
                        }
                        if a != SysAction::Reset {
                            remember(format!("（{a:?}）"));
                        }
                        run_sys_action(app, card, root_p, text, a, "ai");
                        return;
                    }
                    let speak_reply = |r: &str| {
                        if !r.is_empty() {
                            say(card, r, "normal");
                        }
                    };
                    if let Some(mut pend) = pending {
                        // 聞き取り中: 続き → 溜めて発話.txt に追記 / 終わりの合図 → 溜めた全文で実行。
                        if out.action == ACT_WAIT || out.action == ACT_RUN {
                            let part = pending_part(&out.action, text, &out.content, &pend.parts);
                            if !part.is_empty() {
                                pend.parts.push(part.clone());
                            }
                            pend.at = Instant::now();
                            if out.action == ACT_WAIT {
                                let _ = write_say_out(&inbox, &pend.parts.join("\n"));
                                speak_reply(&out.reply);
                                remember(format!("{}（中身を追加: {part}）", out.reply));
                                log_utterance(card, root_p, "gather", text, serde_json::json!({ "target": pend.label, "part": part }));
                                emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "gather",
                                    "label": pend.label, "count": pend.parts.len(), "text": text }));
                                rt().lock().unwrap().pending.insert(card.to_string(), pend);
                                return;
                            }
                            rt().lock().unwrap().pending.remove(card);
                            let Some(i) = targets.iter().position(|t| t.label == pend.label) else {
                                say(card, "待っていたカードが見つかりません", "urgent");
                                return;
                            };
                            if pend.parts.is_empty() {
                                say(card, &format!("{}は中身が無いので取りやめました", pend.label), "normal");
                                remember("（中身が無いので取りやめ）".into());
                                return;
                            }
                            let body = pend.parts.join("\n");
                            speak_reply(&out.reply);
                            replied = !out.reply.is_empty();
                            remember(format!("{}\n（実行: {}／本文: {body}）", out.reply, pend.label));
                            picked = Some((&targets[i], body, None));
                        } else {
                            speak_reply(&out.reply);
                            remember(out.reply.clone());
                            log_utterance(card, root_p, "user", text, serde_json::json!({ "via": "ai", "reply": out.reply }));
                            emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "reply", "text": text }));
                            return;
                        }
                    } else if out.action == ACT_THINK && tconf.enabled() {
                        // v1.12: 思考役へ回す。返事（考えておきます）はすぐ、答えは後から報告。
                        let question = if out.content.trim().is_empty() { text.to_string() } else { out.content.trim().to_string() };
                        let busy = THINKING.load(Ordering::SeqCst);
                        let mut reply = if out.reply.is_empty() { "考えておきます".to_string() } else { out.reply.clone() };
                        if busy > 0 {
                            reply.push_str(&format!("。前に{busy}件考えています"));
                        }
                        speak_reply(&reply);
                        remember(format!("{reply}（思考役に依頼: {question}）"));
                        log_utterance(card, root_p, "think", text, serde_json::json!({ "via": "ai", "reply": reply, "question": question }));
                        emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "think", "text": question }));
                        start_think(app, card, root_p, tconf, question, &hist);
                        return;
                    } else if out.action == ACT_RUN && out.cmd.is_some() && !out.content.trim().is_empty() {
                        // 頼む言葉と中身が一度に来た → すぐ実行。
                        let i = out.cmd.unwrap();
                        speak_reply(&out.reply);
                        replied = !out.reply.is_empty();
                        remember(format!("{}\n（実行: {}／本文: {}）", out.reply, targets[i].label, out.content));
                        picked = Some((&targets[i], out.content.trim().to_string(), None));
                    } else if (out.action == ACT_WAIT || out.action == ACT_RUN) && out.cmd.is_some() {
                        // 頼む言葉だけ（中身が空の実行もここ）→ 聞き取り中にする。
                        let label = targets[out.cmd.unwrap()].label.clone();
                        let reply = if out.reply.is_empty() {
                            "どうぞ。終わったら以上と言ってください".to_string()
                        } else {
                            out.reply.clone()
                        };
                        let _ = write_say_out(&inbox, "");
                        say(card, &reply, "normal");
                        remember(format!("{reply}（「{label}」の中身を聞き取り中）"));
                        log_utterance(card, root_p, "gather", text, serde_json::json!({ "target": label, "start": true }));
                        emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "gather",
                            "label": label, "count": 0, "text": text }));
                        rt().lock().unwrap().pending.insert(
                            card.to_string(),
                            Pending { label, parts: Vec::new(), at: Instant::now() },
                        );
                        return;
                    } else {
                        speak_reply(&out.reply);
                        remember(out.reply.clone());
                        log_utterance(card, root_p, "user", text, serde_json::json!({ "via": "ai", "reply": out.reply }));
                        emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "reply", "text": text }));
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("[voice] AI 会話に失敗・文字判定で続行: {e}");
                    if let Some(a) = word_action(&spec, text) {
                        run_sys_action(app, card, root_p, text, a, "word");
                        return;
                    }
                }
            }
        }
        if !ai_ok {
            picked = best_match(text, &targets).map(|(t, ph)| {
                let arg = strip_phrase(text, &ph).unwrap_or_default();
                (t, arg, Some(ph))
            });
        }
        let Some((target, arg, phrase)) = picked else {
            // 反射は黙る（コマンドを頼んでいない発話）。
            log_utterance(card, root_p, "unmatched", text, serde_json::json!({ "via": via }));
            emit_event(app, card, serde_json::json!({ "ev": "reflex", "kind": "nomatch", "text": text }));
            return;
        };
        // 中身が空なら発話全文。
        let content = if arg.trim().is_empty() { text.to_string() } else { arg };
        (vec![target], content, phrase)
    };
    let role = if dictation { "dictation" } else { "command" };

    // 3) ヒューズ（§5-1: 回数は既定で無制限・間隔の下限だけ。1発話=1カウント）。
    let min_iv = spec.min_interval.unwrap_or(DEFAULT_MIN_INTERVAL);
    {
        let mut g = rt().lock().unwrap();
        if let Some(t) = g.last_fire.get(card) {
            if t.elapsed() < Duration::from_secs(min_iv) {
                log_utterance(card, root_p, role, text, serde_json::json!({ "skipped": "min_interval" }));
                return;
            }
        }
        g.last_fire.insert(card.to_string(), Instant::now());
    }
    let max = spec.max_fires.unwrap_or(DEFAULT_MAX_FIRES);
    if max != 0 && spec.fired_count >= max {
        say(card, "発火の上限に達しています。カードの設定で上限を変えてください", "urgent");
        log_utterance(card, root_p, role, text, serde_json::json!({ "skipped": "max_fires" }));
        return;
    }
    spec.fired_count += 1;
    if let Ok(json) = serde_json::to_string(&spec) {
        let _ = p.conn.execute(
            "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2 AND kind = 'op'",
            rusqlite::params![json, card],
        );
    }
    drop(p);

    // 4) 中身を書き出し先へ（発火の直前・固定名上書き）。
    let labels: Vec<String> = fire.iter().map(|t| t.label.clone()).collect();
    let joined = labels.join("・");
    if let Err(e) = write_say_out(&inbox, &content) {
        say(card, &format!("{joined}、書き出しに失敗しました"), "urgent");
        eprintln!("[voice] {e}");
        return;
    }
    log_utterance(
        card,
        root_p,
        role,
        text,
        serde_json::json!({ "targets": labels, "phrase": phrase, "content": content }),
    );
    emit_event(
        app,
        card,
        serde_json::json!({ "ev": "reflex", "kind": "fire", "text": text, "label": joined, "content": content }),
    );
    // 受け付けの返事: 既定（auto）はコマンドで返事・口述では黙る（話すたびに返事は煩い）。
    let ack = !replied
        && match extra_str(&spec, "ack").as_deref() {
            Some("on") => true,
            Some("off") => false,
            _ => !dictation,
        };
    if ack {
        say(card, &format!("{joined}、始めます"), "normal");
    }

    // 5) 発火（ジョブ台帳に乗る）→ 結果を声で報告。
    let report = extra_str(&spec, "report").unwrap_or_else(|| "fired".into());
    for t in fire {
        rt().lock().unwrap().fired.insert(t.id.clone());
        let (app2, card2, tid, label, report, prompt, root2) = (
            app.clone(),
            card.to_string(),
            t.id.clone(),
            t.label.clone(),
            report.clone(),
            content.clone(),
            root_p.to_path_buf(),
        );
        std::thread::spawn(move || {
            // v1.12 ①: 発話の中身を発火先カードへの入力として直接渡す（受け皿ファイルは従来どおり書く）。
            let res = run_flow_with_prompt(&tid, &app2, &prompt);
            if report == "none" {
                if let Err(e) = &res {
                    md_append(&card2, &format!("- {} ✗ {label}: {}", md_time(), short(e, 60)));
                }
                return;
            }
            let (msg, pri, mark) = match &res {
                // v1.12 ③: 結果の中身の冒頭も読み上げる（report:"brief" は従来どおり「終わりました」だけ）。
                Ok(r) if r.errors.is_empty() && report != "brief" => {
                    let (spoken, detail) = result_summary(&root2, r);
                    if !detail.is_empty() {
                        md_append(&card2, &format!("  - 📄 {detail}"));
                    }
                    (format!("{label}、終わりました。{spoken}"), "normal", "✓")
                }
                Ok(r) if r.errors.is_empty() => (format!("{label}、終わりました"), "normal", "✓"),
                Ok(r) => (format!("{label}でエラーがありました。{}", short(&r.errors[0], 40)), "urgent", "✗"),
                Err(e) => (format!("{label}は実行できませんでした。{}", short(e, 40)), "urgent", "✗"),
            };
            md_append(&card2, &format!("- {} {mark} {msg}", md_time()));
            say(&card2, &msg, pri);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_drops_spaces_and_punctuation() {
        assert_eq!(normalize(" メモして、明日は 歯医者。"), "メモして明日は歯医者");
    }

    #[test]
    fn strip_phrase_keeps_inner_punctuation() {
        assert_eq!(
            strip_phrase("メモして、明日は歯医者。午後3時", "メモして").as_deref(),
            Some("明日は歯医者。午後3時")
        );
        assert_eq!(
            strip_phrase("会議の内容を要約したカードを作って", "要約したカードを作って").as_deref(),
            Some("会議の内容を")
        );
        // 言い回しの中の空白・句読点の揺れは無視して見つける。
        assert_eq!(strip_phrase("メモ して 牛乳", "メモして").as_deref(), Some("牛乳"));
        assert_eq!(strip_phrase("こんにちは", "メモして"), None);
    }

    #[test]
    fn phrases_from_attrs_or_label() {
        assert_eq!(phrases_of("メモして", None), vec!["メモして"]);
        assert_eq!(phrases_of("メモ", Some(r#"{"say":["メモして","覚えて"]}"#)), vec!["メモして", "覚えて"]);
        assert_eq!(phrases_of("メモ", Some(r#"{"say":"記録して"}"#)), vec!["記録して"]);
        assert_eq!(phrases_of("メモ", Some(r#"{"port":1}"#)), vec!["メモ"]);
    }

    #[test]
    fn longest_phrase_wins() {
        let t = vec![
            Target { id: "a".into(), label: "要約".into(), phrases: vec!["要約".into()], memo: None },
            Target { id: "b".into(), label: "要約したカード".into(), phrases: vec!["要約したカードを作って".into()], memo: None },
        ];
        let (hit, ph) = best_match("議事録を要約したカードを作って", &t).unwrap();
        assert_eq!(hit.id, "b");
        assert_eq!(ph, "要約したカードを作って");
        assert!(best_match("こんにちは", &t).is_none());
    }

    #[test]
    fn loose_match_from_real_utterances() {
        // 2026-10-11 実機の発話ログ: 助詞が挟まる・カタカナ/ひらがなの揺れ。
        assert!(locate("そしてこれがメモですメモをしてください", "メモして").is_some());
        assert_eq!(strip_phrase("メモをして、明日は歯医者", "メモして").as_deref(), Some("明日は歯医者"));
        assert!(locate("めもして牛乳", "メモして").is_some());
        assert_eq!(locate("メモして", "メモして").map(|l| l.2), Some(0));
        // 離れすぎは拾わない。
        assert!(locate("メモの話をあとでして", "メモして").is_none());
    }

    #[test]
    fn supervisor_allows_polite_tail_only() {
        assert!(supervisor_hit("止めてください", "止めて"));
        assert!(supervisor_hit("止めて。", "止めて"));
        assert!(supervisor_hit("ストップして", "ストップ"));
        assert!(!supervisor_hit("メモを止めてから要約して", "止めて"));
    }

    #[test]
    fn parse_agent_reads_json_or_falls_back() {
        let labels = vec!["メモして".to_string(), "英訳して".to_string()];
        let a = parse_agent(
            r#"{"action":"コマンドを実行","command":"メモして","content":"明日の10時に歯医者","reply":"メモします"}"#,
            &labels,
        );
        assert_eq!((a.action.as_str(), a.command.as_str(), a.content.as_str()), (ACT_RUN, "メモして", "明日の10時に歯医者"));
        let a = parse_agent(r#"{"action":"読み上げを止める","command":"なし","content":"","reply":""}"#, &labels);
        assert_eq!(a.action, "読み上げを止める");
        // 実測: 思考ありのモデルが素の文を返した形。
        assert_eq!(parse_agent("英訳して content=x", &labels).command, "英訳して");
        assert_eq!(parse_agent("はい", &labels).action, ACT_CHAT);
    }

    #[test]
    fn agent_prompt_lists_commands_with_memo() {
        let t = vec![Target {
            id: "a".into(),
            label: "メモして".into(),
            phrases: vec!["メモして".into(), "覚えて".into()],
            memo: Some("内容をメモに残す".into()),
        }];
        let p = agent_prompt(&t, None, TurnOpts { think: true, sys: true });
        assert!(p.contains("- メモして（言い方の例: 覚えて）: 内容をメモに残す"));
        assert!(p.contains("- 読み上げを止める:"));
        let parts = vec!["明日は10時に歯医者".to_string()];
        let p = agent_prompt(&t, Some(("メモして", &parts)), TurnOpts { think: true, sys: true });
        assert!(p.contains("「メモして」の中身を話している途中"));
        assert!(p.contains("・明日は10時に歯医者"));
    }

    #[test]
    fn pending_part_does_not_duplicate() {
        let parts = vec!["明日は10時に歯医者".to_string(), "保険証も持っていく".to_string()];
        // 続きは原文。
        assert_eq!(pending_part(ACT_WAIT, " 保険証も持っていく ", "明日は10時に歯医者 保険証も持っていく", &parts[..1]), "保険証も持っていく");
        // 終わりの合図で AI が既出の中身を繰り返しても足さない（実測の重複）。
        assert_eq!(pending_part(ACT_RUN, "以上", "明日は10時に歯医者", &parts), "");
        assert_eq!(pending_part(ACT_RUN, "以上", "明日は10時に歯医者、保険証も持っていく", &parts), "");
        // 合図と一緒に言った新しい中身は足す。
        assert_eq!(pending_part(ACT_RUN, "傘も持つ、以上", "傘も持つ", &parts), "傘も持つ");
        assert_eq!(pending_part(ACT_RUN, "以上", "", &parts), "");
    }

    #[test]
    fn history_is_bounded_from_the_old_side() {
        let h: Vec<(String, String)> = (0..10).map(|i| ("user".into(), format!("{i}{}", "あ".repeat(1000)))).collect();
        let b = bounded_history(&h);
        assert!(b.len() < 10);
        assert!(b.last().unwrap().1.starts_with('9'));
    }

    /// 実機の Ollama に当てる（cargo test --lib voice_live -- --ignored --nocapture）。
    /// VoiceAssistantAgent と同じ「発話ごとに履歴つきで1ターン」を、実際の言い方で流す。
    #[test]
    #[ignore]
    fn voice_live_agent_conversation() {
        let t = vec![
            Target { id: "m".into(), label: "メモして".into(), phrases: vec!["メモして".into()], memo: None },
            Target { id: "e".into(), label: "英訳して".into(), phrases: vec!["英訳して".into()], memo: None },
        ];
        let model = std::env::var("VOICE_LIVE_MODEL").unwrap_or_else(|_| DEFAULT_AI_MODEL.into());
        println!("model = {model}");
        let scripts: &[&[&str]] = &[
            &["メモして", "明日は10時に歯医者", "保険証も持っていく", "以上"],
            &["目もして", "明日は10時に歯医者", "保険証も持っていく", "目も終了"],
            &["メモして、牛乳を買う"],
            &["これを英語にしてください 今日はいい天気です"],
            &["発話の中身はどうなっていますか。", "目もが終わったら 終了と 言ってください。"],
            &["ちょっと止めてください", "全部キャンセルして", "最初からやり直そう"],
        ];
        for lines in scripts {
            let mut hist: Vec<(String, String)> = Vec::new();
            let mut pend: Option<(String, Vec<String>)> = None;
            println!("---");
            for text in *lines {
                let t0 = Instant::now();
                let pa = pend.as_ref().map(|(l, p)| (l.as_str(), p.as_slice()));
                let out = agent_turn(&live_ai(&model), &hist, text, &t, pa, TurnOpts { think: true, sys: true }).expect("Ollama");
                // handle_utterance と同じ状態遷移（発火の代わりに印字）。
                let mut did = String::new();
                if let Some(a) = out.sys {
                    did = format!("{a:?}");
                    pend = None;
                } else if let Some((label, mut parts)) = pend.take() {
                    let part = pending_part(&out.action, text, &out.content, &parts);
                    if out.action == ACT_WAIT {
                        if !part.is_empty() {
                            parts.push(part);
                        }
                        did = format!("溜める {parts:?}");
                        pend = Some((label, parts));
                    } else if out.action == ACT_RUN {
                        if !part.is_empty() {
                            parts.push(part);
                        }
                        did = format!("実行 {label} ← {}", parts.join("／"));
                    } else {
                        pend = Some((label, parts));
                    }
                } else if out.action == ACT_RUN && out.cmd.is_some() && !out.content.trim().is_empty() {
                    did = format!("実行 {} ← {}", t[out.cmd.unwrap()].label, out.content);
                } else if (out.action == ACT_WAIT || out.action == ACT_RUN) && out.cmd.is_some() {
                    let l = t[out.cmd.unwrap()].label.clone();
                    did = format!("聞き取り開始 {l}");
                    pend = Some((l, Vec::new()));
                }
                println!("{:5.2}s 「{text}」 → {} 返事「{}」 {did}", t0.elapsed().as_secs_f32(), out.action, out.reply);
                hist.push(("user".into(), text.to_string()));
                hist.push(("assistant".into(), format!("{} {did}", out.reply)));
            }
        }
    }

    /// 手本プロジェクト（scripts/build_voice_demo.py）の5分岐で振り分けを確かめる（実機の Ollama）。
    #[test]
    #[ignore]
    fn voice_live_demo_branches() {
        let tg = |id: &str, label: &str, say: &[&str], memo: &str| Target {
            id: id.into(),
            label: label.into(),
            phrases: if say.is_empty() { vec![label.into()] } else { say.iter().map(|s| s.to_string()).collect() },
            memo: Some(memo.into()),
        };
        let t = vec![
            tg("memo", "メモして", &[], "話した内容を箇条書きのメモに残す"),
            tg("en", "英訳して", &["英訳して", "英語にして", "翻訳して"], "日本語の内容を英語に訳す"),
            tg("sum", "要約して", &["要約して", "まとめて"], "長めに話した内容を3行に要約する"),
            tg("ensum", "英語で要約して", &[], "話した内容を要約してから英語に訳す（要約→英訳の2段）"),
            tg("count", "文字数を数えて", &["文字数を数えて", "何文字か数えて"], "話した内容の行数・単語数・文字数を数える"),
        ];
        let cases: &[(&str, &str)] = &[
            ("メモして、牛乳を買う", "実行 memo"),
            ("これを英語にして、今日はいい天気です", "実行 en"),
            ("要約して", "待つ sum"),
            ("英語で要約して", "待つ ensum"),
            ("文字数を数えて、吾輩は猫である", "実行 count"),
            ("今日は何曜日だっけ", "会話"),
            ("ちょっと止めて", "Hush"),
            // v1.12 思考役へ回す問い／その場で答える問い。
            ("音声アシスタントの返事を速く感じさせる方法を、よく考えて教えて", "考える"),
            ("ローカルLLMとクラウドのAIの使い分けについて、利点と欠点を比べて詳しく説明して", "考える"),
            ("こんにちは、調子はどう？", "会話"),
        ];
        let model = std::env::var("VOICE_LIVE_MODEL").unwrap_or_else(|_| DEFAULT_AI_MODEL.into());
        println!("model = {model}");
        let mut miss = 0;
        for (text, want) in cases {
            let t0 = Instant::now();
            let mut out = agent_turn(&live_ai(&model), &[], text, &t, None, TurnOpts { think: true, sys: true }).expect("Ollama");
            let words: Vec<String> = DEFAULT_THINK_WORDS.iter().map(|s| s.to_string()).collect();
            promote_think(&mut out, text, &words);
            let got = if let Some(a) = out.sys {
                format!("{a:?}")
            } else if out.action == ACT_THINK {
                "考える".to_string()
            } else if out.action == ACT_RUN && out.cmd.is_some() && !out.content.trim().is_empty() {
                format!("実行 {}", t[out.cmd.unwrap()].id)
            } else if (out.action == ACT_WAIT || out.action == ACT_RUN) && out.cmd.is_some() {
                format!("待つ {}", t[out.cmd.unwrap()].id)
            } else {
                "会話".to_string()
            };
            let ok = got == *want;
            if !ok {
                miss += 1;
            }
            println!("{:5.2}s {} 「{text}」 → {got}（期待 {want}）中身「{}」", t0.elapsed().as_secs_f32(), if ok { "○" } else { "×" }, out.content);
        }
        println!("外れ {miss} 件");
    }

    #[test]
    fn speakable_head_skips_markup() {
        let t = "# 要約

**結論**：明日は晴れです。気温は25度。

- 詳細1
";
        assert_eq!(speakable_head(t, 120), "結論：明日は晴れです。気温は25度。");
        let t = "```python
print(1)
```
明日は晴れです。気温は25度。風は弱い。";
        assert_eq!(speakable_head(t, 12), "明日は晴れです。");
        assert_eq!(speakable_head("- 牛乳を買う
- 卵", 50), "牛乳を買う 卵");
    }

    #[test]
    fn describe_skips_heading_and_keeps_first_paragraph() {
        let m = "プログラミング
自然言語を最適な言語を使って
プログラミングしソースコードを保存します。

細則…";
        assert_eq!(describe("プログラミング", m), "自然言語を最適な言語を使って プログラミングしソースコードを保存します。");
        assert_eq!(describe("要約", "長めの話を3行に要約する"), "長めの話を3行に要約する");
    }

    #[test]
    fn sys_action_needs_backing_words() {
        // 実測で gemma3 が「中止」にした発話は裏付けなし → 選び直し。
        assert!(!sys_backed(SysAction::Cancel, "英語で要約して"));
        assert!(!sys_backed(SysAction::Cancel, "文字数を数えて、吾輩は猫である"));
        assert!(sys_backed(SysAction::Cancel, "全部キャンセルして"));
        assert!(sys_backed(SysAction::Hush, "ちょっと止めてください"));
        assert!(sys_backed(SysAction::Reset, "最初からやり直そう"));
    }

    /// 実機テストの応答用 AI。VOICE_LIVE_PROVIDER（ollama/lmstudio/openai）・VOICE_LIVE_ENDPOINT で切替。
    fn live_ai(model: &str) -> FrontAi {
        let mut a = FrontAi::ollama(model);
        if let Ok(p) = std::env::var("VOICE_LIVE_PROVIDER") {
            a.provider = p;
        }
        if let Ok(e) = std::env::var("VOICE_LIVE_ENDPOINT") {
            a.ep = e;
        }
        a
    }

    #[test]
    fn front_ai_reads_card_settings() {
        let s = parse_op_spec(r#"{"op":"voice"}"#).unwrap();
        let a = ai_conf(&s).unwrap();
        assert_eq!(
            (a.provider.as_str(), a.ep.as_str(), a.model.as_str(), a.think.as_str()),
            ("ollama", DEFAULT_AI_ENDPOINT, DEFAULT_AI_MODEL, "off")
        );
        let s = parse_op_spec(
            r#"{"op":"voice","ai_provider":"openai","ai_endpoint":"http://127.0.0.1:8080/v1/","ai_model":"swift","ai_temperature":0.3}"#,
        )
        .unwrap();
        let a = ai_conf(&s).unwrap();
        assert!(!a.is_ollama());
        assert_eq!(a.ep, "http://127.0.0.1:8080/v1");
        assert_eq!(a.temperature, 0.3);
        let mut body = serde_json::json!({ "model": "swift" });
        let extra = serde_json::json!({ "reasoning_budget_tokens": 100, "model": "x" });
        merge_extra(&mut body, extra.as_object().unwrap());
        assert_eq!(body["reasoning_budget_tokens"], 100);
        assert_eq!(body["model"], "swift", "管理しているキーは上書きさせない");
        let s = parse_op_spec(r#"{"op":"voice","ai_provider":"lmstudio"}"#).unwrap();
        assert_eq!(ai_conf(&s).unwrap().ep, "http://localhost:1234/v1");
    }

    #[test]
    fn think_words_promote_chat_only() {
        let words: Vec<String> = DEFAULT_THINK_WORDS.iter().map(|s| s.to_string()).collect();
        let mk = |action: &str| AgentOut {
            reply: "はい".into(),
            action: action.into(),
            cmd: None,
            content: String::new(),
            sys: None,
        };
        let mut o = mk(ACT_CHAT);
        promote_think(&mut o, "使い分けを詳しく説明して", &words);
        assert_eq!(o.action, ACT_THINK);
        assert_eq!(o.content, "使い分けを詳しく説明して");
        let mut o = mk(ACT_CHAT);
        promote_think(&mut o, "こんにちは", &words);
        assert_eq!(o.action, ACT_CHAT);
        // コマンドの実行は格上げしない。
        let mut o = mk(ACT_RUN);
        o.cmd = Some(0);
        promote_think(&mut o, "メモして、詳しく調べる", &words);
        assert_eq!(o.action, ACT_RUN);
        // コマンド名の無い「実行」は会話と同じ扱い → 格上げ。
        let mut o = mk(ACT_RUN);
        promote_think(&mut o, "よく考えて教えて", &words);
        assert_eq!(o.action, ACT_THINK);
    }

    #[test]
    fn prompt_without_targets_still_talks_and_thinks() {
        let p = agent_prompt(&[], None, TurnOpts { think: true, sys: true });
        assert!(p.contains("（なし）"));
        assert!(p.contains(ACT_THINK));
        let p = agent_prompt(&[], None, TurnOpts { think: false, sys: false });
        assert!(!p.contains(ACT_THINK));
        assert!(!p.contains("実行を中止する"));
    }

    #[test]
    fn thinker_line_reads_status() {
        let v = serde_json::json!({ "head": "Strata", "state": "generating", "queued": 2, "busy": 1 });
        assert_eq!(thinker_line(&v), "🧠 Strata：生成中（2件待ち）・考え中 1件");
        let v = serde_json::json!({ "head": "", "state": "down" });
        assert!(thinker_line(&v).contains("思考役なし"));
    }

    #[test]
    fn voice_defaults_and_lists() {
        let s = parse_op_spec(r#"{"op":"voice"}"#).unwrap();
        assert_eq!(extra_list(&s, "hush_words", DEFAULT_HUSH), vec!["止めて", "ストップ", "静かに"]);
        let s = parse_op_spec(r#"{"op":"voice","hush_words":[],"device":"2","threshold":0.02}"#).unwrap();
        assert!(extra_list(&s, "hush_words", DEFAULT_HUSH).is_empty());
        let a = proc_args(&s, "");
        assert!(a.windows(2).any(|w| w[0] == "--device" && w[1] == "2"));
        assert!(a.windows(2).any(|w| w[0] == "--threshold" && w[1] == "0.02"));
    }
}
