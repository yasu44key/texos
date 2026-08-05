#!/usr/bin/env python
"""t23d エンジン（Hunyuan3D-2）互換チェック — 3D生成が落ちたときの一次切り分け。

TexoS の常駐デーモン（python/t23d_server.py）がエンジンに触る API 面を、**実際に入っている
エンジン**と突き合わせて一覧にする。エンジン（WinPortable / hy3dgen）を更新すると
モジュールの置き場所・引数・ロード経路が変わることがあり、3D生成が 500 で落ちる。
v1.7.4 の実機トラブルは3件ともこれだった:

  - `No module named 'hy3dgen.shapegen.models.hunyuan3ddit'` … DiT が models/denoisers/ へ移動
  - `Hunyuan3DTexGenConfig.__init__() missing ... 'subfolder_name'` … 引数が増えた
  - `Weights only load failed ... Unsupported operand 3` … safetensors を渡すのに
    use_safetensors を明示しないと torch.load(weights_only=True) に流れる

使い方（エンジン同梱の Python で実行する。TexoS 本体の Python ではない）:

    F:\\...\\python_standalone\\python.exe -s scripts\\t23d_engine_check.py
    ... --engine <Hunyuan3D-2 のフォルダ>   TexoS の設定を使わず明示指定する
    ... --weights                           重みファイルのキー構造も見る（ヘッダのみ）

終了コード: 0=食い違いなし / 1=食い違いあり（NG 行の「対処」を見る）。
"""

from __future__ import annotations

import argparse
import glob
import importlib
import inspect
import json
import os
import re
import subprocess
import sys

RESULTS: list[tuple[str, str, str, str]] = []  # (状態, 項目, 実測, 対処)


def rec(state: str, item: str, detail: str = "", hint: str = "") -> None:
    RESULTS.append((state, item, detail, hint))


def ok(item: str, detail: str = "") -> None:
    rec("OK", item, detail)


def ng(item: str, detail: str, hint: str) -> None:
    rec("NG", item, detail, hint)


def info(item: str, detail: str) -> None:
    rec("--", item, detail)


# --- エンジンの場所 ---------------------------------------------------------------


def config_path() -> str:
    base = os.environ.get("APPDATA") or os.path.expanduser("~")
    return os.path.join(base, "com.yoshi.texos", "config.json")


def engine_from_config() -> str | None:
    """TexoS の設定（config.t23d_engine）から解決する。Rust 側 t23d_resolve_engine と同判定。"""
    try:
        with open(config_path(), encoding="utf-8") as f:
            raw = (json.load(f).get("t23d_engine") or "").strip()
    except Exception:
        return None
    if not raw:
        return None
    if os.path.isdir(os.path.join(raw, "hy3dgen")):
        return raw
    inner = os.path.join(raw, "Hunyuan3D-2")
    if os.path.isdir(os.path.join(inner, "hy3dgen")):
        return inner
    return None


def hf_cache(engine: str) -> str:
    """ポータブル同梱の HuggingFaceHub（Rust が HF_HUB_CACHE で渡すのと同じ場所）。"""
    env = os.environ.get("HF_HUB_CACHE")
    if env:
        return env
    cand = os.path.join(os.path.dirname(engine), "HuggingFaceHub")
    if os.path.isdir(cand):
        return cand
    return os.path.join(os.path.expanduser("~"), ".cache", "huggingface", "hub")


# --- 環境の素性 -------------------------------------------------------------------


def report_environment(engine: str) -> None:
    info("エンジン", engine)
    try:
        out = subprocess.run(
            ["git", "-C", engine, "log", "-1", "--format=%h %ad %s", "--date=short"],
            capture_output=True, text=True, timeout=15,
        )
        if out.returncode == 0 and out.stdout.strip():
            info("エンジンの版(git)", out.stdout.strip())
    except Exception:
        pass
    try:
        src = open(os.path.join(engine, "setup.py"), encoding="utf-8").read()
        m = re.search(r'version\s*=\s*"([^"]+)"', src)
        if m:
            info("setup.py version", m.group(1))
    except Exception:
        pass
    info("Python", f"{sys.version.split()[0]} ({sys.executable})")
    try:
        import torch

        info("torch", f"{torch.__version__} / cuda={torch.cuda.is_available()}")
    except Exception as e:
        ng("torch", f"{type(e).__name__}: {e}", "エンジン同梱の python_standalone で実行しているか確認")
    info("HF_HUB_CACHE", hf_cache(engine))


