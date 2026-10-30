"""RAG サンプル（V1.13 P1・docs/V1.13_エージェントカード計画.md §5）を組む。

手元の資料（TexoS 自身の docs を3本コピー）を索引にして、問いに資料ベースで答える:

  [📚 索引: 資料フォルダ] ─▶ [🔎 文書検索] ◀─ [問い]
                                  │
                [問い] ──▶ [束（問い＋検索結果）] ──▶ [AI 回答] ◀─ 役割の付箋

規約（docs/AIエージェント向け_フロー構築規約.md）で押さえた点:
  - 検索カードは集約扱い（索引レポートと問いを両方つないでもファンアウトしない）
  - 問いと検索結果は束（text_concat）でまとめてから AI へ（AI は入力ごとに走る・§3-1）
  - 束の1本目は問い（枠を持ち回る・§4-3）
  - 検索結果は記録カードで残す（何を根拠にしたか後で読める・§3-10）

使い方:
  python scripts/build_rag_demo.py [出力先フォルダ] [--model gemma4:latest] [--reset]
  既定の出力先 = F:\\yoshi\\Documents\\TexoS\\カード実験_RAG
前提: Ollama に bge-m3（`ollama pull bge-m3`）と model がある。
"""
import argparse
import os
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import Project  # noqa: E402

DEFAULT_ROOT = r"F:\yoshi\Documents\TexoS\カード実験_RAG"
DOCS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "docs")
SOURCES = ["ユーザーズチップス.md", "MCP接続.md", "V1.10_音声反射カード計画.md"]

ROLE_ANSWER = """役割：資料係
束の1つ目が「問い」、2つ目が「資料からの抜粋」です。
抜粋に書かれていることだけを使って、問いに日本語で答えてください。
- 答えの最後に、根拠にした抜粋の見出し（ファイル名と行）を「出典: …」の形で付ける
- 抜粋に無いことは推測で補わない。足りなければ「資料からは分からない」と書く
- 手順を聞かれたら番号付きの箇条書きで"""


def build(root: str, model: str) -> Project:
    src_dir = os.path.join(root, "資料")
    os.makedirs(src_dir, exist_ok=True)
    for name in SOURCES:
        shutil.copyfile(os.path.join(DOCS, name), os.path.join(src_dir, name))

    p = Project(root, model=model)
    p.note(
        "RAG サンプル\n"
        "「資料」フォルダの文書を索引にして、問いに資料ベースで答えます。\n"
        "・▶ を押すのは右端の「AI 回答」カード（索引 → 検索 → 回答の順に動きます）\n"
        "・1回目は索引を作るので少し待ちます。2回目からは変わった文書だけ作り直します\n"
        f"・要るもの: Ollama の bge-m3（埋め込み）と {model}\n"
        "・黄色の付箋（問い）を書き換えれば別の質問に。資料フォルダに md / txt を足せば索引も増えます",
        40, -300, width=640, font_size=13, title_size=18, color="#d7ecff",
    )
    p.shape(20, -40, 1700, 560, "資料に基づいて答える", fill="#4a90d9")
    q = p.note(
        "問い（ここを書き換える）：\nウォッチャで、フォルダに新しく置いたファイルを自動でカードにするには？",
        60, 260, width=280, color="#fff3c4",
    )
    ix = p.builtin(
        "📚 索引（資料）", 60, 40, name="rag_index",
        index="資料", path="資料", model="bge-m3", provider="ollama",
        accept_mime="text/*", prefix="索引レポート",
        memo="資料フォルダの md / txt を索引にします（保存先 .texos/rag/資料.sqlite）",
    )
    s = p.builtin(
        "🔎 文書検索", 420, 140, name="rag_search",
        limit=5, provider="ollama", accept_mime="text/*",
        memo="索引カードをつないでいるので「資料」の索引から探します",
    )
    r = p.tap("記録：検索結果", 720, 140, prefix="検索結果")
    b = p.cat("束（問い＋抜粋）", 1000, 260, prefix="材料")
    role = p.note(ROLE_ANSWER, 1000, -20, width=340)
    ai = p.ai("AI 回答", 1400, 260, prefix="回答", temperature=0.2, memo="ここの ▶ を押す")
    p.flow(ix, s)
    p.flow(q, s)
    p.flow(s, r)
    p.flow(q, b)     # 束の1本目＝問い
    p.flow(r, b)     # 2本目＝抜粋
    p.flow(b, ai)
    p.flow(role, ai, role="system")
    p.view(0, -340, 0.55)
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
