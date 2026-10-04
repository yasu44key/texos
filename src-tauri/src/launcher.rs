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
                    .map(|c| PathBuf::from(load_path(&p.root, &c)))
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

    // 起動する GUI アプリに「今のプロジェクト直下」を伝える。作業ディレクトリを
    // プロジェクトにし、環境変数 TEXOS_PROJECT_DIR にも入れる。これで「保存」する
    // アプリの書き出しがプロジェクトへ向き、戻ったとき import_new_project_files で
    // 自動回収される（ChillChordMaker の 3 書き出しがこれで手動登録不要になる）。
    // 汎用の契約なので他のランチャーカードにも効く。cmd/powershell は自前で cd 済み。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // run_flow と同じ定石: cmd /C "<cmdline>" を raw_arg で素のまま渡す。
        // CREATE_NO_WINDOW で親 cmd のコンソールを出さない（start が本体の窓を出す）。
        // start の子は親 cmd の cwd/環境を継ぐので、cmd 側に積めば本体アプリに届く。
        std::process::Command::new("cmd")
            .raw_arg(format!("/C \"{cmdline}\""))
            .creation_flags(0x0800_0000)
            .current_dir(&root)
            .env("TEXOS_PROJECT_DIR", &root_disp)
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
            .current_dir(&root)
            .env("TEXOS_PROJECT_DIR", &root_disp)
            .spawn()
            .map_err(|e| format!("起動に失敗しました: {e}"))?;
    }
    Ok(())
}

/// シェルが PowerShell 系か（"powershell" / "pwsh"）。それ以外は cmd 扱い。
fn is_powershell(shell: &str) -> bool {
    shell.eq_ignore_ascii_case("powershell") || shell.eq_ignore_ascii_case("pwsh")
}

/// シェルカード統合（2026-07-06）: 一時スクリプト（cd → 一行目）を書き出す。
/// ウィンドウあり/なしの全モードがこの1本を通る — 引用符の入れ子問題を
/// スクリプト書き出しで避ける定石を起動経路の唯一の形にする（安定化）。
/// Windows 以外では sh スクリプトを書く（hidden モード用）。
fn write_shell_script(shell: &str, firstline: &str, root: &Path) -> Result<PathBuf, String> {
    let dir = temp_view_dir().join("shell");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let root_disp = root.display().to_string();
    #[cfg(windows)]
    let script = if is_powershell(shell) {
        let script = dir.join(format!("{}.ps1", uuid()));
        let body = format!(
            "Set-Location -LiteralPath '{}'\r\n{}\r\n",
            root_disp.replace('\'', "''"),
            firstline
        );
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        script
    } else {
        let script = dir.join(format!("{}.bat", uuid()));
        let body = format!("@echo off\r\ncd /d \"{}\"\r\n{}\r\n", root_disp, firstline);
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        script
    };
    #[cfg(not(windows))]
    let script = {
        let _ = shell;
        let script = dir.join(format!("{}.sh", uuid()));
        let body = format!("cd \"{}\"\n{}\n", root_disp, firstline);
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        script
    };
    Ok(script)
}

/// v1.3.11 対話シェル（フィルタの shell モード）: cmd/PowerShell をウィンドウ付きで
/// detached 起動し、一時スクリプト（cd → 一行目）を実行する。
/// シェルカード統合（2026-07-06）: `keep_open` で「開いたまま（cmd /K・-NoExit）」と
/// 「実行して閉じる（cmd /C・-File のみ）」を選べるようにした（既定は従来どおり開いたまま）。
pub(crate) fn launch_shell_window(
    shell: &str,
    firstline: &str,
    root: &Path,
    keep_open: bool,
) -> Result<(), String> {
    let script = write_shell_script(shell, firstline, root)?;
    let cmdline = if is_powershell(shell) {
        let noexit = if keep_open { "-NoExit " } else { "" };
        format!(
            "start \"\" powershell {noexit}-ExecutionPolicy Bypass -File \"{}\"",
            script.display()
        )
    } else {
        let switch = if keep_open { "/K" } else { "/C" };
        format!("start \"\" cmd {switch} \"{}\"", script.display())
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

/// シェルカード統合（2026-07-06）: ウィンドウなし実行（window:"hidden"）。
/// 一時スクリプトをコンソールなしで走らせ、stdout/stderr（2>&1 で時系列マージ＝
/// シェルウィンドウに見えるものと同じ並び）を {out} に受けて返す。
/// タイムアウト・中断・ツリーkill・出力上限は run_external_full の既存の安全弁をそのまま使う。
pub(crate) fn run_shell_captured(
    shell: &str,
    firstline: &str,
    root: &Path,
    timeout_secs: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<flow::ExternalOutput, String> {
    let script = write_shell_script(shell, firstline, root)?;
    #[cfg(windows)]
    let cmdline = if is_powershell(shell) {
        format!(
            "powershell -ExecutionPolicy Bypass -File \"{}\" > \"{{out}}\" 2>&1",
            script.display()
        )
    } else {
        format!("call \"{}\" > \"{{out}}\" 2>&1", script.display())
    };
    #[cfg(not(windows))]
    let cmdline = format!("sh \"{}\" > \"{{out}}\" 2>&1", script.display());
    flow::run_external_full(&cmdline, &[], "txt", timeout_secs, cancel, Some(root), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// シェルカード統合（2026-07-06）: window:"hidden" の捕捉実行。
    /// stdout と stderr が 2>&1 で1本にマージされ、終了コードが捨てられないこと。
    #[cfg(windows)]
    #[test]
    fn shell_captured_merges_streams_and_exit_code() {
        let root = std::env::temp_dir();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let o = run_shell_captured(
            "cmd",
            "echo hello-out & echo hello-err 1>&2 & exit 5",
            &root,
            Some(30),
            &cancel,
        )
        .unwrap();
        assert_eq!(o.exit_code, Some(5));
        let text = String::from_utf8_lossy(&o.output.unwrap_or_default()).to_string();
        assert!(text.contains("hello-out"), "stdout が捕捉されていない: {text}");
        assert!(text.contains("hello-err"), "stderr がマージされていない: {text}");
    }

    /// PowerShell 側も同経路（-File 実行＋cmd レベルの 2>&1 リダイレクト）で捕捉できること。
    #[cfg(windows)]
    #[test]
    fn shell_captured_powershell() {
        let root = std::env::temp_dir();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let o = run_shell_captured("powershell", "Write-Output ps-out; exit 2", &root, Some(60), &cancel)
            .unwrap();
        assert_eq!(o.exit_code, Some(2));
        let text = String::from_utf8_lossy(&o.output.unwrap_or_default()).to_string();
        assert!(text.contains("ps-out"), "PowerShell の stdout が捕捉されていない: {text}");
    }
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
                    rusqlite::params![store_path(&p.root, &path_str)],
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
            rusqlite::params![hash, store_path(&p.root, &path), id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("ドロップポイントが見つかりません".into());
    }
    Ok(())
}

