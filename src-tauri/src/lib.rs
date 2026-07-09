mod cas;
#[cfg(windows)]
mod clipboard_out;
mod db;
#[cfg(windows)]
mod drag_drop;
mod flow;
mod os_icon;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;
/// A currently-open project: a self-contained folder.
///   <root>/                  project folder
///     ├── <committed files>  editable real files (human-named)
///     └── .texos/
///         ├── texos.db       object graph
///         ├── cas/           immutable real bodies
///         └── snapshots/     full copies (next phase)
pub struct Project {
    pub conn: Connection,
    pub root: PathBuf,
    pub cas_root: PathBuf,
}
pub struct AppState {
    pub current: Mutex<Option<Project>>,
    pub app_data: PathBuf,
    /// v1.5.1 WS-B ジョブレジストリ（凍結仕様 §12.B）: 旧 singleton
    /// （flow_cancel / chat_cancel / rec_stdin / flow_running）を JobId キーの台帳へ移送。
    /// 中断フラグ・stdin 制御はジョブ別。v1.5.2 WS-C: 多重起動判定は「同一終端カードの
    /// Flow が居るか」へ精密化（並行解禁）。
    pub jobs: Jobs,
    /// v1.5.2 WS-C 資源セマフォ（凍結仕様 §6）: heavy（外部プロセス並行上限・FIFO待ち）／
    /// ai（エンドポイント単位・既定1・即拒否）／device（入力デバイス単位・排他・即拒否）。
    pub resources: ResourceLimits,
    /// v1.5.2 WS-C（凍結 §11.4）: プロジェクトルート単位の採番ロック。並行フローが同一
    /// プロジェクトへ materialize するとき、固定名照合と prefix 連番（seq）の採番を直列化する。
    pub write_locks: Mutex<std::collections::HashMap<String, std::sync::Arc<Mutex<()>>>>,
    /// v1.5 Text-To-Image: TexoS が起動・管理する diffusers 常駐デーモン（t2i_server.py）の
    /// 子プロセスハンドル。t2i_stop_server で明示停止できるよう保持する。
    pub t2i_server: Mutex<Option<std::process::Child>>,
}

/// プロジェクトルート単位の書き込み直列化ロックを取得（無ければ作る）。
pub(crate) fn project_write_lock(state: &AppState, root: &Path) -> std::sync::Arc<Mutex<()>> {
    let key = root.to_string_lossy().into_owned();
    state
        .write_locks
        .lock()
        .unwrap()
        .entry(key)
        .or_insert_with(|| std::sync::Arc::new(Mutex::new(())))
        .clone()
}
// --- v1.5.1 WS-B ジョブレジストリ（凍結仕様 §12.B・実装指示書 WS-B）--------------------
// singleton（同時1つを物理強制する AppState フィールド群）を JobId キーの台帳へ置き換える
// ＝中央 kill スイッチ（cancel_all）の獲得・WS-C 並行解禁の土台。ジョブは揮発（凍結 §11.5・
// 永続化しない）。v1.5.2 WS-C: 並行解禁＝Flow の排他は「同一終端カード」だけに縮小し、
// 調停は資源セマフォ（ResourceLimits）が担う。セットアップ系（モデルDL等）も台帳に乗る。

pub type JobId = u64;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Flow,
    Chat,
    /// v1.5.2 WS-C: セットアップ系（whisper/ollama/t2i のDL・uv 環境構築）。
    /// プロジェクト非依存（app_data へ書く）なので project_root は空のまま。
    Setup,
}

pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    /// 表示用ラベル。WS-C から登録前にカードを読むため登録時に確定する。
    pub label: String,
    /// Flow の終端カード id（同一フロー多重起動の拒否キー・パネルの spec 参照用）。
    pub card: Option<String>,
    pub started_at: i64,
    /// 中断フラグ（UIの中断ボタン → break_flow / cancel_all）。run_flow が DB の Mutex を
    /// 握っている間も break_flow はこれだけを触るので外部コマンドがストールしてもブレイクできる。
    pub cancel: std::sync::atomic::AtomicBool,
    /// 子プロセス stdin チャネル（v1.4.9 の rec_stdin のジョブ別版）: 実行中フィルタ
    /// （録音など record モード）の stdin 書き込み端。flow_send_stdin が 'q' 等を書く。
    pub control: flow::StdinSlot,
    /// 起動元プロジェクト（WS-C: ジョブ専用 DB 接続の root＝materialize の行き先）。
    /// セットアップ系は空（プロジェクト非依存）。
    pub project_root: String,
    /// t2i 生成の HTTP を投げている間だけ true。break_flow がデーモンへ /cancel を送るか
    /// の判定（並行時に他ジョブの生成を誤って止めない）。
    pub t2i_active: std::sync::atomic::AtomicBool,
    /// job:progress のスロットル（~10/s・凍結 §11.6）用の最終 emit 時刻。
    pub progress_at: Mutex<std::time::Instant>,
}