def report_daemon_version() -> None:
    """リポジトリの t23d_server.py の版と、稼働中デーモンが名乗る版を突き合わせる。"""
    here = os.path.dirname(os.path.abspath(__file__))
    script = os.path.join(os.path.dirname(here), "python", "t23d_server.py")
    want = None
    try:
        m = re.search(r'SERVER_VERSION\s*=\s*"([^"]+)"', open(script, encoding="utf-8").read())
        want = m.group(1) if m else None
    except Exception:
        pass
    if want:
        info("t23d_server.py の版", want)
    try:
        import urllib.request

        with urllib.request.urlopen("http://127.0.0.1:8766/health", timeout=2) as r:
            live = json.load(r).get("version")
        if want and live and live != want:
            ng("稼働中デーモンの版", f"{live}（スクリプトは {want}）",
               "TexoS が次の生成で自動的に入れ替える。残るなら TexoS を再起動")
        else:
            info("稼働中デーモンの版", live or "(不明)")
    except Exception:
        info("稼働中デーモン", "起動していない（生成時に TexoS が起動する）")


# --- API 面の突き合わせ -------------------------------------------------------------


def check_api(engine: str) -> None:
    sys.path.insert(0, engine)

    # 1) 背景除去
    try:
        importlib.import_module("hy3dgen.rembg").BackgroundRemover
        ok("rembg.BackgroundRemover")
    except Exception as e:
        ng("rembg.BackgroundRemover", f"{type(e).__name__}: {e}",
           "hy3dgen/rembg.py の有無とクラス名を確認（デーモン Engine.rembg）")

    # 2) shape パイプライン（from_single_file の引数＝重みの読み方が変わる箇所）
    try:
        sg = importlib.import_module("hy3dgen.shapegen")
        sig = inspect.signature(sg.Hunyuan3DDiTFlowMatchingPipeline.from_single_file)
        has_ust = "use_safetensors" in sig.parameters
        ok("Hunyuan3DDiTFlowMatchingPipeline.from_single_file", f"{sig}")
        if has_ust:
            ok("use_safetensors 引数", "あり＝TexoS は拡張子に合わせて明示的に渡す")
        else:
            info("use_safetensors 引数", "なし＝旧エンジン（拡張子で判定する実装）")
        try:
            src = inspect.getsource(sg.Hunyuan3DDiTFlowMatchingPipeline.from_single_file)
            if "weights_only=True" in src and has_ust:
                info("重みの読み方",
                     "use_safetensors が偽なら torch.load(weights_only=True)＝safetensors を渡すなら真が必須")
        except Exception:
            pass
    except Exception as e:
        ng("Hunyuan3DDiTFlowMatchingPipeline", f"{type(e).__name__}: {e}",
           "hy3dgen/shapegen/__init__.py の公開名を確認（デーモン Engine.load）")

    # 3) 後処理（テクスチャ時の掃除・面数削減）
    try:
        sg = importlib.import_module("hy3dgen.shapegen")
        for n in ("FloaterRemover", "DegenerateFaceRemover", "FaceReducer"):
            getattr(sg, n)
        fr = inspect.signature(sg.FaceReducer.__call__)
        if "max_facenum" in fr.parameters:
            ok("後処理3種 + FaceReducer(max_facenum=)")
        else:
            ng("FaceReducer の引数", f"{fr}", "デーモンの face_count 渡しを新しい引数名に合わせる")
    except Exception as e:
        ng("後処理3種", f"{type(e).__name__}: {e}",
           "hy3dgen/shapegen/postprocessors.py のクラス名を確認")

    # 4) turbo 互換パッチ①: スケジューラ
    try:
        sch = importlib.import_module("hy3dgen.shapegen.schedulers")
        if hasattr(sch, "ConsistencyFlowMatchEulerDiscreteScheduler"):
            ok("ConsistencyFlowMatchEulerDiscreteScheduler", "本物あり＝互換パッチは不要（自動でスキップ）")
        else:
            info("ConsistencyFlowMatchEulerDiscreteScheduler", "なし＝デーモンが注入する（旧エンジン）")
    except Exception as e:
        ng("shapegen.schedulers", f"{type(e).__name__}: {e}",
           "モジュールの場所が変わった可能性。_ensure_consistency_scheduler の import を見直す")

    # 5) turbo 互換パッチ②: DiT の置き場所と guidance_embed
    found = None
    for mod in ("hy3dgen.shapegen.models.denoisers.hunyuan3ddit",  # 更新版
                "hy3dgen.shapegen.models.hunyuan3ddit"):           # 旧 WinPortable 同梱
        try:
            h = importlib.import_module(mod)
        except ModuleNotFoundError:
            continue
        if hasattr(h, "Hunyuan3DDiT"):
            found = (mod, h)
            break
    if found is None:
        info("Hunyuan3DDiT の場所", "新旧どちらのパスにも無し＝パッチはスキップされる（素の実装に任せる）")
    else:
        mod, h = found
        native = "guidance_embed" in inspect.signature(h.Hunyuan3DDiT.__init__).parameters
        ok("Hunyuan3DDiT の場所", f"{mod} / guidance_embed="
           + ("本物あり＝互換パッチ不要" if native else "なし＝デーモンが注入（旧エンジン）"))

    # 6) テクスチャ（texgen）— 引数の増減が起きた箇所
    try:
        tp = importlib.import_module("hy3dgen.texgen.pipelines")
        sig = inspect.signature(tp.Hunyuan3DTexGenConfig.__init__)
        ok("Hunyuan3DTexGenConfig", f"{sig}")
        if "subfolder_name" in sig.parameters:
            ok("subfolder_name 引数", "あり＝TexoS は paint サブフォルダ名を渡す")
        else:
            info("subfolder_name 引数", "なし＝旧エンジン（2引数で呼ぶ）")
        call = inspect.signature(tp.Hunyuan3DPaintPipeline.__call__)
        params = [p for p in call.parameters if p != "self"]
        if params[:2] == ["mesh", "image"]:
            ok("Hunyuan3DPaintPipeline.__call__", f"{call}")
        else:
            ng("Hunyuan3DPaintPipeline.__call__", f"{call}",
               "デーモンの self.paint(mesh, image) を新しい呼び出し形に合わせる")
        src = inspect.getsource(tp)
        missing = [k for k in ("delight_model", "multiview_model") if f"'{k}'" not in src and f'"{k}"' not in src]
        if missing:
            ng("paint.models のキー", f"見つからない: {', '.join(missing)}",
               "_load_paint の .to(device) が触るキー名を合わせる")
        else:
            ok("paint.models のキー", "delight_model / multiview_model")
        if hasattr(tp.Hunyuan3DPaintPipeline, "enable_model_cpu_offload"):
            info("cpu offload", "opt-in（TexoS は呼ばない＝明示的に .to(device) するのが正しい）")
    except Exception as e:
        ng("texgen", f"{type(e).__name__}: {e}",
           "hy3dgen/texgen/pipelines.py のクラス名・引数を確認（_load_paint）")


