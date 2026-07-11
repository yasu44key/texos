#!/usr/bin/env python
"""TexoS Text-To-Image デーモン（Ollama 相当の常駐生成サーバー）。

TexoS（Rust）が system Python でこのスクリプトを起動・管理し、HTTP で生成を依頼する。
モデルは VRAM に常駐させ、反復生成のロード待ち（5〜15秒）を避ける ＝ Ollama の keep_alive と同じ狙い。
依存は torch / diffusers / transformers / safetensors / Pillow / huggingface_hub のみ（標準 http.server を使い
FastAPI 等の追加依存は要求しない）。この開発 PC には一式導入済み。

モデル種別は Backend で抽象化（SD / FLUX、将来 SDXL / ControlNet 等）。MODELS レジストリの
"kind" で BACKENDS の実装にディスパッチする＝拡張点。img2img は AutoPipelineForImage2Image で
コンポーネントを共有して実現（種別非依存）。

モディファイア層（SD/SDXL 中心の拡張点）: LoRA / Textual Inversion 埋め込み / VAE 差し替え /
サンプラー選択 / Clip skip を「モデル種別（kind）とは直交する修飾」として持つ。いずれも常駐 pipe を
書き換えるので **毎生成、要求状態へリコンサイル**（差しっぱなしの次生成へのリークを防ぐ）。ロード済み
アセットは名前でキャッシュ＝繰り返しは再読み込みなし。アセット置き場は home 直下の loras/ embeddings/
vae/（models/ とは分離＝モデル一覧を汚さない）。新しい修飾は Backend にメソッド1本＋能力フラグを足すだけ。

エンドポイント:
  GET  /health            -> {"ok":true,"device":..,"cuda":bool,"current_model":str|null,"loaded":bool,...}
  GET  /models            -> {"available":[name..],"known":[name..],"current":name|null}
  GET  /assets            -> {"loras":[..],"embeddings":[..],"vae":[..]}   モディファイア用アセット一覧
  POST /load   {name}     -> {"ok":true,"model":name}               モデルをVRAMへ（無ければエラー）
  POST /pull   {name}     -> NDJSON 進捗ストリーム（{"status","percent"} / 末尾 {"done":true}）
  POST /generate {...}    -> image/png バイト列。prompt 必須。任意: negative_prompt / steps / guidance /
                            width / height / seed / model / init_image(base64=img2img) / strength /
                            lora("name:weight, .."/リスト) / vae(name) / sampler(name) / clip_skip(A1111流) /
                            embeddings(既定=embeddings/ 全自動ロード。名前をプロンプトに書けば効く)
引数: --port <n>（既定 8765） --home <dir>（モデル/HFキャッシュの置き場＝共有 app-data 配下）
gated モデル（FLUX 公式）は環境変数 HF_TOKEN（or huggingface-cli login）が要る。
"""

from __future__ import annotations

import argparse
import base64
import io
import json
import os
import sys
import threading
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# スクリプトの版（変更ごとに上げる）。TexoS はこれを /health で照合し、古いデーモンが
# 残っていれば停止して最新スクリプトで起動し直す（再ビルド後に確実に反映するため）。
# Rust 側 T2I_SERVER_VERSION と一致させること。
SERVER_VERSION = "12"

# --- モデルレジストリ（Ollama 流の名前付きモデル）+ 種別ごとの取得パターン -------------
# 各モデルは "kind"（バックエンド種別）を持つ。新しい種別（sdxl / controlnet 等）は
# BACKENDS と PATTERNS_BY_KIND に1エントリ足すだけで拡張できる（拡張点）。
# gated=True（FLUX 公式）は HF でライセンス承諾の上、環境変数 HF_TOKEN（or huggingface-cli login）が要る。
# v1.5.13 方針: 「なんでも対応」をやめ、動作実績のある定番だけを推奨（known）に載せる。
# SD1.5 / SDXL / Flux-lite が3本柱。公式 FLUX dev/schnell（24GB+・gated）は known から外し、
# 上級者が手動配置（diffusers 形式）する道だけ残す（POLICY 参照）。gated 機構自体は保持。
MODELS: dict[str, dict] = {
    "sd1.5":        {"repo": "stable-diffusion-v1-5/stable-diffusion-v1-5",  "kind": "sd"},
    "dreamshaper":  {"repo": "Lykon/dreamshaper-8",                          "kind": "sd"},
    "sdxl":         {"repo": "stabilityai/stable-diffusion-xl-base-1.0",     "kind": "sdxl"},
    "flux-lite":    {"repo": "Freepik/flux.1-lite-8B",                       "kind": "flux"},
}
DEFAULT_MODEL = "sd1.5"

# SD系: fp16 variant の safetensors 一式だけ（~2GB）。fp16 が無い配布は load/pull で素の名前へ
# フォールバック。単一ファイル版チェックポイント・.bin・fp32 重複・非EMA を丸ごと除外。
SD_PATTERNS = [
    "model_index.json", "scheduler/*", "tokenizer/*", "feature_extractor/*",
    "text_encoder/*.json", "text_encoder/*.fp16.safetensors",
    "unet/config.json", "unet/*.fp16.safetensors",
    "vae/config.json", "vae/*.fp16.safetensors",
]
SD_PATTERNS_PLAIN = [  # fp16 variant が無い配布への保険
    "model_index.json", "scheduler/*", "tokenizer/*", "feature_extractor/*",
    "*/config.json", "*/*.json",
    "text_encoder/model.safetensors",
    "unet/diffusion_pytorch_model.safetensors",
    "vae/diffusion_pytorch_model.safetensors",
]
# SDXL系: SD と同構造だが text_encoder_2（OpenCLIP）を持つ。fp16 variant 一式（~6.9GB）。
# feature_extractor/safety_checker は無いので取得しない。
SDXL_PATTERNS = [
    "model_index.json", "scheduler/*", "tokenizer/*", "tokenizer_2/*",
    "text_encoder/config.json", "text_encoder/*.fp16.safetensors",
    "text_encoder_2/config.json", "text_encoder_2/*.fp16.safetensors",
    "unet/config.json", "unet/*.fp16.safetensors",
    "vae/config.json", "vae/*.fp16.safetensors",
]
SDXL_PATTERNS_PLAIN = [  # fp16 variant が無い SDXL 配布への保険
    "model_index.json", "scheduler/*", "tokenizer/*", "tokenizer_2/*",
    "*/config.json", "*/*.json",
    "text_encoder/model.safetensors",
    "text_encoder_2/model.safetensors",
    "unet/diffusion_pytorch_model.safetensors",
    "vae/diffusion_pytorch_model.safetensors",
]
# FLUX系: diffusers サブフォルダの safetensors 一式（単一ファイル版 flux1-*.safetensors /
# ae.safetensors は除外）。transformer + T5(text_encoder_2) + CLIP + VAE で ~26〜34GB。
FLUX_PATTERNS = [
    "model_index.json", "scheduler/*", "tokenizer/*", "tokenizer_2/*",
    "transformer/*.safetensors", "transformer/*.json",
    "text_encoder/*.safetensors", "text_encoder/*.json",
    "text_encoder_2/*.safetensors", "text_encoder_2/*.json",
    "vae/*.safetensors", "vae/*.json",
]
PATTERNS_BY_KIND = {"sd": SD_PATTERNS, "sdxl": SDXL_PATTERNS, "flux": FLUX_PATTERNS}
# fp16 variant が無い配布への保険（kind ごと）。無ければ fp16 のみで完結する種別。
PLAIN_PATTERNS_BY_KIND = {"sd": SD_PATTERNS_PLAIN, "sdxl": SDXL_PATTERNS_PLAIN}

