# -*- coding: utf-8 -*-
r"""AI自己構築の回帰テスト。依頼を投げて、組み立て → 試走まで通るかを測る。

各テーマについて:
  A. 組み立て … AI が書いた案が走り、save() が通ったか
  B. 試走     … 出来たプロジェクトの終端が最後まで流れたか
                （ask/loop/t2i/web_* は texos_dryrun が再現しないので N/A）

使い方: python scripts/自己構築回帰.py [テーマ名...]     （省略で全部）
"""
import importlib.util
import io
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
def リグの場所(model: str) -> str:
    """モデルごとに別のリグを使う（生成物が衝突すると save() が拒む）。"""
    slug = "".join(c if c.isalnum() else "_" for c in model)
    return os.path.join(os.environ.get("TEMP", "/tmp"), f"texos_回帰リグ_{slug}")


リグ = ""   # main で決める
修復ラウンド = 0   # --repair N で増やす
規約パス = os.path.join(REPO, "docs", "AIエージェント向け_フロー構築規約.md")

_spec = importlib.util.spec_from_file_location("bsb", os.path.join(SCRIPTS, "build_self_build.py"))
_bsb = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_bsb)
ROLE0, MODEL0, 規約を絞る = _bsb.ROLE, _bsb.MODEL, _bsb.規約を絞る
ROLE, MODEL = ROLE0, MODEL0

FENCE = "`" * 3
NL = chr(10)

テーマ: dict[str, str] = {
    "C言語実験": "初学者向けの C 言語の実験フローを構築したい。"
                 "課題 → ソース → コンパイル → 実行 が確認できるフローを作ってください。",
    "要約": "お題の付箋と参考資料の付箋2枚を材料に、AI にまとめて読ませて "
            "500字程度の要約を書き出すフローを作ってください。",
    "多視点合議": "ひとつの議題を、賛成派・反対派・中立の3人の AI に別々に検討させ、"
                  "最後に別の AI がそれをまとめて結論を出すフローを作ってください。"
                  "誰がどの担当かはキャンバスから読めるようにしてください。",
    "人の判断": "AI に企画案を書かせ、人がそれを読んで「採用」か「やり直し」かを答え、"
                "その答えを反映した最終文書を書き出すフローを作ってください。"
                "人が答えるまで先へ進まないようにしてください。",
    "推敲反復": "キャッチコピーを AI に書かせ、合格が出るまで最大3回まで練り直させる"
                "フローを作ってください。各周の結果が後から読めるようにしてください。",
    "ログ集計": "テキストのログから特定の語を含む行だけを抜き出し、"
                "並べ替えて重複を消し、行数を数えて結果を書き出すフローを作ってください。",
    "Web要約": "Web ページの URL を書いておくと、その本文を取ってきて "
               "AI が日本語で要約するフローを作ってください。",
    "CSV整形": "CSV ファイルから2列目と4列目だけを取り出し、並べ替えて "
               "先頭20行を書き出すフローを作ってください。",
    "Python実験": "AI に Python のコードを書かせ、それを実行して結果を見せるフローを"
                  "作ってください。課題は人が付箋に書き換えられるようにしてください。",
    "画像縮小": "フォルダに置いた画像を横幅800pxに縮めて PNG で書き出すフローを"
                "作ってください。",
}

# --- 未見テーマ（2026-08-27 追加）------------------------------------------------
# 上の10本は規約をそれに合わせて直してきたので、過学習している。
# **この5本には一度も規約を合わせていない。** 上との通過率の差が過学習の量。
# 直すのは「この5本が踏んだ穴」だけにすること（そうしないと同じことを繰り返す）。
未見: dict[str, str] = {
    "文字起こし": "録音した音声ファイルを置くと、その内容を文字に起こして "
                  "テキストで書き出すフローを作ってください。",
    "ファイル一覧": "指定したフォルダの中にあるファイルの名前と大きさを一覧にして、"
                    "見やすく並べた表を書き出すフローを作ってください。",
    "定期実行": "毎朝9時に自動で動いて、決まったフォルダの中身を数えて記録する"
                "フローを作ってください。人が ▶ を押さなくても動くようにしてください。",
    "画像から画像": "画像を置くと、AI がその内容を言葉で説明し、その説明をもとに"
                    "別の画像を生成するフローを作ってください。",
    "前回との差分": "同じ調査を2回まわしたとき、前回の結果と今回の結果の"
                    "違うところだけを取り出して報告するフローを作ってください。",
}
テーマ.update(未見)

再現しない = ("ask", "loop", "t2i", "t23d", "web_", "speak", "問い合わせ", "反復")


def リグを用意(reset: bool = False) -> None:
    if reset and os.path.isdir(リグ):
        shutil.rmtree(リグ, ignore_errors=True)
    os.makedirs(os.path.join(リグ, "ツール"), exist_ok=True)
    os.makedirs(os.path.join(リグ, "生成物"), exist_ok=True)
    同梱 = os.path.join(SCRIPTS, "自己構築同梱")
    shutil.copy2(os.path.join(同梱, "組み立てる.py"), リグ)
    shutil.copy2(os.path.join(同梱, "ツール", "texos_agent.py"), os.path.join(リグ, "ツール"))
    shutil.copy2(os.path.join(SCRIPTS, "texos_flow.py"), os.path.join(リグ, "ツール"))


