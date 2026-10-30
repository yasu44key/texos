// ============================================================================
// v1.8.7 実行トレース（デバッグモード）
// ----------------------------------------------------------------------------
// フローは中間結果をメモリで次段へ渡す（中間カードを作らない）ため、つないだ処理が
// 「どう動いたか」が後から読めなかった。ここでジョブ1回分の評価をノード単位で記録し、
// 終了時に `.texos/logs/` へ Markdown（人が読む）＋JSON（機械が読む）で書き出す。
// 各カードには直近の記録を `virtual_bodies.last_trace` に置き、右クリック
// 「実行ログを見る」から引ける。
//
// 記録点は3つだけ（eval_filter の出入口に寄せる＝各ノード種別の分岐に手を入れない）:
//   begin   … 評価開始（メモ化ヒット直後・spec 解決後）
//   inputs  … 入力の収集が済んだ時点（collected）
//   end     … finish_eval（全ノードの評価はここを必ず通る）
// `?` で途中脱出したノードは pending に残り、書き出し時に「未完了」として出る。
//
// 記録量: 既定（要約）は名前・MIME・サイズ・成否・時間・エラーだけ。デバッグモード
// （set_trace_detail(true)）では、テキストの先頭 4KB・バイナリの SHA-256・外部コマンドの
// stderr・役割（システムプロンプト）まで残す。
// ============================================================================
use crate::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// デバッグモード（詳細記録）。フロントが起動時とトグル時に set_trace_detail で送る。
/// ジョブ開始時点の値をジョブに焼き込む（実行途中で切り替えても1回の記録は揃う）。
static DETAIL: AtomicBool = AtomicBool::new(false);

/// テキスト中身の記録上限（バイト・文字境界で切る）。
pub(crate) const TRACE_PREVIEW_BYTES: usize = 4096;
/// `.texos/logs/` に残す実行ログの本数（古い順に消す）。
const KEEP_LOGS: usize = 100;

#[tauri::command]
pub(crate) fn set_trace_detail(on: bool) {
    DETAIL.store(on, Ordering::Relaxed);
}

#[tauri::command]
pub(crate) fn get_trace_detail() -> bool {
    DETAIL.load(Ordering::Relaxed)
}

/// カードの直近の実行記録（`last_trace` 列の JSON 文字列。未記録なら None）。
#[tauri::command]
pub(crate) fn get_card_trace(id: String, state: tauri::State<AppState>) -> Result<Option<String>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .query_row(
            "SELECT last_trace FROM virtual_bodies WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .map_err(|e| e.to_string())
}

/// 入出力1件の要約。
#[derive(Serialize, Clone, Debug)]
pub(crate) struct TraceData {
    /// `stem.ext`
    pub(crate) name: String,
    pub(crate) mime: String,
    pub(crate) bytes: usize,
    /// デバッグモードのみ。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sha256: Option<String>,
    /// デバッグモードかつテキストのときだけ（先頭 4KB）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<String>,
    pub(crate) truncated: bool,
}

/// ノード1回分の記録。
#[derive(Serialize, Clone, Debug)]
pub(crate) struct TraceEntry {
    /// 完了順の通し番号（1始まり）。pull 評価は上流から完了するので＝データの流れ順。
    pub(crate) order: u32,
    /// 評価開始順（入れ子の把握用）。
    pub(crate) seq: u32,
    pub(crate) card: String,
    pub(crate) label: String,
    pub(crate) kind: String,
    /// 反復カードの周回接尾辞（`_2` 等・反復の外では空）。
    pub(crate) round: String,
    pub(crate) started_at: i64,
    pub(crate) ms: i64,
    /// ok | error | nodata | reused | unfinished | info
    pub(crate) status: String,
    pub(crate) exit: Option<i32>,
    pub(crate) inputs: Vec<TraceData>,
    pub(crate) outputs: Vec<TraceData>,
    pub(crate) errors: Vec<String>,
    pub(crate) skipped: u32,
    /// 付帯情報（コマンド行・採用した枝・stderr 等）。
    pub(crate) notes: Vec<(String, String)>,
    /// v1.8.9 AI ノードの再現条件（モデル・digest・能力・実際に送ったパラメータ）。
    /// 要約モードでも残す（中身ではなく条件なので）。反復で複数回走れば最後の1回。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ai: Option<crate::ai_models::AiRunRecord>,
}