# --- 対応方針（POLICY）: 種別×形式で「使えるか／使えない場合の推奨」を持つ単一の真実。--------
# UI（手持ちモデル診断）とロード時メッセージがこれを参照する。厳密判定はしない（目安）。
# 形式は "diffusers"（フォルダ）/ "single"（単一 .safetensors/.ckpt）。
SUPPORTED_KINDS = ("sd", "sdxl", "flux")

def model_policy(kind: str, fmt: str) -> dict:
    """(kind, fmt) → {usable, reason, recommend}。使えないものは推奨に誘導する。"""
    if kind == "flux" and fmt == "single":
        return {
            "usable": False,
            "reason": "FLUX の単一ファイルは未対応（T5/CLIP/VAE を含む全部入り形式のため）",
            "recommend": "flux-lite を diffusers 形式で取得するか、SDXL をご利用ください",
        }
    if kind not in SUPPORTED_KINDS:
        return {
            "usable": False,
            "reason": "対応外または種別を判定できませんでした",
            "recommend": "SD1.5 / SDXL / Flux-lite のいずれかを diffusers 形式で配置してください",
        }
    return {"usable": True, "reason": "", "recommend": ""}


# --- サンプラー（スケジューラ）レジストリ。名前 → (diffusers クラス名, from_config kwargs) -----
# 「名前付きモデル」(MODELS) と同じ流儀で名前付きサンプラーを持つ＝拡張は1行足すだけ。
# クラスは遅延 import（起動を軽く）。既定は現行 _finalize_sd と同じ DPM++（karras なし）。
SAMPLERS: dict[str, tuple[str, dict]] = {
    "dpm++_2m":         ("DPMSolverMultistepScheduler", {"algorithm_type": "dpmsolver++"}),
    "dpm++_2m_karras":  ("DPMSolverMultistepScheduler", {"algorithm_type": "dpmsolver++", "use_karras_sigmas": True}),
    "dpm++_sde_karras": ("DPMSolverSDEScheduler",        {"use_karras_sigmas": True}),
    "euler":            ("EulerDiscreteScheduler",        {}),
    "euler_a":          ("EulerAncestralDiscreteScheduler", {}),
    "unipc":            ("UniPCMultistepScheduler",       {}),
    "ddim":             ("DDIMScheduler",                 {}),
    "heun":             ("HeunDiscreteScheduler",         {}),
    "lms":              ("LMSDiscreteScheduler",          {}),
}
DEFAULT_SAMPLER = "dpm++_2m"


def _adapter_name(name: str) -> str:
    """LoRA 名 → PEFT アダプタ名（英数字と _ のみ）。空なら 'lora'。"""
    return "".join(c if c.isalnum() else "_" for c in name) or "lora"


def _parse_lora_specs(val) -> list[tuple[str, float]]:
    """LoRA 指定を [(name, weight), ..] へ正規化。受理: 文字列 "a:0.8, b" / dict{name:weight} /
    リスト[str|dict{name,weight}|[name,weight]]。重み省略は 1.0。"""
    out: list[tuple[str, float]] = []
    if not val:
        return out
    if isinstance(val, str):
        for part in val.split(","):
            part = part.strip()
            if not part:
                continue
            if ":" in part:
                n, _, w = part.rpartition(":")
                try:
                    out.append((n.strip(), float(w)))
                    continue
                except ValueError:
                    pass
            out.append((part, 1.0))
    elif isinstance(val, dict):
        for n, w in val.items():
            try:
                out.append((str(n), float(w)))
            except (TypeError, ValueError):
                out.append((str(n), 1.0))
    elif isinstance(val, (list, tuple)):
        for it in val:
            if isinstance(it, str):
                out.extend(_parse_lora_specs(it))
            elif isinstance(it, dict):
                try:
                    out.append((str(it.get("name", "")), float(it.get("weight", 1.0))))
                except (TypeError, ValueError):
                    out.append((str(it.get("name", "")), 1.0))
            elif isinstance(it, (list, tuple)) and it:
                w = 1.0
                if len(it) > 1:
                    try:
                        w = float(it[1])
                    except (TypeError, ValueError):
                        w = 1.0
                out.append((str(it[0]), w))
    return [(n, w) for (n, w) in out if n]


def _as_name_list(v) -> list[str]:
    """埋め込み指定を名前リストへ。文字列はカンマ区切り、リストはそのまま。"""
    if isinstance(v, str):
        return [s.strip() for s in v.split(",") if s.strip()]
    if isinstance(v, (list, tuple)):
        return [str(s).strip() for s in v if str(s).strip()]
    return []


def _safetensors_header(path) -> dict | None:
    """safetensors のヘッダ（先頭 u64 長 + JSON）だけ読む（重みは読まない＝高速）。失敗は None。"""
    import struct
    try:
        with open(path, "rb") as f:
            n = struct.unpack("<Q", f.read(8))[0]
            if n == 0 or n > 64 * 1024 * 1024:  # 壊れ・別形式
                return None
            return json.loads(f.read(n).decode("utf-8"))
    except Exception:
        return None


