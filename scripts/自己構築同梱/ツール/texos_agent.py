# -*- coding: utf-8 -*-
r"""AI が書いた組み立てスクリプトのための、プロジェクト相対の入口。

**生成スクリプトに絶対パスを1文字も書かせない**ためだけに在る。これがあるので AI は

    from texos_agent import new_project
    p = new_project("C言語実験")

と書くだけでよく、プロジェクトフォルダごと別の PC へコピーしても動く
（`texos_flow.py` は隣に同梱してあり、生成先はプロジェクト基準で解決される）。

配置（この2階層は動かさないこと）:

    <プロジェクト>/
      組み立てる.py        … 外部プログラムカードが呼ぶ踏み台
      ツール/
        texos_agent.py     … これ
        texos_flow.py      … 同梱したビルダー
      生成物/              … new_project() が掘る場所
"""
import os
import sys

ツール = os.path.dirname(os.path.abspath(__file__))
プロジェクト = os.path.dirname(ツール)
生成先 = os.path.join(プロジェクト, "生成物")

if ツール not in sys.path:
    sys.path.insert(0, ツール)

from texos_flow import Project  # noqa: E402

既定モデル = "qwen3.8:latest"

_使えない文字 = '\/:*?"<>|'


def new_project(name: str, model: str = 既定モデル) -> Project:
    r"""`生成物\<name>` に新しいプロジェクトを開いて返す。

    返るのは規約 §5 の `Project` そのもの。以降の API は規約のとおり。
    `name` はフォルダ名なのでパス区切りは書けない（絶対パスを渡す道を塞いである）。
    """
    name = (name or "").strip().rstrip(". ")
    if not name:
        raise ValueError("プロジェクト名が空です")
    if name in (".", "..") or any(c in name for c in _使えない文字):
        raise ValueError(
            f"プロジェクト名に使えない文字があります: {name!r}\n"
            "（フォルダ名だけを渡すこと。パスや絶対パスは書けません）"
        )
    os.makedirs(生成先, exist_ok=True)
    return Project(os.path.join(生成先, name), model=model)
