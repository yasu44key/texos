# -*- coding: utf-8 -*-
"""カードカタログ（事典）を組む — カードの種類を1枚ずつ「押せる標本」として並べる。

**手書きしない。** 内蔵カードの一覧・項目名・既定値は、UI が実際に使っている
`src/ui/BuiltinFilterDialog.tsx` の `BUILTIN_SCHEMAS` から node で抜き出して使う。
だから TexoS 側にカードが増えれば、このスクリプトを流し直すだけで事典が追いつく
（手書きの事典は次のリリースで嘘になる —— それが一番避けたいこと）。

出来るもの: `F:\\yoshi\\Documents\\TexoS\\カードカタログ`

  - 種類ごとに **[サンプル入力の付箋] → [そのカード]** の2枚組。**▶ を押せば挙動が見られる。**
  - 分類ごとに図形でグループ化し、説明の付箋を1枚。
  - `カード事典.md` … 同じ内容の一覧表（AIエージェントが読む用。カードとしても置いてある）。

    python scripts/build_card_catalog.py
"""
import io
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from make_sample_data import make_samples  # noqa: E402
from texos_flow import MIME_BY_EXT, Project  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOT = r"F:\yoshi\Documents\TexoS\カードカタログ"
MODEL = "gemma4:latest"          # §2-1: この環境の ollama list で確認したもの


def load_schemas() -> dict:
    """BuiltinFilterDialog.tsx の BUILTIN_SCHEMAS を node に評価させて取り出す。"""
    src = io.open(os.path.join(REPO, "src", "ui", "BuiltinFilterDialog.tsx"),
                  encoding="utf-8").read().splitlines(True)
    start = next(i for i, l in enumerate(src) if l.startswith("export const BUILTIN_SCHEMAS"))
    end = next(i for i, l in enumerate(src) if i > start and l.startswith("};"))
    body = "".join(src[start:end + 1]).replace(
        "export const BUILTIN_SCHEMAS: Record<string, BuiltinSchema> =", "const S =")
    tmp = os.path.join(os.environ.get("TEMP", "."), "_texos_schemas.mjs")
    io.open(tmp, "w", encoding="utf-8").write(
        body + "\nprocess.stdout.write(JSON.stringify(S));\n")
    r = subprocess.run(["node", tmp], capture_output=True, text=True, encoding="utf-8")
    if r.returncode:
        raise SystemExit("BUILTIN_SCHEMAS を読めませんでした:\n" + r.stderr[:500])
    return json.loads(r.stdout)


# ---------------------------------------------------------------- 標本の材料
# (分類, 見出し, サンプル入力（None＝入力なし）, 追加パラメータ, 動かすのに要るもの)
SAMPLE_LINES = "2026-08-19 INFO  起動しました\n2026-08-20 ERROR ディスクが一杯です\n2026-08-20 INFO  再試行\n2026-08-21 ERROR 接続できません\n"
SAMPLE_TSV = "名前\t年\t所属\n佐藤\t34\t開発\n鈴木\t28\t営業\n"
SAMPLE_MD = "説明の文です。\n\n```python\nprint('hello')\n```\n\n以上。\n"