/// list_jobs の返却行（背景ジョブパネルの復元用）。
#[derive(Serialize)]
pub struct JobInfo {
    pub id: JobId,
    pub kind: JobKind,
    pub label: String,
    pub started_at: i64,
    pub card: Option<String>,
}

pub struct Jobs {
    next: std::sync::atomic::AtomicU64,
    map: Mutex<std::collections::HashMap<JobId, std::sync::Arc<Job>>>,
}

impl Jobs {
    pub fn new() -> Self {
        Jobs {
            next: std::sync::atomic::AtomicU64::new(1),
            map: Mutex::new(std::collections::HashMap::new()),
        }
    }
    fn make_job(
        &self,
        kind: JobKind,
        label: &str,
        card: Option<&str>,
        project_root: &str,
    ) -> std::sync::Arc<Job> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::sync::Arc::new(Job {
            id,
            kind,
            label: label.to_string(),
            card: card.map(str::to_string),
            started_at: db::now_ms(),
            cancel: std::sync::atomic::AtomicBool::new(false),
            control: Mutex::new(None),
            project_root: project_root.to_string(),
            t2i_active: std::sync::atomic::AtomicBool::new(false),
            progress_at: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        })
    }
    /// Chat の登録（排他なし — 同一エンドポイントの同時実行は ai セマフォが拒否する）。
    pub fn register_chat(&self, label: &str, project_root: &str) -> std::sync::Arc<Job> {
        let job = self.make_job(JobKind::Chat, label, None, project_root);
        self.map.lock().unwrap().insert(job.id, job.clone());
        job
    }
    /// Flow の登録（WS-C 並行解禁）: 排他は**同一終端カード**の Flow だけ（v1.4.10 再入
    /// エラーの一般化）。別フローは並行し、資源の取り合いはセマフォが調停する。
    pub fn register_flow(
        &self,
        card: &str,
        label: &str,
        project_root: &str,
    ) -> Result<std::sync::Arc<Job>, String> {
        let mut map = self.map.lock().unwrap();
        if map
            .values()
            .any(|j| j.kind == JobKind::Flow && j.card.as_deref() == Some(card))
        {
            return Err(
                "このフローは実行中です。完了（録音は「停止」）してからもう一度実行してください"
                    .into(),
            );
        }
        let job = self.make_job(JobKind::Flow, label, Some(card), project_root);
        map.insert(job.id, job.clone());
        Ok(job)
    }
    /// セットアップ系の登録（WS-C）: 同一ラベルの Setup が実行中なら二重起動を拒否する
    /// （同じモデルの二重DLなど）。別コンポーネントの並行導入は許す。
    pub fn register_setup(&self, label: &str) -> Result<std::sync::Arc<Job>, String> {
        let mut map = self.map.lock().unwrap();
        if map
            .values()
            .any(|j| j.kind == JobKind::Setup && j.label == label)
        {
            return Err(format!("「{label}」は既に実行中です"));
        }
        let job = self.make_job(JobKind::Setup, label, None, "");
        map.insert(job.id, job.clone());
        Ok(job)
    }
    pub fn get(&self, id: JobId) -> Option<std::sync::Arc<Job>> {
        self.map.lock().unwrap().get(&id).cloned()
    }
    pub fn remove(&self, id: JobId) {
        self.map.lock().unwrap().remove(&id);
    }
    /// 指定プロジェクトに束縛された実行中ジョブがあるか（send_selection の保護用）。
    pub fn any_bound_to(&self, root: &str) -> bool {
        self.map
            .lock()
            .unwrap()
            .values()
            .any(|j| !j.project_root.is_empty() && j.project_root == root)
    }
    /// 中央 kill スイッチ（安全設計 §6.3）: 全ジョブの cancel を立てる。子プロセスの
    /// 物理停止は各実行系の監視ループ（taskkill /T＝既存流用）が担う。
    pub fn cancel_all(&self) {
        for j in self.map.lock().unwrap().values() {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    /// Chat ジョブだけ cancel（job_id 未着の停止ボタンのフォールバック。Chat は単一運用）。
    pub fn cancel_chats(&self) {
        for j in self.map.lock().unwrap().values() {
            if j.kind == JobKind::Chat {
                j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    pub fn list(&self) -> Vec<JobInfo> {
        let mut v: Vec<JobInfo> = self
            .map
            .lock()
            .unwrap()
            .values()
            .map(|j| JobInfo {
                id: j.id,
                kind: j.kind,
                label: j.label.clone(),
                started_at: j.started_at,
                card: j.card.clone(),
            })
            .collect();
        v.sort_by_key(|j| j.id);
        v
    }
}

/// job:progress の emit（凍結 §7.2/§11.6）。percent 連打はジョブ別に ~10/s へスロットル。
/// note（ノード名・順番待ち等）は低頻度なので素通しし、最終 emit 時刻だけ更新する。
pub(crate) fn emit_job_progress(
    app: &tauri::AppHandle,
    job: &Job,
    note: Option<&str>,
    percent: Option<u32>,
) {
    use tauri::Emitter;
    {
        let mut last = job.progress_at.lock().unwrap();
        let now = std::time::Instant::now();
        if note.is_none()
            && now.duration_since(*last) < std::time::Duration::from_millis(100)
            && percent != Some(100)
        {
            return;
        }
        *last = now;
    }
    let _ = app.emit(
        "job:progress",
        serde_json::json!({ "job_id": job.id, "note": note, "percent": percent }),
    );
}

// --- v1.5.2 WS-C 資源セマフォ（凍結仕様 §6・§11.2〜11.3）------------------------------
// 「安全のための上限（ヒューズ）と資源保護のための上限は同じ機構」。permit は RAII
// （Drop で解放）。取得順序は device→ai→heavy に固定（§12.D デッドロック防止）。
// ノードは実行中しか permit を持たない（収集フェーズの pull 再帰では持たない）ため
// 入れ子保持は発生しない。

/// チケット制の公平（FIFO）カウンティングセマフォ。std に Semaphore が無いので
/// Mutex＋Condvar で自前実装する（tokio 非依存・待機は 200ms 刻みで cancel を検査）。
pub struct FairSemaphore {
    permits: usize,
    state: Mutex<SemState>,
    cv: std::sync::Condvar,
}

struct SemState {
    active: usize,
    queue: std::collections::VecDeque<u64>,
    next_ticket: u64,
}

pub struct Permit {
    sem: std::sync::Arc<FairSemaphore>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut st = self.sem.state.lock().unwrap();
        st.active -= 1;
        drop(st);
        self.sem.cv.notify_all();
    }
}

impl FairSemaphore {
    pub fn new(permits: usize) -> std::sync::Arc<Self> {
        std::sync::Arc::new(FairSemaphore {
            permits: permits.max(1),
            state: Mutex::new(SemState {
                active: 0,
                queue: std::collections::VecDeque::new(),
                next_ticket: 0,
            }),
            cv: std::sync::Condvar::new(),
        })
    }

    /// 即時取得（排他資源＝device/ai 用）。取れなければ None（呼び出し側が拒否＋案内）。
    pub fn try_acquire(self: &std::sync::Arc<Self>) -> Option<Permit> {
        let mut st = self.state.lock().unwrap();
        if st.active < self.permits && st.queue.is_empty() {
            st.active += 1;
            Some(Permit { sem: self.clone() })
        } else {
            None
        }
    }

    /// FIFO 待ち取得（heavy 用）。待ちに入るとき `on_wait` を1回呼ぶ（UI へ「順番待ち」）。
    /// 待機中も cancel（中断ボタン/Stop-all）が効く。
    pub fn acquire(
        self: &std::sync::Arc<Self>,
        cancel: &std::sync::atomic::AtomicBool,
        on_wait: impl FnOnce(),
    ) -> Result<Permit, String> {
        use std::sync::atomic::Ordering;
        let my;
        {
            let mut st = self.state.lock().unwrap();
            if st.active < self.permits && st.queue.is_empty() {
                st.active += 1;
                return Ok(Permit { sem: self.clone() });
            }
            my = st.next_ticket;
            st.next_ticket += 1;
            st.queue.push_back(my);
        }
        on_wait();
        let mut st = self.state.lock().unwrap();
        loop {
            if cancel.load(Ordering::Relaxed) {
                st.queue.retain(|&t| t != my);
                drop(st);
                self.cv.notify_all();
                return Err("中断されました".into());
            }
            if st.queue.front() == Some(&my) && st.active < self.permits {
                st.queue.pop_front();
                st.active += 1;
                drop(st);
                self.cv.notify_all();
                return Ok(Permit { sem: self.clone() });
            }
            let (g, _) = self
                .cv
                .wait_timeout(st, std::time::Duration::from_millis(200))
                .unwrap();
            st = g;
        }
    }
}

/// 資源クラス別のセマフォ台帳（凍結 §6）。
pub struct ResourceLimits {
    /// 重い外部プロセスの並行上限（既定=論理コア数・下限2）。超過は FIFO 待ち。
    pub heavy: std::sync::Arc<FairSemaphore>,
    /// AI エンドポイント単位（正規化 URL キー・既定許可数1）。超過は即拒否。
    ai: Mutex<std::collections::HashMap<String, std::sync::Arc<FairSemaphore>>>,
    /// 入力デバイス単位（排他=1）。超過は即拒否。
    device: Mutex<std::collections::HashMap<String, std::sync::Arc<FairSemaphore>>>,
}

impl ResourceLimits {
    pub fn new() -> Self {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2);
        ResourceLimits {
            heavy: FairSemaphore::new(n),
            ai: Mutex::new(std::collections::HashMap::new()),
            device: Mutex::new(std::collections::HashMap::new()),
        }
    }
    /// エンドポイントのセマフォ（無ければ許可数1で作る）。key は正規化済み URL を渡すこと。
    pub fn ai_sem(&self, endpoint_norm: &str) -> std::sync::Arc<FairSemaphore> {
        self.ai
            .lock()
            .unwrap()
            .entry(endpoint_norm.to_string())
            .or_insert_with(|| FairSemaphore::new(1))
            .clone()
    }
    pub fn device_sem(&self, name: &str) -> std::sync::Arc<FairSemaphore> {
        self.device
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_insert_with(|| FairSemaphore::new(1))
            .clone()
    }
}

/// AI エンドポイントの同一性（凍結 §11.3）: trim・スキーム/ホスト部を小文字化・末尾 '/' 除去。
pub(crate) fn normalize_endpoint(url: &str) -> String {
    let s = url.trim().trim_end_matches('/');
    match s.find("://") {
        Some(i) => {
            // スキームとホスト（最初のパス区切りまで）を小文字化。パスは大小を保つ。
            let after = &s[i + 3..];
            let host_end = after.find('/').map(|j| i + 3 + j).unwrap_or(s.len());
            format!("{}{}", s[..host_end].to_lowercase(), &s[host_end..])
        }
        None => s.to_lowercase(),
    }
}

/// v1.5.2 WS-C: セットアップ系コマンド（モデルDL・環境構築）を台帳ジョブとして走らせる
/// 共通ラッパ。登録（同一ラベルは二重起動拒否）→ job:started → 本体 → job:done/failed。
/// 本体クロージャは Job を受け取り、DL ループ等で job.cancel を検査して中断に応える。
pub(crate) fn run_setup_job<T>(
    state: &AppState,
    app: &tauri::AppHandle,
    label: &str,
    f: impl FnOnce(&Job) -> Result<T, String>,
) -> Result<T, String> {
    use tauri::Emitter;
    let job = state.jobs.register_setup(label)?;
    let _guard = JobGuard::new(&state.jobs, job.id);
    let _ = app.emit(
        "job:started",
        serde_json::json!({ "job_id": job.id, "kind": "setup", "label": label }),
    );
    let result = f(&job);
    let _ = app.emit(
        if result.is_ok() { "job:done" } else { "job:failed" },
        serde_json::json!({ "job_id": job.id }),
    );
    result
}

/// ジョブの台帳からの確実な除去。drop で remove する（早期 return / panic / `?` の
/// どの離脱でも解放する — 旧 RunningGuard と同じ役目のジョブ台帳版）。
pub struct JobGuard<'a> {
    jobs: &'a Jobs,
    id: JobId,
}
impl<'a> JobGuard<'a> {
    pub fn new(jobs: &'a Jobs, id: JobId) -> Self {
        JobGuard { jobs, id }
    }
}
impl Drop for JobGuard<'_> {
    fn drop(&mut self) {
        self.jobs.remove(self.id);
    }
}
/// v1.4.11 非ブロッキング実行: current プロジェクトを**短時間だけ**ロックしてクロージャを
/// 実行し、即座に解放する。長時間実行（execute_node = 外部/AI/録音）はこのロックの外で
/// 行うことで「実行中は DB を手放す」を成立させる（録音/AI 中も UI が固まらない）。
/// 注意: std の Mutex は**非再入**。with_db のクロージャ内でさらに with_db を呼ばないこと。
/// （eval_filter / collect_context は各 DB 接触を別々の短い with_db に分け、その間＝
/// 実行・再帰・ファイル読みではロックを保持しない設計。）
fn with_db<T>(
    state: &AppState,
    f: impl FnOnce(&Project) -> Result<T, String>,
) -> Result<T, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    f(p)
}
/// v1.5.2 WS-C: プロジェクト切替ガードは撤廃（ジョブは起動元プロジェクトへの専用 DB 接続で
/// 束縛される＝切替しても評価・materialize は起動元へ当たる）。残るのは**構造を壊す操作**の
/// 保護だけ: send_selection_to_project（カードの移動・実ファイルの持ち出し）は、現在の
/// プロジェクトに束縛された実行中ジョブがある間は拒否する。
fn ensure_no_jobs_bound_to_current(state: &AppState) -> Result<(), String> {
    let root: Option<String> = state
        .current
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.root.to_string_lossy().into_owned());
    if let Some(root) = root {
        if state.jobs.any_bound_to(&root) {
            return Err(
                "このプロジェクトで実行中のジョブがあります。完了か中断（録音は「停止」）してから実行してください"
                    .into(),
            );
        }
    }
    Ok(())
}
#[derive(Serialize, Deserialize, Default)]
struct Config {
    recent: Vec<String>,
    last: Option<String>,
    /// v1.4.8: 現在選択中の Whisper モデル名（"small"/"medium" 等。既定 small）。
    #[serde(default)]
    whisper_model: Option<String>,
    /// v1.5: 現在選択中の Text-To-Image モデル名（"sd1.5" 等。既定 sd1.5）。
    #[serde(default)]
    t2i_model: Option<String>,
    /// v1.5: Hugging Face トークン（ゲート付き FLUX 公式モデルの取得に使う）。
    /// デーモン起動時に環境変数 HF_TOKEN として渡す。
    #[serde(default)]
    t2i_hf_token: Option<String>,
    /// v1.4.27 WS2: Python 実行環境の明示切替（"managed"=TexoS 構築 env / "system"=
    /// システム Python）。None は自動（管理 env → システムの順で解決）。
    #[serde(default)]
    python_env: Option<String>,
}
#[derive(Serialize)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
}
#[derive(Serialize)]
pub struct VirtualBodyInfo {
    pub id: String,
    pub real_id: String,
    pub label: String,
    pub mime_type: String,
    pub pos_x: f64,
    pub pos_y: f64,
    pub committed_path: Option<String>,
    pub committed_modified: bool,
    /// If set, this card references another TexoS project (its folder path).
    pub project_ref: Option<String>,
    /// Original file extension (no dot), for icon selection.
    pub ext: Option<String>,
    /// Whether the in-canvas preview/editor is expanded.
    pub expanded: bool,
    /// If set, this card is a web URL link.
    pub url_ref: Option<String>,
    /// Optional manual comment shown on the card.
    pub memo: Option<String>,
    /// Stacking order on the canvas (higher = front).
    pub z_order: i64,
    /// If set, this card is a link to an OS folder (not a TexoS project).
    pub folder_ref: Option<String>,
    /// Where the file was imported from (借用元; informational only).
    pub origin_path: Option<String>,
    /// Current mtime (unix ms) of the committed file, for session-change detection.
    pub cur_mtime: Option<i64>,
    /// How the view resolves (v1.3-⑩): committed/sibling/origin/cas/fork-live/
    /// fork-cas/none. None for sentinel cards (project/url/folder).
    pub view_source: Option<String>,
    /// Width of the in-canvas text editor panel (v1.3.1, user-resizable).
    pub panel_w: Option<f64>,
    /// カード変種（v1.4 カード統一モデル §1）: file|url|project|folder|filter（拡張予定: note 等）
    pub kind: Option<String>,
    /// kind='filter' のフィルタ定義 JSON（v1.4 オートフロー⑤）
    pub filter_ref: Option<String>,
    /// v1.3.10 窓モデル: 内部仮身が指す実身カードの id（外部=origin_path とは別レーン）。
    pub window_of: Option<String>,
    /// v1.5.3 WS-P1（オペ層 §6.1）: 構造化実行記録 {at, exit, ok, nodata, skipped, message}
    /// の JSON。WS-P2 のウォッチャ・運用前評価・メタ認知カードが読む口（本号は保存と返却まで）。
    pub last_run: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct ViewTransform {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}
#[derive(Serialize)]
pub struct NoteInfo {
    pub id: String,
    pub text: String,
    pub pos_x: f64,
    pub pos_y: f64,
    pub width: f64,
    pub font_size: f64,
    pub bold: bool,
    pub file_ref: Option<String>,
    // v1.4.16 付箋の充実: 色（NULL=既定の黄）と見出し行フォントサイズ（NULL=見出しなし）。
    pub color: Option<String>,
    pub title_size: Option<f64>,
}
#[derive(Serialize)]
pub struct RelationInfo {
    pub id: i64,
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    /// v1.5.3 WS-P1: 矢印の制御属性 JSON（{when, carry, port}・NULL=従来）。
    pub attrs: Option<String>,
}
// ---------------------------------------------------------------------------
// モジュール分割（v1.4.26 WS1: docs/実行環境整備とリファクタリング計画.md）
// 各モジュールは旧 lib.rs のセクションをそのまま移動したもの。glob 再エクスポートで
// 旧単一ファイル時代の相互参照を維持する（名前は全モジュールで一意）。
// ---------------------------------------------------------------------------
mod ai;
mod clipboard_in;
mod filters;
mod flow_run;
mod launcher;
mod modules;
mod notes;
mod objects;
mod ops;
mod os_integration;
mod preflight;
mod proc;
mod project;
mod runtime;
mod shapes;
mod util;
pub(crate) use ai::*;
pub(crate) use clipboard_in::*;
pub(crate) use filters::*;
pub(crate) use flow_run::*;
pub(crate) use launcher::*;
pub(crate) use modules::*;
pub(crate) use notes::*;
pub(crate) use objects::*;
pub(crate) use ops::*;
pub(crate) use os_integration::*;
pub(crate) use preflight::*;
pub(crate) use proc::*;
pub(crate) use project::*;
pub(crate) use runtime::*;
pub(crate) use shapes::*;
pub(crate) use util::*;
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .expect("failed to get app data dir");
            std::fs::create_dir_all(&app_dir)?;
            // Discard last session's temporary view copies.
            let _ = std::fs::remove_dir_all(temp_view_dir());
            // Re-open the last project if it still exists.
            let config = read_config(&app_dir);
            let current = config
                .last
                .as_ref()
                .filter(|p| texos_dir(Path::new(p)).join("texos.db").exists())
                .and_then(|p| open_project_at(Path::new(p)).ok());
            app.manage(AppState {
                current: Mutex::new(current),
                app_data: app_dir,
                jobs: Jobs::new(),
                resources: ResourceLimits::new(),
                write_locks: Mutex::new(std::collections::HashMap::new()),
                t2i_server: std::sync::Mutex::new(None),
            });
            // v1.5.4 WS-P2: オペ層スケジューラ（タイマ/ウォッチャの発火判定・tick 1秒・
            // 現在開いているプロジェクトの op カードだけが対象）。
            start_op_scheduler(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            new_project,
            open_project,
            get_current_project,
            list_recent_projects,
            create_subproject,
            link_project,
            open_project_object,
            open_origin_project,
            create_url,
            update_url,
            set_memo,
            import_file,
            list_objects,
            move_object,
            move_objects,
            set_z_order,
            set_expanded,
            set_view_transform,
            get_view_transform,
            get_thumbnail,
            get_preview,
            get_media_data_url,
            create_relation,
            list_relations,
            delete_relation,
            set_relation_attrs,
            list_flow_inputs,
            set_input_order,
            delete_object,
            restore_object,
            restore_note,
            restore_shape,
            restore_relation,
            set_undo_stack,
            get_undo_stack,
            rename_object,
            rename_real,
            duplicate_virtual,
            send_selection_to_project,
            clean_caches,
            gc_cas_scan,
            gc_cas_clean,
            list_deleted,
            purge_deleted,
            commit_object,
            commit_all,
            relink_object,
            relink_folder,
            uncommit_object,
            make_real,
            open_with_default,
            open_with_chooser,
            open_url_with_chooser,
            reveal_in_dir,
            view_temp,
            materialize_view,
            convert_office_to_pdf,
            create_note,
            list_notes,
            update_note,
            move_note,
            delete_note,
            set_note_style,
            set_note_color,
            set_note_title,
            create_note_linked,
            commit_note_to_file,
            read_text_file,
            read_view_text,
            write_text_file,
            paste_clipboard,
            copy_objects_to_clipboard,
            copy_image_to_clipboard,
            create_text_object,
            create_launcher,
            run_launcher,
            import_new_project_files,
            create_filter,
            update_filter,
            run_flow,
            fire_op,
            toggle_op_paused,
            evaluate_flow,
            break_flow,
            list_jobs,
            cancel_all,
            ai_chat_send,
            ai_chat_history,
            ai_context_labels,
            ai_chat_cancel,
            ai_chat_clear,
            ai_list_models,
            ai_ollama_status,
            ai_install_ollama,
            ai_start_ollama,
            ai_pull_model,
            whisper_status,
            whisper_install_ffmpeg,
            whisper_install,
            whisper_download_model,
            whisper_set_model,
            t2i_status,
            t2i_start_server,
            t2i_stop_server,
            t2i_download_model,
            t2i_set_model,
            t2i_set_hf_token,
            t2i_reveal_model_dir,
            t2i_delete_model,
            python_build_env,
            python_set_env_mode,
            detect_spec,
            flow_send_stdin,
            list_audio_devices,
            check_filter_program,
            list_filter_programs,
            import_filter_program,
            remove_filter_program,
            restore_filter_program,
            list_filter_library,
            save_filter_library,
            delete_filter_library,
            list_launcher_library,
            save_launcher_library,
            delete_launcher_library,
            list_module_library,
            save_module_library,
            place_module_from_library,
            delete_module_library,
            export_libraries_to_project,
            list_library_backup,
            import_library_backup_entry,
            list_installed_apps,
            create_drop_point,
            drop_into_point,
            set_panel_width,
            create_shape,
            list_shapes,
            update_shape_geom,
            set_shape_style,
            set_shape_locked,
            add_shape_member,
            modularize_members,
            dissolve_op,
            set_shape_label,
            set_shape_z,
            delete_shape,
            os_icon::get_os_icon,
            install_drop_target
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
#[cfg(test)]
mod text_decode_tests {
    use super::decode_text_bytes;
    #[test]
    fn plain_utf8_is_passthrough() {
        let (text, enc) = decode_text_bytes("あいうABC".as_bytes());
        assert_eq!(text, "あいうABC");
        assert_eq!(enc, "UTF-8");
    }
    #[test]
    fn shift_jis_is_detected_and_decoded() {
        // Shift_JIS でエンコードした日本語（read_to_string なら壊れていた入力）。
        let (bytes, _, had_err) = encoding_rs::SHIFT_JIS.encode("日本語のテキスト");
        assert!(!had_err);
        let (text, enc) = decode_text_bytes(&bytes);
        assert_eq!(text, "日本語のテキスト");
        assert_eq!(enc, "Shift_JIS");
    }
    #[test]
    fn utf16le_bom_is_honored() {
        // encoding_rs は UTF-16 への encode を持たないので UTF-16LE バイトを手で組む。
        let s = "BOM付きUTF-16";
        let mut with_bom = vec![0xFF, 0xFE];
        for u in s.encode_utf16() {
            with_bom.extend_from_slice(&u.to_le_bytes());
        }
        let (text, enc) = decode_text_bytes(&with_bom);
        assert_eq!(text, "BOM付きUTF-16");
        assert_eq!(enc, "UTF-16LE");
    }
}
#[cfg(test)]
mod ws_c_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 同一終端カードの Flow は拒否・別カードは並行登録できる（WS-C 並行解禁の要）。
    #[test]
    fn register_flow_rejects_same_card_only() {
        let jobs = Jobs::new();
        let a = jobs.register_flow("card-A", "フローA", "C:\\prj").unwrap();
        assert!(jobs.register_flow("card-A", "フローA", "C:\\prj").is_err());
        let b = jobs.register_flow("card-B", "フローB", "C:\\prj").unwrap();
        // 完了（台帳から除去）すれば同じカードを再実行できる。
        jobs.remove(a.id);
        assert!(jobs.register_flow("card-A", "フローA", "C:\\prj").is_ok());
        jobs.remove(b.id);
    }

    /// セットアップは同一ラベルだけ二重起動拒否（別コンポーネントの並行導入は可）。
    #[test]
    fn register_setup_rejects_same_label() {
        let jobs = Jobs::new();
        let _a = jobs.register_setup("Whisper 導入").unwrap();
        assert!(jobs.register_setup("Whisper 導入").is_err());
        assert!(jobs.register_setup("Ollama 導入").is_ok());
    }

    /// any_bound_to は project_root 一致だけを見る（セットアップ系＝空は束縛なし扱い）。
    #[test]
    fn any_bound_to_matches_project_root() {
        let jobs = Jobs::new();
        let _f = jobs.register_flow("c1", "f", "C:\\prjA").unwrap();
        let _s = jobs.register_setup("DL").unwrap();
        assert!(jobs.any_bound_to("C:\\prjA"));
        assert!(!jobs.any_bound_to("C:\\prjB"));
        assert!(!jobs.any_bound_to(""));
    }

    /// 排他資源（permits=1）: try_acquire は2つ目を拒否し、Drop で解放される。
    #[test]
    fn fair_semaphore_exclusive_try_acquire() {
        let sem = FairSemaphore::new(1);
        let p1 = sem.try_acquire().expect("1つ目は取れる");
        assert!(sem.try_acquire().is_none(), "2つ目は拒否");
        drop(p1);
        assert!(sem.try_acquire().is_some(), "解放後は取れる");
    }

    /// heavy（FIFO待ち）: 上限まで並行し、待っている取得は解放後に通る。
    #[test]
    fn fair_semaphore_waits_fifo() {
        let sem = FairSemaphore::new(2);
        let cancel = AtomicBool::new(false);
        let p1 = sem.acquire(&cancel, || {}).unwrap();
        let _p2 = sem.acquire(&cancel, || {}).unwrap();
        // 3つ目は待ちになる → 別スレッドで p1 を解放すると通る。
        let waited = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                let _p3 = sem
                    .acquire(&cancel, || waited.store(true, Ordering::Relaxed))
                    .unwrap();
            });
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(p1);
        });
        assert!(waited.load(Ordering::Relaxed), "3つ目は一度待ちに入った");
    }

    /// 待機中の中断: cancel が立つと acquire はエラーで抜ける（Stop-all が待ち行列にも効く）。
    #[test]
    fn fair_semaphore_wait_cancellable() {
        let sem = FairSemaphore::new(1);
        let cancel = AtomicBool::new(false);
        let _p1 = sem.acquire(&cancel, || {}).unwrap();
        std::thread::scope(|s| {
            let h = s.spawn(|| sem.acquire(&cancel, || {}));
            std::thread::sleep(std::time::Duration::from_millis(100));
            cancel.store(true, Ordering::Relaxed);
            let r = h.join().unwrap();
            assert!(r.is_err(), "中断でエラー復帰");
        });
        // 中断した待ちがキューに残らない（次の try_acquire を邪魔しない）。
        cancel.store(false, Ordering::Relaxed);
        drop(_p1);
        assert!(sem.try_acquire().is_some());
    }

    /// エンドポイント正規化（凍結 §11.3）: trim・ホスト小文字・末尾 '/' 除去。パスは大小保持。
    #[test]
    fn endpoint_normalization() {
        assert_eq!(
            normalize_endpoint(" HTTP://LocalHost:11434/ "),
            "http://localhost:11434"
        );
        assert_eq!(
            normalize_endpoint("http://Host:1234/V1"),
            "http://host:1234/V1"
        );
        assert_eq!(
            normalize_endpoint("http://localhost:11434"),
            normalize_endpoint("http://localhost:11434/")
        );
    }
}