def sniff_lora_kind(path) -> str:
    """LoRA の対象系統を推定 → "sd" | "sdxl" | "flux" | "unknown"。
    kohya 形式はキー名が SD1.5/SDXL 共通で名前では区別できないため、cross-attention
    （attn2 / comfy の _1_1_）の lora_down 入力次元で判別する＝SD1.5:768 / SDXL:2048。
    Rust 側 t2i_sniff_asset_kind と一致させること。判定不能は "unknown"（弾かない）。"""
    if not str(path).lower().endswith(".safetensors"):
        return "unknown"
    h = _safetensors_header(path)
    if not h:
        return "unknown"
    keys = [k for k in h.keys() if k != "__metadata__"]
    if any(("double_blocks" in k) or ("single_blocks" in k) or k.startswith("transformer") for k in keys):
        return "flux"
    if any(("lora_te2" in k) or ("text_encoder_2" in k) for k in keys):
        return "sdxl"
    for k, v in h.items():
        if k == "__metadata__":
            continue
        lk = k.lower()
        if (("attn2" in lk) or ("_1_1_" in lk)) and (("to_k" in lk) or ("to_v" in lk)) \
                and (("lora_down" in lk) or ("lora_a" in lk)):
            shp = v.get("shape") if isinstance(v, dict) else None
            if isinstance(shp, list) and len(shp) == 2:
                if shp[1] == 768:
                    return "sd"
                if shp[1] == 2048:
                    return "sdxl"
    return "unknown"


_KIND_JP = {"sd": "SD1.5系", "sdxl": "SDXL", "flux": "FLUX"}


# --- バックエンド（モデル種別ごとの読み込み＋生成）。★拡張点 ------------------------
# 新しいモデル一族や機能（SDXL / FLUX / 将来の ControlNet 等）は Backend を継承して
# BACKENDS に登録するだけで載る。Engine はこの抽象だけに依存する＝種別非依存。
# img2img は AutoPipelineForImage2Image.from_pipe でコンポーネント（と CPU オフロード設定）を
# 引き継ぐので、どの種別でも追加の VRAM を食わずに txt2img と共存できる。
class Backend:
    kind = "base"
    # モディファイア対応（種別ごとに上書き）。base 既定＝LoRA/VAE のみ（FLUX 相当）。
    # SD/SDXL は TI・サンプラー・Clip skip も対応（SDBackend で有効化）。
    supports_lora = True
    supports_vae = True
    supports_ti = False
    supports_scheduler = False
    supports_clip_skip = False

    def __init__(self, name, pipe, device, torch):
        self.name = name
        self.pipe = pipe
        self.device = device
        self.torch = torch
        self._i2i = None  # img2img パイプ（遅延生成・コンポーネント共有）
        # --- モディファイアの適用状態（pipe に紐づく。リコンサイル用にキャッシュ）---
        self._loaded_loras: dict[str, str] = {}   # LoRA名 → PEFT アダプタ名（ロード済み）
        self._active_loras = None                 # 直近に有効化した集合（None=無効）
        self._loaded_tis: set[str] = set()         # ロード済み埋め込み名（解除不要）
        self._orig_vae = None                      # モデル既定 VAE（差し替えから戻す用）
        self._cur_vae = ""                         # 現在適用中の VAE 名（""=既定）
        self._base_sched_cfg = getattr(getattr(pipe, "scheduler", None), "config", None)
        self._cur_sampler = DEFAULT_SAMPLER if self.supports_scheduler else None

    # --- モディファイア（LoRA / TI / VAE / サンプラー）。SD/SDXL 中心の拡張点 ---------------
    # いずれも常駐 pipe を書き換えるので「毎生成、要求状態へリコンサイル」する。ロード済みアセットは
    # 名前でキャッシュ＝繰り返しは再読み込みなし。img2img パイプ(_i2i)は UNet 等を共有するので、
    # LoRA/埋め込みはそのまま効く。scheduler/vae は参照差し替えなので _i2i にも反映する。
    def set_sampler(self, name: str) -> None:
        if not self.supports_scheduler:
            return
        name = name or DEFAULT_SAMPLER
        if name == self._cur_sampler:
            return
        entry = SAMPLERS.get(name)
        if entry is None:
            return  # 未知名は無視（現状維持）
        import diffusers
        cls = getattr(diffusers, entry[0], None)
        cfg = self._base_sched_cfg or self.pipe.scheduler.config
        if cls is None:
            return
        self.pipe.scheduler = cls.from_config(cfg, **entry[1])
        if self._i2i is not None:
            self._i2i.scheduler = self.pipe.scheduler
        self._cur_sampler = name

    def apply_vae(self, name: str, resolved) -> None:
        # resolved: (path, is_single_file) or None。name="" は既定 VAE へ戻す。
        if not self.supports_vae:
            return
        name = name or ""
        if name == self._cur_vae:
            return
        if self._orig_vae is None:
            self._orig_vae = self.pipe.vae
        if not name:
            self.pipe.vae = self._orig_vae
        else:
            if resolved is None:
                raise ValueError(f"VAE '{name}' が見つかりません（home/vae に置いてください）")
            from diffusers import AutoencoderKL
            path, single = resolved
            dtype = self.torch.float16 if self.device == "cuda" else self.torch.float32
            if single:
                vae = AutoencoderKL.from_single_file(path, torch_dtype=dtype)
            else:
                vae = AutoencoderKL.from_pretrained(path, torch_dtype=dtype)
            self.pipe.vae = vae.to(self.device)
        if self._i2i is not None:
            self._i2i.vae = self.pipe.vae
        self._cur_vae = name

    def apply_embeddings(self, names, resolve) -> None:
        # names: ロードしておく埋め込み名。トリガ語＝名前（プロンプトにその語を書けば効く／A1111 流）。
        # 一度ロードすればトークンが tokenizer に残る＝解除不要（未使用なら無影響）。
        if not self.supports_ti:
            return
        for n in names:
            if not n or n in self._loaded_tis:
                continue
            path = resolve(n)
            if path is None:
                continue
            try:
                self.pipe.load_textual_inversion(path, token=n)
            except Exception:
                # 二重ロード・形式不一致・種別不一致(SD↔SDXL)は握りつぶす
                # （1つの不良埋め込みで生成全体を止めない）。
                pass
            self._loaded_tis.add(n)  # 失敗も記録＝毎回リトライしない

    def _adapter_registered(self, adapter: str) -> bool:
        """adapter が実際に pipe のどこかの層に登録されたか。get_list_adapters は
        {"unet":[..],"text_encoder":[..]} 形。判定不能（API 無し）なら True（従来動作）。"""
        try:
            listed = self.pipe.get_list_adapters()
        except Exception:
            return True
        return any(adapter in names for names in listed.values())

    def _adapter_present(self, adapter: str) -> bool:
        """adapter が**確実に** pipe に登録済みか。判定不能なら False（＝ロードを試みる）。
        _adapter_registered と逆の安全側: 「確信があるときだけ True」＝再ロードのスキップ判定用。"""
        try:
            listed = self.pipe.get_list_adapters()
        except Exception:
            return False
        return any(adapter in names for names in listed.values())

    def apply_loras(self, specs, resolve) -> None:
        # specs: [(name, weight), ..]。PEFT アダプタ API で毎回リコンサイル（要求集合だけ有効化）。
        if not self.supports_lora:
            return
        want = list(specs)
        for n, _ in want:
            if n in self._loaded_loras:
                continue
            path = resolve(n)
            if path is None:
                raise ValueError(f"LoRA '{n}' が見つかりません（home/loras に置いてください）")
            adapter = _adapter_name(n)
            # 冪等化: pipe に既に同名アダプタが居るなら再ロードしない。_loaded_loras との
            # 不整合（前回失敗の残骸・モデル再ロード跨ぎ等）で「already in use」になるのを防ぐ。
            if self._adapter_present(adapter):
                self._loaded_loras[n] = adapter
                continue
            # 事前照合: 系統不一致（SD1.5 LoRA を SDXL へ等）は diffusers 内部で
            # 「list index out of range」等の不親切なエラーになるので、ロード前に弾く。
            lk = sniff_lora_kind(path)
            if lk != "unknown" and lk != self.kind:
                raise ValueError(
                    f"LoRA '{n}' は{_KIND_JP.get(lk, lk)}用のため、現在のモデル"
                    f"（{_KIND_JP.get(self.kind, self.kind)}）には適用できません。"
                    f"モデルと同系統の LoRA を選んでください"
                )
            try:
                self.pipe.load_lora_weights(path, adapter_name=adapter)
            except Exception as ex:
                # 列挙不能な環境で pipe に既存＝競合するケースは「ロード済み」として拾い、成功扱いにする。
                if "already in use" in str(ex).lower():
                    self._loaded_loras[n] = adapter
                    continue
                raise RuntimeError(
                    f"LoRA '{n}' の適用に失敗しました。モデルと LoRA の系統不一致"
                    f"（SD1.5↔SDXL↔FLUX）か、非対応形式の可能性があります"
                    f"（peft 未導入なら『pip install peft』）: {ex}"
                ) from ex
            # load は通っても、キーがどの層にも一致しないと**アダプタが0層＝未登録**になる
            # （例: SD/SDXL のパイプに DiT/FLUX 用 LoRA を入れた）。この場合そのまま進むと
            # set_adapters が「present adapters: set()」で落ちる＝分かりにくいので、ここで明示的に弾く。
            if not self._adapter_registered(adapter):
                try:
                    self.pipe.delete_adapters(adapter)
                except Exception:
                    pass
                raise ValueError(
                    f"LoRA '{n}' は現在のモデル（{self.kind}）に適合しません＝対応する層が無く未適用でした。"
                    f"SD/SDXL 用でない可能性（DiT/FLUX 等の別アーキ向け）。モデルと LoRA の種別を合わせてください"
                )
            self._loaded_loras[n] = adapter
        if want:
            self.pipe.set_adapters(
                [self._loaded_loras[n] for n, _ in want],
                adapter_weights=[float(w) for _, w in want],
            )
        elif self._active_loras:
            try:
                self.pipe.disable_lora()
            except Exception:
                pass
        self._active_loras = want or None

    @classmethod
    def load(cls, name, model_path, device, torch):
        raise NotImplementedError

    @classmethod
    def load_single_file(cls, name, path, device, torch):
        # 既定では単一ファイル非対応（FLUX 等）。SD/SDXL がオーバーライドする。
        # 通常は UI 側の「手持ちモデル診断」で事前に弾かれ、ここには到達しない（保険）。
        p = model_policy(getattr(cls, "kind", "?"), "single")
        raise ValueError(f"{p['reason']}。推奨: {p['recommend']}")

    def _img2img(self):
        if self._i2i is None:
            from diffusers import AutoPipelineForImage2Image
            self._i2i = AutoPipelineForImage2Image.from_pipe(self.pipe)
            self._i2i.set_progress_bar_config(disable=True)
        return self._i2i

    def _gen(self, params):
        # 再現用乱数。CPU 生成器は CUDA でも CPU オフロードでも安全に効く（diffusers が転送）。
        seed = params.get("seed")
        if seed is None or str(seed) == "":
            return None
        return self.torch.Generator("cpu").manual_seed(int(seed))

    def generate(self, params, init_image=None):
        raise NotImplementedError

    def unload(self):
        self.pipe = None
        self._i2i = None