EXHIBITS = {
    # --- テキスト（1入力・純関数） ---
    "text_grep":    ("テキスト", SAMPLE_LINES, {"pattern": "ERROR"}, None),
    "text_replace": ("テキスト", SAMPLE_LINES, {"pattern": "ERROR", "replace": "重大"}, None),
    "text_head":    ("テキスト", SAMPLE_LINES, {"lines": 2}, None),
    "text_tail":    ("テキスト", SAMPLE_LINES, {"lines": 2}, None),
    "text_sort":    ("テキスト", "みかん\nりんご\nばなな\nりんご\n", {"unique": "remove"}, None),
    "text_count":   ("テキスト", SAMPLE_LINES, {"mode": "all"}, None),
    "text_cut":     ("テキスト", SAMPLE_TSV, {"delimiter": "tab", "fields": "1,3"}, None),
    "text_convert": ("テキスト", "文字コードを変換します\n", {}, None),
    "md_codeblock": ("テキスト", SAMPLE_MD, {"lang": "python"}, None),
    # --- 集約（複数入力を1回で受ける） ---
    "text_concat":  ("集約", ["1本目：前提\n", "2本目：材料\n", "3本目：問い\n"],
                     {"separator": "rule"}, None),
    "text_diff":    ("集約", ["赤\n青\n黄\n", "赤\n緑\n黄\n"], {"lines": 3}, None),
    "text_paste":   ("集約", ["佐藤\n鈴木\n", "34\n28\n"], {"delimiter": "tab"}, None),
    # --- 生成 ---
    "t23d":         ("生成", None, {}, "Hunyuan3D-2 常駐デーモン＋GPU"),
    # --- 記法・制御 ---
    "tap":          ("記法", "この中身が固定名で残る\n", {"prefix": "記録の例"}, None),
    "ask":          ("記法", "この案でよいですか\n", {"question": "この案でよいですか",
                                                     "skip_upstream": "on"}, "人の答え"),
    "loop":         ("記法", "練り直す対象\n", {"max_rounds": 3}, None),
    "select":       ("記法", None, {}, None),
    # --- 口 ---
    "op_in":        ("口", "口から入る中身\n", {}, None),
    "op_out":       ("口", None, {}, None),
    # --- 自動発行（pull ではない） ---
    "op_trigger":   ("自動発行", None, {}, None),
    "op_timer":     ("自動発行", None, {"every": 300, "min_interval": 5, "max_fires": 100}, None),
    "op_watch":     ("自動発行", None, {"path": ROOT, "debounce": 2,
                                        "min_interval": 5, "max_fires": 10}, None),
    # --- 情報収集 ---
    "web_fetch":    ("情報収集", None, {"url": "https://example.com/"}, "ネット接続"),
    "web_feed":     ("情報収集", None, {"url": "https://example.com/feed"}, "ネット接続"),
    "web_capture":  ("情報収集", None, {"url": "https://example.com/"}, "ネット接続"),
    "web_search":   ("情報収集", None, {"query": "TexoS"}, "ネット接続・SearXNG 導入済み"),
    "to_url":       ("情報収集", "https://example.com/a\nhttps://example.com/b\n", {}, None),
    # --- 画像 ---
    "img_resize":   ("画像", None, {"width": 800}, "画像カードを繋ぐ"),
    "img_convert":  ("画像", None, {"format": "png"}, "画像カードを繋ぐ"),
    # --- OS ---
    "speak":        ("OS機能", "読み上げの試験です\n", {}, "OS の読み上げ"),
    "import_new":   ("OS機能", None, {"path": ROOT}, None),
}

ORDER = ["テキスト", "集約", "生成", "記法", "口", "自動発行", "情報収集", "画像", "OS機能", "外部"]

CATEGORY_NOTE = {
    "テキスト": "入力1本を受けて1本返す純関数のカード。\n複数繋ぐと**入力ごとに別々に実行**される（ファンアウト・§3-1）。",
    "集約": "**全入力を1回でまとめて受ける**のはこの3枚だけ。\n入力の並び順は矢印を繋いだ順（§3-3）。AIに複数の資料を読ませるときは必ず前に挟む。",
    "生成": "入力から新しい実体を作るカード。AI（text→text）・t2i（text→画像）・t23d（画像→3D）。\nどれも常駐サーバーやモデルが要る（§2-1）。",
    "記法": "流れそのものを書くためのカード（v1.8）。\n記録＝途中を残す／問い合わせ＝人に聞く／反復＝練り直す／セレクタ＝選ぶ。",
    "口": "素通しの札。IN＝入口・OUT＝出口。**OUT は ▶ を押せる終端になれる**が IN はなれない。\n人が書き込む場所が要るなら付箋か実身を使うこと（§3-15）。",
    "自動発行": "▶ の代わりに下流を走らせる仕掛け。**pull の評価には出てこない**ので、フローの矢印では繋がない。\n作った直後は一時停止。配線後にカードの「▶ 稼働開始」で動かす。",
    "情報収集": "外から材料を取ってくるカード（v1.7）。URL/フィード/ページ保存と、テキスト中のURLをカードに変える to_url。",
    "画像": "画像を変換するカード。入力には画像の実身カードを繋ぐ（text/* は受けない・§3-7）。",
    "OS機能": "OS の機能をそのまま借りるカード。読み上げは出力カードを作らない（呼ぶだけで完結）。",
    "外部": "内蔵に無いものは**外部プログラムカード**で呼ぶ。`{in}`/`{out}` を引数に書く。\n**名前は拡張子まで**（`python` ではなく `python.exe`・§3-13）。",
}


