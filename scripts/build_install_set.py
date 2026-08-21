# -*- coding: utf-8 -*-
"""配布用の「インストールセット」を1つのフォルダにまとめる。

このフォルダごとコピーして渡せば、相手の PC に TexoS 一式が移せる:

  インストールセット/
    はじめにお読みください.md
    texos_<版>_x64-setup.exe          … インストーラ（NSIS）
    ドキュメント/                      … 道具の入手先・AI向け規約・チップス・更新履歴
    サンプルプロジェクト/               … そのまま開けるプロジェクト（カードカタログ・レシピ集ほか）
    ツール/                            … 引っ越しスクリプトと、AI にフローを組ませる足場

**プロジェクトの中の絶対パス**が唯一の移植の壁。カードの中身は `.texos/cas/` に
控えがあるので別 PC でも読めるが、実ファイルへのリンク（committed_path）は
元の場所を指したままになる。`ツール\\プロジェクトの引っ越し.py` を1回流すと、
今ある場所に合わせて張り直す。

    python scripts/build_install_set.py
    python scripts/build_install_set.py --dest <別の場所>
"""
import argparse
import io
import json
import os
import shutil
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROJECT_SRC = r"F:\yoshi\Documents\TexoS"

# 配布に載せる文書（元のファイル名 → 配布時の名前）
DOCS = [
    ("外部依存プログラム一覧.md", "道具のインストールガイド.md"),
    ("AIエージェント向け_フロー構築規約.md", "AIエージェント向け_フロー構築規約.md"),
    ("カード化プログラム作成規約.md", "カード化プログラム作成規約.md"),
    ("ユーザーズチップス.md", "ユーザーズチップス.md"),
    ("更新履歴.md", "更新履歴.md"),
]

# 配布に載せるプロジェクト（フォルダ名, 一言）
PROJECTS = [
    ("カードカタログ", "カードの種類を1枚ずつ並べた事典。データの見本26形式つき"),
    ("レシピ集", "目的別の実例18本。見本の素材を同梱してあるので押せば動く"),
    ("サンプル_簡易DTM", "曲づくりの手本。プログラム（ChillChordMaker / wavmix）を同梱した形の実例"),
    ("サンプル_人の判断を挟む", "AI の下書きを人が読んで決める形"),
    ("カード実験_反復", "納得するまで練り直す（反復カード）"),
    ("カード実験_多言語実行", "C / Java / Python のソースと実行結果を並べる"),
]

TOOLS = ["texos_flow.py", "texos_dryrun.py"]

