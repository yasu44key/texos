use crate::*;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use tauri::{Manager, State};

// ---------------------------------------------------------------------------
// オペレーション層 発行系 (v1.5.4 WS-P2 A: トリガ/タイマ/ウォッチャ)
// 正本 = オペレーション層設計.md §5.4.1 グループB＋§6。
// op カードは「フィルタ形のカード」— spec は filter_ref 列を流用し（新列なし・migration なし）、
// 実身は付箋/ドロップと同じセンチネル方式（op:empty を全 op カードで共有）。
// 発火 = run_flow の代理発行（req 矢印の先を終端に run_flow_for を呼ぶ）＝ジョブ台帳に乗る。
// ---------------------------------------------------------------------------

pub(crate) const OP_MIME: &str = "application/vnd.texos.op";

/// ヒューズの既定値（§6.2 — 無制限は明示設定のみ。生成時に UI が明示値を入れるが、
/// JSON 直編集でキーが消えた場合もこの既定で守る）。max_fires=0 だけが「無制限」。
const DEFAULT_MIN_INTERVAL: u64 = 5;
const DEFAULT_MAX_FIRES: u64 = 100;
const DEFAULT_DEBOUNCE: u64 = 2;

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct OpSpec {
    /// "trigger"（設置型▶）| "timer" | "watch"
    pub op: String,
    /// timer: 周期秒。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every: Option<u64>,
    /// timer: 時刻指定 "HH:MM"（ローカル時刻・1日1回）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// watch: 監視パス（ファイルまたはフォルダ）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// watch: 反応するイベント（"create"/"modify"）。省略=両方。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    /// watch: 静穏秒（変化が止まってから発火）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debounce: Option<u64>,
    /// ヒューズ: 発火間隔の下限秒。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_interval: Option<u64>,
    /// ヒューズ: 累計発火数の上限（0=無制限）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fires: Option<u64>,
    /// 累計発火数（seq の流儀で spec へ書き戻し・再起動しても残る）。
    #[serde(default)]
    pub fired_count: u64,
    /// v1.5.5: 一時停止（構成中の誤発火防止スイッチ・実機確認フィードバック）。
    /// true の間はスケジューラが発火しない。手動▶（fire_op）は動作確認の入口として通す。
    /// タイマ/ウォッチャは**停止状態で生成**され、配線後に「▶ 稼働開始」で動き出す。
    /// false は省略して保存（v1.5.4 の既存カードと同形＝稼働扱い・後方互換）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
    /// 未知キーの温存（設定ダイアログの流儀と同じ後方互換）。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl OpSpec {
    fn min_interval(&self) -> u64 {
        self.min_interval.unwrap_or(DEFAULT_MIN_INTERVAL)
    }
    fn max_fires(&self) -> u64 {
        self.max_fires.unwrap_or(DEFAULT_MAX_FIRES)
    }
    fn debounce(&self) -> u64 {
        self.debounce.unwrap_or(DEFAULT_DEBOUNCE)
    }
    fn wants(&self, ev: &str) -> bool {
        match &self.events {
            Some(v) => v.iter().any(|e| e == ev),
            None => true,
        }
    }
    /// max_fires ヒューズ（0=無制限）。
    fn fuse_blown(&self) -> bool {
        let max = self.max_fires();
        max != 0 && self.fired_count >= max
    }
}

/// spec JSON が op カードのものか（create_filter / update_filter の分岐点）。
pub(crate) fn is_op_spec(spec: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(spec)
        .map(|v| v.get("op").map(|o| o.is_string()).unwrap_or(false))
        .unwrap_or(false)
}

