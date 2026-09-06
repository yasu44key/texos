# -*- coding: utf-8 -*-
r"""TexoS を TexoS 自身の生成AIで組み立てる「自己構築」プロジェクトを作る。

    依頼（付箋）＋ フロー構築規約（実身） ──▶ 束 ──▶ AI ──▶ コード取り出し
        ──▶ 組み立て案.py（実身）──▶ python 実行 ──▶ 新しいプロジェクトが生える

設計の要点（なぜこの形か）:

1. **規約はプロジェクトに実身として同梱する。**
   AI カードは `text/*` を受けるので `.md` はそのまま材料になる（§3-7）。
   同梱しておけば、規約を書き換えて即その場で挙動を変えられる（キャンバスが設定画面）。

2. **役割は付箋 → role="system"。**（§3-9）
   ローカルモデルへの一番効く指示は出力形式の固定なので、それを付箋に置いて
   人が読み書きできるようにする。JSON ダイアログの中に隠さない。

3. **「案を作る」と「実行する」を実身カードで切る。**（§3-12 対策②）
   一続きの矢印にすると、実行の ▶ が上流の AI を走らせ直し、
   **人が読んだ案とは別のスクリプトが走る**。間に実身（組み立て案.py）を置くと、
   実行カードは「ファイルの今」を読む＝レビューした通りのものが動く。
   `md_codeblock` の終端 prefix と、あらかじめ置いた実身のファイル名を一致させると、
   TexoS は新しいカードを作らずその実身の中身だけ差し替える（flow_run: 固定名上書き）。

4. **生成物は自分の下の `生成物\` へ。**（§3-8）
   TexoS が開いているプロジェクトの DB を外から書いても面に出ない。
   自分自身へ書き込ませない ＝ `link_into()` は禁止。

5. **絶対パスを AI に書かせない。**（可搬性）
   `ツール	exos_agent.py` の `new_project()` だけを入口にし、`texos_flow.py` は
   プロジェクトに同梱する。外部プログラムカードは作業ディレクトリがプロジェクト直下
   （`flow_run.rs :: execute_node` が `Some(root)` を渡す）なので、踏み台
   `組み立てる.py` を相対名で呼べば、そこから先はすべてプロジェクト基準で解ける。
   **フォルダごとコピーすれば別の PC でも動く。**

前提: Ollama に tools/thinking 対応の中〜大型モデル（既定 qwen3.8:latest）。
      実測 = 規約10.9k tok を読ませて設計スクリプト2.2k tok を書くのに約55秒（RTX 3090）。
"""
import io
import os
import shutil
import sys

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SCRIPTS)
from texos_flow import Project  # noqa: E402

REPO = os.path.dirname(SCRIPTS)
SPEC_MD = os.path.join(REPO, "docs", "AIエージェント向け_フロー構築規約.md")
同梱元 = os.path.join(SCRIPTS, "自己構築同梱")

ROOT = r"F:\yoshi\Documents\TexoS\AI自己構築"
PARENT = r"F:\yoshi\Documents\TexoS\AIプロジェクト"
MODEL = "qwen3.8:latest"

