#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// オートフロー (v1.4-⑤: UNIXパイプラインの再来 — フィルタカードとフロー実行)
// ---------------------------------------------------------------------------

pub(crate) const FILTER_MIME: &str = "application/vnd.texos.filter";

/// フィルタカードを作成（kind='filter'、定義は filter_ref の JSON）。
#[tauri::command]
pub(crate) fn create_filter(
    label: String,
    spec: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    flow::parse_spec(&spec)?; // 保存前に検証（壊れたJSONをDBに入れない）
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    // sentinel real body (URLカードと同じ方式: 全フィルタカードで共有)
    let real_id = "filter:sentinel".to_string();
    p.conn
        .execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES (?1, ?2, 0, ?3)",
            rusqlite::params![real_id, FILTER_MIME, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies
                (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'filter', ?7)",
            rusqlite::params![vid, real_id, label, x, y, db::now_ms(), spec],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// プロジェクト同梱プログラムの置き場（v1.5: 可搬なフィルタはここに入れて持ち運ぶ）。
pub(crate) fn filters_dir(root: &Path) -> PathBuf {
    texos_dir(root).join("filters")
}

/// 外部フィルタの実行コマンドラインを組み立てる。
/// 新形式（name あり）: プログラムを解決して `"exe" + CMD書式`。旧形式: cmd をそのまま。
pub(crate) fn build_cmdline(root: &Path, spec: &flow::FilterSpec) -> Result<String, String> {
    if let Some(name) = spec.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let exe = flow::resolve_program(name, spec.path.as_deref(), &filters_dir(root))
            .ok_or_else(|| {
                format!(
                    "プログラム「{name}」が見つかりません（指定パス → .texos\\filters → 環境変数PATH の順で探索）"
                )
            })?;
        Ok(format!("\"{}\" {}", exe.display(), spec.cmd.as_deref().unwrap_or(""))
            .trim()
            .to_string())
    } else if let Some(cmd) = &spec.cmd {
        Ok(cmd.clone())
    } else {
        Err("フィルタ定義に builtin も cmd もありません".into())
    }
}

/// 外部プログラムの所在チェック（設定ダイアログの事前チェック）。
/// 見つかれば解決済みのフルパス、見つからなければ null を返す。
#[tauri::command]
pub(crate) fn check_filter_program(
    name: String,
    path: Option<String>,
    state: State<AppState>,
) -> Result<Option<String>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    Ok(
        flow::resolve_program(&name, path.as_deref(), &filters_dir(&p.root))
            .map(|pb| pb.to_string_lossy().into_owned()),
    )
}