MIGRATE = '''# -*- coding: utf-8 -*-
"""サンプルプロジェクトのパスを、今ある場所に合わせて張り直す（引っ越し）。

TexoS のプロジェクトは、カードが実ファイルを**絶対パス**で指している。
別の PC やドライブへコピーすると、そのパスは元の場所を指したままになる。
（カードの中身は .texos/cas/ に控えがあるので**読むだけなら問題ない**。
  書き換えたりフローを流したりするなら、これを1回流しておく。）

使い方 — このファイルをダブルクリックするか、

    python プロジェクトの引っ越し.py            隣のサンプルプロジェクトを全部直す
    python プロジェクトの引っ越し.py --確認だけ   何を変えるか表示するだけ

TexoS を終了した状態で実行すること（開いていると反映されない）。
"""
import json
import os
import sqlite3
import sys

DRY = "--確認だけ" in sys.argv or "--dry-run" in sys.argv


def migrate(root: str) -> int:
    db = os.path.join(root, ".texos", "texos.db")
    if not os.path.exists(db):
        return 0
    conn = sqlite3.connect(db)
    changed = 0

    root_l = os.path.normcase(os.path.abspath(root))

    def inside(path: str) -> bool:
        return os.path.normcase(os.path.abspath(path)).startswith(root_l + os.sep)

    def fix_committed(path: str | None) -> str | None:
        """実身は必ずプロジェクトの中にある。外を指していたら引っ越し前のパス。

        「存在しないパスだけ直す」では駄目 —— **元の PC では古いパスがまだ実在する**ので
        1枚も直らない（コピー元で試して気づいた）。見るべきは「今いるプロジェクトの
        中を指しているか」。
        """
        if not path or inside(path):
            return None
        cand = os.path.join(root, os.path.basename(path))
        return cand if os.path.exists(cand) else None

    def fix_origin(path: str | None) -> str | None:
        """窓（origin_path）は外のファイルを指していて正しいこともある。
        届かなくなっていて、同名のものが一緒に引っ越してきたときだけ張り直す。"""
        if not path or os.path.exists(path) or inside(path):
            return None
        cand = os.path.join(root, os.path.basename(path))
        return cand if os.path.exists(cand) else None

    rows = conn.execute(
        "SELECT id, committed_path, origin_path, filter_ref FROM virtual_bodies"
        " WHERE deleted_at IS NULL").fetchall()
    old_roots = set()
    for _, cp, op, _ in rows:
        for p in (cp, op):
            if p and not inside(p):
                d = os.path.dirname(p)
                if d and os.path.basename(d) == os.path.basename(root):
                    old_roots.add(d)      # 引っ越し前のこのプロジェクトの場所

    for vid, cp, op, ref in rows:
        ncp, nop = fix_committed(cp), fix_origin(op)
        nref = ref
        if ref:
            for old in old_roots:
                if old in nref:
                    nref = nref.replace(old, root)
        if ncp or nop or nref != ref:
            changed += 1
            if not DRY:
                conn.execute(
                    "UPDATE virtual_bodies SET committed_path = COALESCE(?,committed_path),"
                    " origin_path = COALESCE(?,origin_path), filter_ref = ? WHERE id = ?",
                    (ncp, nop, nref, vid))
    if not DRY:
        conn.commit()
    conn.close()
    return changed


def main() -> int:
    here = os.path.dirname(os.path.abspath(__file__))
    base = os.path.abspath(os.path.join(here, "..", "サンプルプロジェクト"))
    if not os.path.isdir(base):
        base = os.path.abspath(os.path.join(here, ".."))
    targets = [os.path.join(base, d) for d in sorted(os.listdir(base))
               if os.path.isdir(os.path.join(base, d, ".texos"))]
    if not targets:
        print("プロジェクトが見つかりません:", base)
        return 1
    print(("確認: " if DRY else "引っ越し: ") + base)
    for t in targets:
        n = migrate(t)
        print(f"  {os.path.basename(t):24} 直すカード {n:3} 枚")
    print("\\n完了。TexoS で「プロジェクトを開く」から、上のフォルダを開いてください。"
          if not DRY else "\\n（--確認だけ のため、書き換えていません）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
'''