#[derive(Default)]
struct TraceInner {
    next_seq: u32,
    next_order: u32,
    done: Vec<TraceEntry>,
    pending: HashMap<String, TraceEntry>,
}

/// 実行1回の書き出し先（run_flow_for が begin_run で決める）。
struct RunInfo {
    /// `.texos/logs`
    dir: PathBuf,
    /// `<日時>_<ラベル>_job<id>`（.md/.json の共通名）
    base: String,
}

/// ジョブ1回分のトレース（Job に1つ持たせる）。
pub struct Trace {
    pub(crate) detail: bool,
    pub(crate) started_at: i64,
    inner: Mutex<TraceInner>,
    run: Mutex<Option<RunInfo>>,
}

impl Default for Trace {
    fn default() -> Self {
        Self::new()
    }
}

impl Trace {
    pub fn new() -> Self {
        Trace {
            detail: DETAIL.load(Ordering::Relaxed),
            started_at: db::now_ms(),
            inner: Mutex::new(TraceInner::default()),
            run: Mutex::new(None),
        }
    }

    /// フロー実行の開始時にログの書き出し名を決める。
    pub(crate) fn begin_run(&self, root: &Path, label: &str, job_id: u64) {
        let base = format!("{}_{}_job{}", stamp_of(self.started_at), safe_name(label), job_id);
        *self.run.lock().unwrap() = Some(RunInfo { dir: texos_dir(root).join("logs"), base });
    }


    #[cfg(test)]
    pub(crate) fn with_detail(detail: bool) -> Self {
        Trace {
            detail,
            started_at: db::now_ms(),
            inner: Mutex::new(TraceInner::default()),
            run: Mutex::new(None),
        }
    }


    fn blank(&self, inner: &mut TraceInner, card: &str, label: &str, kind: &str, round: &str) -> TraceEntry {
        inner.next_seq += 1;
        TraceEntry {
            order: 0,
            seq: inner.next_seq,
            card: card.to_string(),
            label: label.to_string(),
            kind: kind.to_string(),
            round: round.to_string(),
            started_at: db::now_ms(),
            ms: 0,
            status: "unfinished".into(),
            exit: None,
            inputs: vec![],
            outputs: vec![],
            errors: vec![],
            skipped: 0,
            notes: vec![],
            ai: None,
        }
    }

    fn push_done(inner: &mut TraceInner, mut e: TraceEntry) {
        inner.next_order += 1;
        e.order = inner.next_order;
        inner.done.push(e);
    }

    /// 評価開始。
    pub(crate) fn begin(&self, card: &str, label: &str, kind: &str, round: &str) {
        let mut g = self.inner.lock().unwrap();
        let e = self.blank(&mut g, card, label, kind, round);
        g.pending.insert(card.to_string(), e);
    }

    /// メモ化ヒット（同じジョブ内で2回目以降に引かれた＝再実行していない）。
    pub(crate) fn reused(&self, card: &str, label: &str) {
        let mut g = self.inner.lock().unwrap();
        let mut e = self.blank(&mut g, card, label, "", "");
        e.status = "reused".into();
        Self::push_done(&mut g, e);
    }

    /// 入力（collect 済み）の記録。
    pub(crate) fn inputs(&self, card: &str, payloads: &[FlowPayload]) {
        let items: Vec<TraceData> = payloads.iter().map(|pl| self.summarize(pl)).collect();
        let mut g = self.inner.lock().unwrap();
        if let Some(e) = g.pending.get_mut(card) {
            e.inputs = items;
        }
    }

    /// 付帯情報。評価中ならそのエントリへ、評価済みなら直近の同カードへ、
    /// どちらも無ければ独立の info エントリとして残す（シェル終端の直結ファイル等）。
    pub(crate) fn note(&self, card: &str, key: &str, value: &str) {
        let value = clip_text(value, TRACE_PREVIEW_BYTES).0;
        let mut g = self.inner.lock().unwrap();
        if let Some(e) = g.pending.get_mut(card) {
            e.notes.push((key.to_string(), value));
            return;
        }
        if let Some(e) = g.done.iter_mut().rev().find(|e| e.card == card && e.status != "reused") {
            e.notes.push((key.to_string(), value));
            return;
        }
        let mut e = self.blank(&mut g, card, "", "", "");
        e.status = "info".into();
        e.notes.push((key.to_string(), value));
        Self::push_done(&mut g, e);
    }