# --- 役割（システムプロンプト）。ここが「コンテキストの設定」の本体 ----------------
# ローカルモデルに効くのは (a) 出力形式の固定 (b) 骨格の提示 (c) 落とし穴の再掲。
# 規約は 1 万トークンあるので、中ほどの決まりは薄れる。効かせたいものは短くここへ。
ROLE = rf"""役割：TexoS フロービルダー（ここを書き換えると AI の作法が変わる）

あなたは TexoS のフローを組み立てるエージェントです。
渡された「フロー構築規約」と「依頼」を読み、組み立てスクリプトを **1 本だけ** 書きます。

## 出力の決まり（厳守）
- 説明文・前置き・後書きを書かない。```python のコードブロックを **1つだけ** 出力する。
- **絶対パスを1文字も書かない。** 骨格は必ずこれ:

```python
# -*- coding: utf-8 -*-
from texos_agent import new_project

p = new_project("<依頼に合う短い日本語名>", model="{MODEL}")
# ここでカードを作り、p.flow(...) で繋ぐ
p.view()
p.save()
```

- `new_project()` が返すのは規約 §5 の `Project` そのもの。使える API はそこに書いてあるものだけ。
- `Project(...)` を直接呼ばない。`sys.path` をいじらない。`p.link_into()` は呼ばない。
- 座標は左上から右下へ流れるように（x は 0〜1400、y は 0〜900 くらい）。

## 特に間違えやすいところ（規約の再掲）
- AI カードに入力を 2 本以上繋ぐと **AI が入力ごとに別々に走る**。
  複数の材料を1回で読ませるなら必ず `p.cat(...)`（束）を前に挟む。
- AI の役割は `p.note(...)` に書いて `p.flow(付箋, ai, role="system")` で渡す。
  役割の矢印は入力に数えないので束は要らない。
- AI が書いたソースをコンパイラ／インタプリタへ渡すなら、あいだに
  `p.builtin("...", x, y, name="md_codeblock", lang="c")` を挟む（AI の出力は必ず .md）。
- 外部プログラムの名前は拡張子まで書く（`python.exe` / `gcc.exe` / `ffmpeg.exe`）。
- 人が書き換える入口は **付箋か実身**。IN カードは中身を持てない。
- ファイル名を決めるのは **▶ を押す終端カードの prefix だけ**。
"""

# --- 依頼（既定の中身。人が毎回ここを書き換える）--------------------------------
REQUEST = r"""依頼（ここを書き換えて ▶ を押す）

お題を書いた付箋と、参考資料の付箋2枚を材料にして、AI にそれらをまとめて読ませ、
500字程度の要約を書き出すフローを作ってください。
AI の役割（要約担当だという指示）はキャンバスから読めるようにしてください。

フォルダ名: 要約フロー
"""

HOWTO = r"""使い方 — TexoS が TexoS を組む

① 左下の「依頼」の付箋に、作りたい仕組みを日本語で書く。
② 「コード取り出し」カードの ▶ を押す。
   規約と依頼を読んだ AI が組み立てスクリプトを書き、
   その中身が右の「組み立て案.py」に入る（1〜3分。初回はモデルのロードで +1分）。
③ **組み立て案.py を開いて読む。** ここが人の関門。
   AI が書いた Python がこの後そのまま走るので、納得できなければ ①へ戻るか手で直す。
   手で直した内容もそのまま効く（実行カードはファイルの今を読む）。
④ 「実行」カードの ▶ を押す。新しいプロジェクトが **生成物\** の下に生まれる。
   結果は「実行ログ」カードに出る（カード枚数・矢印本数・終端の名前）。
⑤ TexoS の「プロジェクトを開く」で 生成物\<名前> を開く。

④が③の案をそのまま動かせるのは、間に実身カードを挟んであるから。
一本の矢印で繋ぐと、実行の ▶ が AI を走らせ直して別の案が動く（規約 §3-12）。

**このフォルダは丸ごとコピーして配れる。** 生成される案には絶対パスが1つも無く、
ビルダー（texos_flow.py）も ツール\ に同梱してある。移した先で要るのは
Python と Ollama だけ。モデル名を変えるときは、この上の「役割」の付箋を直す。

うまくいかないとき:
- 「コードブロックが見つかりません」→ AI が地の文で答えた。
  「AI出力.md」を開いて実際の返事を見る。役割の付箋の出力形式を強く書き直す。
- 実行ログに Python の例外 → 規約に無い API を使った。使い方の例を規約に1つ足すと直る
  （実測でも、落ちた2件はどちらも「規約に例が無い API」だった）。
- 案が毎回ぶれる → 「設計」カードの temperature を下げる（既定 0.2）。

同梱物（消さないこと）:
  組み立てる.py … 実行カードが呼ぶ踏み台。プロジェクト基準で案を走らせる
  ツール\        … texos_agent.py（new_project の入口）と texos_flow.py（ビルダー本体）
  生成物\        … ここに新しいプロジェクトが生える
"""

