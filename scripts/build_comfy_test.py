"""ComfyUI バックエンド（t2i backend:"comfyui"）の画像生成テスト用プロジェクトを組む。

    python scripts/build_comfy_test.py [出力フォルダ] [--link 親プロジェクト]

前提: ComfyUI（127.0.0.1:8188）に Qwen-Image 2.1 の t2i / 画像編集テンプレートのモデルが入っていること。
ワークフローは scripts/comfy_workflows/ の API 形式 JSON（ComfyUI の /history から採取したもの）。
差し込み口は TexoS の自動検出（src-tauri/src/comfy.rs detect_bindings）と同じ結果を書いてある。
設計: docs/ComfyUIバックエンド設計.md
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from texos_flow import Project  # noqa: E402

DEFAULT_OUT = r"X:\Yoshiki\TexoS\ComfyUI画像生成テスト"
SAMPLE_IMAGE = (r"E:\ComfyUI_windows_portable_nvidia\ComfyUI_windows_portable_New"
                r"\ComfyUI\input\gothic_cathedral_00003.png")


def load(name: str) -> dict:
    return json.loads((HERE / "comfy_workflows" / name).read_text(encoding="utf-8"))


T2I_WF = load("qwen_image_2_1_t2i_api.json")
EDIT_WF = load("qwen_image_2_1_edit_api.json")

# 自動検出と同じ口（プロンプトは「プロンプト強化」スイッチ off 側のリテラルへ）。
T2I_BIND = [
    {"slot": "prompt", "node": "459:474", "input": "on_false"},
    {"slot": "negative", "node": "459:452", "input": "negative_prompt"},
    {"slot": "seed", "node": "459:458", "input": "seed"},
    {"slot": "steps", "node": "459:458", "input": "steps"},
    {"slot": "guidance", "node": "459:458", "input": "cfg"},
]
EDIT_BIND = [
    {"slot": "prompt", "node": "459:484", "input": "on_false"},
    {"slot": "negative", "node": "459:474", "input": "negative_prompt"},
    {"slot": "seed", "node": "459:458", "input": "seed"},
    {"slot": "steps", "node": "459:458", "input": "steps"},
    {"slot": "guidance", "node": "459:458", "input": "cfg"},
    {"slot": "image", "node": "470", "input": "image", "index": 1},
]


def comfy(p: Project, label: str, x: float, y: float, wf: dict, bind: list, prefix: str, memo: str) -> str:
    return p.builtin(label, x, y, name="t2i", memo=memo,
                     backend="comfyui", comfy_workflow=wf, comfy_bindings=bind, comfy_output="461",
                     timeout_secs=900, accept_mime="*/*", output_ext="png", prefix=prefix)


def main() -> None:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    out = args[0] if args else DEFAULT_OUT
    p = Project(out)

    p.note("ComfyUI 画像生成テスト\n\n"
           "t2i カードの生成エンジンを ComfyUI（外部サーバ・127.0.0.1:8188）にしたもの。\n"
           "各行の右端のカードの ▶ を押す。1枚 約60〜80秒（Qwen-Image 2.1・1024px・25step）。\n"
           "カードをダブルクリック＝設定（接続テスト・ワークフロー・差し込み口）。\n"
           "設計: docs/ComfyUIバックエンド設計.md", 0, 0, width=420)

    # A: テキスト → 画像（Qwen-Image 2.1 t2i）
    pa = p.file("A_プロンプト.md",
                "A cozy reading nook by a rainy window at dusk, warm lamp light, a tabby cat "
                "sleeping on a knitted cushion, stacks of old books, detailed digital illustration",
                0, 260, memo="ここを書き換えて ▶")
    ta = comfy(p, "A 画像生成（ComfyUI・Qwen t2i）", 420, 260, T2I_WF, T2I_BIND, "A_qwen_t2i",
               "プロンプト → Qwen-Image 2.1 t2i。シード空欄＝毎回変わる")
    p.flow(pa, ta)
    p.shape(-20, 230, 760, 170, "A: テキスト → 画像")

    # B: 画像＋指示 → 画像（Qwen-Image 2.1 画像編集・入力画像 1 枚）
    ib = p.file("B_編集指示.md", EDIT_WF["459:484"]["inputs"]["on_false"], 0, 470,
                memo="編集の指示（英語が安定）")
    img = p.copy_file(SAMPLE_IMAGE, 0, 620, name="B_元画像.png", memo="編集する画像（差し替え可）")
    tb = comfy(p, "B 画像編集（ComfyUI・Qwen edit）", 420, 540, EDIT_WF, EDIT_BIND, "B_qwen_edit",
               "指示テキスト＋画像 → 編集結果。画像は矢印を作った順に image_1, image_2 …")
    p.flow(ib, tb)
    p.flow(img, tb)
    p.shape(-20, 440, 760, 380, "B: 画像＋指示 → 画像（フィギュア化）")

    # C: テキスト → 画像 → 編集（2 段・約2〜3分）
    pc = p.file("C_プロンプト.md",
                "Anime illustration of a girl in an elaborate black and red gothic lolita dress with lace "
                "and ribbons, sitting on an ornate antique wine-red velvet chair, full body, soft lighting",
                0, 880)
    gc = comfy(p, "C1 画像生成", 420, 880, T2I_WF, T2I_BIND, "C1_gen", "C の1段目（中間＝ファイルにならない）")
    ic = p.file("C_編集指示.md",
                "Turn the character in this image into a realistic 1/7 scale commercial PVC figure, keeping "
                "her outfit, hairstyle, face, pose and colors exactly the same. The figure stands on a round "
                "clear acrylic base on a wooden desk. Studio product photography, realistic photo.",
                420, 1060)
    ec = comfy(p, "C2 フィギュア化", 800, 960, EDIT_WF, EDIT_BIND, "C_figure",
               "C1 の画像を編集。▶ はこのカード（C1 も走る）")
    p.flow(pc, gc)
    p.flow(ic, ec)
    p.flow(gc, ec)
    p.shape(-20, 850, 1140, 360, "C: テキスト → 画像 → フィギュア化（2段）")

    p.view(x=40, y=40, scale=0.55)
    p.save()
    if "--link" in sys.argv:
        i = sys.argv.index("--link")
        parent = sys.argv[i + 1] if i + 1 < len(sys.argv) else None
        p.link_into(parent)


if __name__ == "__main__":
    main()
