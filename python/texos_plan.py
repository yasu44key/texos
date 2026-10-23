"""TexoS コンストラクタの設計図を検査する（v1.12・docs/V1.12_声の窓口と思考役計画.md §6）。

コンストラクタカード（builtin:"construct"）が AI に書かせた設計図（ビルダーの呼び出し一覧の JSON）を、
`texos_flow.Project` の呼び出しに直して `validate()` を通し、カード・矢印・設定を JSON で返す。
**DB には書かない**（save() を呼ばない）。並べるのは TexoS 自身（construct.rs）。

検査の正本は texos_flow.py（規約違反を理由つきで弾く）。ここは訳すだけで、ルールを持たない。
AI に任意の Python を書かせて実行する方式（AI自己構築）は採らない — 受け付ける call は下の表だけ。

    stdin : {"plan": {...}, "model": "...", "provider": "...", "endpoint": "..."}
    stdout: {"ok": bool, "errors": [...], "warnings": [...], "title": "...",
             "cards": [...], "edges": [...], "terminals": [[label, 出力名], ...]}
"""
import io
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import BuildError, Project  # noqa: E402

# call 名 → (Project のメソッド名, 先頭の位置引数のキー)。座標は並べる側が決めるので 0,0 を渡す。
# copy_file・bundle・launcher（PC 上の任意のファイル・プログラムに触る）と発行系の op は受け付けない。
CARD_CALLS = {
    "note": ("note", ["text"]),
    "file": ("file", ["name", "text"]),
    "ai": ("ai", ["label"]),
    "cat": ("cat", ["label"]),
    "builtin": ("builtin", ["label"]),
    "program": ("program", ["label"]),
    "tap": ("tap", ["label"]),
    "ask": ("ask", ["label"]),
    "loop": ("loop", ["label"]),
    "op_in": ("op_in", ["label"]),
    "op_out": ("op_out", ["label"]),
}
# 設計図の作法として書かせない引数（座標は並べる側・幅などは見た目）。
DROP_KEYS = {"call", "id", "x", "y"}

# この PC 固有の値（Whisper の場所・モデル・マイク名）が要るカード。AI には名前だけ書かせ、
# 中身は TexoS の UI と同じ設定にする（App.tsx whisperSpec / to_wav16k / mic_record:）。
# {{WHISPER_CLI}} {{WHISPER_MODEL}} {{MIC_DEVICE}} は並べる側（construct.rs resolve_presets）が埋める。
PRESETS = {
    "whisper": lambda c: dict(
        name="whisper-cli.exe", path="{{WHISPER_CLI}}", mode="command",
        cmd='-m "{{WHISPER_MODEL}}" -f {in} -otxt -of {out_noext} -l ' + str(c.get("lang") or "ja"),
        output_ext="txt", accept_mime="audio/*", timeout_secs=1800),
    "to_wav16k": lambda c: dict(
        name="ffmpeg.exe", mode="command", cmd="-y -i {in} -ar 16000 -ac 1 -vn {out}",
        output_ext="wav", accept_mime="audio/*", timeout_secs=600),
    "mic_record": lambda c: dict(
        name="ffmpeg.exe", mode="generator",
        cmd='-hide_banner -f dshow -i "audio={{MIC_DEVICE}}" -ar 16000 -ac 1 -y {out}',
        output_ext="wav", accept_mime="audio/*", timeout_secs=0),
}
PRESET_EXTRA = {"mic_record": {"record": True, "stop_cmd": "q"}}


def layout(cards, edges):
    """矢印の深さで列（左→右）、同じ深さは置いた順に縦。戻り矢印（previous）は辿らない。"""
    ins = {cid: [] for cid in cards}
    for f, t, role in edges:
        if role != "previous":
            ins[t].append(f)
    depth = {}

    def d(c, seen=()):
        if c in depth:
            return depth[c]
        if c in seen:
            return 0
        v = 0 if not ins[c] else 1 + max(d(u, seen + (c,)) for u in ins[c])
        depth[c] = v
        return v

    for c in cards:
        d(c)
    # 役割の付箋は相手の1つ左の列へ寄せる（付箋は入力を持たないので 0 列に溜まりがち）。
    for f, t, role in edges:
        if role in ("system", "answer") and cards[f]["kind"] == "note":
            depth[f] = max(0, depth[t] - 1)
    rows = {}
    pos = {}
    for c in cards:  # dict は挿入順＝設計図の順
        col = depth[c]
        r = rows.get(col, 0)
        rows[col] = r + 1
        pos[c] = (col * 340.0, r * 200.0)
    return pos