# ---------------------------------------------------------------- データの見本
# TexoS は「カードの上でそのまま中身が見える」形式が広い。その範囲そのものが事典になる。
# 判定は src/App.tsx のプレビュー分岐を写したもの（あちらが変わったらここも直す）。
DOC_PREVIEW_EXTS = {"docx", "xlsx", "xls", "ods", "csv", "tsv",
                    "pptx", "ppt", "doc", "odt", "odp"}


def previewable(ext: str, mime: str) -> str:
    if mime.startswith(("image/", "audio/", "video/")):
        return "カードの上で見える（表示・再生）"
    if ext == "pdf" or mime == "application/pdf":
        return "カードの上で見える（PDFビューア）"
    if ext in DOC_PREVIEW_EXTS:
        return "カードの上で見える（Officeプレビュー）"
    if ext in ("glb", "gltf") or mime.startswith("model/gltf"):
        return "カードの上で見える（3Dビューア・回せる）"
    if mime.startswith("text/") or mime in ("application/json", "application/xml"):
        return "カードの上で見える（そのまま編集できる）"
    return "カードの上では見えない（ダブルクリックで既定のアプリが開く）"


def accepts(mime: str) -> str:
    if mime.startswith("text/") or mime == "application/json":
        return "text/* のカード全部（AI・grep・連結…）"
    if mime.startswith("image/"):
        return "image/* のカード（リサイズ・形式変換・3D生成）"
    if mime.startswith("audio/"):
        return "audio/* のカード（ffmpeg・文字起こし）"
    if mime.startswith("video/"):
        return "video/* のカード（ffmpeg）"
    return "*/* を受けるカードだけ（外部プログラムなど）"


DATA_GROUPS = [
    ("画像", ["png", "jpg", "webp", "gif", "bmp", "svg"],
     "image/* のカードに繋がる。SVG だけはテキストでもあるので、\n中身を書き換えて絵を変えられる。"),
    ("音", ["wav", "mp3", "flac", "ogg"],
     "audio/* のカードに繋がる。文字起こしは 16kHz mono WAV に\n揃えてから渡すのが定石（レシピ集）。"),
    ("動画", ["mp4", "webm"],
     "video/* のカードに繋がる。切り出し・GIF化は ffmpeg の\n外部プログラムカードで。"),
    ("文書", ["md", "txt", "html", "pdf", "docx", "odt"],
     "Markdown・テキスト・HTML は text/* なので AI にそのまま渡せる。\n"
     "**PDF と docx は text/* ではない**ので、AI に読ませたいなら\npandoc で変換してから（§3-7）。"),
    ("表・構造", ["csv", "tsv", "json", "xml", "xlsx"],
     "csv/tsv/json は text/* のカードが受ける（json は特例）。\n"
     "**xml は application/xml なので text/* に通らない**。\n"
     "xlsx は表として見えるが、流すなら csv に落とすこと。"),
    ("3D・その他", ["glb", "py", "zip"],
     "glb はカードの上で回せる（t23d の出力もこれ）。zip のように\n"
     "見えない形式はダブルクリックで既定のアプリに渡る＝\nTexoS が抱え込まない統合。"),
]


