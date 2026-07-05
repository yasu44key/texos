#![allow(unused_imports)]
use crate::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::State;

// --- v1.4.28 WS3: PCスペック検出と推奨構成 ---------------------------------------------
// docs/実行環境整備とリファクタリング計画.md WS3。
// 初回構築時にスペックを検出し「確実に動く構成」を既定で選ぶ（上位構成は選べるが既定にしない）。
// 検出結果は t2i 設定画面・AI 設定画面に表示し、ユーザーが推奨の根拠を見られるようにする。
//
// 可搬性（WS3 方針）: 外部コマンド名は python.rs の定数（NVIDIA_SMI_CMD）を再利用して一点管理。
// OS 分岐（RAM 取得）は本ファイル（runtime/ 内）に #[cfg] で閉じ込め、外へ漏らさない。
// パスは扱わない（app_data 起点のみ）。torch index の実 URL も python.rs の定数を参照する。

/// 検出した NVIDIA GPU（AMD/Intel は当面 None＝CPU 扱い＝「動く」を優先）。
#[derive(Serialize, Clone)]
pub(crate) struct GpuInfo {
    /// nvidia-smi が報告する GPU 名
    pub name: String,
    /// VRAM 総量（MB）
    pub vram_mb: u64,
}

/// PC スペックと、それに基づく t2i 推奨構成。UI（t2i/AI 設定）へ返す。
#[derive(Serialize, Clone)]
pub(crate) struct SpecInfo {
    /// NVIDIA GPU（見つからなければ None＝CPU 扱い）
    pub gpu: Option<GpuInfo>,
    /// システム RAM 総量（MB。取得不能なら 0）
    pub ram_mb: u64,
    /// 判定区分ラベル（UI 表示用の一言）
    pub tier: String,
    /// torch の取得先 index（実 URL: cu124 / cpu）。ensure_env にそのまま渡せる。
    pub torch_index: String,
    /// GPU 加速で torch を入れる想定か（cu124 なら true）。UI 表示用。
    pub cuda: bool,
    /// 既定に選ぶ t2i モデル名（T2I_KNOWN_MODELS の name）
    pub recommended_model: String,
    /// 生成の目安・注意（UI 表示用の平文）
    pub note: String,
    /// Ollama（AI 対話）向けの推奨モデルサイズの一言（努力目標・best-effort）
    pub ollama_hint: String,
}

/// nvidia-smi で先頭 GPU の名前と VRAM(MB) を取得。失敗＝NVIDIA GPU なし扱い。
pub(crate) fn detect_gpu() -> Option<GpuInfo> {
    let mut c = std::process::Command::new(NVIDIA_SMI_CMD);
    // nounits で "12288"（MiB 数値）が得られる。1台目だけ採用。
    c.args([
        "--query-gpu=name,memory.total",
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    // 行フォーマット: "NVIDIA GeForce RTX 3060, 12288"
    let (name, mem) = line.rsplit_once(',')?;
    let vram_mb: u64 = mem.trim().parse().ok()?;
    Some(GpuInfo {
        name: name.trim().to_string(),
        vram_mb,
    })
}

/// システム RAM 総量（MB）。OS 依存はここに閉じ込める（WS3 可搬性方針）。
#[cfg(windows)]
pub(crate) fn detect_ram_mb() -> u64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut ms = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // Safety: ms は正しい dwLength を持つ有効な MEMORYSTATUSEX。
    unsafe {
        if GlobalMemoryStatusEx(&mut ms).is_ok() {
            ms.ullTotalPhys / (1024 * 1024)
        } else {
            0
        }
    }
}
#[cfg(not(windows))]
pub(crate) fn detect_ram_mb() -> u64 {
    0 // mac/Linux は WS3 可搬性方針に従い後日実装（未取得は 0）
}

/// GPU（VRAM）から t2i 推奨構成を決める。値は「実測で調整」の初期値（WS3 推奨テーブル）。
/// - NVIDIA VRAM ≥ 12GB: cu124 / 重いモデルも可
/// - NVIDIA VRAM 6〜12GB: cu124 / SD1.5 fp16 が快適
/// - NVIDIA VRAM < 6GB もしくは GPU なし: cpu / SD1.5（低ステップ）＝確実に動く
fn recommend(gpu: &Option<GpuInfo>) -> (String, String, bool, String, String) {
    // 返り値: (tier, torch_index, cuda, recommended_model, note)
    match gpu {
        Some(g) if g.vram_mb >= 12_000 => (
            "NVIDIA GPU・VRAM 12GB以上（余裕あり）".into(),
            TORCH_INDEX_CU124.into(),
            true,
            "sd1.5".into(),
            "VRAM に余裕があります。SD1.5 は快適に動き、SDXL 級の重いモデルも狙えます。".into(),
        ),
        Some(g) if g.vram_mb >= 6_000 => (
            "NVIDIA GPU・VRAM 6〜12GB".into(),
            TORCH_INDEX_CU124.into(),
            true,
            "sd1.5".into(),
            "SD1.5（fp16・~2GB）が快適に動きます。SDXL は注意（VRAM 次第）です。".into(),
        ),
        Some(_) => (
            "NVIDIA GPU・VRAM 6GB未満".into(),
            TORCH_INDEX_CPU.into(),
            false,
            "sd1.5".into(),
            "VRAM が少なめのため、確実に動く CPU 構成を既定にします。SD1.5 の低ステップ生成を推奨します。".into(),
        ),
        None => (
            "GPU なし（CPU 動作）".into(),
            TORCH_INDEX_CPU.into(),
            false,
            "sd1.5".into(),
            "対応 GPU が見つかりません。CPU でも生成できますが、1枚に数分かかります（低ステップ推奨）。".into(),
        ),
    }
}

/// RAM/VRAM から Ollama（対話）向け推奨サイズの一言（努力目標）。
fn ollama_hint(ram_mb: u64, gpu: &Option<GpuInfo>) -> String {
    // GPU があれば VRAM、無ければ RAM を目安にざっくり。gemma3 のサイズ選択の案内。
    let budget = gpu.as_ref().map(|g| g.vram_mb).unwrap_or(ram_mb);
    if ram_mb == 0 && gpu.is_none() {
        return String::new(); // 何も取れなければ提示しない
    }
    if budget >= 16_000 {
        "gemma3:12b など大きめのモデルも動かせます。".into()
    } else if budget >= 8_000 {
        "gemma3:4b（既定）が快適。余裕があれば gemma3:12b も試せます。".into()
    } else {
        "gemma3:4b（既定・軽量）を推奨します。".into()
    }
}

/// スペックを検出して推奨構成を組み立てる（コマンド／UI 双方から使う）。
pub(crate) fn gather_spec() -> SpecInfo {
    let gpu = detect_gpu();
    let ram_mb = detect_ram_mb();
    let (tier, torch_index, cuda, recommended_model, note) = recommend(&gpu);
    let ollama_hint = ollama_hint(ram_mb, &gpu);
    SpecInfo {
        gpu,
        ram_mb,
        tier,
        torch_index,
        cuda,
        recommended_model,
        note,
        ollama_hint,
    }
}

/// PC スペックと t2i 推奨構成を返す（t2i / AI 設定画面が表示に使う）。
#[tauri::command(async)]
pub(crate) fn detect_spec() -> SpecInfo {
    gather_spec()
}
