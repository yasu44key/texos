#!/usr/bin/env python
"""TexoS Image-To-3D デーモン（Hunyuan3D-2 エンジン・t2x 生成デーモン規約準拠）。

TexoS（Rust）が Hunyuan3D-2 WinPortable の python_standalone でこのスクリプトを起動・管理し、
HTTP で生成を依頼する。t2i_server.py と同じ規約（/health + version 照合・/models・/pull・
/generate・/cancel）だが、プロセス・ポート・Python 環境は完全に分離する（V2.0_t23d設計.md）。

エンジン（--engine）は Hunyuan3D-2 リポジトリのフォルダ（hy3dgen パッケージがある場所）。
sys.path に足して import する＝TexoS 本体は torch にも HF にも触らない。モデルは
HF_HUB_CACHE（Rust がポータブルの HuggingFaceHub へ向けて渡す）のスナップショットを使う。

エンドポイント:
  GET  /health            -> {"ok":true,"version":..,"device":..,"cuda":bool,"current_model":..,"loaded":bool}
  GET  /models            -> {"available":[name..],"known":[name..],"current":name|null}
  POST /load   {name}     -> {"ok":true,"model":name}     モデルをVRAMへ（無ければエラー）
  POST /pull   {name}     -> NDJSON 進捗ストリーム（{"status","percent"} / 末尾 {"done":true}）
  POST /generate {...}    -> model/gltf-binary（glb バイト列）。image(base64 PNG) 必須。
                            任意: model / steps / guidance / octree / seed /
                            texture(true でテクスチャ付き) / face_count(texture 時の上限面数)
  POST /cancel            -> 実行中の生成を次ステップ境界で中断（部分出力は Rust 側で破棄）。
                            texture 段はステージ境界（shape→リダクション→paint の間）で best-effort
引数: --port <n>（既定 8766） --engine <dir>（必須）

texture=true は delight（光影除去）＋ multiview（6視点拡散）＋ paint パイプラインを追加ロード
する（合計 VRAM ~16GB）。ポータブル版の texgen は mmgp オフロード前提で .to(device) が
コメントアウトされているため、常駐デーモンでは upstream 相当に明示的へ CUDA に移す。
"""

from __future__ import annotations

import argparse
import base64
import glob as _glob
import io
import json
import os
import sys
import threading
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# スクリプトの版（変更ごとに上げる）。TexoS はこれを /health で照合し、古いデーモンが
# 残っていれば停止して最新スクリプトで起動し直す。Rust 側 T23D_SERVER_VERSION と一致させること。
SERVER_VERSION = "4"

# --- モデルレジストリ（定番限定・POLICY）。name → (HF repo, スナップショット内サブフォルダ) ---
# Tencent Hunyuan 3D 2.0 Community License: 商用可（月間1億MAU未満）・EU/英国/韓国は使用不可・
# 生成物（Outputs）は Model Derivatives に含まれず自由。Rust 側 T23D_KNOWN_MODELS と一致させること。
# 2mini / 2mv は実測後に追加する（V2.0_t23d設計.md §2）。
MODELS: dict[str, dict] = {
    "hunyuan3d-2":       {"repo": "tencent/Hunyuan3D-2", "subfolder": "hunyuan3d-dit-v2-0"},
    "hunyuan3d-2-turbo": {"repo": "tencent/Hunyuan3D-2", "subfolder": "hunyuan3d-dit-v2-0-turbo"},
}
DEFAULT_MODEL = "hunyuan3d-2"

# shape チェックポイントの候補ファイル名（存在するものを先頭から使う。fp16 優先＝小さく速い）。
_CKPT_CANDIDATES = (
    "model.fp16.safetensors",
    "model.safetensors",
    "model.fp16.ckpt",
    "model.ckpt",
)


class _Cancelled(Exception):
    """/cancel による生成中断（denoise ループを callback からの例外で抜ける）。"""


def hf_cache_dir() -> str:
    """HF のモデルキャッシュ置き場。Rust がポータブルの HuggingFaceHub を HF_HUB_CACHE で渡す。"""
    return os.environ.get("HF_HUB_CACHE") or os.path.join(
        os.path.expanduser("~"), ".cache", "huggingface", "hub"
    )