def readme(version: str) -> str:
    lines = [
        f"# TexoS v{version} インストールセット", "",
        "このフォルダをまるごとコピーすれば、別の PC に TexoS 一式を移せます。", "",
        "---", "",
        "## 1. インストール", "",
        f"`texos_{version}_x64-setup.exe` を実行してください（Windows 64bit）。",
        "署名を付けていないので、SmartScreen が出たら「詳細情報」→「実行」を選びます。", "",
        "**TexoS 単体では他に何も要りません。** テキスト処理・連結・差分・画像のリサイズと",
        "形式変換・文字コード変換・Web取得・記法カードは全部内蔵で動きます。",
        "AI や音声認識を使いたくなったら `ドキュメント\\道具のインストールガイド.md` を見て、",
        "**必要な分だけ**足してください。", "",
        "## 2. サンプルプロジェクトを開く", "",
        "`サンプルプロジェクト\\` を、置きたい場所（ドキュメントの下など）へコピーします。",
        "**コピーしたら、`ツール\\プロジェクトの引っ越し.py` を1回流してください。**", "",
        "```",
        "python ツール\\プロジェクトの引っ越し.py",
        "```", "",
        "TexoS のプロジェクトはカードが実ファイルを**絶対パス**で指しているので、",
        "場所が変わるとリンクが元の場所を向いたままになります（中身は控えから読めるので",
        "**眺めるだけなら流さなくても大丈夫**です。書き換えたりフローを流すなら流してください）。",
        "Python が無ければこの手順は飛ばして構いません。", "",
        "そのあと TexoS の「プロジェクトを開く」でフォルダを選びます。", "",
        "### 入っているもの", "",
    ]
    for name, desc in PROJECTS:
        lines.append(f"- **{name}** — {desc}")
    lines += [
        "", "**まず開くなら `カードカタログ` と `レシピ集`** です。",
        "カードカタログは種類ごとにサンプル入力が繋いであるので、▶ を押すと挙動が見られます。",
        "レシピ集は枠ひとつが1レシピで、左の付箋に「何ができるか／要るもの／出典」があります。", "",
        "## 3. ドキュメント", "",
        "- **道具のインストールガイド.md** — レシピの「要るもの」の入手先と注意点。まずこれ",
        "- **ユーザーズチップス.md** — フィルタの組み方のレシピ集（文章版）",
        "- **AIエージェント向け_フロー構築規約.md** — **AI に仕組みを組ませるための規約**。",
        "  Claude や ChatGPT にこの文書と `ツール\\texos_flow.py` を渡して",
        "  「TexoS で○○する仕組みを作って」と頼むと、カードを並べてくれます",
        "- **カード化プログラム作成規約.md** — 自作プログラムをカードに載せるときの約束",
        "- **更新履歴.md** — 版ごとの変更点", "",
        "## 4. ツール", "",
        "- **プロジェクトの引っ越し.py** — 上の 2. で使う",
        "- **texos_flow.py** — AI がフローを組むための足場（規約と1組）",
        "- **texos_dryrun.py** — TexoS を起動せずにフローを1周流して確かめる", "",
        "どちらも Python 3.10 以降で動きます。", "",
        "---", "",
        "## 困ったとき", "",
        "- **カードの ▶ で「プログラムが見つかりません」** … 外部プログラムの名前は",
        "  **拡張子まで**書きます（`python` ではなく `python.exe`）。導入直後は PATH の反映のため",
        "  TexoS を再起動してください。",
        "- **繋いだのに何も起きない／結果が変わらない** … 矢印は型（MIME）で検査され、",
        "  **合わない入力はエラーにならず飛ばされます**。カードカタログの「データの見本」に",
        "  形式ごとの MIME と繋ぎ先が書いてあります。",
        "- **カードが薄赤い** … 実ファイルに届いていない印です。引っ越しスクリプトを流してください。",
        "- **AI カードが動かない** … Ollama が起動しているか、モデル名が",
        "  `ollama list` にあるか確かめてください。",
    ]
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description="配布用インストールセットを組み立てる")
    ap.add_argument("--dest", default=os.path.join(REPO, "インストールセット"))
    a = ap.parse_args()

    version = json.load(io.open(os.path.join(REPO, "package.json"),
                                encoding="utf-8"))["version"]
    installer = os.path.join(REPO, "src-tauri", "target", "release", "bundle",
                             "nsis", f"texos_{version}_x64-setup.exe")
    if not os.path.exists(installer):
        print(f"インストーラがありません: {installer}\n"
              f"  先に `npm run release -- {version}` を流してください")
        return 1

    dest = os.path.abspath(a.dest)
    if os.path.exists(dest):
        shutil.rmtree(dest)
    os.makedirs(dest)
    print(f"インストールセット v{version} → {dest}")

    shutil.copy2(installer, os.path.join(dest, os.path.basename(installer)))
    print(f"  インストーラ: {os.path.basename(installer)}"
          f" ({os.path.getsize(installer) // 1024:,} KB)")

    doc_dir = os.path.join(dest, "ドキュメント")
    os.makedirs(doc_dir)
    for src, name in DOCS:
        s = os.path.join(REPO, "docs", src)
        if os.path.exists(s):
            shutil.copy2(s, os.path.join(doc_dir, name))
            print(f"  文書: {name}")
        else:
            print(f"  警告: {src} が見つかりません")

    prj_dir = os.path.join(dest, "サンプルプロジェクト")
    os.makedirs(prj_dir)
    # 試走結果は開発の作業跡なので配らない
    ignore = shutil.ignore_patterns("試走結果_*", "__pycache__", "*.pyc", "venv")
    for name, _ in PROJECTS:
        s = os.path.join(PROJECT_SRC, name)
        if not os.path.isdir(os.path.join(s, ".texos")):
            print(f"  警告: プロジェクトがありません: {name}")
            continue
        shutil.copytree(s, os.path.join(prj_dir, name), ignore=ignore)
        kb = sum(os.path.getsize(os.path.join(dp, f))
                 for dp, _, fs in os.walk(os.path.join(prj_dir, name))
                 for f in fs) // 1024
        print(f"  プロジェクト: {name} ({kb:,} KB)")

    tool_dir = os.path.join(dest, "ツール")
    os.makedirs(tool_dir)
    io.open(os.path.join(tool_dir, "プロジェクトの引っ越し.py"), "w",
            encoding="utf-8", newline="\n").write(MIGRATE)
    for t in TOOLS:
        shutil.copy2(os.path.join(REPO, "scripts", t), os.path.join(tool_dir, t))
    print(f"  ツール: プロジェクトの引っ越し.py / {' / '.join(TOOLS)}")

    io.open(os.path.join(dest, "はじめにお読みください.md"), "w",
            encoding="utf-8", newline="\n").write(readme(version))
    total = sum(os.path.getsize(os.path.join(dp, f))
                for dp, _, fs in os.walk(dest) for f in fs)
    print(f"\n合計 {total // 1024 // 1024:,} MB / {total:,} バイト")
    print("このフォルダごとコピーして配ってください。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
