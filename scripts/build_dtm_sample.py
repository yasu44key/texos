# -*- coding: utf-8 -*-
"""「簡易DTM」のサンプルプロジェクトを組む。

TexoS で曲を作る形の手本。**曲の正本はアレンジ表という実身**で、
素材（wav）とプログラム（ChillChordMaker / wavmix）はその材料と道具にすぎない、
という組み方を1枚のキャンバスで見せる。

  ① 素材（プロジェクト直下のファイル）
       コード4枚（ChillChordMaker が書き出した wav）＋ ドラム2枚
  ② 設計   アレンジ表.json（bpm・どの素材を何小節目に置くか）← 書き換える場所
  ③ 合成   wavmix カード ── ▶実行 ──▶ 曲.wav
  おまけ   素材を作り直すカード（ChillChordMaker）／{in*} で束ねるカード

素材とアレンジ表の間に矢印は無い。アレンジ表は素材を**ファイル名で**指していて、
TexoS は外部プログラムを cwd＝プロジェクト直下で走らせるから `--dir "."` で届く。
（矢印で束ねる形も見せたいので、おまけとして {in*} のカードを1枚だけ置いてある）

道具は `.texos/filters/` に同梱するので、このフォルダごと zip で配れる
（受け手は解凍して開くだけ・絶対パスはどこにも入らない）。

実行前に TexoS を終了しておくこと（規約 §3-8）。新規フォルダなら開いていないので不要。
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import Project  # noqa: E402

ROOT = r"F:\yoshi\Documents\TexoS\サンプル_簡易DTM"
CCM = r"C:\Users\yoshi\dev\ChillChordMaker"
WAVMIX = r"C:\Users\yoshi\dev\wavmix"
ARRANGE = r"C:\Users\yoshi\dev\arrange"
DRUM_SRC = r"F:\yoshi\Documents\TexoSSound"   # 手持ちのドラム一発もの

BPM = 75
PROGRESSION = "D'Angelo Vamp"
SYNTH = "Clean LoFi"

os.makedirs(ROOT, exist_ok=True)
p = Project(ROOT)

# 道具を同梱（.texos/filters/<名前>/ 起点で解決される＝絶対パスなしで配れる）
ccm_dir = p.bundle(CCM, ignore=("venv", "__pycache__", ".git", ".claude", "*.lnk"))
mix_dir = p.bundle(WAVMIX, ignore=("venv", "__pycache__", ".git", ".claude"))
arr_dir = p.bundle(ARRANGE, ignore=("__pycache__", ".git", ".claude"))

# ── ① 素材 ────────────────────────────────────────────────────────────────
# コードは ChillChordMaker にその場で書き出させて、実身として取り込む
# （＝配る側は音源を用意しなくてよい。受け手も同じものを作り直せる）
tmp = tempfile.mkdtemp(prefix="ccm_")
subprocess.run(
    [os.path.join(CCM, "ccm.cmd"), "--progression", PROGRESSION, "--key-note", "C",
     "--bpm", str(BPM), "--synth-preset", SYNTH, "--prefix", "lofi", "--outdir", tmp],
    check=True,
)
chords = sorted(f for f in os.listdir(tmp) if f.endswith(".wav"))

p.shape(40, 40, 780, 470, label="① 素材 — プロジェクト直下のファイル", fill="#4a90d9")
chord_cards = [
    p.copy_file(os.path.join(tmp, name), 80 + i * 180, 130,
                memo=f"{PROGRESSION} の {i + 1} 番目のコード（{SYNTH} / {BPM}BPM）")
    for i, name in enumerate(chords)
]
shutil.rmtree(tmp, ignore_errors=True)

drums = [
    p.copy_file(os.path.join(DRUM_SRC, "BassDramKick.wav"), 80, 320, memo="キック（一発もの）"),
    p.copy_file(os.path.join(DRUM_SRC, "Snear.wav"), 260, 320, memo="スネア（一発もの）"),
]

p.note(
    "素材はプロジェクト直下のファイル（この枠のカード）。\n\n"
    "アレンジ表からは**ファイル名**で指すので、素材からアレンジ表への矢印は要りません。\n"
    "TexoS は外部プログラムをプロジェクト直下を cwd にして走らせるので、\n"
    "wavmix カードの --dir \".\" がこの枠を指しています。\n\n"
    "ここに wav を足す → アレンジ表に1行足す、が素材を増やす手順です。",
    460, 320, width=340,
)

# ── ② 設計 ────────────────────────────────────────────────────────────────
score = {
    "bpm": BPM,
    "tail": 2.0,
    "normalize": True,
    "tracks": [
        {"lane": "コード", "file": "lofi_*", "at": "1:1", "every": "1:0",
         "repeat": 2, "gain": -3},
        {"lane": "キック", "file": "BassDramKick", "at": "1:1", "every": "0:2",
         "repeat": 16, "gain": -2},
        {"lane": "スネア", "file": "Snear", "at": "1:3", "every": "1:0",
         "repeat": 8, "gain": -6},
    ],
}

p.shape(40, 560, 390, 330, label="② 設計 — 曲の正本", fill="#d9a04a")
hyou = p.file(
    "アレンジ表.json", json.dumps(score, ensure_ascii=False, indent=2), 80, 650,
    memo="曲の正本。ここを書き換えて ▶実行 すれば曲ができ直す。"
         "at=置く位置（小節:拍・1始まり）／every=並べる間隔／repeat=並べる回数／gain=dB",
)

p.note(
    "アレンジ表（ここを書き換える）\n\n"
    "・lofi_* … 名前順に並べて置く＝進行そのまま\n"
    "・every \"0:2\" repeat 16 … 2拍ごとに16回＝キックの打ち込み\n"
    "・at \"1:3\" every \"1:0\" … 毎小節の3拍目＝スネア\n"
    "・bpm を変えると全部が追従（コードの長さは素材のまま）\n\n"
    "TSV でも書けます（テキストカードで組みたいとき）。",
    250, 650, width=170,
)

# ── ③ 合成 ────────────────────────────────────────────────────────────────
p.shape(470, 560, 350, 330, label="③ 合成 — ここを ▶実行", fill="#4ad98f")
mix = p.program(
    "曲にする（wavmix）", 520, 650,
    name=rf"{mix_dir}\wavmix.cmd",
    cmd='--score "{in}" --dir "." --out "{out}"',
    mode="command", accept_mime="text/*", output_ext="wav",
    prefix="曲", timeout_secs=300,
    memo="アレンジ表を読み、プロジェクト直下の素材を混ぜて 曲.wav を作る。"
         "prefix があるので毎回同じ名前で上書きされる（作り直しに向く）。",
)
p.flow(hyou, mix)

# ── ②' 編集: アレンジ表を並べ直す（ランチャー）─────────────────────────────
p.shape(40, 920, 780, 250, label="②' 編集 — アレンジ表を並べ直す", fill="#d9a04a")
p.launcher(
    "アレンジを編集（ARRANGE）", 80, 1000,
    name=rf"{arr_dir}\arrange.cmd",
    memo="アレンジ表を小節のグリッドで編集する。TexoS が作業フォルダと "
         "TEXOS_PROJECT_DIR を渡すので、開くべきプロジェクトが画面に出る。"
         "黒い窓（ローカルサーバ）は閉じると終了。",
)
p.note(
    "アレンジ表は手で書いても、ARRANGE で並べ替えてもよい（同じファイル）。\n\n"
    "・「フォルダ」でこのプロジェクトを開く → アレンジ表.json を選ぶ\n"
    "・クリップを掴んで移動／素材をドラッグして追加／Delete で削除\n"
    "・▶ は試聴（目安）。**確定は「曲にする（wavmix）」カード**\n\n"
    "触っていないレーンは every / repeat の書き方のまま残り、\n"
    "動かしたレーンだけ clips（1クリップずつ）に変わる。\n"
    "だから手書きの表を GUI で開いても壊れない。",
    300, 1000, width=340,
)

# ── おまけ: 素材を作り直す（ジェネレータ）──────────────────────────────────
p.shape(860, 40, 360, 300, label="おまけ — 素材を作り直す", fill="#8f4ad9")
p.program(
    "コードを作る（ChillChordMaker）", 900, 130,
    name=rf"{ccm_dir}\ccm.cmd",
    cmd=f'--progression "{PROGRESSION}" --key-note C --bpm {BPM} '
        f'--synth-preset "{SYNTH}" --prefix pad --outdir "{{out*}}"',
    mode="generator", output_ext="wav", timeout_secs=300,
    memo="入力の要らない生成カード。▶実行 で pad_小節_拍_コード名.wav が一式できる。"
         "アレンジ表の lofi_* を pad_* に変えれば音色を差し替えられる。"
         "進行・キー・BPM・音色はこのカードの設定（cmd）で変える。",
)

# ── おまけ: 矢印で束ねる形（{in*} の例）───────────────────────────────────
p.shape(860, 380, 360, 300, label="おまけ — 矢印で束ねる（{in*}）", fill="#8f4ad9")
cat = p.program(
    "素材をつなげる", 900, 470,
    name=rf"{mix_dir}\wavmix.cmd",
    cmd='--mode cat --out "{out}" {in*}',
    mode="command", accept_mime="audio/*", output_ext="wav", prefix="つなげた素材",
    memo="アレンジ表を使わず、繋いだ素材を順に連結するだけの形。"
         "cmd に {in*} があるので集約（全入力を1回で受ける）。"
         "{in} だけだと入力ごとに別々に実行される（ファンアウト）。",
)
for d in drums:
    p.flow(d, cat)

# ── 説明 ──────────────────────────────────────────────────────────────────
p.file(
    "使い方.md",
    f"""# サンプル: 簡易DTM

