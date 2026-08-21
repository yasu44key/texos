# -*- coding: utf-8 -*-
"""「フィボナッチ数列を C / Java / Python で書いて、ソースと実行結果を並べて見せる」フロー。

AI エージェントに TexoS のフローを組ませる依頼の**手本**として残すもの。
依頼文の例（そのまま投げられる形）:

    「フィボナッチ数列を作るプログラムを C, Java, Python で作成して、
      ソースをそれぞれ提示し、実行結果を見せるフローを作って」

この依頼に対する設計判断が2つある。ここが手本の中身:

1. **ソースは実行時に AI に書かせず、エージェントが実身カードとして置く。**
   コードは依頼の時点で決まりきっており、毎回生成し直す理由がない。実身にしておけば
   人がその場で書き換えて ▶ を押すだけで結果が変わる（フローが「教材」になる）。
   AI に書かせる版は md_codeblock で囲いを剥がす形になるが、そちらは 3-7（MIME）で
   足を取られる — `.java` は octet-stream 扱いで下流に**黙って無視される**。
   ビルダー側の注意: C のソースを普通の三重引用符で書くと Python が `\n` を
   本物の改行に変えてしまい、`printf("` で行が切れてコンパイルが落ちる。**生文字列で書く**。
   このとき TexoS は終端を止めず、**その入力だけ欠けた出力を黙って作る**（⑤・§3-13）。

   なお **プログラム名は Windows では拡張子まで書く**（`gcc` ではなく `gcc.exe`）。
   `flow.rs: resolve_program` は PATH の各フォルダに名前をそのまま繋いで
   `is_file()` を見るだけで、`.exe` を補わない。

2. **実行は言語ごとに1枚の外部プログラムカード**（コンパイルと実行を && で繋ぐ）。
   `{out_noext}` を借りると中間生成物（exe・class）が一時フォルダ側に落ちるので、
   プロジェクト直下が散らからない。

   C 言語  ─┬─▶ [gcc 実行] ─┐
            └───────────────┼─▶ [束] ─┐
   Java    ─┬─▶ [javac 実行]┘         │
            └───────────────┼─▶ [束] ─┼─▶ [まとめ] ─▶ フィボナッチ_3言語.md
   Python  ─┬─▶ [python 実行]         │
            └───────────────┼─▶ [束] ─┘

各レーンの束は「見出し付箋 → ソース → 実行結果」の順（§3-3・繋いだ順が中身の順）。
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import Project  # noqa: E402

ROOT = r"F:\yoshi\Documents\TexoS\カード実験_多言語実行"

C_SRC = r"""#include <stdio.h>

int main(void) {
    unsigned long long a = 0, b = 1;
    for (int i = 0; i < 20; i++) {
        printf("%llu ", a);
        unsigned long long t = a + b;
        a = b;
        b = t;
    }
    printf("\n");
    return 0;
}
"""

JAVA_SRC = """\
// クラス名は Fib。public を付けないこと —— TexoS は入力を一時ファイル
// （in_<番号>.java）として渡すため、public class だと「ファイル名と一致しない」で
// javac が落ちる。
class Fib {
    public static void main(String[] args) {
        long a = 0, b = 1;
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 20; i++) {
            sb.append(a).append(' ');
            long t = a + b;
            a = b;
            b = t;
        }
        System.out.println(sb.toString().trim());
    }
}
"""

PY_SRC = """\
a, b = 0, 1
out = []
for _ in range(20):
    out.append(str(a))
    a, b = b, a + b
print(" ".join(out))
"""

LANES = [
    dict(
        y=60, lang="C 言語", src_name="fib.c", src=C_SRC, prog="gcc.exe",
        cmd='"{in}" -o "{out_noext}.exe" && "{out_noext}.exe" > "{out}"',
        note="## C 言語（gcc）\n\nコンパイルして実行する。実行ファイルは一時フォルダ側に作られる。",
    ),
    dict(
        y=520, lang="Java", src_name="Fib.java", src=JAVA_SRC, prog="javac.exe",
        cmd='-d "{out_noext}_cls" "{in}" && java -cp "{out_noext}_cls" Fib > "{out}"',
        note="## Java（javac + java）\n\n-d でクラスファイルの置き場を一時フォルダ側に逃がしている。",
    ),
    dict(
        y=980, lang="Python", src_name="fib.py", src=PY_SRC, prog="python.exe",
        cmd='"{in}" > "{out}"',
        note="## Python（python）\n\nそのまま実行して標準出力を受け取る。",
    ),
]

p = Project(ROOT)

p.note(
    "フィボナッチ数列を3言語で — ソースと実行結果を並べる\n\n"
    "右端の【まとめ】の ▶ を押すと、3本のレーンが動いて\n"
    "「フィボナッチ_3言語.md」ができます。\n\n"
    "左のソースカードは実身（本物のファイル）です。\n"
    "書き換えて ▶ を押し直すと、結果もそのまま変わります。\n"
    "レーン1本だけ見たいときは、そのレーンの【束】の ▶ を押してください。",
    60, -300, width=560, title_size=18,
)

lane_outs = []
for lane in LANES:
    y = lane["y"]
    midashi = p.note(lane["note"], 60, y, width=340)
    src = p.file(lane["src_name"], lane["src"], 60, y + 180,
                 memo="ここを書き換えると結果が変わる（実身）")
    run = p.program(
        f"実行（{lane['lang']}）", 520, y + 90,
        name=lane["prog"], cmd=lane["cmd"],
        output_ext="txt", accept_mime="text/*",
        prefix=f"実行結果_{lane['lang']}",
        memo="コンパイルと実行を && で繋いだ1枚。中間生成物は {out_noext} 側に落ちる",
    )
    taba = p.cat(
        f"束（{lane['lang']}）", 900, y + 90, separator="rule",
        prefix=f"{lane['lang']}_ソースと結果",
        memo="1本目=見出し / 2本目=ソース / 3本目=実行結果（繋いだ順がそのまま中身の順）",
    )
    p.flow(src, run)
    p.flow(midashi, taba)   # ← 順序が意味を持つ（§3-3）
    p.flow(src, taba)
    p.flow(run, taba)
    p.shape(30, y - 40, 1140, 400, lane["lang"])
    lane_outs.append(taba)

matome = p.cat(
    "まとめ", 1320, 560, separator="blank", prefix="フィボナッチ_3言語",
    memo="終端カード。ここの ▶ を押すと3レーンぶんが1つの md になる",
)
for t in lane_outs:
    p.flow(t, matome)

p.view(0, -240, 0.5)
p.save(reset=True)

# 既にカードのあるプロジェクトへフローを流し込むことはしない（座標が衝突し、二重生成の元）。
# 「別プロジェクトとして組んで、親の面には入口を1枚貼る」——人はダブルクリックで中に入れる。
# parent を省略すると current_project()（＝TexoS が最後に開いたプロジェクト）が使われる。
p.link_into(os.path.dirname(ROOT))
