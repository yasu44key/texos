# -*- coding: utf-8 -*-
"""カードカタログ用の見本データを作る（画像・音・動画・文書・3D・書庫）。

TexoS は**カードの上でそのまま中身が見える**形式が広い（画像／音声／動画／PDF／
Office／glb／テキスト）。その範囲を「事典」として見せるには、実物が1つずつ要る。
ここでは手元の道具で見本を作る:

  ffmpeg  … 画像の元・音・動画        magick  … 画像形式の変換
  pandoc  … Markdown → docx          soffice … docx → PDF / csv → xlsx
  blender … glb（3Dモデル）           python  … テキスト系・zip

道具が無い形式は**黙って飛ばす**（作れたものだけ返す）。生成結果はキャッシュするので、
2回目以降は速い。

    from make_sample_data import make_samples
    files = make_samples(r"C:\\...\\どこか")     # {"png": "...\\見本_画像.png", ...}
"""
import io
import os
import subprocess
import sys
import tempfile
import zipfile

BLENDER = next(
    (p for p in (
        r"C:\Program Files\Blender Foundation\Blender 4.5\blender.exe",
        r"C:\Program Files\Blender Foundation\Blender 4.4\blender.exe",
        r"C:\Program Files\Blender Foundation\Blender 5.0\blender.exe",
    ) if os.path.exists(p)), "")
SOFFICE = r"C:\Program Files\LibreOffice\program\soffice.exe"

SAMPLE_MD = """\
# 見本の文書

TexoS のカードカタログに入っている**見本データ**です。
この段落は Markdown で書かれ、pandoc で Word に、LibreOffice で PDF になりました。

## 表も入れておく

| 形式 | 用途 |
| --- | --- |
| docx | ワープロ |
| pdf  | 配布・印刷 |

- カードの上でそのまま読めます
- 実身なので、書き換えるとプロジェクトのファイルが変わります
"""

SAMPLE_CSV = "商品,個数,単価\nりんご,3,180\nみかん,12,60\nぶどう,1,980\n"
SAMPLE_TSV = "名前\t年\t所属\n佐藤\t34\t開発\n鈴木\t28\t営業\n"
SAMPLE_JSON = '{\n "題名": "見本のJSON",\n "配列": [1, 2, 3],\n "入れ子": {"可": true}\n}\n'
SAMPLE_XML = '<?xml version="1.0" encoding="UTF-8"?>\n<蔵書>\n  <本 id="1">\n' \
             '    <題名>見本のXML</題名>\n  </本>\n</蔵書>\n'
SAMPLE_HTML = '<h1>見本のHTML</h1>\n<p>TexoS はカードの上でそのまま表示します。</p>\n'
SAMPLE_SVG = (
    '<svg xmlns="http://www.w3.org/2000/svg" width="360" height="240" viewBox="0 0 360 240">\n'
    '  <rect width="360" height="240" fill="#1e3a5f"/>\n'
    '  <circle cx="180" cy="110" r="70" fill="#f0c419"/>\n'
    '  <text x="180" y="210" font-size="24" fill="#ffffff" text-anchor="middle">見本のSVG</text>\n'
    '</svg>\n')

BLENDER_SCRIPT = """
import bpy, sys, math
out = sys.argv[-1]
bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.mesh.primitive_ico_sphere_add(subdivisions=3, radius=1.0)
ob = bpy.context.active_object
ob.name = "見本"
bpy.ops.object.shade_smooth()
mat = bpy.data.materials.new("見本マテリアル")
mat.use_nodes = True
bsdf = mat.node_tree.nodes["Principled BSDF"]
bsdf.inputs["Base Color"].default_value = (0.85, 0.45, 0.15, 1.0)
# Hunyuan3D の glb は metallic 既定 1.0 で暗くなる。見本は 0 にしておく
bsdf.inputs["Metallic"].default_value = 0.0
bsdf.inputs["Roughness"].default_value = 0.45
ob.data.materials.append(mat)
bpy.ops.export_scene.gltf(filepath=out, export_format='GLB')
"""


def _run(args, **kw) -> bool:
    try:
        subprocess.run(args, check=True, capture_output=True, timeout=180, **kw)
        return True
    except (OSError, subprocess.SubprocessError):
        return False