    /// v1.8.9 AI ノードの実行条件（評価中のエントリへ。成否に関わらず呼ぶ）。
    pub(crate) fn ai(&self, card: &str, rec: crate::ai_models::AiRunRecord) {
        let mut g = self.inner.lock().unwrap();
        if let Some(e) = g.pending.get_mut(card) {
            e.ai = Some(rec);
        }
    }

    /// デバッグモードのときだけ残す付帯情報（stderr・プロンプト等の中身）。
    pub(crate) fn note_detail(&self, card: &str, key: &str, value: &str) {
        if self.detail && !value.trim().is_empty() {
            self.note(card, key, value);
        }
    }

    /// 評価終了（finish_eval から）。
    pub(crate) fn end(&self, card: &str, out: &EvalOut, skipped: u32) {
        let outputs: Vec<TraceData> = out.payloads.iter().map(|pl| self.summarize(pl)).collect();
        let mut g = self.inner.lock().unwrap();
        let mut e = match g.pending.remove(card) {
            Some(e) => e,
            None => self.blank(&mut g, card, "", "", ""),
        };
        e.ms = db::now_ms() - e.started_at;
        e.status = if out.nodata {
            "nodata"
        } else if out.ok {
            "ok"
        } else {
            "error"
        }
        .into();
        e.exit = out.exit;
        e.outputs = outputs;
        e.errors = out.errors.clone();
        e.skipped = skipped;
        Self::push_done(&mut g, e);
    }

    fn summarize(&self, pl: &FlowPayload) -> TraceData {
        let name = if pl.ext.is_empty() { pl.stem.clone() } else { format!("{}.{}", pl.stem, pl.ext) };
        let (text, truncated) = if self.detail && looks_text(&pl.mime, &pl.ext, &pl.bytes) {
            let (s, _enc) = notes::decode_text_bytes(&pl.bytes);
            let (s, t) = clip_text(&s, TRACE_PREVIEW_BYTES);
            (Some(s), t)
        } else {
            (None, false)
        };
        let sha256 = if self.detail {
            let mut h = Sha256::new();
            h.update(&pl.bytes);
            Some(hex::encode(h.finalize()))
        } else {
            None
        };
        TraceData { name, mime: pl.mime.clone(), bytes: pl.bytes.len(), sha256, text, truncated }
    }

    /// 記録を取り出す（完了順＋未完了を末尾に）。
    pub(crate) fn snapshot(&self) -> Vec<TraceEntry> {
        let g = self.inner.lock().unwrap();
        let mut v = g.done.clone();
        let mut rest: Vec<TraceEntry> = g.pending.values().cloned().collect();
        rest.sort_by_key(|e| e.seq);
        let mut order = g.next_order;
        for mut e in rest {
            order += 1;
            e.order = order;
            v.push(e);
        }
        v
    }
}

/// テキストとして記録してよいか（MIME/拡張子で判定、不明なら UTF-8 として読めて NUL なし）。
fn looks_text(mime: &str, ext: &str, bytes: &[u8]) -> bool {
    let m = mime.to_ascii_lowercase();
    if m.starts_with("text/")
        || m.contains("json")
        || m.contains("xml")
        || m.contains("javascript")
        || m.contains("x-sh")
        || m.contains("x-python")
    {
        return true;
    }
    const EXTS: &[&str] = &[
        "txt", "md", "c", "h", "cpp", "hpp", "cc", "java", "py", "js", "ts", "rs", "go", "cs", "sql",
        "ino", "bat", "cmd", "ps1", "sh", "csv", "tsv", "json", "xml", "html", "htm", "css", "yaml",
        "yml", "toml", "ini", "log",
    ];
    if EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
        return true;
    }
    let head = &bytes[..bytes.len().min(8192)];
    !head.contains(&0) && std::str::from_utf8(head).is_ok()
}