# --- 重み（モデル）側 ---------------------------------------------------------------

MODELS = {
    "hunyuan3d-2": "hunyuan3d-dit-v2-0",
    "hunyuan3d-2-turbo": "hunyuan3d-dit-v2-0-turbo",
}
CKPT_CANDIDATES = ("model.fp16.safetensors", "model.safetensors", "model.fp16.ckpt", "model.ckpt")
PAINT_SUBFOLDERS = ("hunyuan3d-delight-v2-0", "hunyuan3d-paint-v2-0")


def check_models(engine: str, deep: bool) -> None:
    cache = hf_cache(engine)
    repo = os.path.join(cache, "models--tencent--Hunyuan3D-2")
    snaps = sorted(glob.glob(os.path.join(repo, "snapshots", "*")))
    if not snaps:
        ng("モデルキャッシュ", f"スナップショットが無い: {repo}",
           "ポータブルの 2-download-models.bat か TexoS の取得で入れる")
        return
    for name, sub in MODELS.items():
        hit = None
        for s in snaps:
            d = os.path.join(s, sub)
            if os.path.isfile(os.path.join(d, "config.yaml")):
                picked = next((c for c in CKPT_CANDIDATES if os.path.isfile(os.path.join(d, c))), None)
                if picked:
                    hit = (d, picked)
                    break
        if hit is None:
            info(f"モデル {name}", "未取得（config.yaml＋重みが揃うスナップショットなし）")
            continue
        d, picked = hit
        ok(f"モデル {name}", f"{picked}（TexoS が選ぶファイル）")
        if deep:
            check_weight_keys(os.path.join(d, picked), name)
    dirs = None
    for s in snaps:
        cand = [os.path.join(s, x) for x in PAINT_SUBFOLDERS]
        if all(os.path.isfile(os.path.join(c, "model_index.json")) for c in cand):
            dirs = cand
            break
    if dirs is None:
        info("テクスチャ用モデル", "未取得（delight＋paint が揃うスナップショットなし）＝形状のみ利用可")
    else:
        ok("テクスチャ用モデル", f"{os.path.basename(dirs[0])} + {os.path.basename(dirs[1])}")