def _finalize_sd(pipe, device, torch):
    """SD/SDXL 共通の仕上げ: DPM++ スケジューラ・デバイス転送・スライシング。"""
    from diffusers import DPMSolverMultistepScheduler
    try:
        pipe.scheduler = DPMSolverMultistepScheduler.from_config(
            pipe.scheduler.config, algorithm_type="dpmsolver++"
        )
    except Exception:
        pass
    pipe = pipe.to(device)
    pipe.set_progress_bar_config(disable=True)
    try:
        pipe.enable_vae_slicing()
        pipe.enable_attention_slicing()
    except Exception:
        pass
    return pipe


class SDBackend(Backend):
    """Stable Diffusion 1.x 系。負プロンプト対応・DPM++・fp16。diffusers/単一ファイル両対応。
    モディファイア（LoRA/TI/VAE/サンプラー/Clip skip）フル対応＝SD/SDXL の拡張はここに乗る。"""
    kind = "sd"
    default_size = 512
    supports_ti = True
    supports_scheduler = True
    supports_clip_skip = True

    @staticmethod
    def _pipe_cls():
        from diffusers import StableDiffusionPipeline
        return StableDiffusionPipeline

    @classmethod
    def load(cls, name, model_path, device, torch):
        dtype = torch.float16 if device == "cuda" else torch.float32
        kw = dict(torch_dtype=dtype, safety_checker=None, requires_safety_checker=False)
        P = cls._pipe_cls()
        try:
            pipe = P.from_pretrained(model_path, variant="fp16", **kw)
        except Exception:
            pipe = P.from_pretrained(model_path, **kw)
        return cls(name, _finalize_sd(pipe, device, torch), device, torch)

    @classmethod
    def load_single_file(cls, name, path, device, torch):
        # Civitai 等の単一ファイル(.safetensors/.ckpt)を from_single_file で読む。
        dtype = torch.float16 if device == "cuda" else torch.float32
        pipe = cls._pipe_cls().from_single_file(path, torch_dtype=dtype, safety_checker=None)
        return cls(name, _finalize_sd(pipe, device, torch), device, torch)

    def generate(self, params, init_image=None):
        # SDXL は 512 だと崩れるので、サイズ未変更（既定512）なら種別既定（1024）に上げる。
        w, h = params["width"], params["height"]
        if self.default_size != 512:
            if w == 512:
                w = self.default_size
            if h == 512:
                h = self.default_size
        common = dict(
            prompt=params["prompt"],
            negative_prompt=params.get("negative_prompt") or None,
            num_inference_steps=int(params.get("steps") or 25),
            guidance_scale=float(params.get("guidance") or 7.5),
            generator=self._gen(params),
            callback_on_step_end=params.get("_callback"),
        )
        # Clip skip（A1111 流: 1=スキップなし・2=penultimate）。diffusers の clip_skip は
        # 「末尾から飛ばす層数」で A1111 の値-1 に相当。1 以下は指定しない（＝スキップなし）。
        try:
            cs = int(params.get("clip_skip") or 0)
        except (TypeError, ValueError):
            cs = 0
        if cs > 1:
            common["clip_skip"] = cs - 1
        with self.torch.inference_mode():
            if init_image is not None:
                return self._img2img()(
                    image=init_image, strength=float(params.get("strength") or 0.6), **common
                ).images[0]
            return self.pipe(width=w, height=h, **common).images[0]