def resolve_model_dir(name: str) -> str | None:
    """モデル名 → スナップショット内の shape パイプラインフォルダ（config.yaml＋重みが揃う場所）。
    無ければ None。複数スナップショットがあれば揃っている最初のものを使う。"""
    spec = MODELS.get(name)
    if not spec:
        return None
    repo_dir = os.path.join(hf_cache_dir(), "models--" + spec["repo"].replace("/", "--"))
    for snap in sorted(_glob.glob(os.path.join(repo_dir, "snapshots", "*"))):
        d = os.path.join(snap, spec["subfolder"])
        if os.path.isfile(os.path.join(d, "config.yaml")) and _find_ckpt(d):
            return d
    return None


def _find_ckpt(d: str) -> str | None:
    for n in _CKPT_CANDIDATES:
        p = os.path.join(d, n)
        if os.path.isfile(p):
            return p
    return None


# テクスチャ生成が使うサブフォルダ（標準 paint のみ。turbo paint は実測後に検討）。
_PAINT_SUBFOLDERS = ("hunyuan3d-delight-v2-0", "hunyuan3d-paint-v2-0")


def resolve_paint_dirs() -> tuple[str, str] | None:
    """テクスチャ用の (delight, paint) フォルダ。delight＋paint が揃うスナップショットを探す。"""
    repo_dir = os.path.join(hf_cache_dir(), "models--tencent--Hunyuan3D-2")
    for snap in sorted(_glob.glob(os.path.join(repo_dir, "snapshots", "*"))):
        dirs = [os.path.join(snap, s) for s in _PAINT_SUBFOLDERS]
        if all(os.path.isfile(os.path.join(d, "model_index.json")) for d in dirs):
            return (dirs[0], dirs[1])
    return None


