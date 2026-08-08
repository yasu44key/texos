# -*- coding: utf-8 -*-
"""v1.8 ③問い合わせカードの動作確認プロジェクトを組む。

確かめたいのは4つ:
  1. 1回目の実行が「答え待ち」で止まる（エラーではなく no-data）
  2. 答えの付箋が自動で現れ、質問と**そのとき届いていた材料**が引用で入っている
  3. 付箋に答えを書いて2回目を実行すると、答えが下流の材料として使われる
  4. 答えが空のあいだは何度実行しても先へ進まない

  お題(付箋) ──▶ 下書き(AI) ──┬──▶ 記録 ─────────┐
                                │                    束 ──▶ 仕上げ(AI) ──▶ 決定
                                └──▶ 問い合わせ ────┘
                                        ▲
                                     答えの付箋（1回目の実行で自動生成）

注意（案B の性質・計画 §3）: 2回目の実行では上流の「下書き(AI)」も走り直すので、
人が読んだ案とは別の文面が出ることがある。だから記録カードで案を残してある。
「答えたときの案そのもの」を固定したいなら案C（止めて待つ）が要る。
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import Project  # noqa: E402

ROOT = r"F:\yoshi\Documents\TexoS\カード実験_問い合わせ"

p = Project(ROOT)

odai = p.note(
    "社内向けに、ファイルの自動バックアップ機能を1段落で説明する文章を書いてください。",
    60, 60, width=380,
)

yakuwari = p.note(
    "あなたは社内広報の書き手です。渡された指示に従って、本文だけを1段落で書いてください。\n"
    "説明・前置き・見出しは書かないこと。",
    60, 260, width=380,
)

shitagaki = p.ai(
    "下書き", 520, 60, system="（役割ポートで上書きされます）", prefix="下書き",
    memo="1回目の実行で走る。ここの出力が答えの付箋に引用される。",
)

kiroku = p.tap(
    "記録：下書き", 900, 60, prefix="下書き控え",
    memo="2回目の実行では下書きが書き直されるので、読んだ案をここに残す（案B の性質）。",
)

toi = p.ask(
    "確認", 520, 300,
    question="この下書きでよいですか？ 直したいところがあれば書いてください（そのままでよければ「このまま」と書く）",
    prefix="人の指示",
    memo="答えが空のあいだは no-data＝ここから先へは進まない。エラーではない。",
)

taba = p.cat(
    "束（下書き＋人の指示）", 900, 300, separator="rule", prefix="仕上げ材料",
)

shiage = p.ai(
    "仕上げ", 900, 500, system="渡された下書きを、続けて書かれた指示のとおりに直して、本文だけを出力してください。",
    prefix="決定稿",
    memo="人の指示が入って初めて走る（1回目は上流が no-data なので走らない）。",
)

p.flow(yakuwari, shitagaki, role="system")
p.flow(odai, shitagaki)
p.flow(shitagaki, kiroku)
p.flow(shitagaki, toi)          # 問い合わせに材料として届く＝答えの付箋に引用される
p.flow(kiroku, taba)            # 束の1本目＝下書き
p.flow(toi, taba)               # 束の2本目＝人の答え
p.flow(taba, shiage)

p.view(x=20, y=20, scale=0.7)
p.save()
