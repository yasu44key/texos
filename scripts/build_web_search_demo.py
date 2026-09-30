"""Web検索サンプル（Web検索 P3・docs/Web検索_SearXNG計画.md §4）を組む。

2つの形を1つのプロジェクトに並べる:
  A1 = 検索語を人が書く      [検索語] → Web検索 → 記録 → [束: 課題＋結果] → AIレポート
  A2 = 課題から AI が検索語を作る
       [課題] → AI検索語 → 記録 → Web検索 → 記録 → [束: 課題＋結果] → AIレポート

規約（docs/AIエージェント向け_フロー構築規約.md）で押さえた点:
  - 役割は付箋から role="system" で渡す（入力に数えない＝ファンアウトしない・§3-9）
  - 課題と検索結果は束（text_concat）でまとめてから AI へ（AI は入力ごとに走る・§3-1）
  - 束の1本目は課題（枠を持ち回る・§4-3）
  - 途中（検索語・検索結果）は記録カードで残す（中間はファイルにならない・§3-10）

使い方:
  python scripts/build_web_search_demo.py [出力先フォルダ] [--model gemma4:latest]
  既定の出力先 = F:\\yoshi\\Documents\\TexoS\\TexoS_Web_SandBox\\Web検索サンプル
前提: TexoS の「Web ... → Web検索エンジンの管理」で SearXNG を導入済み・Ollama に model がある。
"""
import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import Project  # noqa: E402

DEFAULT_ROOT = r"F:\yoshi\Documents\TexoS\TexoS_Web_SandBox\Web検索サンプル"

ROLE_REPORT = """役割：レポート担当
束の1つ目が「課題」、2つ目以降が「Web検索結果」です。
検索結果に書かれていることだけを使って、課題に答える Markdown のレポートを書いてください。
- 各項目の最後に、根拠にした検索結果の URL を「出典: URL」の形で必ず付ける
- 検索結果に無いこと（価格・発売日など）を推測で補わない。足りなければ「検索結果からは不明」と書く
- 同じ製品・話題が複数の結果に出ていたら1項目にまとめる"""

ROLE_KEYWORDS = """役割：検索担当
課題を調べるための Web 検索語を考えてください。
出力は検索語だけにすること。
- 1行に1つ、最大3行
- 1つの検索語は、名詞を2〜4語、空白で区切る
- 課題が「最新」を求めていたら、「発表」「発売」「新製品」など新しさを表す語を入れる
- 「おすすめ」「比較」「まとめ」は入れない（まとめ記事ばかりが出て、新しい話が埋もれるため）
- 番号・記号・説明・前置き・後書きは書かない"""


def build(root: str, model: str) -> Project:
    p = Project(root, model=model)

    # ---- 全体の説明 ----
    p.note(
        "Web検索サンプル\n"
        "キーワードで Web を検索し、その結果をもとに AI がレポートを書きます。\n"
        "・A1＝検索語を自分で書く／A2＝課題から AI が検索語を考える\n"
        "・▶ を押すのは各枠の右端「レポート」カード\n"
        "・要るもの: Web検索エンジン（SearXNG・「Web ...」メニューから導入）と "
        f"Ollama の {model}\n"
        "・黄色の付箋（ここを書き換える）を直せば、別の話題でも使えます",
        40, -300, width=620, font_size=13, title_size=18, color="#d7ecff",
    )

    # ---- A1: 検索語を人が書く ----
    y1 = 0
    p.shape(20, y1 - 40, 1720, 520, "A1  検索語を自分で書く", fill="#4a90d9")
    kw1 = p.note(
        "検索語（ここを書き換える・1行に1つ）：\n最新 ガジェット",
        60, y1 + 40, width=260, color="#fff3c4",
    )
    task1 = p.note(
        "課題（ここを書き換える）：\n最新のガジェット情報を5つレポートしてください。",
        60, y1 + 260, width=260, color="#fff3c4",
    )
    s1 = p.builtin(
        "Web検索（ニュース・1週間）", 400, y1 + 40, name="web_search",
        categories="news", time_range="week", language="ja", limit=10, max_queries=3,
        accept_mime="text/*",
        memo="分類=ニュース・期間=1週間以内。設定は右クリック →「設定」で変えられます",
    )
    r1 = p.tap("記録：検索結果", 700, y1 + 40, prefix="A1_検索結果")
    b1 = p.cat("束（課題＋検索結果）", 1000, y1 + 150, prefix="A1_材料")
    role1 = p.note(ROLE_REPORT, 1000, y1 - 20 + 0, width=330)
    ai1 = p.ai("A1 レポート", 1400, y1 + 150, prefix="A1_レポート", temperature=0.3,
               memo="ここの ▶ を押す")
    p.flow(kw1, s1)
    p.flow(s1, r1)
    p.flow(task1, b1)      # 束の1本目＝課題（枠を持ち回る）
    p.flow(r1, b1)         # 2本目＝検索結果
    p.flow(b1, ai1)
    p.flow(role1, ai1, role="system")

    # ---- A2: 課題から AI が検索語を作る ----
    y2 = 640
    p.shape(20, y2 - 40, 2000, 600, "A2  課題から AI が検索語を考える", fill="#5fbf77")
    task2 = p.note(
        "課題（ここを書き換える）：\n最新のガジェット情報を5つレポートしてください。",
        60, y2 + 260, width=260, color="#fff3c4",
    )
    role_kw = p.note(ROLE_KEYWORDS, 340, y2 + 10, width=320)
    ai_kw = p.ai("検索語を考える", 400, y2 + 260, temperature=0.2)
    r_kw = p.tap("記録：検索語", 680, y2 + 260, prefix="A2_検索語")
    s2 = p.builtin(
        "Web検索（ニュース・1週間）", 960, y2 + 260, name="web_search",
        categories="news", time_range="week", language="ja", limit=5, max_queries=3,
        accept_mime="text/*",
        memo="AI が出した検索語（最大3つ）で順に検索します。記録：検索語 を開くと何で検索したか分かります",
    )
    r2 = p.tap("記録：検索結果", 1240, y2 + 260, prefix="A2_検索結果")
    b2 = p.cat("束（課題＋検索結果）", 1520, y2 + 380, prefix="A2_材料")
    role2 = p.note(ROLE_REPORT, 1360, y2 + 10, width=330)
    ai2 = p.ai("A2 レポート", 1760, y2 + 380, prefix="A2_レポート", temperature=0.3,
               memo="ここの ▶ を押す")
    p.flow(task2, ai_kw)
    p.flow(role_kw, ai_kw, role="system")
    p.flow(ai_kw, r_kw)
    p.flow(r_kw, s2)
    p.flow(s2, r2)
    p.flow(task2, b2)      # 束の1本目＝課題（レポート担当にも課題を届ける）
    p.flow(r2, b2)
    p.flow(b2, ai2)
    p.flow(role2, ai2, role="system")

    p.view(0, -340, 0.5)
    return p


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("root", nargs="?", default=DEFAULT_ROOT)
    ap.add_argument("--model", default="gemma4:latest")
    ap.add_argument("--reset", action="store_true", help="既存のカードを消して組み直す")
    a = ap.parse_args()
    p = build(a.root, a.model)
    p.save(reset=a.reset)


if __name__ == "__main__":
    main()
