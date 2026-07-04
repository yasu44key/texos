#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// v1.3.11 ランチャーカード（一般プログラムの登録・起動＝アイデアメモ #2 の実用最小版）
// フィルタ（同期・タイムアウト・stdout捕捉）と違い、GUIアプリを detached 起動し待たない。
// 時間制限なし（暴走停止は当面 Windows タスクマネージャ任せ。将来 Stop を装備）。
// {in} 接続があればそのファイルを引数で開く。保存後の新規ファイルは import_new_project_files で回収。
// ---------------------------------------------------------------------------

pub(crate) const LAUNCHER_MIME: &str = "application/vnd.texos.launcher";

/// ランチャーカードを作成（kind='launcher'、定義は filter_ref の JSON を流用）。
/// spec 例: 一般プログラム {"name":"app.exe","path":"C:\\...\\app.exe","type":"launcher"}
///          ターミナル     {"name":"cmd","type":"launcher"} / {"name":"powershell",...}
#[tauri::command]
pub(crate) fn create_launcher(
    label: String,
    spec: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<String, String> {
    flow::parse_spec(&spec)?; // 保存前に検証
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    p.conn
        .execute(
            "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
             VALUES ('launcher:sentinel', ?1, 0, ?2)",
            rusqlite::params![LAUNCHER_MIME, db::now_ms()],
        )
        .map_err(|e| e.to_string())?;
    let vid = uuid();
    p.conn
        .execute(
            "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, filter_ref)
             VALUES (?1, 'launcher:sentinel', ?2, ?3, ?4, ?5, 'launcher', ?6)",
            rusqlite::params![vid, label, x, y, db::now_ms(), spec],
        )
        .map_err(|e| e.to_string())?;
    Ok(vid)
}

/// ランチャーを起動。接続された入力（flow 矢印）の「生きた実ファイル」を引数で開く。
/// GUI アプリは detached 起動し待たない（時間制限なし）。
#[tauri::command(async)]
pub(crate) fn run_launcher(id: String, state: State<AppState>) -> Result<(), String> {
    let (spec, root, inputs): (flow::FilterSpec, PathBuf, Vec<PathBuf>) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let fref: String = p
            .conn
            .query_row(
                "SELECT COALESCE(filter_ref, '{}') FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let spec = flow::parse_spec(&fref)?;
        // 入力: このランチャーへ入る flow 矢印（接続順）。編集が永続するよう
        // committed_path（実身の実ファイル）優先。無ければ窓の今を解決。
        let mut paths: Vec<PathBuf> = Vec::new();
        {
            let mut stmt = p
                .conn
                .prepare(
                    "SELECT v.id, v.real_id, v.committed_path
                     FROM relations rel
                     JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                     WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                     ORDER BY rel.created_at",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (vid, real_id, committed) = row.map_err(|e| e.to_string())?;
                let path = committed
                    .map(PathBuf::from)
                    .filter(|c| c.is_file())
                    .unwrap_or_else(|| resolve_view_path(p, &vid, &real_id));
                paths.push(path);
            }
        }
        (spec, p.root.clone(), paths)
    };

    let name = spec.name.as_deref().unwrap_or("").trim().to_lowercase();
    let root_disp = root.display().to_string();

    // 起動するコマンドライン（cmd /C "<...>" で実行）。`start` で新プロセスを切り離す。
    let cmdline = if name == "cmd" || name == "cmd.exe" {
        format!("start \"\" cmd /K cd /d \"{root_disp}\"")
    } else if name == "powershell" || name == "powershell.exe" || name == "pwsh" {
        format!("start \"\" powershell -NoExit -Command \"Set-Location -LiteralPath '{root_disp}'\"")
    } else {
        let exe = flow::resolve_program(
            spec.name.as_deref().unwrap_or("").trim(),
            spec.path.as_deref(),
            &filters_dir(&root),
        )
        .ok_or("登録されたプログラムが見つかりません（パスを確認してください）")?;
        let mut s = format!("start \"\" \"{}\"", exe.display());
        for inp in &inputs {
            s.push_str(&format!(" \"{}\"", inp.display()));
        }
        s
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // run_flow と同じ定石: cmd /C "<cmdline>" を raw_arg で素のまま渡す。
        // CREATE_NO_WINDOW で親 cmd のコンソールを出さない（start が本体の窓を出す）。
        std::process::Command::new("cmd")
            .raw_arg(format!("/C \"{cmdline}\""))
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| format!("起動に失敗しました: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        let _ = cmdline;
        let exe = flow::resolve_program(
            spec.name.as_deref().unwrap_or("").trim(),
            spec.path.as_deref(),
            &filters_dir(&root),
        )
        .ok_or("登録されたプログラムが見つかりません")?;
        std::process::Command::new(exe)
            .args(inputs.iter().map(|p| p.as_os_str()))
            .spawn()
            .map_err(|e| format!("起動に失敗しました: {e}"))?;
    }
    Ok(())
}

/// v1.3.11 対話シェル（フィルタの shell モード）: cmd/PowerShell を detached で開き、
/// 一時スクリプト（cd → 一行目）を実行して対話を継続（cmd /K / powershell -NoExit）。
/// 引用符の入れ子を避けるため一時 .bat/.ps1 に書き出してから起動する。
pub(crate) fn launch_interactive_shell(shell: &str, firstline: &str, root: &Path) -> Result<(), String> {
    let dir = temp_view_dir().join("shell");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let root_disp = root.display().to_string();
    let cmdline = if shell.eq_ignore_ascii_case("powershell") || shell.eq_ignore_ascii_case("pwsh") {
        let script = dir.join(format!("{}.ps1", uuid()));
        let body = format!(
            "Set-Location -LiteralPath '{}'\r\n{}\r\n",
            root_disp.replace('\'', "''"),
            firstline
        );
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        format!(
            "start \"\" powershell -NoExit -ExecutionPolicy Bypass -File \"{}\"",
            script.display()
        )
    } else {
        let script = dir.join(format!("{}.bat", uuid()));
        let body = format!("@echo off\r\ncd /d \"{}\"\r\n{}\r\n", root_disp, firstline);
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        format!("start \"\" cmd /K \"{}\"", script.display())
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .raw_arg(format!("/C \"{cmdline}\""))
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| format!("シェルの起動に失敗しました: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        let _ = cmdline;
    }
    Ok(())
}

/// v1.3.11: プロジェクトフォルダ直下の「未登録ファイル」を実身として取り込む。
/// ランチャーで起動したアプリが新規保存したファイルを TexoS に回収する用途（保存→新しい実身）。
/// .texos 配下とサブフォルダは対象外（フラット運用の原則）。新規カードを (x,y) から並べる。
#[tauri::command]
pub(crate) fn import_new_project_files(
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<Vec<String>, String> {
    // 1) 未登録ファイルを収集（ここだけロック）。import_file が内部で再ロックするため、
    //    収集後にロックを解放してから取り込む（再ロック＝デッドロック回避）。
    let candidates: Vec<String> = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let mut cands = Vec::new();
        let entries = std::fs::read_dir(&p.root).map_err(|e| e.to_string())?;
        for ent in entries.flatten() {
            let path = ent.path();
            if !path.is_file() {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            let known: bool = p
                .conn
                .query_row(
                    "SELECT 1 FROM virtual_bodies WHERE committed_path = ?1 AND deleted_at IS NULL",
                    rusqlite::params![path_str],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .is_some();
            if !known {
                cands.push(path_str);
            }
        }
        cands
    };

    // 2) ロック解放後に取り込む（import_file が in_project 判定で実身として登録）。
    let mut created = Vec::new();
    for (i, path_str) in candidates.into_iter().enumerate() {
        let cx = x + (i % 5) as f64 * 170.0;
        let cy = y + (i / 5) as f64 * 140.0;
        if let Ok(vid) = import_file(path_str, String::new(), cx, cy, state.clone()) {
            created.push(vid);
        }
    }
    Ok(created)
}

/// ドロップポイントへファイルを投入: CASスナップショット＋窓の付け替え。
/// （取り込み = import_file と同じ意味論。カードは増えず、入口の中身が入れ替わる）
#[tauri::command]
pub(crate) fn drop_into_point(id: String, path: String, state: State<AppState>) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let src = std::path::Path::new(&path);
    if !src.is_file() {
        return Err("ファイルではありません（フォルダは投入できません）".into());
    }
    let mime = mime_from_path(src);
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    let hash = cas::store_file(&p.cas_root, src).map_err(|e| e.to_string())?;
    let size = src.metadata().map_err(|e| e.to_string())?.len();
    db::insert_real_body(&p.conn, &hash, &mime, size, ext.as_deref())
        .map_err(|e| e.to_string())?;
    let n = p
        .conn
        .execute(
            "UPDATE virtual_bodies SET real_id = ?1, origin_path = ?2
             WHERE id = ?3 AND kind = 'drop' AND deleted_at IS NULL",
            rusqlite::params![hash, path, id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("ドロップポイントが見つかりません".into());
    }
    Ok(())
}