class Engine:
    """ロード済み shape パイプラインを保持する常駐エンジン（プロセス内シングルトン）。"""

    def __init__(self, engine_dir: str):
        self.engine_dir = os.path.abspath(engine_dir)
        self.lock = threading.Lock()  # 生成・ロードは1度に1つ（単一VRAM）
        self.cancel_event = threading.Event()  # /cancel で立てる＝次ステップ境界で中断
        self.pipeline = None
        self.model_name: str | None = None
        self.paint = None  # テクスチャ用（delight＋multiview＋レンダラ）。初回 texture 要求でロード
        self._rembg = None
        self._torch = None
        self._device = None

    # --- torch / hy3dgen は遅延 import（起動を軽く・/health を即応答に）---
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

    @property
    def rembg(self):
        """背景除去（初回のみセッション構築）。入力画像に常時適用＝api_server.py と同じ。"""
        if self._rembg is None:
            from hy3dgen.rembg import BackgroundRemover

            self._rembg = BackgroundRemover()
        return self._rembg

    def list_available(self) -> list[str]:
        return sorted(n for n in MODELS if resolve_model_dir(n))

    # --- ロード（VRAM 常駐）。チェックポイント実在確認 → from_single_file 直呼び ------------
    def load(self, name: str) -> None:
        with self.lock:
            if self.pipeline is not None and self.model_name == name:
                return
            d = resolve_model_dir(name)
            if d is None:
                avail = self.list_available()
                hint = "、".join(avail) if avail else "なし"
                raise ValueError(
                    f"モデル '{name}' は未取得です（取得済み: {hint}）。ポータブル版で一度生成するか、"
                    f"設定から取得してください"
                )
            ckpt = _find_ckpt(d)
            self._ensure_turbo_compat()  # turbo（蒸留版）対応（スケジューラ＋guidance 埋め込み注入）
            from hy3dgen.shapegen import Hunyuan3DDiTFlowMatchingPipeline

            # 旧モデルを確実に解放してからロード。
            if self.pipeline is not None:
                self.pipeline = None
                self.model_name = None
                if self.device == "cuda":
                    self.torch.cuda.empty_cache()
            self.pipeline = Hunyuan3DDiTFlowMatchingPipeline.from_single_file(
                ckpt,
                os.path.join(d, "config.yaml"),
                device=self.device,
                dtype=self.torch.float16 if self.device == "cuda" else self.torch.float32,
            )
            self.model_name = name

    # --- ダウンロード（pull）。shape サブフォルダだけ取得。進捗を on_line(dict) で流す ---------
    def pull(self, name: str, on_line) -> None:
        spec = MODELS.get(name)
        if not spec:
            raise ValueError(f"未知のモデル名: {name}（既知: {', '.join(MODELS)}）")
        from huggingface_hub import snapshot_download
        from huggingface_hub.utils import tqdm as hf_tqdm

        on_line({"status": f"{name} のダウンロードを開始…", "percent": 0})

        # tqdm をフックして進捗を on_line へ（t2i_server と同方式・整数%変化時のみ通知）。
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

        snapshot_download(
            spec["repo"],
            allow_patterns=[f"{spec['subfolder']}/*", "config.json"],
            tqdm_class=_Tqdm,
        )
        if resolve_model_dir(name) is None:
            raise RuntimeError("ダウンロードしましたが shape モデルとして認識できません")
        on_line({"status": "完了", "percent": 100})

    # --- turbo 対応の互換パッチ ----------------------------------------------------------
    @classmethod
    def _ensure_turbo_compat(cls):
        """turbo（ステップ蒸留版）を WinPortable 同梱の古い hy3dgen で動かすための2点パッチ
        （いずれも upstream Tencent-Hunyuan/Hunyuan3D-2 main から移植・実機エラーで確認）:
        ① スケジューラ: config.yaml が要求する ConsistencyFlowMatchEulerDiscreteScheduler が
           無い（AttributeError）→ 同名クラスをモジュールへ注入。
        ② DiT 本体: guidance_embed（蒸留版のガイダンス埋め込み）非対応で、チェックポイントの
           guidance_in.* が Unexpected key(s) になる → __init__/forward をラップして
           guidance_in（MLPEmbedder）と埋め込み加算を追加。
        エンジン側が更新されて本物が入ったら hasattr / フラグ判定で何もしない。"""
        cls._ensure_consistency_scheduler()
        cls._ensure_guidance_embed()

    @staticmethod
    def _ensure_consistency_scheduler():
        import hy3dgen.shapegen.schedulers as sch

        if hasattr(sch, "ConsistencyFlowMatchEulerDiscreteScheduler"):
            return

        from dataclasses import dataclass
        from typing import List, Optional, Tuple, Union

        import numpy as np
        import torch
        from diffusers.configuration_utils import ConfigMixin, register_to_config
        from diffusers.schedulers.scheduling_utils import SchedulerMixin
        from diffusers.utils import BaseOutput

        @dataclass
        class ConsistencyFlowMatchEulerDiscreteSchedulerOutput(BaseOutput):
            prev_sample: torch.FloatTensor
            pred_original_sample: torch.FloatTensor

        class ConsistencyFlowMatchEulerDiscreteScheduler(SchedulerMixin, ConfigMixin):
            _compatibles: list = []
            order = 1

            @register_to_config
            def __init__(self, num_train_timesteps: int = 1000, pcm_timesteps: int = 50):
                sigmas = np.linspace(0, 1, num_train_timesteps)
                step_ratio = num_train_timesteps // pcm_timesteps
                euler_timesteps = (np.arange(1, pcm_timesteps) * step_ratio).round().astype(np.int64) - 1
                euler_timesteps = np.asarray([0] + euler_timesteps.tolist())
                self.euler_timesteps = euler_timesteps
                self.sigmas = sigmas[self.euler_timesteps]
                self.sigmas = torch.from_numpy((self.sigmas.copy())).to(dtype=torch.float32)
                self.timesteps = self.sigmas * num_train_timesteps
                self._step_index = None
                self._begin_index = None
                self.sigmas = self.sigmas.to("cpu")

            @property
            def step_index(self):
                return self._step_index

            @property
            def begin_index(self):
                return self._begin_index

            def set_begin_index(self, begin_index: int = 0):
                self._begin_index = begin_index

            def _sigma_to_t(self, sigma):
                return sigma * self.config.num_train_timesteps

            def set_timesteps(
                self,
                num_inference_steps: int = None,
                device=None,
                sigmas: "Optional[List[float]]" = None,
            ):
                self.num_inference_steps = (
                    num_inference_steps if num_inference_steps is not None else len(sigmas)
                )
                inference_indices = np.linspace(
                    0, self.config.pcm_timesteps, num=self.num_inference_steps, endpoint=False
                )
                inference_indices = np.floor(inference_indices).astype(np.int64)
                inference_indices = torch.from_numpy(inference_indices).long()
                self.sigmas_ = self.sigmas[inference_indices]
                timesteps = self.sigmas_ * self.config.num_train_timesteps
                self.timesteps = timesteps.to(device=device)
                self.sigmas_ = torch.cat(
                    [self.sigmas_, torch.ones(1, device=self.sigmas_.device)]
                )
                self._step_index = None
                self._begin_index = None

            def index_for_timestep(self, timestep, schedule_timesteps=None):
                if schedule_timesteps is None:
                    schedule_timesteps = self.timesteps
                indices = (schedule_timesteps == timestep).nonzero()
                pos = 1 if len(indices) > 1 else 0
                return indices[pos].item()

            def _init_step_index(self, timestep):
                if self.begin_index is None:
                    if isinstance(timestep, torch.Tensor):
                        timestep = timestep.to(self.timesteps.device)
                    self._step_index = self.index_for_timestep(timestep)
                else:
                    self._step_index = self._begin_index

            def step(
                self,
                model_output: torch.FloatTensor,
                timestep,
                sample: torch.FloatTensor,
                generator=None,
                return_dict: bool = True,
            ):
                if isinstance(timestep, (int, torch.IntTensor, torch.LongTensor)):
                    raise ValueError(
                        "Passing integer indices as timesteps to step() is not supported."
                    )
                if self.step_index is None:
                    self._init_step_index(timestep)
                sample = sample.to(torch.float32)
                sigma = self.sigmas_[self.step_index]
                sigma_next = self.sigmas_[self.step_index + 1]
                prev_sample = sample + (sigma_next - sigma) * model_output
                prev_sample = prev_sample.to(model_output.dtype)
                pred_original_sample = sample + (1.0 - sigma) * model_output
                pred_original_sample = pred_original_sample.to(model_output.dtype)
                self._step_index += 1
                if not return_dict:
                    return (prev_sample,)
                return ConsistencyFlowMatchEulerDiscreteSchedulerOutput(
                    prev_sample=prev_sample, pred_original_sample=pred_original_sample
                )

            def __len__(self):
                return self.config.num_train_timesteps

        sch.ConsistencyFlowMatchEulerDiscreteSchedulerOutput = (
            ConsistencyFlowMatchEulerDiscreteSchedulerOutput
        )
        sch.ConsistencyFlowMatchEulerDiscreteScheduler = ConsistencyFlowMatchEulerDiscreteScheduler

    @staticmethod
    def _ensure_guidance_embed():
        import torch
        import hy3dgen.shapegen.models.hunyuan3ddit as hdit

        DiT = hdit.Hunyuan3DDiT
        if getattr(DiT, "_texos_guidance_patch", False):
            return
        # エンジンが更新済み（本物が guidance_embed 対応）なら何もしない。
        import inspect

        if "guidance_embed" in inspect.signature(DiT.__init__).parameters:
            return

        orig_init = DiT.__init__
        orig_forward = DiT.forward

        def patched_init(self, *args, guidance_embed: bool = False, **kw):
            orig_init(self, *args, **kw)
            self.guidance_embed = guidance_embed
            if guidance_embed:
                # upstream: MLPEmbedder(in_dim=256, hidden_dim=hidden_size)。
                self.guidance_in = hdit.MLPEmbedder(in_dim=256, hidden_dim=self.hidden_size)

        def patched_forward(self, x, t, contexts, **kw):
            if not getattr(self, "guidance_embed", False):
                return orig_forward(self, x, t, contexts, **kw)
            # upstream の forward と同処理（time 埋め込みに guidance 埋め込みを加算）。
            cond = contexts["main"]
            latent = self.latent_in(x)
            vec = self.time_in(
                hdit.timestep_embedding(t, 256, self.time_factor).to(dtype=latent.dtype)
            )
            guidance = kw.get("guidance", None)
            if guidance is None:
                raise ValueError("Didn't get guidance strength for guidance distilled model.")
            vec = vec + self.guidance_in(
                hdit.timestep_embedding(guidance, 256, self.time_factor).to(dtype=latent.dtype)
            )
            cond = self.cond_in(cond)
            pe = None
            for block in self.double_blocks:
                latent, cond = block(img=latent, txt=cond, vec=vec, pe=pe)
            latent = torch.cat((cond, latent), 1)
            for block in self.single_blocks:
                latent = block(latent, vec=vec, pe=pe)
            latent = latent[:, cond.shape[1]:, ...]
            return self.final_layer(latent, vec)

        DiT.__init__ = patched_init
        DiT.forward = patched_forward
        DiT._texos_guidance_patch = True

    # --- テクスチャパイプライン（遅延ロード・VRAM 常駐）--------------------------------
    def _load_paint(self) -> None:
        if self.paint is not None:
            return
        dirs = resolve_paint_dirs()
        if dirs is None:
            raise ValueError(
                "テクスチャ用モデル（hunyuan3d-delight/paint-v2-0）が見つかりません。"
                "ポータブル版の HuggingFaceHub に同梱されているはずです（キャッシュの場所を確認してください）"
            )
        from hy3dgen.texgen import Hunyuan3DPaintPipeline
        from hy3dgen.texgen.pipelines import Hunyuan3DTexGenConfig

        cfg = Hunyuan3DTexGenConfig(dirs[0], dirs[1])
        cfg.device = self.device
        paint = Hunyuan3DPaintPipeline(cfg)
        # ポータブル版の texgen は mmgp オフロード前提で .to(device) がコメントアウトされて
        # いる（CPU に居座る）。常駐デーモンでは upstream 相当に明示的に移す。
        if self.device == "cuda":
            paint.models["delight_model"].pipeline.to(self.device)
            paint.models["multiview_model"].pipeline.to(self.device)
        self.paint = paint

    def _check_cancel(self) -> None:
        if self.cancel_event.is_set():
            raise _Cancelled()

    def _cancel_cb(self):
        """hy3dgen パイプラインの callback 用。/cancel が来たら例外で denoise ループを抜ける
        （t2i の pipe._interrupt 相当の機構が無いため例外方式。部分出力は生まれない）。"""
        ev = self.cancel_event

        def cb(step, timestep, outputs):
            if ev.is_set():
                raise _Cancelled()

        return cb

    # --- 生成（image → glb）。texture=true でテクスチャ付き（shape→リダクション→paint）----
    def generate(self, params: dict) -> bytes:
        img_b64 = params.get("image")
        if not img_b64:
            raise ValueError("入力画像がありません（image を base64 で指定してください）")
        from PIL import Image

        image = Image.open(io.BytesIO(base64.b64decode(img_b64)))

        # モデル決定: 明示指定 > ロード済み > 取得済みの先頭 > 既定。
        req = (params.get("model") or "").strip()
        if req:
            name = req
        elif self.model_name is not None:
            name = self.model_name
        else:
            avail = self.list_available()
            name = avail[0] if avail else DEFAULT_MODEL
        self.load(name)

        # 再現用乱数（未指定/空＝毎回ランダム）。
        seed = params.get("seed")
        generator = None
        if seed is not None and str(seed) != "":
            generator = self.torch.Generator(self.device).manual_seed(int(seed))

        texture = bool(params.get("texture"))
        if texture:
            # 先にロード可否を確かめる（shape 生成後に「モデルが無い」で無駄にしない）。
            self._load_paint()

        self.cancel_event.clear()  # 新しい生成の開始 = 前のキャンセルをリセット
        with self.lock:
            try:
                # 背景除去は常時適用（アルファ＝マスクとして shape/paint の条件付けに使われる）。
                image = self.rembg(image)
                with self.torch.inference_mode():
                    mesh = self.pipeline(
                        image=image,
                        num_inference_steps=int(params.get("steps") or 30),
                        guidance_scale=float(params.get("guidance") or 5.5),
                        octree_resolution=int(params.get("octree") or 256),
                        generator=generator,
                        mc_algo="mc",
                        callback=self._cancel_cb(),
                        callback_steps=1,
                        enable_pbar=False,
                    )[0]
                if texture:
                    # api_server.py と同手順: 掃除→面数削減→paint。paint 内部（delight 拡散・
                    # 6視点拡散・ベイク）は中断フックが無いため、cancel はステージ境界で判定。
                    from hy3dgen.shapegen import (
                        DegenerateFaceRemover,
                        FaceReducer,
                        FloaterRemover,
                    )

                    self._check_cancel()
                    mesh = FloaterRemover()(mesh)
                    mesh = DegenerateFaceRemover()(mesh)
                    mesh = FaceReducer()(
                        mesh, max_facenum=int(params.get("face_count") or 40000)
                    )
                    self._check_cancel()
                    mesh = self.paint(mesh, image)
            except _Cancelled:
                raise ValueError("中断されました")
            finally:
                if self.device == "cuda":
                    self.torch.cuda.empty_cache()
        # trimesh: glb はバイト列で直接エクスポートできる（一時ファイル不要）。
        data = mesh.export(file_type="glb")
        if not isinstance(data, (bytes, bytearray)):
            buf = io.BytesIO()
            mesh.export(buf, file_type="glb")
            data = buf.getvalue()
        return bytes(data)


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
                    "current_model": e.model_name,
                    "loaded": e.pipeline is not None,
                    "engine": e.engine_dir,
                    "available": e.list_available(),
                    "paint_available": resolve_paint_dirs() is not None,
                    "paint_loaded": e.paint is not None,
                })
            elif self.path == "/models":
                e = ENGINE
                self._json(200, {
                    "available": e.list_available(),
                    "known": sorted(MODELS.keys()),
                    "current": e.model_name,
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
                glb = ENGINE.generate(body)
                self.send_response(200)
                self.send_header("Content-Type", "model/gltf-binary")
                self.send_header("Content-Length", str(len(glb)))
                self.end_headers()
                self.wfile.write(glb)
            else:
                self._json(404, {"error": "not found"})
        except Exception as ex:
            self._json(500, {"error": str(ex), "trace": traceback.format_exc()})

    def _pull(self):
        """NDJSON 進捗ストリーム（t2i_server と同方式）。ダウンロードは worker スレッドへ、
        進捗は Queue 経由でこのハンドラスレッド1本だけが書く（chunked 交錯防止）。
        Connection: close の「本文＝接続が閉じるまで」方式。"""
        import queue as _queue

        body = self._read_json()
        name = body.get("name") or DEFAULT_MODEL
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
    ap.add_argument("--port", type=int, default=8766)
    ap.add_argument("--engine", required=True, help="Hunyuan3D-2 リポジトリのフォルダ（hy3dgen がある場所）")
    args = ap.parse_args()

    engine_dir = os.path.abspath(args.engine)
    if not os.path.isdir(os.path.join(engine_dir, "hy3dgen")):
        sys.stderr.write(f"engine に hy3dgen がありません: {engine_dir}\n")
        sys.exit(2)
    sys.path.insert(0, engine_dir)

    ENGINE = Engine(engine_dir)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    # 起動時の自己診断（server.log で「モデル未取得」系の不一致を切り分ける）。
    sys.stderr.write(
        f"t23d_server listening on 127.0.0.1:{args.port}\n"
        f"  engine={engine_dir}\n"
        f"  hf_cache={hf_cache_dir()}\n"
        f"  available={ENGINE.list_available()}\n"
    )
    sys.stderr.flush()
    server.serve_forever()


if __name__ == "__main__":
    main()