PLACEHOLDER = r"""# -*- coding: utf-8 -*-
# ここに AI が書いた組み立てスクリプトが入る（「コード取り出し」の ▶ を押す）。
print("まだ組み立て案がありません。左の『コード取り出し』カードの ▶ を押してください。")
"""


# --- API 署名表（自動生成）------------------------------------------------------
# 実測（2026-08-27・3モデル比較）: 小さいモデルの失敗は**ほぼ全部が API の打ち間違い**で、
# フローの設計そのものは合っていた。署名を1枚渡せば消える種類なので、規約の写しに添える。
# **手書きしないこと** —— ビルダーが変われば次のリリースで嘘になる。
def API署名表() -> str:
    import inspect
    from texos_flow import Project
    NL = chr(10)
    順 = ("file", "copy_file", "note", "drop", "ai", "cat", "tap", "ask", "loop",
          "builtin", "program", "launcher", "bundle", "op_in", "op_out", "op",
          "flow", "shape", "view", "save", "link_into")
    行 = []
    for 名 in 順:
        f = getattr(Project, 名, None)
        if f is None:
            continue
        引数 = []
        for i, (pn, par) in enumerate(inspect.signature(f).parameters.items()):
            if i == 0 and pn == "self":
                continue
            if par.kind is par.VAR_KEYWORD:
                引数.append("**params"); continue
            if par.kind is par.KEYWORD_ONLY and "*" not in 引数:
                引数.append("*")
            引数.append(pn if par.default is par.empty else f"{pn}={par.default!r}")
        行.append(f"p.{名}({', '.join(引数)})")
    本文 = NL.join(行)
    return (
        "SIGNATURE_TABLE_BEGIN" + NL + NL
        + "## 5-1. 使える関数と、その引数（これで全部）★" + NL + NL
        + "**ここに無い関数は存在しない。ここに無い引数は渡せない。**" + NL
        + "`*` より後ろはキーワード引数（`name=...` の形でしか渡せない）。" + NL
        + "実測では、小さいモデルの失敗はほぼ全部がこの表の打ち間違いだった —— "
        + "設計は合っていて、書き写せなかった。" + NL + NL
        + "```python" + NL + 本文 + NL + "```" + NL + NL
        + "とくに間違えやすいところ:" + NL + NL
        + "- **付箋 `note()` は `memo=` も `prefix=` も取らない。** 付箋に出力名は無い" + NL
        + "- **`builtin()` と `program()` は `name=` が必須**（第1引数はカードのラベル）" + NL
        + "- **`op_in()` / `op_out()` は `x` と `y` が必須**（省略できない）" + NL
        + "- キーワード引数の後ろに位置引数は書けない（Python の構文エラー）" + NL + NL
        + "SIGNATURE_TABLE_END" + NL
    )


# --- 規約の絞り込み（可読性のためではなく、コンテキスト節約のため）-------------
# 規約は2つの読者を持つ: (a) シェルを持つ大きなエージェント (b) TexoS の中の AI カード。
# (b) には要らない節がはっきりある —— 足場の確認・モデルの実機確認・link_into・
# 試走スクリプト・この文書自身の話・理念。**リポジトリの規約は1本のまま**にして、
# プロジェクトへ入れる写しだけをここで機械的に落とす（`build_card_catalog.py` と同じ流儀）。
落とす節 = (
    "0. なぜこの文書が要るか",        # 足場の確認・事典とレシピ集（面からは読めない）
    "2-0. どのプロジェクトに組むか",  # new_project() が決めるので選ばせない
    "2-1. AI カードのモデルは",       # モデルは役割の付箋で渡している
    "2-2. この規約そのものを",        # この文書自身についてのメタな節
    "5-2. 成果物は",                  # link_into は禁止している
    "6. 納品前に必ずやること",        # 試走スクリプトは面からは呼べない
    "7. この文書の位置づけ",          # 人間向けの理念
)