pub(crate) fn parse_op_spec(spec: &str) -> Result<OpSpec, String> {
    let s: OpSpec = serde_json::from_str(spec).map_err(|e| format!("opカード定義が不正です: {e}"))?;
    match s.op.as_str() {
        "trigger" => {}
        "timer" => {
            let every_ok = s.every.map(|e| e > 0).unwrap_or(false);
            let at_ok = s
                .at
                .as_deref()
                .map(|a| parse_hhmm(a).is_some())
                .unwrap_or(false);
            if !every_ok && !at_ok {
                return Err("タイマには every（秒）か at（HH:MM）のどちらかが必要です".into());
            }
        }
        "watch" => {
            if s.path.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err("ウォッチャには path（監視するファイル/フォルダ）が必要です".into());
            }
        }
        other => return Err(format!("未知のオペレーション種別です: {other}")),
    }
    Ok(s)
}

fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

/// op カードを作成（kind='op'・センチネル実身 op:empty・spec は filter_ref 列）。
/// create_filter から is_op_spec 判定で委譲される（フロントは create_filter を使い続ける）。
pub(crate) fn create_op_card(
    label: String,
    spec: String,
    x: f64,
    y: f64,
    state: &AppState,
) -> Result<String, String> {
    parse_op_spec(&spec)?;
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES ('op:empty', ?1, 0, ?2)",
            rusqlite::params![OP_MIME, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
             VALUES (?1, 'op:empty', ?2, ?3, ?4, ?5, 'op', ?6)",
            rusqlite::params![vid, label, x, y, db::now_ms(), spec],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// op カードの spec 更新（update_filter から委譲）。
pub(crate) fn update_op_card(id: &str, spec: &str, state: &AppState) -> Result<(), String> {
    parse_op_spec(spec)?;
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let n = p
        .conn
        .execute(
            "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2 AND kind = 'op'",
            rusqlite::params![spec, id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("opカードが見つかりません".into());
    }
    Ok(())
}

/// 発火準備（短ロック内で呼ぶ）: ヒューズ確認 → req ターゲット収集 → fired_count 書き戻し。
/// 返り値 = 発行すべき終端フィルタカード id 群。ターゲット 0 件なら書き戻さない。
fn fire_prepare(p: &Project, id: &str) -> Result<Vec<String>, String> {
    let spec_json: String = p
        .conn
        .query_row(
            "SELECT filter_ref FROM virtual_bodies
             WHERE id = ?1 AND kind = 'op' AND deleted_at IS NULL",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("opカードが見つかりません")?;
    let mut spec = parse_op_spec(&spec_json)?;
    if spec.fuse_blown() {
        return Err(format!(
            "発火上限（max_fires={}）に達しています。設定で上限を増やすか 0（無制限）にしてください",
            spec.max_fires()
        ));
    }
    // req 矢印の先の生きているフィルタカードだけを終端候補にする（データカード等は無視）。
    let mut stmt = p
        .conn
        .prepare(
            "SELECT v.id FROM relations rel
             JOIN virtual_bodies v ON v.id = rel.to_id AND v.deleted_at IS NULL
             WHERE rel.from_id = ?1 AND rel.kind = 'req' AND rel.deleted_at IS NULL
               AND v.kind = 'filter'
             ORDER BY rel.created_at",
        )
        .map_err(|e| e.to_string())?;
    let targets: Vec<String> = stmt
        .query_map(rusqlite::params![id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    if targets.is_empty() {
        return Ok(targets);
    }
    // fired_count++（seq の流儀 = spec へ書き戻し。1発火=1カウント・対象数は数えない）
    spec.fired_count += 1;
    let json = serde_json::to_string(&spec).map_err(|e| e.to_string())?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2 AND kind = 'op'",
            rusqlite::params![json, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(targets)
}

/// ターゲット群へ run_flow を代理発行（各終端 1 スレッド）。ジョブ台帳に乗るので
/// パネル表示・Stop-all・同一終端の多重拒否（＝スキップ・キューしない）は台帳側が担う。
fn dispatch(targets: Vec<String>, app: &tauri::AppHandle) {
    for t in targets {
        let app2 = app.clone();
        std::thread::spawn(move || {
            if let Err(e) = run_flow_for(&t, &app2) {
                // 多重起動の拒否（追い越しスキップ）や実行エラーはここで完結（ヒューズの精神）。
                eprintln!("[op] fire skipped/failed for {t}: {e}");
            }
        });
    }
}

/// タイマ/ウォッチャの稼働/停止トグル（v1.5.5・構成中の誤発火防止スイッチ）。
/// 返り値 = 新しい paused 状態。ランタイム状態の破棄は tick 側が paused を見て行う。
#[tauri::command]
pub(crate) fn toggle_op_paused(id: String, state: State<AppState>) -> Result<bool, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let spec_json: String = p
        .conn
        .query_row(
            "SELECT filter_ref FROM virtual_bodies
             WHERE id = ?1 AND kind = 'op' AND deleted_at IS NULL",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("opカードが見つかりません")?;
    let mut spec = parse_op_spec(&spec_json)?;
    spec.paused = !spec.paused;
    let json = serde_json::to_string(&spec).map_err(|e| e.to_string())?;
    p.conn
        .execute(
            "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2 AND kind = 'op'",
            rusqlite::params![json, id],
        )
        .map_err(|e| e.to_string())?;
    Ok(spec.paused)
}

/// トリガ（設置型▶）の手動発火。req 矢印の先（複数可）へ一括発行する。
/// タイマ/ウォッチャも手動発火できる（動作確認の入口・スケジューラと同じ経路。
/// **一時停止中でも通す** — 停止＝自動発火の停止であり、手動テストの禁止ではない）。
#[tauri::command(async)]
pub(crate) fn fire_op(id: String, app: tauri::AppHandle) -> Result<usize, String> {
    let state: State<AppState> = app.state();
    let targets = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        fire_prepare(p, &id)?
    };
    if targets.is_empty() {
        return Err(
            "リクエスト矢印の先に実行できるカードがありません（opカードから実行したいフィルタカードへ矢印を引いてください）"
                .into(),
        );
    }
    let n = targets.len();
    dispatch(targets, &app);
    Ok(n)
}

// ---------------------------------------------------------------------------
// スケジューラ（アプリ常駐 1 スレッド・tick 1 秒）
// 対象は「現在開いているプロジェクト」の op カードだけ（v1.5 は個人ツール —
// プロジェクトを閉じたら止まる）。state.current は短く読み、保持したまま発行しない。
// ---------------------------------------------------------------------------

#[derive(Default)]
struct OpRuntime {
    /// 最終発火（min_interval / every の基準。揮発 — 再起動でリセット）。
    last_fire: Option<Instant>,
    /// timer at: 今日すでに発火したか（"YYYY-MM-DD"）。
    fired_date: Option<String>,
    /// timer every: 初回観測時刻（起動直後の即発火を避けるベースライン）。
    baseline: Option<Instant>,
    /// watch: 前回スナップショット（ファイル名 → mtime ms）。None=未観測。
    snapshot: Option<HashMap<String, i64>>,
    /// watch: debounce の締め切り（変化を見るたび先送り・静穏になったら発火）。
    pending: Option<Instant>,
}

pub(crate) fn start_op_scheduler(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        // キー = プロジェクトルート + カード id（切替や開き直しで状態が混ざらない）。
        let mut rt: HashMap<String, OpRuntime> = HashMap::new();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            tick(&app, &mut rt);
        }
    });
}

fn tick(app: &tauri::AppHandle, rt: &mut HashMap<String, OpRuntime>) {
    let state: State<AppState> = app.state();
    // 短ロック①: op 一覧だけ読んですぐ放す。
    let ops: Vec<(String, String, String)> = {
        let guard = match state.current.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let p = match guard.as_ref() {
            Some(p) => p,
            None => return,
        };
        let root = p.root.to_string_lossy().into_owned();
        let mut stmt = match p.conn.prepare(
            "SELECT id, filter_ref FROM virtual_bodies
             WHERE kind = 'op' AND deleted_at IS NULL AND filter_ref IS NOT NULL",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let rows: Vec<(String, String, String)> =
            match stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                Ok(rows) => rows
                    .filter_map(|r| r.ok())
                    .map(|(id, spec)| (root.clone(), id, spec))
                    .collect(),
                Err(_) => return,
            };
        rows
    };
    let now = Instant::now();
    for (root, id, spec_json) in ops {
        let Ok(spec) = parse_op_spec(&spec_json) else {
            continue; // 壊れた spec は黙ってスキップ（設定を開けば validation が案内する）
        };
        let key = format!("{root}\n{id}");
        // v1.5.5: 一時停止中は発火しない。ランタイム状態も破棄＝再開時にベースラインを
        // 取り直す（停止中に構成し直したフォルダの変化・溜まった時間で発火させない）。
        if spec.paused {
            rt.remove(&key);
            continue;
        }
        if spec.fuse_blown() {
            continue;
        }
        let r = rt.entry(key).or_default();
        let due = match spec.op.as_str() {
            "timer" => timer_due(&spec, r, now),
            "watch" => watch_due(&spec, r, now),
            _ => false, // trigger は手動（▶）のみ
        };
        if !due {
            continue;
        }
        r.last_fire = Some(now);
        // 短ロック②: ターゲット収集＋fired_count 書き戻し（fuse は fire_prepare が再確認）。
        let targets = {
            let guard = match state.current.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            let p = match guard.as_ref() {
                Some(p) => p,
                None => continue,
            };
            // tick 中にプロジェクトが切り替わっていたら発行しない（起動元束縛の原則）。
            if p.root.to_string_lossy() != root {
                continue;
            }
            match fire_prepare(p, &id) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[op] scheduler fire_prepare failed for {id}: {e}");
                    continue;
                }
            }
        };
        dispatch(targets, app);
    }
}

/// min_interval ヒューズ（発火間隔の下限）。
fn min_ok(spec: &OpSpec, r: &OpRuntime, now: Instant) -> bool {
    match r.last_fire {
        Some(t) => now.duration_since(t).as_secs() >= spec.min_interval(),
        None => true,
    }
}

fn timer_due(spec: &OpSpec, r: &mut OpRuntime, now: Instant) -> bool {
    // at "HH:MM": ローカル時刻が一致した最初の tick で 1 日 1 回。
    if let Some((h, m)) = spec.at.as_deref().and_then(parse_hhmm) {
        let local = chrono::Local::now();
        use chrono::Timelike;
        let today = local.format("%Y-%m-%d").to_string();
        if local.hour() == h && local.minute() == m && r.fired_date.as_deref() != Some(&today) {
            if min_ok(spec, r, now) {
                r.fired_date = Some(today);
                return true;
            }
        }
    }
    // every: 初回観測をベースラインにし、以後は間隔経過で発火（起動直後の即発火はしない）。
    if let Some(every) = spec.every.filter(|e| *e > 0) {
        let base = match r.last_fire.or(r.baseline) {
            Some(t) => t,
            None => {
                r.baseline = Some(now);
                return false;
            }
        };
        let iv = every.max(spec.min_interval());
        if now.duration_since(base).as_secs() >= iv {
            return true;
        }
    }
    false
}

fn watch_due(spec: &OpSpec, r: &mut OpRuntime, now: Instant) -> bool {
    let path = Path::new(spec.path.as_deref().unwrap_or(""));
    let snap = read_snapshot(path);
    let Some(old) = r.snapshot.take() else {
        // 初回観測 = ベースライン（既存ファイルでは発火しない）。
        r.snapshot = Some(snap);
        return false;
    };
    let mut changed = false;
    for (name, mtime) in &snap {
        match old.get(name) {
            None => changed |= spec.wants("create"),
            Some(o) if o != mtime => changed |= spec.wants("modify"),
            _ => {}
        }
    }
    r.snapshot = Some(snap);
    if changed {
        // 変化を見るたび締め切りを先送り（連続書き込み中は待つ＝debounce）。
        r.pending = Some(now + Duration::from_secs(spec.debounce()));
        return false;
    }
    if let Some(d) = r.pending {
        if now >= d && min_ok(spec, r, now) {
            r.pending = None;
            return true;
        }
    }
    false
}

/// 監視対象のスナップショット。フォルダ=直下エントリの {名前→mtime}、ファイル=1エントリ、
/// 不在=空（→ ファイルが現れたら create）。notify クレートではなくポーリング（§6.2 の
/// 自己発火リスクに対し debounce/min_interval のヒューズと相性がよく、実装も安全）。
fn read_snapshot(path: &Path) -> HashMap<String, i64> {
    fn mtime_ms(p: &Path) -> Option<i64> {
        let t = std::fs::metadata(p).ok()?.modified().ok()?;
        Some(
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        )
    }
    let mut map = HashMap::new();
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    if let Some(mt) = mtime_ms(&p) {
                        map.insert(e.file_name().to_string_lossy().into_owned(), mt);
                    }
                }
            }
        }
    } else if path.is_file() {
        if let Some(mt) = mtime_ms(path) {
            map.insert(
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                mt,
            );
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_spec_validation() {
        assert!(parse_op_spec(r#"{"op":"trigger"}"#).is_ok());
        assert!(parse_op_spec(r#"{"op":"timer","every":60}"#).is_ok());
        assert!(parse_op_spec(r#"{"op":"timer","at":"07:30"}"#).is_ok());
        assert!(parse_op_spec(r#"{"op":"timer"}"#).is_err()); // every も at も無い
        assert!(parse_op_spec(r#"{"op":"timer","at":"25:00"}"#).is_err());
        assert!(parse_op_spec(r#"{"op":"watch","path":"C:/tmp"}"#).is_ok());
        assert!(parse_op_spec(r#"{"op":"watch"}"#).is_err()); // path 無し
        assert!(parse_op_spec(r#"{"op":"unknown"}"#).is_err());
        assert!(is_op_spec(r#"{"op":"timer","every":5}"#));
        assert!(!is_op_spec(r#"{"builtin":"select"}"#));
    }

    #[test]
    fn op_spec_roundtrip_keeps_unknown_keys() {
        // fired_count 書き戻しで未知キーが落ちない（設定ダイアログの後方互換の流儀）。
        let mut s = parse_op_spec(r#"{"op":"timer","every":60,"custom_note":"x"}"#).unwrap();
        s.fired_count += 1;
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("custom_note"));
        assert!(json.contains("\"fired_count\":1"));
    }

    #[test]
    fn fuse_defaults() {
        let s = parse_op_spec(r#"{"op":"timer","every":60}"#).unwrap();
        assert_eq!(s.min_interval(), 5);
        assert_eq!(s.max_fires(), 100);
        // 明示 0 = 無制限
        let s = parse_op_spec(r#"{"op":"timer","every":60,"max_fires":0,"fired_count":9999}"#).unwrap();
        assert!(!s.fuse_blown());
        let s = parse_op_spec(r#"{"op":"timer","every":60,"max_fires":3,"fired_count":3}"#).unwrap();
        assert!(s.fuse_blown());
    }

    #[test]
    fn paused_flag_roundtrip() {
        // 既定=稼働（v1.5.4 の既存カードは paused キー無し＝動き続ける・後方互換）。
        let s = parse_op_spec(r#"{"op":"timer","every":60}"#).unwrap();
        assert!(!s.paused);
        let mut s = parse_op_spec(r#"{"op":"watch","path":"C:/x","paused":true}"#).unwrap();
        assert!(s.paused);
        // 稼働に戻したら paused キーは省略される（既存カードと同形を保つ）。
        s.paused = false;
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("paused"));
    }

    #[test]
    fn hhmm_parse() {
        assert_eq!(parse_hhmm("07:30"), Some((7, 30)));
        assert_eq!(parse_hhmm("23:59"), Some((23, 59)));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("0730"), None);
    }
}