def make_samples(dest: str | None = None, verbose: bool = True) -> dict[str, str]:
    d = dest or os.path.join(tempfile.gettempdir(), "texos_sample_data")
    os.makedirs(d, exist_ok=True)
    out: dict[str, str] = {}

    def path(ext, stem="見本"):
        return os.path.join(d, f"{stem}.{ext}")

    def keep(ext, stem="見本"):
        p = path(ext, stem)
        if os.path.exists(p) and os.path.getsize(p):
            out[ext] = p
            return True
        return False

    def text(ext, body, stem="見本"):
        if not keep(ext, stem):
            io.open(path(ext, stem), "w", encoding="utf-8", newline="\n").write(body)
            keep(ext, stem)

    # --- テキスト系（道具不要） ---
    text("md", SAMPLE_MD)
    text("txt", "見本のプレーンテキストです。\n2行目。\n")
    text("csv", SAMPLE_CSV)
    text("tsv", SAMPLE_TSV)
    text("json", SAMPLE_JSON)
    text("xml", SAMPLE_XML)
    text("html", SAMPLE_HTML)
    text("svg", SAMPLE_SVG)
    text("py", "for i in range(5):\n    print(i, i * i)\n")

    # --- 画像（ffmpeg で元を作り、magick で他形式へ） ---
    if not keep("png"):
        _run(["ffmpeg.exe", "-hide_banner", "-loglevel", "error", "-y",
              "-f", "lavfi", "-i", "testsrc=size=480x360:rate=1:duration=1",
              "-frames:v", "1", path("png")])
        keep("png")
    if "png" in out:
        for ext, args in (("jpg", []), ("webp", []), ("gif", []), ("bmp", [])):
            if not keep(ext):
                _run(["magick.exe", out["png"], *args, path(ext)])
                keep(ext)

    # --- 音（ffmpeg） ---
    for ext, args in (("wav", ["-ar", "16000", "-ac", "1"]), ("mp3", []),
                      ("flac", []), ("ogg", [])):
        if not keep(ext):
            _run(["ffmpeg.exe", "-hide_banner", "-loglevel", "error", "-y",
                  "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                  *args, path(ext)])
            keep(ext)

    # --- 動画（ffmpeg） ---
    for ext, args in (("mp4", ["-pix_fmt", "yuv420p"]),
                      ("webm", ["-c:v", "libvpx-vp9", "-b:v", "200k"])):
        if not keep(ext):
            _run(["ffmpeg.exe", "-hide_banner", "-loglevel", "error", "-y",
                  "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=2",
                  *args, path(ext)])
            keep(ext)

    # --- Word（pandoc: md → docx） ---
    if not keep("docx") and "md" in out:
        _run(["pandoc.exe", out["md"], "-o", path("docx")])
        keep("docx")

    # --- PDF / Excel（LibreOffice） ---
    if os.path.exists(SOFFICE):
        prof = "-env:UserInstallation=file:///" + \
               os.path.join(d, "_lo_profile").replace("\\", "/")
        if not keep("pdf") and "docx" in out:
            _run([SOFFICE, prof, "--headless", "--convert-to", "pdf",
                  "--outdir", d, out["docx"]])
            keep("pdf")
        if not keep("xlsx") and "csv" in out:
            _run([SOFFICE, prof, "--headless", "--convert-to", "xlsx",
                  "--outdir", d, out["csv"]])
            keep("xlsx")
        if not keep("odt") and "md" in out and "docx" in out:
            _run([SOFFICE, prof, "--headless", "--convert-to", "odt",
                  "--outdir", d, out["docx"]])
            keep("odt")

    # --- 3D（Blender: glb） ---
    if not keep("glb") and BLENDER:
        script = os.path.join(d, "_make_glb.py")
        io.open(script, "w", encoding="utf-8").write(BLENDER_SCRIPT)
        _run([BLENDER, "--background", "--factory-startup",
              "--python", script, "--", path("glb")])
        keep("glb")

    # --- 書庫（zip） ---
    if not keep("zip"):
        with zipfile.ZipFile(path("zip"), "w", zipfile.ZIP_DEFLATED) as z:
            for ext in ("md", "csv", "png"):
                if ext in out:
                    z.write(out[ext], os.path.basename(out[ext]))
        keep("zip")

    if verbose:
        got = ", ".join(sorted(out))
        print(f"見本データ {len(out)} 形式 → {d}")
        print(f"  {got}")
    return out


if __name__ == "__main__":
    make_samples(sys.argv[1] if len(sys.argv) > 1 else None)