# 絞ったあとに残る「もう無い節・面からは開けないファイル」への言及を畳む。
# 迷子の参照はモデルを迷わせるだけで、1つも得がない。
掃除 = (
    ("（`project` だけは §5-2 で使う）。", "（この用途では使わない）。"),
    ("理念（`V2.0課題.md §8-1`）で、", "理念であり、"),
    ("グラフを見ても実行が成功したかは分からない（§6 と同じ話）。",
     "グラフを見ても実行が成功したかは分からない。"),
    ("- 例 → `scripts/build_sample_human_decision.py`（課題に人の判断を挟む・2パス）。\n", ""),
    ("## 5. 組み立て方 — `scripts/texos_flow.py`", "## 5. 組み立て方"),
)


def 規約を絞る(text: str, 署名表: bool = True) -> str:
    """`落とす節` の見出しから、同じか上の階層の次の見出しまでを削る。"""
    出力, 捨てる, 深さ = [], False, 0
    for line in text.splitlines(keepends=True):
        m = None
        if line.startswith("#"):
            h = len(line) - len(line.lstrip("#"))
            if 2 <= h <= 3 and line[h:h + 1] == " ":
                m = (h, line[h:].strip())
        if m:
            if 捨てる and m[0] <= 深さ:
                捨てる = False
            if not 捨てる and any(m[1].startswith(k) for k in 落とす節):
                捨てる, 深さ = True, m[0]
        if not 捨てる:
            出力.append(line)
    out = "".join(出力)
    # 前文の3行（姉妹編・足場の紹介）は面からは辿れないので落とす
    out = "\n".join(l for l in out.splitlines()
                    if not (l.startswith("> ") and ("scripts/" in l or "姉妹編" in l)))
    for a, b in 掃除:
        out = out.replace(a, b)
    if 署名表:
        out = out.replace("## 5. 組み立て方", API署名表() + "\n## 5. 組み立て方")
    return out + "\n"


def 同梱する(root: str) -> None:
    """踏み台とツール一式をプロジェクトへ入れる（カードにはしない＝面の邪魔をしない）。"""
    os.makedirs(os.path.join(root, "ツール"), exist_ok=True)
    os.makedirs(os.path.join(root, "生成物"), exist_ok=True)
    shutil.copy2(os.path.join(同梱元, "組み立てる.py"), root)
    shutil.copy2(os.path.join(同梱元, "ツール", "texos_agent.py"),
                 os.path.join(root, "ツール"))
    # ビルダー本体も同梱する（リポジトリへの依存を断つのが目的なので必須）
    shutil.copy2(os.path.join(SCRIPTS, "texos_flow.py"), os.path.join(root, "ツール"))
    print(r"同梱: 組み立てる.py / ツール\texos_agent.py / ツール\texos_flow.py")


def 引数(名: str, 既定: str) -> str:
    """`--root <パス>` のような指定を拾う（無ければ既定）。"""
    旗 = f"--{名}"
    for i, a in enumerate(sys.argv):
        if a == 旗 and i + 1 < len(sys.argv):
            return sys.argv[i + 1]
        if a.startswith(旗 + "="):
            return a.split("=", 1)[1]
    return 既定


def 既存の依頼(root: str) -> str | None:
    """組み直す前に、いまの面に置かれている依頼の付箋を引き継ぐ。

    人が書いた文はこのビルダーの持ち物ではない。reset で消す前に拾っておく。
    """
    import sqlite3
    db = os.path.join(root, ".texos", "texos.db")
    if not os.path.isfile(db):
        return None
    try:
        conn = sqlite3.connect(db)
        for (t,) in conn.execute(
                "SELECT note_text FROM virtual_bodies"
                " WHERE kind='note' AND deleted_at IS NULL"):
            if t and t.lstrip().startswith(("依頼", "課題")):
                return t
    except Exception:
        pass
    return None