def check_weight_keys(path: str, name: str) -> None:
    """from_single_file(use_safetensors=True) が期待するキー構造か（ヘッダのみ読む）。"""
    if not path.endswith(".safetensors"):
        info(f"{name} のキー構造", "safetensors ではないので確認をスキップ")
        return
    try:
        from collections import Counter

        from safetensors import safe_open

        with safe_open(path, framework="pt", device="cpu") as f:
            keys = list(f.keys())
        top = Counter(k.split(".")[0] for k in keys)
        missing = [n for n in ("model", "vae") if n not in top]
        detail = f"{len(keys)}キー / {dict(top)}"
        if missing:
            ng(f"{name} のキー構造", detail + f" — 欠落: {', '.join(missing)}",
               "from_single_file の振り分け（key.split('.')[0]）と合っていない")
        else:
            ok(f"{name} のキー構造", detail)
    except Exception as e:
        ng(f"{name} のキー構造", f"{type(e).__name__}: {e}", "重みファイルが壊れている可能性")


# --- 出力 ---------------------------------------------------------------------------


def disp_width(s: str) -> int:
    """コンソール上の表示幅（全角は2）。日本語の項目名でも列が揃うようにする。"""
    import unicodedata

    return sum(2 if unicodedata.east_asian_width(c) in "WF" else 1 for c in s)


def main() -> int:
    ap = argparse.ArgumentParser(description="t23d エンジン互換チェック")
    ap.add_argument("--engine", help="Hunyuan3D-2 のフォルダ（省略時は TexoS の設定から解決）")
    ap.add_argument("--weights", action="store_true", help="重みのキー構造も見る（ヘッダのみ）")
    args = ap.parse_args()

    engine = args.engine or engine_from_config()
    if not engine or not os.path.isdir(os.path.join(engine, "hy3dgen")):
        print("エンジンが見つかりません。--engine で Hunyuan3D-2 のフォルダを指定してください")
        print(f"（TexoS の設定を見た場所: {config_path()}）")
        return 2

    report_environment(engine)
    report_daemon_version()
    check_api(engine)
    check_models(engine, args.weights)

    width = max(disp_width(r[1]) for r in RESULTS) + 2
    print()
    for state, item, detail, hint in RESULTS:
        pad = " " * max(1, width - disp_width(item))
        print(f"{state}  {item}{pad}{detail}")
        if hint:
            print(f"    → 対処: {hint}")
    bad = [r for r in RESULTS if r[0] == "NG"]
    print()
    if bad:
        print(f"食い違い {len(bad)} 件。上の「対処」に沿って python/t23d_server.py を合わせ、"
              f"SERVER_VERSION を上げてください（古い常駐デーモンの入れ替えに必要）。")
        return 1
    print("食い違いなし。エンジン側の API は t23d_server.py の想定どおりです。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