class SDXLBackend(SDBackend):
    """Stable Diffusion XL。SD と同じ流儀（負プロンプト/DPM++）だがパイプは XL・既定 1024。
    fp16 で 24GB に収まるのでオフロード不要。"""
    kind = "sdxl"
    default_size = 1024

    @staticmethod
    def _pipe_cls():
        from diffusers import StableDiffusionXLPipeline
        return StableDiffusionXLPipeline


class FluxBackend(Backend):
    """FLUX.1 系（schnell/dev/lite）。負プロンプト非対応・bf16・24GB GPU は CPU オフロード。"""
    kind = "flux"

    @classmethod
    def load(cls, name, model_path, device, torch):
        from diffusers import FluxPipeline
        pipe = FluxPipeline.from_pretrained(model_path, torch_dtype=torch.bfloat16)
        if device == "cuda":
            # FLUX(~12B)+T5 は 24GB に収まらない → モデル CPU オフロードで段階転送（やや遅いが確実）。
            pipe.enable_model_cpu_offload()
        else:
            pipe = pipe.to(device)
        pipe.set_progress_bar_config(disable=True)
        try:
            pipe.vae.enable_slicing()
            pipe.vae.enable_tiling()
        except Exception:
            pass
        return cls(name, pipe, device, torch)

    def generate(self, params, init_image=None):
        # FLUX は negative_prompt 非対応。schnell/lite はガイダンス蒸留 → guidance 0・少ステップ。
        common = dict(
            prompt=params["prompt"],
            num_inference_steps=int(params.get("steps") or 4),
            guidance_scale=float(params.get("guidance") or 0.0),
            generator=self._gen(params),
            callback_on_step_end=params.get("_callback"),
        )
        with self.torch.inference_mode():
            if init_image is not None:
                return self._img2img()(
                    image=init_image, strength=float(params.get("strength") or 0.6), **common
                ).images[0]
            return self.pipe(width=params["width"], height=params["height"], **common).images[0]


BACKENDS = {"sd": SDBackend, "sdxl": SDXLBackend, "flux": FluxBackend}