def main() -> None:
    global ROLE, MODEL
    root = os.path.abspath(引数("root", ROOT))
    MODEL = 引数("model", MODEL)
    ROLE = ROLE.replace("qwen3.8:latest", MODEL)
    依頼 = 既存の依頼(root) if "--reset" in sys.argv else None
    if 依頼:
        print(f"依頼の付箋を引き継ぎます: {依頼.splitlines()[0][:40]}")
    os.makedirs(root, exist_ok=True)
    同梱する(root)
    p = Project(root, model=MODEL)

    # --- 材料 -----------------------------------------------------------------
    原文 = io.open(SPEC_MD, encoding="utf-8").read()
    絞り = 規約を絞る(原文)
    print(f"規約: {len(原文):,}字 → {len(絞り):,}字"
          f"（{100 - len(絞り) * 100 // len(原文)}% 削減・面から使えない節を落とした）")
    規約 = p.file("フロー構築規約.md", 絞り, 40, 340,
                  memo="AI に渡す教科書。書き換えれば作法が変わる")
    依頼 = p.note(依頼 or REQUEST, 40, 620, width=360)
    役割 = p.note(ROLE, 480, 30, width=420, color="#fff3c4")

    # --- 案を作るレーン --------------------------------------------------------
    束 = p.cat("束 材料", 480, 500, prefix="材料",
               memo="規約→依頼の順で1本にまとめる（AI は入力ごとにファンアウトするため）")
    設計 = p.ai("設計", 800, 500, temperature=0.2, prefix="設計",
                memo="役割は左上の付箋から system として渡る")
    控え = p.tap("記録：AI出力", 1080, 500, prefix="AI出力",
                 memo="AI の生の返事。コード取り出しが失敗したらここを読む")
    取出 = p.builtin("コード取り出し", 1380, 500, name="md_codeblock",
                     lang="python", output_ext="py", prefix="組み立て案",
                     accept_mime="text/*",
                     memo="▶ ここを押すと案ができる。終端なので prefix が効く")

    # 実行レーンの入口。取出の prefix と同じ名前にしてあるので、TexoS は
    # 新しいカードを作らずこの実身の中身だけ差し替える（位置と矢印は残る）。
    案 = p.file("組み立て案.py", PLACEHOLDER, 1380, 720,
                memo="人が読む関門。手で直してもよい（実行はファイルの今を読む）")

    # --- 実行レーン ------------------------------------------------------------
    実行 = p.program("実行", 1380, 940, name="python.exe",
                     cmd=r'-X utf8 "組み立てる.py" "{in}" > "{out}" 2>&1',
                     output_ext="md", prefix="実行ログ", timeout_secs=300,
                     memo="▶ ここを押すと組み立て案が走り、新しいプロジェクトが生まれる")

    案内 = p.note(HOWTO, 40, 940, width=400, color="#d7ecff")

    # --- 配線 ------------------------------------------------------------------
    p.flow(規約, 束)          # 1本目＝教科書（枠は持ち回る・§4-3）
    p.flow(依頼, 束)          # 2本目＝そのときの注文
    p.flow(束, 設計)
    p.flow(役割, 設計, role="system")
    p.flow(設計, 控え)
    p.flow(控え, 取出)
    p.flow(案, 実行)          # ← ここが切れ目。実行は AI を遡らない

    p.shape(420, 440, 1180, 220, "① 案を作る（AI が書く）")
    p.shape(1320, 680, 280, 400, "② 読んで、実行する（人が押す）")
    p.view(0, 0, 0.45)
    p.save(reset="--reset" in sys.argv)
    if "--no-link" not in sys.argv:
        p.link_into(PARENT)


if __name__ == "__main__":
    main()
