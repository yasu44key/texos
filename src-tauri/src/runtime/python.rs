#![allow(unused_imports)]
use crate::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager, State};

// --- v1.4.27 WS2: Python 実行環境の TexoS 管理化（uv + venv）--------------------------
// docs/実行環境整備とリファクタリング計画.md WS2。
// TexoS が uv（Astral製・単一exe）を GitHub Releases から取得し、uv に CPython の取得と
// venv 構築・パッケージ導入をさせる。システム Python 非依存＝「アプリが面倒を見る」思想
// （Ollama/Whisper 自動導入と同じ流儀）。uv のグローバルキャッシュはハードリンク共有なので
// env が増えてもディスクは肥大しない。
//
// ディレクトリ構成（app_data = %APPDATA%\com.yoshi.texos\ 配下・既存 whisper/t2i と同居）:
//   runtime/bin/uv.exe   … TexoS がDL（バージョン固定）
//   runtime/python/      … uv 管理の CPython（UV_PYTHON_INSTALL_DIR）
//   runtime/cache/       … UV_CACHE_DIR（ハードリンク共有の要）
//   runtime/envs/<用途>/ … venv（現状 t2i のみ。将来増えてもキャッシュ共有で増分は小さい）
//
// 可搬性（WS3 方針の先取り）: OS 分岐は本ファイル（runtime/ 内）に閉じ込め、外部コマンド名は
// 定数化する。パスはすべて app_data 起点。

/// uv のバージョン（GitHub Releases のタグにピン留め。上げるときはここだけ）。
pub(crate) const UV_VERSION: &str = "0.8.4";

/// uv リリースアセット名（OS 依存はここに閉じ込める）。
#[cfg(windows)]
pub(crate) const UV_ASSET: &str = "uv-x86_64-pc-windows-msvc.zip";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) const UV_ASSET: &str = "uv-aarch64-apple-darwin.tar.gz";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) const UV_ASSET: &str = "uv-x86_64-unknown-linux-gnu.tar.gz";

#[cfg(windows)]
pub(crate) const UV_EXE_NAME: &str = "uv.exe";
#[cfg(not(windows))]
pub(crate) const UV_EXE_NAME: &str = "uv";

/// GPU 判定に使う外部コマンド（WS3 で runtime/spec.rs に発展予定）。
pub(crate) const NVIDIA_SMI_CMD: &str = "nvidia-smi";

/// 管理 env に入れる CPython の版（uv が python-build-standalone から取得）。
pub(crate) const MANAGED_PYTHON_VERSION: &str = "3.12";

/// torch の取得先 index（WS3 のスペック判定で選択の精度を上げる。WS2 は nvidia-smi の有無のみ）。
pub(crate) const TORCH_INDEX_CU124: &str = "https://download.pytorch.org/whl/cu124";
pub(crate) const TORCH_INDEX_CPU: &str = "https://download.pytorch.org/whl/cpu";

/// 用途 → 導入パッケージのテーブル（環境の要件定義はコードに埋める）。
/// torch は index-url 切替のため別枠で入れるのでここには含めない。
/// t2i_server.py 側の import と一致させること（検証は check_deps() が正）。
pub(crate) const ENV_REQUIREMENTS: &[(&str, &[&str])] = &[(
    "t2i",
    &[
        "diffusers",
        "transformers",
        "accelerate",
        "safetensors",
        "pillow",
        "huggingface_hub",
        // LoRA 適用に必要（無くても生成自体は動くので check_deps() には含めない＝
        // peft 無しの既存 env を「壊れている」と誤判定させない）。
        "peft",
    ],
)];

pub(crate) fn runtime_root(app_data: &Path) -> PathBuf {
    app_data.join("runtime")
}
pub(crate) fn uv_exe(app_data: &Path) -> PathBuf {
    runtime_root(app_data).join("bin").join(UV_EXE_NAME)
}
pub(crate) fn uv_python_dir(app_data: &Path) -> PathBuf {
    runtime_root(app_data).join("python")
}
pub(crate) fn uv_cache_dir(app_data: &Path) -> PathBuf {
    runtime_root(app_data).join("cache")
}
pub(crate) fn env_dir(app_data: &Path, name: &str) -> PathBuf {
    runtime_root(app_data).join("envs").join(name)
}
/// 管理 env の python 実行ファイル（venv レイアウトの OS 差はここに閉じ込める）。
pub(crate) fn env_python(app_data: &Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        env_dir(app_data, name).join("Scripts").join("python.exe")
    }
    #[cfg(not(windows))]
    {
        env_dir(app_data, name).join("bin").join("python")
    }
}