def main():
    req = json.load(io.TextIOWrapper(sys.stdin.buffer, encoding="utf-8"))
    plan = req.get("plan") or {}
    errors, warnings = [], []
    if not isinstance(plan, dict) or not isinstance(plan.get("calls"), list):
        return {"ok": False, "errors": ['設計図は {"title": ..., "calls": [...]} の形にしてください'],
                "warnings": []}
    p = Project(os.path.join(os.getcwd(), "_plan"), model=req.get("model") or "gemma3:latest",
                provider=req.get("provider") or "ollama")
    ids = {}       # 設計図の id → ビルダーのカード id
    keys = {}      # ビルダーのカード id → 設計図の id
    for i, c in enumerate(plan["calls"]):
        where = f"calls[{i}]"
        if not isinstance(c, dict) or "call" not in c:
            errors.append(f"{where}: call がありません")
            continue
        name = c["call"]
        if name == "flow":
            src, dst = ids.get(c.get("from")), ids.get(c.get("to"))
            if not src or not dst:
                errors.append(f"{where}: flow の from/to は先に作ったカードの id にしてください"
                              f"（from={c.get('from')!r} to={c.get('to')!r}）")
                continue
            try:
                p.flow(src, dst, role=c.get("role"))
            except BuildError as e:
                errors.append(f"{where}: {e}")
            continue
        if name in PRESETS:
            cid_key = c.get("id")
            if not cid_key or cid_key in ids:
                errors.append(f"{where}: id が無いか重複しています（{cid_key!r}）")
                continue
            if "label" not in c:
                errors.append(f"{where}（{cid_key}）: {name} には label が要ります")
                continue
            kw = PRESETS[name](c)
            if c.get("prefix"):
                kw["prefix"] = c["prefix"]
            vid = p.program(c["label"], 0, 0, **kw)
            p._cards[vid]["spec"].update(PRESET_EXTRA.get(name, {}))
            ids[cid_key] = vid
            keys[vid] = cid_key
            continue
        if name not in CARD_CALLS:
            errors.append(f"{where}: call「{name}」は使えません（使えるもの: "
                          f"{', '.join(list(CARD_CALLS) + list(PRESETS) + ['flow'])}）")
            continue
        cid_key = c.get("id")
        if not cid_key or cid_key in ids:
            errors.append(f"{where}: id が無いか重複しています（{cid_key!r}）")
            continue
        meth, pos_keys = CARD_CALLS[name]
        args = []
        kw = {k: v for k, v in c.items() if k not in DROP_KEYS and k not in pos_keys}
        missing = [k for k in pos_keys if k not in c]
        if missing:
            errors.append(f"{where}（{cid_key}）: {name} には {', '.join(missing)} が要ります")
            continue
        args = [c[k] for k in pos_keys]
        if name == "file":
            fname = str(c["name"])
            if os.path.isabs(fname) or ".." in fname.replace("\\", "/").split("/"):
                errors.append(f"{where}（{cid_key}）: file の name はプロジェクトからの相対名にしてください（{fname}）")
                continue
            if not isinstance(c["text"], str):
                errors.append(f"{where}（{cid_key}）: file の text は文字列にしてください")
                continue
        if name == "ai" and req.get("endpoint") and "endpoint" not in kw and "provider" not in kw:
            kw["endpoint"] = req["endpoint"]
        try:
            vid = getattr(p, meth)(*args, 0, 0, **kw)
        except BuildError as e:
            errors.append(f"{where}（{cid_key}）: {e}")
            continue
        except TypeError as e:
            errors.append(f"{where}（{cid_key}）: {name} の引数が違います（{e}）")
            continue
        ids[cid_key] = vid
        keys[vid] = cid_key
    if not ids and not errors:
        errors.append("カードが1枚もありません")
    # 呼び出しの誤りがあっても、組めた分の配線は検査する（直す材料をまとめて AI に返す）。
    if ids:
        try:
            warnings = p.validate()
        except BuildError as e:
            errors.extend(l.strip()[2:] if l.strip().startswith("- ") else l.strip()
                          for l in str(e).splitlines()[1:] if l.strip())
    if errors:
        return {"ok": False, "errors": errors, "warnings": warnings}
    pos = layout(p._cards, p._edges)
    root = p.root
    cards = []
    for vid, c in p._cards.items():
        out = {"key": keys[vid], "label": c["label"], "kind": c["kind"], "spec": c.get("spec"),
               "x": pos[vid][0], "y": pos[vid][1], "memo": c.get("memo")}
        if c["kind"] == "note":
            out["note_text"] = c.get("note_text", "")
            out["style"] = c.get("style")
        if c["kind"] == "file":
            rel = os.path.relpath(c["path"], root)
            data = next(b for (path, b, fid) in p._files if fid == vid)
            out["file"] = {"name": rel.replace("\\", "/"), "text": data.decode("utf-8", "replace")}
        cards.append(out)
    edges = [{"from": keys[f], "to": keys[t], "role": r} for f, t, r in p._edges]
    return {"ok": True, "errors": [], "warnings": warnings, "title": str(plan.get("title") or "構築"),
            "cards": cards, "edges": edges, "terminals": p.terminals()}


if __name__ == "__main__":
    try:
        res = main()
    except Exception as e:  # 想定外は理由つきで返す（並べる側がレポートに出す）
        res = {"ok": False, "errors": [f"検査役の内部エラー: {type(e).__name__}: {e}"], "warnings": []}
    sys.stdout.buffer.write(json.dumps(res, ensure_ascii=False).encode("utf-8"))