#[cfg(test)]
mod gc_tests {
    use super::*;
    /// scan_orphan_cas は「どのカード（deleted_at 問わず）からも参照されない」ブロブだけを
    /// 孤立として返し、生きたカード・ソフト削除カードが指す実身は残すこと。
    #[test]
    fn orphan_cas_reachability() {
        let dir = std::env::temp_dir().join(format!("texos_gc_test_{}", db::now_ms()));
        let cas_root = dir.join(".texos").join("cas");
        std::fs::create_dir_all(&cas_root).unwrap();
        let conn = db::open(&dir.join(".texos").join("texos.db")).unwrap();
        let p = Project {
            conn,
            root: dir.clone(),
            cas_root: cas_root.clone(),
        };
        // 生きたカードが指す実身、ソフト削除カードが指す実身、どのカードも指さない孤立実身。
        let live = cas::store_bytes(&cas_root, b"live-content").unwrap();
        let trashed = cas::store_bytes(&cas_root, b"trashed-content").unwrap();
        let orphan = cas::store_bytes(&cas_root, b"orphan-content").unwrap();
        for h in [&live, &trashed, &orphan] {
            db::insert_real_body(&p.conn, h, "text/plain", 12, Some("txt")).unwrap();
        }
        db::insert_virtual_body(&p.conn, "v-live", &live, "live", 0.0, 0.0, None, None).unwrap();
        db::insert_virtual_body(&p.conn, "v-trash", &trashed, "trash", 0.0, 0.0, None, None)
            .unwrap();
        // ソフト削除（ゴミ箱＝復活の呪文で戻せる）カードも real_id は生存扱い。
        p.conn
            .execute(
                "UPDATE virtual_bodies SET deleted_at = ?1 WHERE id = 'v-trash'",
                rusqlite::params![db::now_ms()],
            )
            .unwrap();
        let (orphans, total) = scan_orphan_cas(&p).unwrap();
        assert_eq!(total, 3, "CAS には3ブロブある");
        let orphan_hashes: Vec<String> = orphans
            .iter()
            .map(|(path, _)| {
                let file = path.file_name().unwrap().to_str().unwrap();
                let prefix = path.parent().unwrap().file_name().unwrap().to_str().unwrap();
                format!("{}{}", prefix, file)
            })
            .collect();
        assert_eq!(orphan_hashes, vec![orphan.clone()], "孤立は orphan のみ");
        assert!(!orphan_hashes.contains(&live));
        assert!(!orphan_hashes.contains(&trashed));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