/// 文字境界を守ってバイト数で切る。返り値 (切った文字列, 切ったか)。
pub(crate) fn clip_text(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// ノード種別の表示名。
pub(crate) fn trace_kind_name(spec: &flow::FilterSpec, passthrough: bool) -> String {
    use flow::NodeKind;
    if passthrough {
        return "IN/OUT".into();
    }
    if spec.is_shell() {
        return "シェル".into();
    }
    match spec.builtin.as_deref() {
        Some("tap") => return "記録".into(),
        Some("ask") => return "問い合わせ".into(),
        Some("loop") => return "反復".into(),
        Some("import_new") => return "新規カードを登録".into(),
        Some("voice_send") => return "会話へ送る".into(),
        Some("rag_index") => return "索引".into(),
        Some("rag_search") => return "文書検索".into(),
        Some("agent") => return "エージェント".into(),
        _ => {}
    }
    match spec.node_kind() {
        NodeKind::Generator => "ジェネレータ".into(),
        NodeKind::Ai => "AI".into(),
        NodeKind::T2i => "画像生成".into(),
        NodeKind::T23d => "3D生成".into(),
        NodeKind::Select => "セレクタ".into(),
        NodeKind::Builtin => format!("内蔵:{}", spec.builtin.as_deref().unwrap_or("")),
        NodeKind::External => "外部コマンド".into(),
        NodeKind::Invalid => "不正な定義".into(),
    }
}

fn status_label(s: &str) -> &'static str {
    match s {
        "ok" => "✓ 成功",
        "error" => "✗ エラー",
        "nodata" => "― no-data（材料なし・実行せず）",
        "reused" => "↺ 再利用（同じ実行内の結果を配布・再実行なし）",
        "unfinished" => "⚠ 未完了（途中で中断／構造エラー）",
        "info" => "ℹ 情報",
        _ => "?",
    }
}

fn fmt_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

fn fmt_local(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn stamp_of(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y%m%d-%H%M%S").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn safe_name(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(40)
        .collect();
    if t.trim().is_empty() { "flow".into() } else { t }
}

/// ```` で囲めるよう、本文中の最長のバッククォート連続より長いフェンスを返す。
fn fence_for(s: &str) -> String {
    let mut max = 0;
    let mut cur = 0;
    for c in s.chars() {
        if c == '`' {
            cur += 1;
            max = max.max(cur);
        } else {
            cur = 0;
        }
    }
    "`".repeat((max + 1).max(3))
}

fn data_line(d: &TraceData) -> String {
    let mut s = format!("`{}` （{}・{}）", d.name, d.mime, fmt_bytes(d.bytes));
    if let Some(h) = &d.sha256 {
        s.push_str(&format!(" sha256:{}", &h[..h.len().min(16)]));
    }
    s
}

fn push_data(md: &mut String, title: &str, items: &[TraceData]) {
    if items.is_empty() {
        md.push_str(&format!("- {title}: なし\n"));
        return;
    }
    md.push_str(&format!("- {title}:\n"));
    for d in items {
        md.push_str(&format!("  - {}\n", data_line(d)));
        if let Some(t) = &d.text {
            let f = fence_for(t);
            md.push_str(&format!("\n{f}\n{t}\n{f}\n"));
            if d.truncated {
                md.push_str(&format!("（先頭 {} で打ち切り）\n", fmt_bytes(TRACE_PREVIEW_BYTES)));
            }
            md.push('\n');
        }
    }
}

/// ジョブ全体の結果（フロー報告）をログに添える材料。
pub(crate) struct TraceSummary<'a> {
    pub(crate) job_id: u64,
    pub(crate) label: &'a str,
    pub(crate) card: &'a str,
    pub(crate) report: Option<&'a FlowReport>,
    pub(crate) fatal: Option<&'a str>,
}