/// .texos/filters に同梱されたプログラムの一覧（設定ダイアログの選択肢）。
#[tauri::command]
pub(crate) fn list_filter_programs(state: State<AppState>) -> Result<Vec<String>, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(filters_dir(&p.root)) {
        for e in rd.flatten() {
            if e.path().is_file() {
                if let Some(n) = e.file_name().to_str() {
                    // .removed は同梱解除の退避ファイル（一覧に出さない）
                    if !n.ends_with(".removed") {
                        names.push(n.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// 同梱を解除（.texos/filters から退避）。完全削除はせず「<name>.removed」へ
/// 改名して、Ctrl+Z（元に戻す）の余地を残す。.removed は一覧・解決の対象外。
#[tauri::command]
pub(crate) fn remove_filter_program(name: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dir = filters_dir(&p.root);
    let src = dir.join(&name);
    if !src.is_file() {
        return Err(format!("同梱されていません: {name}"));
    }
    let bak = dir.join(format!("{name}.removed"));
    let _ = std::fs::remove_file(&bak); // 古い退避は捨てて最新を残す
    std::fs::rename(&src, &bak).map_err(|e| e.to_string())
}

/// 同梱解除を取り消す（.removed を元の名前へ戻す）。
#[tauri::command]
pub(crate) fn restore_filter_program(name: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dir = filters_dir(&p.root);
    let bak = dir.join(format!("{name}.removed"));
    if !bak.is_file() {
        return Err("元に戻せるファイルがありません".into());
    }
    let dst = dir.join(&name);
    if dst.exists() {
        return Err(format!("既に {name} が同梱されています"));
    }
    std::fs::rename(&bak, &dst).map_err(|e| e.to_string())
}

/// プログラムをプロジェクトに同梱（.texos/filters へコピー）。戻り値はファイル名。
#[tauri::command]
pub(crate) fn import_filter_program(src: String, state: State<AppState>) -> Result<String, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let srcp = Path::new(&src);
    if !srcp.is_file() {
        return Err(format!("ファイルが見つかりません: {src}"));
    }
    let name = srcp
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("ファイル名を取得できません")?
        .to_string();
    let dir = filters_dir(&p.root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::copy(srcp, dir.join(&name)).map_err(|e| format!("コピーに失敗: {e}"))?;
    Ok(name)
}

// --- v1.5 フィルタライブラリ（アプリ共通・プロジェクト横断） ---------------------

/// ライブラリの1エントリ。spec は filter_ref と同じ JSON。
#[derive(Serialize, Deserialize)]
pub(crate) struct LibFilter {
    name: String,
    spec: String,
    created_at: i64,
}

pub(crate) fn library_path(app_data: &Path) -> PathBuf {
    app_data.join("filter_library.json")
}

pub(crate) fn read_library(app_data: &Path) -> Vec<LibFilter> {
    std::fs::read_to_string(library_path(app_data))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn write_library(app_data: &Path, list: &[LibFilter]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    std::fs::write(library_path(app_data), json).map_err(|e| e.to_string())
}

/// 登録済みフィルタの一覧（メニュー「ユーザーのフィルタ」の素材）。
#[tauri::command]
pub(crate) fn list_filter_library(state: State<AppState>) -> Result<Vec<LibFilter>, String> {
    Ok(read_library(&state.app_data))
}

/// フィルタ設定に名前を付けてライブラリへ登録。同名は上書き。
#[tauri::command]
pub(crate) fn save_filter_library(name: String, spec: String, state: State<AppState>) -> Result<(), String> {
    flow::parse_spec(&spec)?; // 壊れた定義を登録しない
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("フィルタ名を入力してください".into());
    }
    let mut list = read_library(&state.app_data);
    list.retain(|e| e.name != name);
    list.push(LibFilter {
        name,
        spec,
        created_at: db::now_ms(),
    });
    list.sort_by(|a, b| a.name.cmp(&b.name));
    write_library(&state.app_data, &list)
}

/// ライブラリからエントリを削除。
#[tauri::command]
pub(crate) fn delete_filter_library(name: String, state: State<AppState>) -> Result<(), String> {
    let mut list = read_library(&state.app_data);
    let before = list.len();
    list.retain(|e| e.name != name);
    if list.len() == before {
        return Err(format!("「{name}」はライブラリにありません"));
    }
    write_library(&state.app_data, &list)
}

// v1.3.11 ランチャーライブラリ（一度登録→リスト化）。フィルタライブラリと同方式・別ファイル。
pub(crate) fn launcher_library_path(app_data: &Path) -> PathBuf {
    app_data.join("launcher_library.json")
}
pub(crate) fn read_launcher_library(app_data: &Path) -> Vec<LibFilter> {
    std::fs::read_to_string(launcher_library_path(app_data))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub(crate) fn list_launcher_library(state: State<AppState>) -> Result<Vec<LibFilter>, String> {
    Ok(read_launcher_library(&state.app_data))
}

/// ランチャー設定に名前を付けてライブラリへ登録。同名は上書き。
#[tauri::command]
pub(crate) fn save_launcher_library(name: String, spec: String, state: State<AppState>) -> Result<(), String> {
    flow::parse_spec(&spec)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("名前を入力してください".into());
    }
    let mut list = read_launcher_library(&state.app_data);
    list.retain(|e| e.name != name);
    list.push(LibFilter {
        name,
        spec,
        created_at: db::now_ms(),
    });
    list.sort_by(|a, b| a.name.cmp(&b.name));
    let json = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    std::fs::write(launcher_library_path(&state.app_data), json).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn delete_launcher_library(name: String, state: State<AppState>) -> Result<(), String> {
    let mut list = read_launcher_library(&state.app_data);
    let before = list.len();
    list.retain(|e| e.name != name);
    if list.len() == before {
        return Err(format!("「{name}」はランチャーライブラリにありません"));
    }
    let json = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    std::fs::write(launcher_library_path(&state.app_data), json).map_err(|e| e.to_string())
}

/// v1.3.11 #A: スタートメニューのショートカット(.lnk)からインストール済みアプリを列挙。
/// %ProgramData% と %APPDATA% の Start Menu\Programs を再帰走査。.lnk はそのまま起動でき
/// アイコンも取れるので、パスを探さず一覧から選べる。
#[derive(Serialize)]
pub struct InstalledApp {
    pub name: String,
    pub path: String,
}
#[tauri::command]
pub(crate) fn list_installed_apps() -> Result<Vec<InstalledApp>, String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ProgramData", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            roots.push(
                PathBuf::from(base)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs"),
            );
        }
    }
    let mut found: Vec<InstalledApp> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 再帰走査（深さ制限つき）。スタックで回す。
    let mut stack: Vec<(PathBuf, u32)> = roots.into_iter().map(|r| (r, 0u32)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if depth > 5 {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.is_dir() {
                stack.push((path, depth + 1));
            } else if path.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("lnk")).unwrap_or(false) {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                // Uninstall/アンインストール系の .lnk は除外（実用上ノイズ）。
                if name.is_empty() || name.contains("Uninstall") || name.contains("アンインストール") {
                    continue;
                }
                let key = name.to_lowercase();
                if seen.insert(key) {
                    found.push(InstalledApp {
                        name,
                        path: path.to_string_lossy().into_owned(),
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(found)
}

/// フィルタ定義（JSON）を更新。フォーム自動生成（JSON Schema）はフェーズ2。
#[tauri::command]
pub(crate) fn update_filter(id: String, spec: String, state: State<AppState>) -> Result<(), String> {
    flow::parse_spec(&spec)?;
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let n = p
        .conn
        .execute(
            "UPDATE virtual_bodies SET filter_ref = ?1 WHERE id = ?2 AND kind = 'filter'",
            rusqlite::params![spec, id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("フィルタカードが見つかりません".into());
    }
    Ok(())
}

