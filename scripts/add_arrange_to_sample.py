# -*- coding: utf-8 -*-
"""できあがっている「サンプル_簡易DTM」に、ARRANGE（アレンジ表エディタ）を後から足す。

`build_dtm_sample.py` を流し直せば同じ形になるが、それだと **アレンジ表.json が
作り直されて、手で直した内容が消える**。既に触っているプロジェクトなので、
足りないものだけを継ぎ足す。

やること:
  1. `.texos/filters/arrange/` に ARRANGE を同梱
  2. ランチャーカード「アレンジを編集（ARRANGE）」を1枚置く
  3. 説明の付箋と枠を置く
既にあるカード・実身・矢印には触らない（同名のカードがあれば何もしない）。

TexoS で対象プロジェクトを開いたまま流しても、画面には出ない（規約 §3-8）。
実行後にプロジェクトを開き直すこと。
"""
import json
import os
import shutil
import sqlite3
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import LAUNCHER_SENTINEL, NOTE_SENTINEL  # noqa: E402

ROOT = r"F:\yoshi\Documents\TexoS\サンプル_簡易DTM"
ARRANGE = r"C:\Users\yoshi\dev\arrange"
LABEL = "アレンジを編集（ARRANGE）"

NOTE = (
    "アレンジ表は手で書いても、ARRANGE で並べ替えてもよい（同じファイル）。\n\n"
    "・「フォルダ」でこのプロジェクトを開く → アレンジ表.json を選ぶ\n"
    "・クリップを掴んで移動／素材をドラッグして追加／Delete で削除\n"
    "・▶ は試聴（目安）。確定は「曲にする（wavmix）」カード\n\n"
    "触っていないレーンは every / repeat の書き方のまま残り、\n"
    "動かしたレーンだけ clips（1クリップずつ）に変わる。\n"
    "だから手書きの表を GUI で開いても壊れない。"
)


def now() -> int:
    return int(time.time() * 1000)


def main() -> int:
    db = os.path.join(ROOT, ".texos", "texos.db")
    if not os.path.isfile(db):
        print(f"プロジェクトが見つかりません: {db}")
        return 1

    # 1. 同梱（毎回入れ替え＝dev 側の更新を配れる）
    dest = os.path.join(ROOT, ".texos", "filters", "arrange")
    shutil.rmtree(dest, ignore_errors=True)
    shutil.copytree(ARRANGE, dest,
                    ignore=shutil.ignore_patterns("__pycache__", ".git", ".claude"))
    print(f"同梱: .texos/filters/arrange/  ({len(os.listdir(dest))} ファイル)")

    conn = sqlite3.connect(db)
    conn.execute("PRAGMA journal_mode=WAL")
    dup = conn.execute(
        "SELECT COUNT(*) FROM virtual_bodies WHERE label = ? AND deleted_at IS NULL",
        (LABEL,)).fetchone()[0]
    if dup:
        print(f"カード「{LABEL}」は既にあります（同梱だけ更新しました）")
        conn.commit()
        conn.close()
        return 0

    conn.execute(
        "INSERT OR IGNORE INTO real_bodies (id,mime_type,size,created_at) VALUES (?,?,0,?)",
        (LAUNCHER_SENTINEL, "application/vnd.texos.launcher", now()))
    conn.execute(
        "INSERT OR IGNORE INTO real_bodies (id,mime_type,size,created_at) VALUES (?,?,0,?)",
        (NOTE_SENTINEL, "text/vnd.texos.note", now()))

    spec = {"name": r"arrange\arrange.cmd", "type": "launcher"}
    conn.execute(
        "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,kind,filter_ref,memo)"
        " VALUES (?,?,?,?,?,?,'launcher',?,?)",
        (uuid.uuid4().hex, LAUNCHER_SENTINEL, LABEL, 80, 1000, now(),
         json.dumps(spec, ensure_ascii=False),
         "アレンジ表を小節のグリッドで編集する。TexoS が作業フォルダと "
         "TEXOS_PROJECT_DIR を渡すので、開くべきプロジェクトが画面に出る。"
         "黒い窓（ローカルサーバ）は閉じると終了。"))
    conn.execute(
        "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,kind,note_text,style_ref)"
        " VALUES (?,?,?,?,?,?,'note',?,?)",
        (uuid.uuid4().hex, NOTE_SENTINEL, NOTE.splitlines()[0][:40], 300, 1000, now(),
         NOTE, json.dumps({"width": 340, "font_size": 13}, ensure_ascii=False)))
    conn.execute(
        "INSERT INTO shapes (id,kind,pos_x,pos_y,width,height,rotation,fill,opacity,"
        "z_order,label,created_at) VALUES (?,'rect',?,?,?,?,0,?,?,0,?,?)",
        (uuid.uuid4().hex, 40, 920, 780, 250, "#d9a04a", 0.15,
         "②' 編集 — アレンジ表を並べ直す", now()))
    conn.commit()
    conn.close()
    print(f"追加: ランチャー「{LABEL}」／説明の付箋／枠")
    print("TexoS でこのプロジェクトを開き直すと出ます。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