def 聞く(会話: list[dict]) -> tuple[dict, float]:
    body = {"model": MODEL, "stream": False, "keep_alive": "30m",
            "messages": [{"role": "system", "content": ROLE}] + 会話,
            "options": {"temperature": 0.2}}
    t0 = time.time()
    req = urllib.request.Request("http://localhost:11434/api/chat",
                                 data=json.dumps(body).encode("utf-8"),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.load(r), time.time() - t0


def 切り出す(md: str) -> str | None:
    """flow.rs の md_codeblock と同じ行ベース規則（行頭の ``` だけが囲い）。"""
    inb, cur, found, lang = False, [], None, ""
    for line in md.splitlines():
        t = line.lstrip()
        if not inb:
            if t.startswith(FENCE):
                inb, lang, cur = True, t[3:].strip().lower(), []
        elif t.startswith(FENCE):
            inb = False
            if found is None and lang in ("python", "py"):
                found = "\n".join(cur)
        else:
            cur.append(line)
    return found


def 走らせる(argv: list[str], cwd: str, timeout: int = 900) -> subprocess.CompletedProcess:
    env = dict(os.environ, PYTHONUTF8="1", PYTHONIOENCODING="utf-8")
    return subprocess.run(argv, cwd=cwd, capture_output=True, text=True,
                          encoding="utf-8", errors="replace", env=env, timeout=timeout)


def 一本(name: str, 規約: str, 記録先: str) -> dict:
    """1テーマ。組み立てが落ちたら、エラーを見せて直させる（最大 修復ラウンド 回）。

    エラー文（`TypeError: note() got an unexpected keyword argument 'memo'`）は、
    **必要な仕様が必要な瞬間にピンポイントで届く**という意味で規約より情報密度が高い。
    偶発的な取りこぼし（引数の数え違い・構文の滑り）はここでしか直らない。
    """
    依頼 = f"依頼（ここを書き換えて ▶ を押す）{NL}{NL}{テーマ[name]}{NL}{NL}フォルダ名: {name}{NL}"
    会話 = [{"role": "user", "content": 規約 + f"{NL}{NL}---{NL}{NL}" + 依頼}]
    r = {"テーマ": name, "秒": 0, "prompt": None, "out": 0, "A": "-", "B": "-",
         "理由": "", "修復": 0}
    先 = os.path.join(リグ, "生成物", name)
    shutil.rmtree(先, ignore_errors=True)
    生成物 = os.path.join(リグ, "生成物")
    os.makedirs(生成物, exist_ok=True)
    前 = set(os.listdir(生成物))

    for 回 in range(修復ラウンド + 1):
        # モデルが固まることがある（小さいモデルで実測）。1本の道連れで15本を落とさない。
        try:
            v, dt = 聞く(会話)
        except Exception as e:
            r["A"], r["理由"] = "NG", f"モデルが応答しない（{type(e).__name__}）"
            return r
        生md = v["message"]["content"]
        r["秒"] += round(dt)
        r["out"] += v.get("eval_count") or 0
        if r["prompt"] is None:
            r["prompt"] = v.get("prompt_eval_count")
        r["修復"] = 回
        io.open(os.path.join(記録先, f"{name}_AI出力{回 or ''}.md"), "w",
                encoding="utf-8").write(生md)

        code = 切り出す(生md)
        if not code:
            失敗 = "コードブロックが出ない"
        else:
            案 = os.path.join(記録先, f"{name}.py")
            io.open(案, "w", encoding="utf-8", newline=NL).write(code + NL)
            proc = 走らせる([sys.executable, "-X", "utf8", "組み立てる.py", 案], リグ, 300)
            if proc.returncode == 0:
                r["A"] = "OK"
                break
            失敗 = ((proc.stderr or proc.stdout).strip().splitlines() or ["不明"])[-1]
        r["A"], r["理由"] = "NG", 失敗[:160]
        if 回 >= 修復ラウンド:
            return r
        # 修復ラウンド: 直前の答えと、その実行結果を見せて出し直させる
        会話 += [
            {"role": "assistant", "content": 生md},
            {"role": "user", "content":
             f"上のスクリプトを実行したらエラーになりました。{NL}{NL}"
             f"```{NL}{失敗[:600]}{NL}```{NL}{NL}"
             f"原因を直したスクリプト**全体**を、同じ決まり"
             f"（説明を書かず ```python のコードブロック1つだけ）で出し直してください。"},
        ]

    if not os.path.isdir(先):
        # 依頼の「フォルダ名」に従わないことがある。組み立て自体は成功しているので、
        # 新しく出来たフォルダを拾って試走まで進める（名前の厳密一致は本質ではない）。
        新 = [d for d in set(os.listdir(生成物)) - 前
              if os.path.isdir(os.path.join(生成物, d))]
        if len(新) != 1:
            r["A"], r["理由"] = "NG", f"生成物が特定できない（{新 or 'なし'}）"
            return r
        r["理由"] = f"フォルダ名が依頼と違う（{新[0]}）"
        先 = os.path.join(生成物, 新[0])
    d = 走らせる([sys.executable, "-X", "utf8",
                  os.path.join(SCRIPTS, "texos_dryrun.py"), 先], REPO, 1800)
    out = (d.stdout or "") + (d.returncode and (NL + (d.stderr or "")) or "")
    io.open(os.path.join(記録先, f"{name}_試走.txt"), "w", encoding="utf-8").write(out)
    if "試走で止まりました" in out:
        理由 = out.split("試走で止まりました:", 1)[1].strip().splitlines()[0][:160]
        r["B"] = "N/A" if ("再現しません" in 理由 or "試走の対象外" in 理由) else "NG"
        r["理由"] = 理由
    elif d.returncode != 0:
        行 = ((d.stderr or "") + (d.stdout or "")).strip().splitlines()
        r["B"], r["理由"] = "NG", (行[-1][:160] if 行 else f"試走が異常終了（rc={d.returncode}）")
    else:
        空 = [m.group(1) for m in re.finditer(r"^\s+(\S+)\s+\(0 バイト\)", out, re.M)]
        if "まだファイルが投入されていません" in out:
            r["B"], r["理由"] = "N/A", "ドロップポイント待ち（人がファイルを置いてから動く）"
        elif 空:
            r["B"], r["理由"] = "NG", f"出力が0バイト: {', '.join(空)}（§3-14）"
        else:
            r["B"] = "OK"
    return r


def モデルを降ろす(model: str) -> None:
    """次のモデルを載せる前に VRAM を空ける（24GB に2本は載らない）。"""
    try:
        subprocess.run(["ollama", "stop", model], capture_output=True, timeout=120)
    except Exception:
        pass


def 一巡(model: str, 名: list[str], 規約: str) -> list[dict]:
    global リグ, ROLE, MODEL
    リグ, MODEL = リグの場所(model), model
    # 役割の骨格に埋まっているモデル名も差し替える（生成されるフローの AI カードも揃える）
    ROLE = ROLE0.replace(MODEL0, model)
    リグを用意(reset=True)
    記録先 = os.path.join(リグ, "記録")
    os.makedirs(記録先, exist_ok=True)
    print(f"\n===== {model} =====")
    結果 = []
    for n in 名:
        r = 一本(n, 規約, 記録先)
        r["モデル"] = model
        結果.append(r)
        print(f"  {r['A']:>3} {r['B']:>3}  {n:<10} {r['秒']:>4}秒 "
              f"修復{r['修復']}  out={r['out']}  {r['理由']}")
    io.open(os.path.join(記録先, "結果.json"), "w", encoding="utf-8").write(
        json.dumps(結果, ensure_ascii=False, indent=2))
    モデルを降ろす(model)
    return 結果


def main() -> None:
    global 修復ラウンド
    旗 = [a for a in sys.argv[1:] if a.startswith("--")]
    for f in 旗:
        if f.startswith("--repair"):
            修復ラウンド = int(f.split("=")[1]) if "=" in f else 2
    署名 = "--no-sig" not in 旗
    引数 = [a for a in sys.argv[1:] if not a.startswith("--")]
    models = [a for a in 引数 if ":" in a] or [MODEL0]
    名 = [a for a in 引数 if ":" not in a] or list(テーマ)
    原文 = io.open(規約パス, encoding="utf-8").read()
    規約 = 規約を絞る(原文, 署名表=署名)
    print(f"規約 {len(原文):,}→{len(規約):,}字 / 役割 {len(ROLE0):,}字")
    print(f"モデル {len(models)} 本 × テーマ {len(名)} 本"
          f" / 署名表={'あり' if 署名 else 'なし'} / 修復={修復ラウンド}ラウンド")
    全部 = []
    for m in models:
        全部 += 一巡(m, 名, 規約)
    # まとめ
    print("\n===== まとめ =====")
    print(f"  {'モデル':<26} 組立  試走OK  N/A  失敗  中央値秒")
    for m in models:
        rs = [r for r in 全部 if r["モデル"] == m]
        a = sum(1 for r in rs if r["A"] == "OK")
        b = sum(1 for r in rs if r["B"] == "OK")
        na = sum(1 for r in rs if r["B"] == "N/A")
        ng = sum(1 for r in rs if r["A"] == "NG" or r["B"] == "NG")
        秒 = sorted(r["秒"] for r in rs)[len(rs) // 2] if rs else 0
        直 = sum(r["修復"] for r in rs)
        print(f"  {m:<26} {a:>2}/{len(rs)}  {b:>4}  {na:>4}  {ng:>4}  {秒:>7}  修復計{直}")
    out = os.path.join(os.environ.get("TEMP", "/tmp"), "texos_モデル比較.json")
    io.open(out, "w", encoding="utf-8").write(json.dumps(全部, ensure_ascii=False, indent=2))
    print(f"\n生データ → {out}")


if __name__ == "__main__":
    main()
