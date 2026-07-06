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
    /// 中断フラグ・stdin 制御はジョブ別、多重起動判定は「台帳に Flow が居るか」。
    pub jobs: Jobs,
    /// v1.5 Text-To-Image: TexoS が起動・管理する diffusers 常駐デーモン（t2i_server.py）の
    /// 子プロセスハンドル。t2i_stop_server で明示停止できるよう保持する。
    pub t2i_server: Mutex<Option<std::process::Child>>,
}
// --- v1.5.1 WS-B ジョブレジストリ（凍結仕様 §12.B・実装指示書 WS-B）--------------------
// singleton（同時1つを物理強制する AppState フィールド群）を JobId キーの台帳へ置き換える
// ＝中央 kill スイッチ（cancel_all）の獲得・WS-C 並行解禁の土台。ジョブは揮発（凍結 §11.5・
// 永続化しない）。WS-B の間の同時実行ルールは従来同等: Flow 同時1・Chat 単一・相互並行可。

pub type JobId = u64;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Flow,
    Chat,
}

pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    /// 表示用ラベル。登録はカード読み出し前（ロック前拒否のため）なので後から確定する。
    pub label: Mutex<String>,
    pub started_at: i64,
    /// 中断フラグ（UIの中断ボタン → break_flow / cancel_all）。run_flow が DB の Mutex を
    /// 握っている間も break_flow はこれだけを触るので外部コマンドがストールしてもブレイクできる。
    pub cancel: std::sync::atomic::AtomicBool,
    /// 子プロセス stdin チャネル（v1.4.9 の rec_stdin のジョブ別版）: 実行中フィルタ
    /// （録音など record モード）の stdin 書き込み端。flow_send_stdin が 'q' 等を書く。
    pub control: flow::StdinSlot,
    /// 起動元プロジェクト。WS-B では**束縛の形だけ**（記録のみ・検査/撤廃は WS-C）。
    pub project_root: Mutex<String>,
}

/// list_jobs の返却行（WS-C 背景ジョブパネルの土台。WS-B の UI は未使用）。
#[derive(Serialize)]
pub struct JobInfo {
    pub id: JobId,
    pub kind: JobKind,
    pub label: String,
    pub started_at: i64,
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
    fn make_job(&self, kind: JobKind, label: &str) -> std::sync::Arc<Job> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::sync::Arc::new(Job {
            id,
            kind,
            label: Mutex::new(label.to_string()),
            started_at: db::now_ms(),
            cancel: std::sync::atomic::AtomicBool::new(false),
            control: Mutex::new(None),
            project_root: Mutex::new(String::new()),
        })
    }
    /// 登録（排他なし）。Chat 用 — 現状 UI はチャットを直列に使うため物理ガードは要らない。
    pub fn register(&self, kind: JobKind, label: &str) -> std::sync::Arc<Job> {
        let job = self.make_job(kind, label);
        self.map.lock().unwrap().insert(job.id, job.clone());
        job
    }
    /// Flow の登録（WS-B は従来同等＝同時1）。台帳に Flow ジョブが居れば登録せず従来文言で
    /// 即拒否する。台帳ロックは短く、state.current（DB）をロックする**前**に弾くのが要点
    /// — ここで待つと録音中フローの裏で実行が積み上がる（v1.4.10 の教訓）。
    pub fn register_flow_exclusive(&self, label: &str) -> Result<std::sync::Arc<Job>, String> {
        let mut map = self.map.lock().unwrap();
        if map.values().any(|j| j.kind == JobKind::Flow) {
            return Err(
                "ほかのフローを実行中です。完了（録音は「停止」）してからもう一度実行してください"
                    .into(),
            );
        }
        let job = self.make_job(JobKind::Flow, label);
        map.insert(job.id, job.clone());
        Ok(job)
    }
    pub fn get(&self, id: JobId) -> Option<std::sync::Arc<Job>> {
        self.map.lock().unwrap().get(&id).cloned()
    }
    pub fn remove(&self, id: JobId) {
        self.map.lock().unwrap().remove(&id);
    }
    pub fn any_flow(&self) -> bool {
        self.map.lock().unwrap().values().any(|j| j.kind == JobKind::Flow)
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
                label: j.label.lock().unwrap().clone(),
                started_at: j.started_at,
            })
            .collect();
        v.sort_by_key(|j| j.id);
        v
    }
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
/// v1.4.11: フロー実行中はプロジェクト切替を拒否する。非ブロッキング実行では run_flow が
/// 実行中に DB ロックを手放すため、途中で current が別プロジェクトに差し替わると、評価や
/// 結果の書き込みが別プロジェクトに当たって壊れる。切替系コマンドの入口でこれを弾く。
/// v1.5.1 WS-B: 判定を「台帳に Flow ジョブが存在するか」に置換（呼び出し側の挙動不変。
/// 撤廃＝ジョブの起動元プロジェクト束縛は WS-C）。
fn ensure_no_flow_running(state: &AppState) -> Result<(), String> {
    if state.jobs.any_flow() {
        return Err(
            "フロー実行中はプロジェクトを切り替えられません（完了／録音は「停止」してから）".into(),
        );
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
mod notes;
mod objects;
mod os_integration;
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
pub(crate) use notes::*;
pub(crate) use objects::*;
pub(crate) use os_integration::*;
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
                t2i_server: std::sync::Mutex::new(None),
            });
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
            list_installed_apps,
            create_drop_point,
            drop_into_point,
            set_panel_width,
            create_shape,
            list_shapes,
            update_shape_geom,
            set_shape_style,
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