TexoS で曲を組み立てる最小の形です。**曲の正本はアレンジ表**（`アレンジ表.json`）で、
wav もプログラムもその材料と道具です。

## まず動かす

1. 「曲にする（wavmix）」カードの ▶実行 を押す
2. `曲.wav` ができてカードになる。ダブルクリックで再生

## 変えてみる

- `アレンジ表.json` をダブルクリックして編集 → もう一度 ▶実行
  - `bpm` を変える／`gain` を変える／`repeat` を増やす
  - `at` は「小節:拍」で1始まり。`every` は並べる間隔、`repeat` は並べる回数
- 並べ替えを目で見てやりたいときは「アレンジを編集（ARRANGE）」を起動。
  同じ `アレンジ表.json` を読み書きするので、手で書いた表と行き来できる
  （触っていない行は書き方ごと残る）
- 「コードを作る（ChillChordMaker）」の ▶実行 で `pad_*.wav` が一式できる。
  アレンジ表の `lofi_*` を `pad_*` に書き換えると音色が差し替わる
- 素材を足したいときは、このフォルダに wav を置いて（またはカードを作って）
  アレンジ表に1行足す

## 仕組み

- 素材とアレンジ表の間に矢印はありません。アレンジ表は素材を**ファイル名**で指し、
  wavmix カードの `--dir "."` がプロジェクト直下を見ています
  （TexoS は外部プログラムをプロジェクト直下を cwd にして走らせます）
- 矢印で束ねる形も見たいときは「素材をつなげる」カード（`{{in*}}` の例）を見てください
- 道具は `.texos/filters/` に同梱してあります。**このフォルダごと zip で配れます**
  （絶対パスはどこにも入っていません。受け取った人は解凍して開くだけ）
- Python が要ります。初回実行時に必要なライブラリ（numpy / scipy）を
  自動で用意します（`%LOCALAPPDATA%` 配下に作るので、このフォルダは汚れません）

## 道具

- `ChillChordMaker` … コード進行を wav で書き出す（`.texos/filters/ChillChordMaker/`）
- `wavmix` … アレンジ表にしたがって wav を1本にまとめる（`.texos/filters/wavmix/`）
  書式は `.texos/filters/wavmix/README.md`
- `ARRANGE` … アレンジ表を並べて編集する（`.texos/filters/arrange/`）。
  ブラウザで動くので、フォルダの読み書きの許可を一度押す必要がある
""",
    460, 90,
    memo="このサンプルの説明（TexoS 上で読めるように実身として置いてある）",
)

p.view(x=20, y=20, scale=0.62)
p.save(reset=True)   # 手本として何度でも組み直せるように（既存カードは作り直す）
