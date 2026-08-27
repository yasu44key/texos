# -*- coding: utf-8 -*-
r"""「組み立て案.py」をこのプロジェクトを基準にして走らせるための踏み台。

外部プログラムカードは**作業ディレクトリがプロジェクト直下**で実行される
（`flow_run.rs :: execute_node` が `Some(root)` を渡す）。だからカード側を

    python.exe -X utf8 "組み立てる.py" "{in}" > "{out}" 2>&1

と書いておけば、ここから先は全部プロジェクト相対で解ける ——
案の側は絶対パスを1つも知らなくてよい。

案は `{in}` として一時フォルダに置かれた写しで渡ってくるので、`sys.path` には
**このファイルの場所**を入れる（写しの隣ではなく）。
"""
import os
import runpy
import sys

if len(sys.argv) < 2:
    raise SystemExit("使い方: python 組み立てる.py <組み立て案.py>")

ここ = os.path.dirname(os.path.abspath(__file__))
for p in (os.path.join(ここ, "ツール"), ここ):
    if p not in sys.path:
        sys.path.insert(0, p)

案 = os.path.abspath(sys.argv[1])
sys.argv = [案]
runpy.run_path(案, run_name="__main__")