/// Python 実行環境の検出結果。
pub(crate) struct PyEnv {
    /// python 実行ファイルの絶対パス（見つからなければ None）
    pub path: Option<String>,
    /// torch/diffusers 等の依存が揃っているか
    pub deps_ok: bool,
    /// torch.cuda.is_available()（deps_ok のときのみ意味を持つ）
    pub cuda: bool,
    /// 解決元: "managed"（TexoS 構築 env）| "system"（従来のシステム Python）| "none"
    pub kind: &'static str,
}

/// python 実行ファイルの依存チェック。Ok(cuda) = 依存あり、Err = 依存なし/実行不能。
/// チェック内容は t2i_server.py の import と一致させる（旧 detect_python の判定が正）。
fn check_deps(py: &str) -> Result<bool, ()> {
    let mut d = std::process::Command::new(py);
    d.args([
        "-c",
        "import torch,diffusers,transformers,safetensors,PIL,huggingface_hub; \
         import sys; sys.stdout.write('1' if torch.cuda.is_available() else '0')",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        d.creation_flags(0x0800_0000);
    }
    match d.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim() == "1"),
        _ => Err(()),
    }
}

/// PATH のシステム Python（python / py / python3）を探す。戻り値は sys.executable。
fn find_system_python() -> Option<String> {
    for cand in ["python", "py", "python3"] {
        let mut c = std::process::Command::new(cand);
        c.args(["-c", "import sys; sys.stdout.write(sys.executable)"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        if let Ok(o) = c.output() {
            if o.status.success() {
                let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// torch/diffusers が使える Python を探す（WS2 で解決順を変更）。
/// 解決順（config.python_env で明示切替も可・既定は自動）:
///   ① runtime/envs/t2i の管理 env（依存が揃っていれば採用）
///   ② 従来のシステム Python（依存が揃っている場合のみ採用＝既存構築済みPCを壊さない）
///   ③ どちらも無ければ「環境を構築する」ボタンを UI に出す（kind="none" または deps_ok=false）
pub(crate) fn detect_python(app_data: &Path) -> PyEnv {
    let mode = read_config(app_data).python_env.unwrap_or_default(); // ""=自動
    let managed = env_python(app_data, "t2i");
    let managed_path = managed.is_file().then(|| managed.to_string_lossy().to_string());

    // 明示 "managed": 管理 env のみを見る（無ければ構築ボタンへ）。
    if mode == "managed" {
        return match &managed_path {
            Some(p) => match check_deps(p) {
                Ok(cuda) => PyEnv { path: managed_path, deps_ok: true, cuda, kind: "managed" },
                Err(()) => PyEnv { path: managed_path, deps_ok: false, cuda: false, kind: "managed" },
            },
            None => PyEnv { path: None, deps_ok: false, cuda: false, kind: "none" },
        };
    }
    // 明示 "system" 以外（自動含む）: まず管理 env。
    if mode != "system" {
        if let Some(p) = &managed_path {
            if let Ok(cuda) = check_deps(p) {
                return PyEnv { path: managed_path, deps_ok: true, cuda, kind: "managed" };
            }
            // 管理 env はあるが壊れている → 自動ではシステム Python にフォールバック。
        }
    }
    // システム Python（従来動作）。
    if let Some(p) = find_system_python() {
        return match check_deps(&p) {
            Ok(cuda) => PyEnv { path: Some(p), deps_ok: true, cuda, kind: "system" },
            Err(()) => PyEnv { path: Some(p), deps_ok: false, cuda: false, kind: "system" },
        };
    }
    // システムも無い: 壊れた管理 env があればそれを報告（UI が再構築を出せるように）。
    match managed_path {
        Some(_) => PyEnv { path: managed_path, deps_ok: false, cuda: false, kind: "managed" },
        None => PyEnv { path: None, deps_ok: false, cuda: false, kind: "none" },
    }
}

// WS2 の暫定 GPU 判定（nvidia_gpu_available）は WS3 で runtime/spec.rs の
// gather_spec()（VRAM 量まで見て torch index を決める）へ発展・置換した。

/// "python-env" 進捗イベント（{env, status, percent}）。whisper-install の流儀を踏襲。
fn emit_env_progress(app: &tauri::AppHandle, env: &str, status: &str, percent: u32) {
    let _ = app.emit(
        "python-env",
        serde_json::json!({ "env": env, "status": status, "percent": percent }),
    );
}

/// uv.exe が無ければ GitHub Releases から取得して runtime/bin に置く。
/// 進捗は "python-env" イベントの 0〜10% に割り当てる。
pub(crate) fn ensure_uv(app: &tauri::AppHandle, app_data: &Path, env: &str) -> Result<(), String> {
    let exe = uv_exe(app_data);
    if exe.is_file() {
        return Ok(());
    }
    let bin_dir = runtime_root(app_data).join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let url = format!(
        "https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/{UV_ASSET}"
    );
    emit_env_progress(app, env, "uv（環境構築ツール）をダウンロード中…", 0);
    use std::io::Read;
    let client = reqwest::blocking::Client::builder()
        .build()
        .map_err(|e| format!("HTTPクライアント生成に失敗: {e}"))?;
    let mut resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("uv のダウンロードを開始できません（オフライン？）: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("uv のダウンロードに失敗 ({})", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];
    let mut downloaded = 0u64;
    loop {
        let n = resp
            .read(&mut chunk)
            .map_err(|e| format!("uv の読み取りに失敗: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        downloaded += n as u64;
        let pct = if total > 0 {
            (downloaded as f64 / total as f64 * 10.0) as u32
        } else {
            0
        };
        emit_env_progress(app, env, "uv（環境構築ツール）をダウンロード中…", pct);
    }
    // zip 展開: basename が uv.exe / uvx.exe のものを bin 直下へ（フォルダ階層は剥がす）。
    #[cfg(windows)]
    {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(buf))
            .map_err(|e| format!("uv の zip を開けません: {e}"))?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
            if f.is_dir() {
                continue;
            }
            let full = f.name().to_string();
            let base = full.rsplit(['/', '\\']).next().unwrap_or(&full);
            if !base.ends_with(".exe") {
                continue;
            }
            let out = bin_dir.join(base);
            let mut o = std::fs::File::create(&out)
                .map_err(|e| format!("uv の書き出しに失敗 ({base}): {e}"))?;
            std::io::copy(&mut f, &mut o).map_err(|e| e.to_string())?;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = buf; // tar.gz 展開は mac/Linux 対応時に実装（WS3 可搬性方針）
        return Err("この OS での uv 自動導入は未対応です".into());
    }
    if !exe.is_file() {
        return Err("uv を展開しましたが uv.exe が見つかりません".into());
    }
    Ok(())
}

/// uv をサブコマンド実行（キャッシュ/Python置き場は runtime/ 配下に固定）。
/// 出力（stderr）を進捗イベントとして流し、失敗時は末尾を含むエラーを返す。
fn run_uv(
    app: &tauri::AppHandle,
    app_data: &Path,
    env: &str,
    args: &[&str],
    step_status: &str,
    percent: u32,
) -> Result<(), String> {
    emit_env_progress(app, env, step_status, percent);
    let mut c = std::process::Command::new(uv_exe(app_data));
    c.args(args)
        .env("UV_CACHE_DIR", uv_cache_dir(app_data))
        .env("UV_PYTHON_INSTALL_DIR", uv_python_dir(app_data))
        // システム Python を絶対に拾わせない（管理 env の独立性の要）。
        .env("UV_PYTHON_PREFERENCE", "only-managed")
        // torch は 2.4GiB 超。uv 既定の 30 秒では回線次第でタイムアウトする（実機で確認済み）。
        .env("UV_HTTP_TIMEOUT", "600")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let mut child = c
        .spawn()
        .map_err(|e| format!("uv の起動に失敗: {e}"))?;
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    // 末尾ログ（エラー表示用）を取りつつ、行単位で進捗イベントに流す。
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    std::thread::scope(|s| {
        // stdout はパイプ詰まり防止に読み捨て（uv のログは stderr 側）。
        if let Some(out) = stdout {
            s.spawn(move || {
                use std::io::Read;
                let mut sink = Vec::new();
                let _ = std::io::BufReader::new(out).read_to_end(&mut sink);
            });
        }
        if let Some(err) = stderr {
            use std::io::BufRead;
            for line in std::io::BufReader::new(err).lines() {
                let Ok(line) = line else { break };
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if tail.len() >= 15 {
                    tail.pop_front();
                }
                tail.push_back(line.clone());
                // uv の出力行（Resolved / Downloading / Installed 等）をそのまま見せる。
                emit_env_progress(app, env, &format!("{step_status} {line}"), percent);
            }
        }
    });
    let status = child
        .wait()
        .map_err(|e| format!("uv の終了待ちに失敗: {e}"))?;
    if !status.success() {
        let log: Vec<String> = tail.into_iter().collect();
        return Err(format!(
            "uv {} に失敗 (exit {:?})\n{}",
            args.first().unwrap_or(&""),
            status.code(),
            log.join("\n")
        ));
    }
    Ok(())
}

/// 管理 env を構築する: uv 取得 → CPython 取得 → venv 作成 → torch（index 切替）→ 残りの
/// パッケージ → import 検証。進捗は "python-env"（{env, status, percent}）で配信。
/// torch_index は呼び出し側が決める（WS2 は nvidia-smi の有無、WS3 で spec 判定に置換）。
pub(crate) fn ensure_env(
    app: &tauri::AppHandle,
    app_data: &Path,
    name: &str,
    torch_index: &str,
) -> Result<(), String> {
    let reqs = ENV_REQUIREMENTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, r)| *r)
        .ok_or_else(|| format!("未知の環境名: {name}"))?;
    ensure_uv(app, app_data, name)?;
    run_uv(
        app,
        app_data,
        name,
        &["python", "install", MANAGED_PYTHON_VERSION],
        &format!("Python {MANAGED_PYTHON_VERSION} を取得中…"),
        15,
    )?;
    let edir = env_dir(app_data, name);
    std::fs::create_dir_all(edir.parent().unwrap()).map_err(|e| e.to_string())?;
    let edir_s = edir.to_string_lossy().to_string();
    run_uv(
        app,
        app_data,
        name,
        &["venv", &edir_s, "--python", MANAGED_PYTHON_VERSION],
        "仮想環境を作成中…",
        25,
    )?;
    let py = env_python(app_data, name).to_string_lossy().to_string();
    run_uv(
        app,
        app_data,
        name,
        &["pip", "install", "--python", &py, "--index-url", torch_index, "torch"],
        "torch をインストール中…（数GB・時間がかかります）",
        35,
    )?;
    let mut args: Vec<&str> = vec!["pip", "install", "--python", &py];
    args.extend_from_slice(reqs);
    run_uv(app, app_data, name, &args, "生成ライブラリをインストール中…", 80)?;
    emit_env_progress(app, name, "環境を検証中…", 95);
    match check_deps(&py) {
        Ok(cuda) => {
            emit_env_progress(
                app,
                name,
                if cuda { "完了（GPU 有効）" } else { "完了（CPU 動作）" },
                100,
            );
            Ok(())
        }
        Err(()) => Err(
            "環境を構築しましたが import 検証に失敗しました（runtime/envs を削除して再試行してください）"
                .into(),
        ),
    }
}

/// 管理 env をワンクリック構築する（t2i 設定 UI の「環境を構築する」ボタン）。
/// torch の index（cu124 / cpu）は WS3 の spec 判定（VRAM 量）に委ねる。
/// force=Some(true) は既存 env を削除してから作り直す（GPU 入替後などに CPU 版 torch→GPU 版
/// torch へ確実に切り替えるため。同一バージョンだと通常の再導入はスキップされ得るため）。
#[tauri::command(async)]
pub(crate) fn python_build_env(
    window: tauri::WebviewWindow,
    state: State<AppState>,
    env: String,
    force: Option<bool>,
) -> Result<(), String> {
    let app_data = state.app_data.clone();
    let app = window.app_handle().clone();
    let name = if env.trim().is_empty() { "t2i".to_string() } else { env.trim().to_string() };
    // 作り直し: 既存 env を丸ごと削除（uv キャッシュはハードリンク共有なので torch 以外は即再構築）。
    if force.unwrap_or(false) {
        let dir = env_dir(&app_data, &name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("既存環境の削除に失敗しました（{}）: {e}", dir.display()))?;
        }
    }
    // spec.rs の推奨に従って torch の取得先を選ぶ（GPU なし／VRAM 少は cpu ＝確実に動く）。
    let torch_index = gather_spec().torch_index;
    // v1.5.2 WS-C: 台帳ジョブ化（パネルに出る・二重起動拒否）。uv はプロセス wait 型なので
    // 中断は非対応（ベストエフォート方針）。
    run_setup_job(
        state.inner(),
        &app,
        &format!("Python環境構築（{name}）"),
        |_job| {
            std::thread::scope(|s| {
                s.spawn(|| ensure_env(&app, &app_data, &name, &torch_index))
                    .join()
                    .map_err(|_| "環境構築スレッドが異常終了しました".to_string())?
            })
        },
    )
}

/// Python 実行環境の明示切替（"managed" / "system" / "auto"）。auto は設定を消す。
#[tauri::command]
pub(crate) fn python_set_env_mode(state: State<AppState>, mode: String) -> Result<(), String> {
    let m = mode.trim().to_string();
    if !["auto", "managed", "system"].contains(&m.as_str()) {
        return Err(format!("不正なモード: {m}"));
    }
    let mut cfg = read_config(&state.app_data);
    cfg.python_env = if m == "auto" { None } else { Some(m) };
    write_config(&state.app_data, &cfg);
    Ok(())
}