class Engine:
    """ロード済みパイプラインを保持する常駐エンジン（プロセス内シングルトン）。"""

    def __init__(self, home: str):
        self.home = os.path.abspath(home)
        self.models_dir = os.path.join(self.home, "models")
        os.makedirs(self.models_dir, exist_ok=True)
        # モディファイア用アセット置き場（models/ とは分離＝モデル一覧を汚さない）。
        self.loras_dir = os.path.join(self.home, "loras")
        self.embeddings_dir = os.path.join(self.home, "embeddings")
        self.vae_dir = os.path.join(self.home, "vae")
        for _d in (self.loras_dir, self.embeddings_dir, self.vae_dir):
            os.makedirs(_d, exist_ok=True)
        # HF のキャッシュも共有 app-data 配下へ（小さい起動ドライブを埋めない）。
        os.environ.setdefault("HF_HOME", os.path.join(self.home, "hf"))
        os.makedirs(os.environ["HF_HOME"], exist_ok=True)
        # ゲート付きモデル用 HF トークンの置き場（TexoS が config から書き出す）。
        # **pull のたびにここを読む**ので、トークン更新にデーモン再起動が要らない。
        self.hf_token_file = os.path.join(self.home, "hf_token")

        self.lock = threading.Lock()  # 生成・ロードは1度に1つ（単一VRAM）
        self.backend: Backend | None = None  # 現在ロード中のバックエンド（None=未ロード）
        self.cancel_event = threading.Event()  # /cancel で立てる＝生成を次ステップで中断
        self._torch = None
        self._device = None

    def _cancel_cb(self):
        """diffusers の callback_on_step_end 用。/cancel が来たら次ステップ境界で中断する
        （pipe._interrupt=True で denoise ループが抜ける → すぐ GPU が空く）。"""
        ev = self.cancel_event

        def cb(pipe, step, timestep, kw):
            if ev.is_set():
                pipe._interrupt = True
            return kw

        return cb

    @property
    def current_model(self):
        return self.backend.name if self.backend else None

    def hf_token(self):
        """HF トークンを取得（ゲート付きモデル用）。環境変数 → home/hf_token ファイルの順。
        ファイルから読むので、デーモンを再起動しなくてもトークン更新が効く。"""
        t = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
        if t and t.strip():
            return t.strip()
        try:
            with open(self.hf_token_file, encoding="utf-8") as f:
                t = f.read().strip()
                return t or None
        except OSError:
            return None

    # --- torch は遅延 import（起動を軽く、/health だけなら不要にしたいが実質常時要る）---
    @property
    def torch(self):
        if self._torch is None:
            import torch

            self._torch = torch
            self._device = "cuda" if torch.cuda.is_available() else "cpu"
        return self._torch

    @property
    def device(self) -> str:
        _ = self.torch
        return self._device

    def model_path(self, name: str) -> str:
        return os.path.join(self.models_dir, name)

    # 単一ファイルチェックポイント（Civitai 等）の拡張子。
    _CKPT_EXTS = (".safetensors", ".ckpt")

    def _checkpoint_in_dir(self, d):
        """フォルダ内に単一ファイルのチェックポイントが1つだけあればそのパス。"""
        try:
            cks = [f for f in os.listdir(d) if f.lower().endswith(self._CKPT_EXTS)]
        except OSError:
            return None
        return os.path.join(d, cks[0]) if len(cks) == 1 else None

    def resolve_load(self, name):
        """モデル名 → (path, is_single_file)。無ければ None。対応:
        ①models/<name>/（diffusers）②models/<name>/ 内の単一ファイル
        ③models/<name>.safetensors / .ckpt（直置きの単一ファイル）。"""
        d = os.path.join(self.models_dir, name)
        if os.path.isfile(os.path.join(d, "model_index.json")):
            return (d, False)
        if os.path.isdir(d):
            ck = self._checkpoint_in_dir(d)
            if ck:
                return (ck, True)
        for ext in self._CKPT_EXTS:
            f = os.path.join(self.models_dir, name + ext)
            if os.path.isfile(f):
                return (f, True)
        return None

    def is_available(self, name: str) -> bool:
        """ロード可能か。単一ファイルは自己完結なので存在すれば OK。diffusers フォルダは
        **完全性**（model_index.json＋重み一式＋シャード・.incomplete 無し）を確認する。"""
        r = self.resolve_load(name)
        if r is None:
            return False
        path, single = r
        return True if single else self._diffusers_complete(path)

    def _diffusers_complete(self, d) -> bool:
        """diffusers フォルダ d が完全か。DL 途中/失敗（重みシャード欠落）を弾く。"""
        idx = os.path.join(d, "model_index.json")
        if not os.path.isfile(idx):
            return False
        try:
            for _r, _ds, fs in os.walk(d):
                if any(f.endswith(".incomplete") for f in fs):
                    return False
            with open(idx, encoding="utf-8") as f:
                mi = json.load(f)
        except Exception:
            return False
        no_weight = {"scheduler", "tokenizer", "tokenizer_2", "tokenizer_3",
                     "feature_extractor", "safety_checker", "image_encoder"}
        for comp, val in mi.items():
            if comp.startswith("_") or comp in no_weight:
                continue
            if not (isinstance(val, list) and len(val) == 2 and val[0]):
                continue
            sub = os.path.join(d, comp)
            if not os.path.isdir(sub):
                return False
            files = os.listdir(sub)
            shard_idxs = [f for f in files if f.endswith(".index.json")]
            if shard_idxs:
                for ix in shard_idxs:
                    try:
                        wm = json.load(open(os.path.join(sub, ix), encoding="utf-8")).get("weight_map", {})
                    except Exception:
                        return False
                    for shard in set(wm.values()):
                        if not os.path.isfile(os.path.join(sub, shard)):
                            return False
            elif not any(f.endswith((".safetensors", ".bin")) for f in files):
                return False
        return True

    def _safetensors_keys(self, path):
        """safetensors のヘッダ（先頭）だけ読んでテンソル名一覧を返す（重みは読まない＝高速）。"""
        import struct
        with open(path, "rb") as f:
            n = struct.unpack("<Q", f.read(8))[0]
            header = json.loads(f.read(n).decode("utf-8"))
        return [k for k in header.keys() if k != "__metadata__"]

    def _detect_kind_file(self, path) -> str:
        """単一ファイルの種別を safetensors のキーから推定（sd / sdxl / flux）。"""
        if not path.lower().endswith(".safetensors"):
            return "sd"  # .ckpt はヘッダ読みが面倒 → SD 既定（from_single_file は読める）
        try:
            keys = self._safetensors_keys(path)
        except Exception:
            return "sd"
        if any(("double_blocks" in k) or ("single_blocks" in k) for k in keys):
            return "flux"
        # SDXL は2つ目のテキストエンコーダ / add_embedding を持つ
        if any(k.startswith("conditioner.embedders.1") for k in keys) or any("add_embedding" in k for k in keys):
            return "sdxl"
        return "sd"

    def _infer_kind(self, name: str) -> str:
        """手動配置の diffusers フォルダの種別を model_index.json の _class_name から推定。"""
        try:
            with open(os.path.join(self.model_path(name), "model_index.json"), encoding="utf-8") as f:
                cls = json.load(f).get("_class_name", "")
        except Exception:
            return "sd"
        if "Flux" in cls:
            return "flux"
        if "XL" in cls:
            return "sdxl"
        return "sd"

    def list_available(self) -> list[str]:
        out = set()
        try:
            entries = os.listdir(self.models_dir)
        except OSError:
            return []
        for n in entries:
            full = os.path.join(self.models_dir, n)
            if os.path.isdir(full):
                if self.is_available(n):  # diffusers 完全 or 単一ファイル入りフォルダ
                    out.add(n)
            else:
                base, ext = os.path.splitext(n)
                if ext.lower() in self._CKPT_EXTS:
                    out.add(base)  # 直置きの単一ファイル（名前=拡張子抜き）
        return sorted(out)

    # --- モディファイア用アセット（LoRA / 埋め込み / VAE）の解決と一覧。★拡張点 ----------
    # 置き場は home 直下の loras/ embeddings/ vae/。名前は「拡張子なしのファイル名」で指定でき、
    # 完全一致 → 拡張子補完の順に解く（VAE は diffusers フォルダも許可）。
    _LORA_EXTS = (".safetensors", ".pt", ".ckpt")
    _EMB_EXTS = (".safetensors", ".pt", ".bin")
    _VAE_EXTS = (".safetensors", ".ckpt", ".pt", ".bin")

    @staticmethod
    def _resolve_file(d, name, exts):
        if not name:
            return None
        p = os.path.join(d, name)
        if os.path.isfile(p):
            return p
        for ext in exts:
            f = os.path.join(d, name + ext)
            if os.path.isfile(f):
                return f
        return None

    def resolve_lora(self, name):
        return self._resolve_file(self.loras_dir, name, self._LORA_EXTS)

    def resolve_embedding(self, name):
        return self._resolve_file(self.embeddings_dir, name, self._EMB_EXTS)

    def resolve_vae(self, name):
        """VAE 名 → (path, is_single_file) or None。diffusers フォルダ or 単一ファイル両対応。"""
        if not name:
            return None
        d = os.path.join(self.vae_dir, name)
        if os.path.isdir(d):
            return (d, False)
        f = self._resolve_file(self.vae_dir, name, self._VAE_EXTS)
        return (f, True) if f else None

    @staticmethod
    def _list_assets(d, exts, dirs=False):
        out = set()
        try:
            entries = os.listdir(d)
        except OSError:
            return []
        for n in entries:
            full = os.path.join(d, n)
            if os.path.isdir(full):
                if dirs:
                    out.add(n)
            else:
                base, ext = os.path.splitext(n)
                if ext.lower() in exts:
                    out.add(base)
        return sorted(out)

    def list_loras(self):
        return self._list_assets(self.loras_dir, self._LORA_EXTS)

    def list_embeddings(self):
        return self._list_assets(self.embeddings_dir, self._EMB_EXTS)

    def list_vae(self):
        return self._list_assets(self.vae_dir, self._VAE_EXTS, dirs=True)

    def _apply_modifiers(self, params: dict) -> None:
        """生成直前に、要求されたモディファイアを現在の backend/pipe へリコンサイルする。
        呼び出しは generate 内のロック下（pipe を1度に1つだけ触る）。"""
        b = self.backend
        if b is None:
            return
        b.set_sampler((params.get("sampler") or "").strip().lower())
        vname = (params.get("vae") or "").strip()
        b.apply_vae(vname, self.resolve_vae(vname))
        if b.supports_ti:
            emb = params.get("embeddings")
            # 既定（未指定/"auto"）は embeddings/ を全自動ロード（A1111 流）。プロンプトに名前を書けば効く。
            names = self.list_embeddings() if emb in (None, "", "auto") else _as_name_list(emb)
            b.apply_embeddings(names, self.resolve_embedding)
        b.apply_loras(_parse_lora_specs(params.get("lora")), self.resolve_lora)

    # --- ダウンロード（pull）。種別ごとの取得パターン。進捗を on_line(dict) で流す ----------
    def pull(self, name: str, on_line) -> None:
        spec = MODELS.get(name)
        if not spec:
            raise ValueError(f"未知のモデル名: {name}（既知: {', '.join(MODELS)}）")
        kind = spec.get("kind", "sd")
        patterns = PATTERNS_BY_KIND.get(kind, SD_PATTERNS)
        # gated（FLUX 公式）は HF トークンが必要。pull のたびにファイル/環境から読む（公開モデルは None でよい）。
        token = self.hf_token()
        from huggingface_hub import snapshot_download
        from huggingface_hub.utils import tqdm as hf_tqdm

        on_line({"status": f"{name} のダウンロードを開始…", "percent": 0})

        # tqdm をフックして進捗を on_line に流す。snapshot_download は複数スレッドで
        # ダウンロードする（各ファイルが別バー）。on_line には**整数%が変化したときだけ**
        # 通知＝チャンク数を抑える（_pull 側でキュー直列化するので並行書き込みは起きない）。
        class _Tqdm(hf_tqdm):  # type: ignore[misc]
            def update(self, n=1):
                r = super().update(n)
                try:
                    if self.total:
                        pct = int(self.n / self.total * 100)
                        if getattr(self, "_lastpct", -1) != pct:
                            self._lastpct = pct
                            desc = (self.desc or "").split("/")[-1][:48]
                            on_line({"status": f"取得中 {desc}", "percent": pct})
                except Exception:
                    pass
                return r

        def _dl(pats):
            snapshot_download(
                spec["repo"], local_dir=self.model_path(name),
                allow_patterns=pats, tqdm_class=_Tqdm, token=token,
            )

        try:
            _dl(patterns)
        except Exception as ex:
            if spec.get("gated"):
                raise RuntimeError(
                    f"{name} はゲート付きです。①モデルページで「Agree and access repository」を押す "
                    f"②read 権限の HF トークンを設定、の両方を確認してください（{type(ex).__name__}）"
                ) from ex
            raise
        # SD/SDXL で fp16 variant が無い配布への保険: 素の safetensors 一式を取り直す。
        plain = PLAIN_PATTERNS_BY_KIND.get(kind)
        if plain:
            import glob as _glob
            if not _glob.glob(os.path.join(self.model_path(name), "unet", "*.fp16.safetensors")):
                on_line({"status": "fp16 が無いため標準版を取得…", "percent": 0})
                _dl(plain)
        if not self.is_available(name):
            raise RuntimeError("ダウンロードしましたが diffusers モデルとして認識できません")
        on_line({"status": "完了", "percent": 100})

    # --- ロード（VRAM 常駐）。diffusers / 単一ファイルの両方・種別ごとの Backend へ -----------
    def load(self, name: str) -> None:
        with self.lock:
            if self.backend is not None and self.backend.name == name:
                return
            r = self.resolve_load(name)
            if r is None:
                avail = self.list_available()
                hint = "、".join(avail) if avail else "なし"
                raise ValueError(
                    f"モデル '{name}' は未取得です（取得済み: {hint}）。設定で取得するか手動配置してください"
                )
            path, single = r
            if not single and not self._diffusers_complete(path):
                raise ValueError(f"モデル '{name}' は不完全です（ファイル欠落）。再取得してください")
            # 種別決定: 単一ファイル=safetensorsキーから推定 / フォルダ=レジストリ or _class_name。
            if single:
                kind = self._detect_kind_file(path)
            else:
                spec = MODELS.get(name)
                kind = spec.get("kind", "sd") if spec else self._infer_kind(name)
            backend_cls = BACKENDS.get(kind)
            if backend_cls is None:
                raise ValueError(f"未対応のモデル種別: {kind}")
            # 旧モデルを確実に解放（FLUX は巨大なので空けてから）。
            if self.backend is not None:
                self.backend.unload()
                self.backend = None
            if self.device == "cuda":
                self.torch.cuda.empty_cache()
            if single:
                self.backend = backend_cls.load_single_file(name, path, self.device, self.torch)
            else:
                self.backend = backend_cls.load(name, path, self.device, self.torch)

    # --- 生成（txt2img / img2img）。実体は種別ごとの Backend.generate に委譲する ----------
    def generate(self, params: dict) -> bytes:
        prompt = (params.get("prompt") or "").strip()
        if not prompt:
            raise ValueError("prompt が空です")
        # モデル決定: 明示指定 > ロード済み > 取得済みの先頭 > 既定。
        req = (params.get("model") or "").strip()
        if req:
            name = req
        elif self.backend is not None:
            name = self.backend.name
        else:
            avail = self.list_available()
            name = avail[0] if avail else DEFAULT_MODEL
        self.load(name)

        # 入力整形（幅高さは 8 の倍数）。init_image があれば img2img。
        params = dict(params)
        params["prompt"] = prompt
        params["width"] = max(256, (int(params.get("width") or 512) // 8) * 8)
        params["height"] = max(256, (int(params.get("height") or 512) // 8) * 8)
        params["_callback"] = self._cancel_cb()  # /cancel で中断できるように
        init = None
        if params.get("init_image"):
            from PIL import Image
            init = Image.open(io.BytesIO(base64.b64decode(params["init_image"]))).convert("RGB")

        self.cancel_event.clear()  # 新しい生成の開始 = 前のキャンセルをリセット
        with self.lock:
            self._apply_modifiers(params)  # LoRA/TI/VAE/サンプラー/Clip skip を要求状態へ
            image = self.backend.generate(params, init_image=init)
        buf = io.BytesIO()
        image.save(buf, format="PNG")
        return buf.getvalue()


ENGINE: Engine | None = None


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # コンソールに出さない（TexoS がプロセスを握る）
        pass

    def _json(self, code: int, obj: dict):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_json(self) -> dict:
        n = int(self.headers.get("Content-Length") or 0)
        if n <= 0:
            return {}
        return json.loads(self.rfile.read(n) or b"{}")

    def do_GET(self):
        try:
            if self.path == "/health":
                e = ENGINE
                self._json(200, {
                    "ok": True,
                    "version": SERVER_VERSION,
                    "device": e.device,
                    "cuda": e.device == "cuda",
                    "current_model": e.current_model,
                    "loaded": e.backend is not None,
                    "models_dir": e.models_dir,
                    "available": e.list_available(),
                    "hf_token": bool(e.hf_token()),  # デーモンがトークンを見えているか
                })
            elif self.path == "/models":
                e = ENGINE
                self._json(200, {
                    "available": e.list_available(),
                    "known": sorted(MODELS.keys()),
                    "current": e.current_model,
                })
            elif self.path == "/assets":
                # モディファイア用アセット一覧（loras/embeddings/vae）。UI のドロップダウン等に。
                e = ENGINE
                self._json(200, {
                    "loras": e.list_loras(),
                    "embeddings": e.list_embeddings(),
                    "vae": e.list_vae(),
                    "samplers": sorted(SAMPLERS.keys()),
                })
            else:
                self._json(404, {"error": "not found"})
        except Exception as ex:
            self._json(500, {"error": str(ex), "trace": traceback.format_exc()})

    def do_POST(self):
        try:
            if self.path == "/cancel":
                # 実行中の生成を次ステップ境界で中断させる（別スレッド・ロック不要）。
                ENGINE.cancel_event.set()
                self._json(200, {"ok": True})
            elif self.path == "/load":
                body = self._read_json()
                name = body.get("name") or DEFAULT_MODEL
                ENGINE.load(name)
                self._json(200, {"ok": True, "model": name})
            elif self.path == "/pull":
                self._pull()
            elif self.path == "/generate":
                body = self._read_json()
                png = ENGINE.generate(body)
                self.send_response(200)
                self.send_header("Content-Type", "image/png")
                self.send_header("Content-Length", str(len(png)))
                self.end_headers()
                self.wfile.write(png)
            else:
                self._json(404, {"error": "not found"})
        except Exception as ex:
            self._json(500, {"error": str(ex), "trace": traceback.format_exc()})

    def _pull(self):
        """NDJSON 進捗ストリーム。Rust 側は1行ずつ読んで "t2i-model" イベントへ。

        重要: snapshot_download は複数スレッドで並行ダウンロードし、tqdm 進捗も各スレッドから
        飛んでくる。これを直接 self.wfile へ書くと chunked フレームが交錯して壊れ、クライアント
        （reqwest）が "error decoding response body" になる。そこでダウンロードは worker スレッドへ、
        進捗は thread-safe な Queue へ積み、**書き込みはこのハンドラスレッド1本だけ**にする。
        """
        import queue as _queue

        body = self._read_json()
        name = body.get("name") or DEFAULT_MODEL
        # chunked transfer は手書きするとフレームが交錯して reqwest が
        # 「error decoding response body」になりやすい。そこで **Connection: close** にして
        # 「本文＝接続が閉じるまで」方式にする（Content-Length も Transfer-Encoding も付けない）。
        # クライアントは EOF まで1行ずつ読むだけ＝フレーミング崩れが起きない。
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson; charset=utf-8")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True

        q: "_queue.Queue" = _queue.Queue()
        SENTINEL = object()

        def worker():
            try:
                ENGINE.pull(name, q.put)
                q.put({"done": True})
            except Exception as ex:
                q.put({"error": str(ex), "trace": traceback.format_exc()})
            finally:
                q.put(SENTINEL)

        threading.Thread(target=worker, daemon=True).start()

        try:
            while True:
                obj = q.get()
                if obj is SENTINEL:
                    break
                self.wfile.write((json.dumps(obj) + "\n").encode("utf-8"))
                self.wfile.flush()
        except Exception:
            pass


def main():
    global ENGINE
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--home", default=os.path.join(os.path.expanduser("~"), ".texos_t2i"))
    args = ap.parse_args()
    ENGINE = Engine(args.home)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    # 起動時の自己診断（どのディレクトリを走査し、何を見つけたか）。
    # 「モデル未取得」系の不一致を server.log で切り分けるため。
    try:
        listing = os.listdir(ENGINE.models_dir) if os.path.isdir(ENGINE.models_dir) else "MISSING"
    except Exception as ex:
        listing = f"ERROR:{ex}"
    sys.stderr.write(
        f"t2i_server listening on 127.0.0.1:{args.port}\n"
        f"  home={ENGINE.home}\n"
        f"  models_dir={ENGINE.models_dir}\n"
        f"  listdir={listing}\n"
        f"  available={ENGINE.list_available()}\n"
    )
    sys.stderr.flush()
    server.serve_forever()


if __name__ == "__main__":
    main()