def build_data_section(p: Project, doc: list, y: float) -> float:
    """見本データを実身カードとして並べる。メモに MIME と「どこへ繋がるか」を書く。"""
    files = make_samples()
    if not files:
        return y
    p.note(
        "■ データの見本 — TexoS が扱える形式\n\n"
        "**カードをダブルクリックすると中身が出ます。**画像・音・動画・PDF・Office・\n"
        "3D(glb)・テキストは面の上でそのまま見えます。見えない形式は既定のアプリへ。\n\n"
        "大事なのは MIME です。矢印は accept_mime で検査され、**合わない入力は\n"
        "エラーにならず黙って飛ばされます**（§3-7）。各カードのメモに MIME と\n"
        "「どのカードに繋がるか」を書いてあります。",
        60, y + 40, width=520, title_size=15)
    doc += ["## データ形式", "",
            "TexoS が扱えるデータの見本（カードカタログに実物が入っている）。",
            "矢印は `accept_mime` で検査され、**合わない入力は黙って飛ばされる**（§3-7）ので、",
            "MIME を見てから繋ぐこと。", ""]

    top = row = y
    for gname, exts, note in DATA_GROUPS:
        have = [e for e in exts if e in files]
        if not have:
            continue
        p.note(f"● {gname}\n\n{note}", 620, row, width=330)
        doc += [f"### {gname}", "", note.replace("\n", ""), ""]
        for i, ext in enumerate(have):
            mime = MIME_BY_EXT.get(ext, "application/octet-stream")
            p.copy_file(files[ext], 1010 + (i % 3) * 250, row + (i // 3) * 150,
                        name=f"見本.{ext}",
                        memo=f"MIME: {mime}\n{previewable(ext, mime)}\n"
                             f"繋げるカード: {accepts(mime)}")
            doc.append(f"- **.{ext}** — `{mime}` ／ {previewable(ext, mime)}"
                       f" ／ 繋げるカード: {accepts(mime)}")
        doc.append("")
        row += max(200, ((len(have) + 2) // 3) * 150 + 70)
    p.shape(40, top - 30, 1740, row - top + 40, "データの見本")
    return row + 80


def main() -> int:
    sch = load_schemas()
    p = Project(ROOT, model=MODEL)

    p.note(
        "カードカタログ — TexoS のカードを1枚ずつ並べた事典\n\n"
        "・左の付箋がサンプル入力、右がそのカード。**カードの ▶ を押せば挙動が見られます。**\n"
        "・カードの設定（キーと既定値）は右クリック →「設定」で見られます。\n"
        "・一覧表は「カード事典.md」に入っています（AIエージェントはこちらを読みます）。\n\n"
        "この面は scripts/build_card_catalog.py が UI のスキーマ表から生成しています。\n"
        "カードが増えたら組み直してください（手書きしないこと）。",
        60, -320, width=620, title_size=18,
    )

    doc = ["# カード事典（TexoS）", "",
           "内蔵カードの一覧。`src/ui/BuiltinFilterDialog.tsx` の `BUILTIN_SCHEMAS` から生成。",
           "フローの組み方は `docs/AIエージェント向け_フロー構築規約.md` を見ること。", ""]

    made: dict[str, str] = {}
    y = 0
    for cat in ORDER:
        names = [n for n, v in EXHIBITS.items() if v[0] == cat]
        if cat == "外部":
            names = []
        top = y
        p.note(f"■ {cat}\n\n{CATEGORY_NOTE[cat]}", 60, y + 40, width=340)
        doc += [f"## {cat}", "", CATEGORY_NOTE[cat].replace("\n", " "), ""]

        row = y
        for name in names:
            meta = sch.get(name, {})
            cat_, sample, params, needs = EXHIBITS[name]
            title = meta.get("title", name)
            fixed = dict(meta.get("fixed") or {})
            is_op = bool(meta.get("opCard"))
            fields = meta.get("fields") or []

            # 設定値: 既定値 → 標本用の上書き
            spec = {}
            for f in fields:
                if f.get("default") is not None:
                    spec[f["key"]] = f["default"]
            spec.update(params)

            keys = "／".join(
                f"{f['key']}（{f['label']}"
                + (f"・既定 {f['default']}" if f.get("default") is not None else "")
                + "）" for f in fields) or "設定項目なし"
            memo = f"{title}\nキー: {keys}"
            if needs:
                memo += f"\n要るもの: {needs}"

            # 押したときの出力名を決めておく（既定の「入力名_フィルタ名」だと読みにくい）
            if not is_op and "prefix" not in spec:
                spec["prefix"] = f"例_{name}"

            # カード本体
            if is_op:
                card = p.op(title, 900, row, kind=fixed.get("op", "in"),
                            memo=memo, **{k: v for k, v in spec.items() if k != "op"})
            else:
                card = p.builtin(title, 900, row, name=name, memo=memo,
                                 **{**fixed, **spec})

            # サンプル入力
            if isinstance(sample, list):
                for i, t in enumerate(sample):
                    n = p.note(f"入力{i + 1}\n{t}", 480, row + i * 110, width=260)
                    p.flow(n, card)
            elif sample is not None:
                n = p.note(f"サンプル入力（書き換えて ▶）\n\n{sample}", 480, row, width=280)
                p.flow(n, card)

            made[name] = card
            doc.append(f"- **{name}** — {title}")
            doc.append(f"  - キー: {keys}")
            if needs:
                doc.append(f"  - 要るもの: {needs}")
            row += (len(sample) * 110 if isinstance(sample, list) else 0) + 210

        # 口の標本は「付箋 → IN → OUT」で1本にする。IN は終端になれない（規約 §3-15）ので、
        # 並べただけだとビルダーが弾く —— カタログ自身がその規約の実例になっている。
        if cat == "口" and "op_in" in made and "op_out" in made:
            p.flow(made["op_in"], made["op_out"])
        if names:
            p.shape(40, top + 10, 1300, row - top, cat)
        y = row + 80
        doc.append("")

    # --- スキーマ表に載らないカード（専用ダイアログ／内蔵ではない） ---
    p.note(
        "■ 外部・専用ダイアログのカード\n\n" + CATEGORY_NOTE["外部"],
        60, y + 40, width=340)
    ai_in = p.note("サンプル入力（書き換えて ▶）\n\n一文で自己紹介してください。", 480, y, width=280)
    ai = p.ai("AI（builtin:\"ai\"）", 900, y, prefix="AIの出力",
              system="簡潔に答えてください。",
              memo="キー: provider（ollama/lmstudio/openai）／model（必須）／system／temperature／"
                   "endpoint（既定 ollama=11434, openai互換=1234/v1）\n"
                   "専用ダイアログ。**入力ごとにファンアウトする**ので複数の材料は連結で束ねる")
    p.flow(ai_in, ai)

    t2i_in = p.note("サンプル入力（書き換えて ▶）\n\na cat sitting on a bookshelf, soft light",
                    480, y + 230, width=280)
    t2i = p.builtin("画像生成（builtin:\"t2i\"）", 900, y + 230, name="t2i",
                    accept_mime="text/*", prefix="生成画像",
                    memo="キー: model（sd15/sdxl/flux等）／steps／width／height／seed／lora\n"
                         "専用ダイアログ。常駐 diffusers デーモンと GPU が要る")
    p.flow(t2i_in, t2i)

    prog_in = p.note("サンプル入力（書き換えて ▶）\n\nprint('外部プログラムから', 1 + 2)",
                     480, y + 460, width=280)
    prog = p.program("外部プログラム（例: python.exe）", 900, y + 460,
                     name="python.exe", cmd='"{in}" > "{out}"',
                     output_ext="txt", accept_mime="text/*", prefix="実行結果",
                     memo="キー: name（**拡張子まで**）／path／cmd（{in} {in*} {out} {out_noext} {out*}）"
                          "／type（command/filter/generator）／output_ext／accept_mime／prefix\n"
                          "{in*} か {in2} 以降を書くと集約、{in}/{in1} だけならファンアウト")
    p.flow(prog_in, prog)

    launcher = p.launcher("ランチャー（アプリをそのまま起動）", 900, y + 690,
                          name="notepad.exe",
                          memo="対話的なアプリを起動するだけのカード。作業フォルダはプロジェクト直下、"
                               "環境変数 TEXOS_PROJECT_DIR にもその場所が入る")
    p.shape(40, y + 10, 1300, 840, "外部・専用ダイアログ")

    doc += ["## 外部・専用ダイアログ", "",
            "- **ai** — AI（transform）。provider / **model（必須）** / system / temperature / endpoint。"
            "入力ごとにファンアウトするので、複数の材料は `text_concat` で束ねる。",
            "- **t2i** — 画像生成。常駐デーモン＋GPU。",
            "- **t23d** — 画像→3D。常駐デーモン＋GPU。",
            "- **外部プログラム** — `name`（**拡張子まで**）/ `cmd`（`{in}` `{in*}` `{out}` "
            "`{out_noext}` `{out*}`）/ `type` / `output_ext` / `accept_mime` / `prefix`。",
            "- **ランチャー** — 対話的なアプリを起動するだけ（入出力を持たない）。", ""]

    y2 = build_data_section(p, doc, y + 950)

    p.file("カード事典.md", "\n".join(doc), 60, y2,
           memo="この面と同じ内容の一覧。AIエージェントはこれを読めばソースを読まなくてよい")

    p.view(0, -280, 0.4)
    p.save(reset=True)
    p.link_into(os.path.dirname(ROOT))
    return 0


if __name__ == "__main__":
    sys.exit(main())