/// Markdown 本文を組み立てる（テスト可能な純関数）。
pub(crate) fn render_markdown(trace: &Trace, entries: &[TraceEntry], sum: &TraceSummary, finished_at: i64) -> String {
    let mut md = String::new();
    md.push_str(&format!("# 実行ログ: {}\n\n", sum.label));
    md.push_str(&format!(
        "- 開始: {}　所要: {:.2} 秒　ジョブ #{}\n",
        fmt_local(trace.started_at),
        (finished_at - trace.started_at) as f64 / 1000.0,
        sum.job_id
    ));
    md.push_str(&format!(
        "- 記録モード: {}\n",
        if trace.detail { "詳細（デバッグモード：テキスト先頭4KB・SHA-256・stderr）" } else { "要約（中身はデバッグモードで記録）" }
    ));
    let n_err = entries.iter().filter(|e| e.status == "error" || e.status == "unfinished").count();
    let verdict = match (sum.fatal, sum.report) {
        (Some(e), _) => format!("✗ 実行失敗: {e}"),
        (None, Some(r)) if !r.errors.is_empty() || n_err > 0 => "⚠ エラーあり".to_string(),
        _ => "✓ 完了".to_string(),
    };
    md.push_str(&format!("- 結果: {verdict}\n"));
    md.push_str(&format!("- 終端カード id: `{}`\n\n", sum.card));

    md.push_str("## 処理順（完了した順＝データの流れ順）\n\n");
    md.push_str("| # | カード | 種別 | 状態 | 時間 | 入力 | 出力 |\n|---|---|---|---|---|---|---|\n");
    for e in entries {
        let st = match e.status.as_str() {
            "ok" => "✓",
            "error" => "✗",
            "nodata" => "―",
            "reused" => "↺",
            "unfinished" => "⚠",
            _ => "ℹ",
        };
        md.push_str(&format!(
            "| {} | {}{} | {} | {} | {} ms | {} | {} |\n",
            e.order,
            e.label.replace('|', "｜"),
            e.round,
            e.kind,
            st,
            e.ms,
            e.inputs.len(),
            e.outputs.len()
        ));
    }
    md.push('\n');

    for e in entries {
        md.push_str(&format!(
            "## {}. {}{}{}\n\n",
            e.order,
            e.label,
            e.round,
            if e.kind.is_empty() { String::new() } else { format!("（{}）", e.kind) }
        ));
        md.push_str(&format!("- 状態: {}", status_label(&e.status)));
        if e.status != "reused" && e.status != "info" {
            md.push_str(&format!("　{} ms", e.ms));
        }
        if let Some(x) = e.exit {
            md.push_str(&format!("　終了コード {x}"));
        }
        md.push('\n');
        md.push_str(&format!("- 開始: {}\n", fmt_local(e.started_at)));
        if e.status != "reused" && e.status != "info" {
            push_data(&mut md, "入力", &e.inputs);
            push_data(&mut md, "出力", &e.outputs);
        }
        if e.skipped > 0 {
            md.push_str(&format!("- スキップした入力: {} 件（型不一致）\n", e.skipped));
        }
        if let Some(ai) = &e.ai {
            md.push_str("- AI 実行条件:
");
            for (k, v) in ai.lines() {
                md.push_str(&format!("  - {k}: {v}
"));
            }
        }
        for err in &e.errors {
            md.push_str(&format!("- エラー:\n\n{}\n{}\n{}\n\n", fence_for(err), err, fence_for(err)));
        }
        for (k, v) in &e.notes {
            if v.contains('\n') {
                let f = fence_for(v);
                md.push_str(&format!("- {k}:\n\n{f}\n{v}\n{f}\n\n"));
            } else {
                md.push_str(&format!("- {k}: {v}\n"));
            }
        }
        md.push('\n');
    }

    md.push_str("## フロー全体の報告\n\n");
    if let Some(e) = sum.fatal {
        md.push_str(&format!("- 実行失敗: {e}\n"));
    }
    if let Some(r) = sum.report {
        for o in &r.outputs {
            md.push_str(&format!("- 出力ファイル: {o}\n"));
        }
        if !r.created.is_empty() {
            md.push_str(&format!("- 新しく作られたカード: {} 枚\n", r.created.len()));
        }
        if !r.updated.is_empty() {
            md.push_str(&format!("- 中身を更新したカード: {} 枚\n", r.updated.len()));
        }
        for s in &r.skipped {
            md.push_str(&format!("- スキップ: {s}\n"));
        }
        for e in &r.errors {
            md.push_str(&format!("- エラー: {e}\n"));
        }
        if let Some(c) = &r.shell_capture {
            md.push_str(&format!(
                "- シェル出力（ウィンドウなし実行・終了コード {}）:\n\n",
                c.exit_code.map(|x| x.to_string()).unwrap_or_else(|| "不明".into())
            ));
            let (t, cut) = clip_text(&c.output, TRACE_PREVIEW_BYTES * 4);
            let f = fence_for(&t);
            md.push_str(&format!("{f}\n{t}\n{f}\n"));
            if cut || c.truncated {
                md.push_str("（以下省略）\n");
            }
        }
    }
    md
}

/// ジョブ終了時の書き出し: `.texos/logs/<日時>_<ラベル>.md/.json`、各カードの last_trace 更新、
/// 古いログの間引き。best-effort（失敗してもフローの結果には影響させない）。
/// 返り値 = 書いた .md のパス。
pub(crate) fn write_trace_log(root: &Path, trace: &Trace, sum: &TraceSummary) -> Option<PathBuf> {
    let entries = trace.snapshot();
    if entries.is_empty() && sum.fatal.is_none() {
        return None;
    }
    let finished_at = db::now_ms();
    let (dir, base) = match trace.run.lock().unwrap().as_ref() {
        Some(r) => (r.dir.clone(), r.base.clone()),
        None => (
            texos_dir(root).join("logs"),
            format!("{}_{}_job{}", stamp_of(trace.started_at), safe_name(sum.label), sum.job_id),
        ),
    };
    std::fs::create_dir_all(&dir).ok()?;
    let md_path = dir.join(format!("{base}.md"));
    let json_path = dir.join(format!("{base}.json"));
    let md = render_markdown(trace, &entries, sum, finished_at);
    std::fs::write(&md_path, md).ok()?;
    let json = serde_json::json!({
        "job_id": sum.job_id,
        "label": sum.label,
        "card": sum.card,
        "detail": trace.detail,
        "started_at": trace.started_at,
        "finished_at": finished_at,
        "fatal": sum.fatal,
        "entries": entries,
    });
    let _ = std::fs::write(&json_path, json.to_string());

    // 各カードの直近記録（反復などで同じカードが複数回あれば全部）。
    if let Ok(p) = open_project_at(root) {
        let mut by_card: HashMap<&str, Vec<&TraceEntry>> = HashMap::new();
        for e in &entries {
            by_card.entry(e.card.as_str()).or_default().push(e);
        }
        for (card, es) in by_card {
            let v = serde_json::json!({
                "log": md_path.to_string_lossy(),
                "job_label": sum.label,
                "job_id": sum.job_id,
                "at": trace.started_at,
                "detail": trace.detail,
                        "entries": es,
            });
            let _ = p.conn.execute(
                "UPDATE virtual_bodies SET last_trace = ?1 WHERE id = ?2",
                rusqlite::params![v.to_string(), card],
            );
        }
    }
    prune_logs(&dir);
    Some(md_path)
}

fn prune_logs(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut mds: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    if mds.len() <= KEEP_LOGS {
        return;
    }
    mds.sort(); // 先頭が日時なので名前順＝古い順
    let n = mds.len() - KEEP_LOGS;
    for p in mds.into_iter().take(n) {
        let _ = std::fs::remove_file(p.with_extension("json"));
        // v1.8.7 開発中の試作（実行ごとの中間結果フォルダ・不採用）の残骸があれば一緒に消す。
        let _ = std::fs::remove_dir_all(p.with_extension(""));
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(stem: &str, ext: &str, mime: &str, bytes: &[u8]) -> FlowPayload {
        FlowPayload { bytes: bytes.to_vec(), ext: ext.into(), mime: mime.into(), stem: stem.into(), meta: None }
    }

    #[test]
    fn records_in_completion_order_with_inputs_outputs() {
        let t = Trace::with_detail(true);
        t.begin("down", "コード抽出", "内蔵:md_codeblock", "");
        t.begin("up", "AI対話", "AI", "");
        t.end("up", &EvalOut::data(vec![pl("ans", "md", "text/markdown", b"```c\nint main(){}\n```")], None), 0);
        t.inputs("down", &[pl("ans", "md", "text/markdown", b"```c\nint main(){}\n```")]);
        t.note("down", "コマンド", "gcc {in} -o {out}");
        t.end("down", &EvalOut::data(vec![pl("ans", "c", "text/x-c", b"int main(){}")], None), 0);
        let v = t.snapshot();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].card, "up");
        assert_eq!(v[0].order, 1);
        assert_eq!(v[1].card, "down");
        assert_eq!(v[1].inputs.len(), 1);
        assert_eq!(v[1].outputs[0].text.as_deref(), Some("int main(){}"));
        assert!(v[1].outputs[0].sha256.is_some());
        assert_eq!(v[1].notes[0].0, "コマンド");
    }

    #[test]
    fn summary_mode_keeps_no_contents() {
        let t = Trace::with_detail(false);
        t.begin("a", "A", "AI", "");
        t.end("a", &EvalOut::data(vec![pl("x", "txt", "text/plain", b"secret")], None), 0);
        let v = t.snapshot();
        assert!(v[0].outputs[0].text.is_none());
        assert!(v[0].outputs[0].sha256.is_none());
        assert_eq!(v[0].outputs[0].bytes, 6);
        t.note_detail("a", "stderr", "warn");
        assert!(t.snapshot()[0].notes.is_empty());
    }

    #[test]
    fn unfinished_and_error_are_reported() {
        let t = Trace::with_detail(true);
        t.begin("a", "A", "外部コマンド", "");
        t.begin("b", "B", "外部コマンド", "");
        t.end("b", &EvalOut::failed(vec!["gcc: error".into()], Some(1), vec![]), 0);
        let v = t.snapshot();
        assert_eq!(v[0].status, "error");
        assert_eq!(v[0].exit, Some(1));
        assert_eq!(v[1].status, "unfinished");
        let md = render_markdown(
            &t,
            &v,
            &TraceSummary { job_id: 1, label: "L", card: "a", report: None, fatal: Some("boom") },
            db::now_ms(),
        );
        assert!(md.contains("gcc: error"));
        assert!(md.contains("未完了"));
        assert!(md.contains("実行失敗: boom"));
    }

    #[test]
    fn ai_run_record_is_rendered_and_serialized() {
        use crate::ai_models::*;
        let t = Trace::with_detail(false);
        t.begin("a", "AI", "AI", "");
        t.ai(
            "a",
            AiRunRecord {
                provider: "ollama".into(),
                endpoint: "http://localhost:11434".into(),
                model: "qwen3:8b".into(),
                digest: Some("abc123".into()),
                capabilities: Some(ModelCapabilities {
                    completion: Tri::Yes,
                    tools: Tri::Yes,
                    thinking: Tri::Yes,
                    vision: Tri::No,
                    embedding: Tri::No,
                }),
                thinking_mode: Some(ThinkingMode::BooleanOrLevels { levels: vec!["low".into()] }),
                think_setting: Some("low".into()),
                sent: SentParams { think: Some(serde_json::json!("low")), num_ctx: Some(8192), ..Default::default() },
                notes: vec![],
                thinking_returned: true,
            },
        );
        t.end("a", &EvalOut::failed(vec!["AI APIエラー (400)".into()], None, vec![]), 0);
        let v = t.snapshot();
        let md = render_markdown(
            &t,
            &v,
            &TraceSummary { job_id: 1, label: "L", card: "a", report: None, fatal: None },
            db::now_ms(),
        );
        assert!(md.contains("digest: abc123"));
        assert!(md.contains("think \"low\""));
        assert!(md.contains("num_ctx 8192"));
        assert!(md.contains("AI APIエラー (400)"), "失敗しても条件とエラーが両方残る");
        let j = serde_json::to_value(&v[0]).unwrap();
        assert_eq!(j["ai"]["digest"], "abc123");
        assert_eq!(j["ai"]["capabilities"]["vision"], "no");
        assert_eq!(j["ai"]["thinking_mode"]["kind"], "boolean_or_levels");
        assert_eq!(j["ai"]["sent"]["tools"], false);
    }

    #[test]
    fn clip_respects_char_boundary() {
        let s = "あいう"; // 9 bytes
        let (c, cut) = clip_text(s, 4);
        assert_eq!(c, "あ");
        assert!(cut);
    }

    #[test]
    fn fence_is_longer_than_content_backticks() {
        assert_eq!(fence_for("abc"), "```");
        assert_eq!(fence_for("x ```` y"), "`````");
    }

    #[test]
    fn binary_is_not_recorded_as_text() {
        let t = Trace::with_detail(true);
        t.begin("a", "A", "外部コマンド", "");
        t.end("a", &EvalOut::data(vec![pl("prog", "exe", "application/octet-stream", &[0x4d, 0x5a, 0, 1])], Some(0)), 0);
        let v = t.snapshot();
        assert!(v[0].outputs[0].text.is_none());
        assert!(v[0].outputs[0].sha256.is_some());
    }
}
