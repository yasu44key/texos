# -*- coding: utf-8 -*-
"""組んだフローを TexoS を起動せずに1周流す（試走）。

規約 `docs/AIエージェント向け_フロー構築規約.md` §6 は「納品前に必ず1周走らせて出力を
読むこと」を求めるが、▶ を押すには GUI が要る。この足りない半分を埋める道具。
DB は**読むだけ**で、出力はプロジェクト直下ではなく `試走結果_YYYY-MM-DD/` に置く。

    python scripts/texos_dryrun.py <プロジェクトのフォルダ>
    python scripts/texos_dryrun.py <フォルダ> --card "まとめ"      # 特定の終端だけ
    python scripts/texos_dryrun.py <フォルダ> --list               # 終端の一覧だけ見る

**これは TexoS ではない。** pull 評価の骨（終端から遡る・入力順は接続順・役割ポートは
入力に数えない・型が合わない入力は黙って飛ばす・集約以外はファンアウトする）を写した
確認用の実装であって、本物は `src-tauri/src/flow_run.rs`。ここで通ったからといって
TexoS で必ず通るとは限らない —— **通らなかったものは確実に直す価値がある**、という向きで使う。

再現するもの: file / note / IN・OUT / 記録(tap) / AI(ollama・openai互換) /
連結・差分・横結合 / text_* / md_codeblock / 外部プログラム。
再現しないもの: 問い合わせ(ask)・反復(loop)・t2i・t23d・web_*・speak など
（当たったらその場で止めて名前を告げる）。
"""
from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

AGGREGATE_BUILTINS = {"text_concat", "text_diff", "text_paste"}
UNSUPPORTED = {
    "ask": "問い合わせカード（人の答えを待つので試走では止まる）",
    "loop": "反復カード（周回制御は flow_run.rs の実行文脈に依存する）",
    "t2i": "画像生成（常駐デーモンが要る）",
    "t23d": "3D生成（常駐デーモンが要る）",
    "web_fetch": "Web取得", "web_feed": "フィード取得", "web_capture": "ページ保存",
    "web_search": "Web検索（SearXNG デーモンが要る）",
    "to_url": "URLカード化", "speak": "読み上げ", "import_new": "新規カードの登録",
    "voice_send": "会話へ送る（稼働中の会話コマンドが要る）",
    "select": "セレクタ",
}
_AGG_CMD = re.compile(r"\{in\*\}|\{in([2-9]|[1-9]\d+)\}")


class DryRunError(Exception):
    pass


class Payload:
    __slots__ = ("data", "ext", "mime", "stem")

    def __init__(self, data: bytes, ext: str, mime: str, stem: str):
        self.data, self.ext, self.mime, self.stem = data, ext, mime, stem

    @property
    def text(self) -> str:
        return self.data.decode("utf-8", "replace")


def mime_matches(accept: str, mime: str) -> bool:
    """flow.rs: mime_matches と同じ規則（空・*/* は全通し／text/* は text 系＋JSON）。"""
    if not accept or accept == "*/*":
        return True
    if accept.endswith("/*"):
        prefix = accept[:-2]
        if prefix == "text" and mime == "application/json":
            return True
        return mime.startswith(prefix + "/")
    return accept == mime


class DryRun:
    def __init__(self, root: str, verbose: bool = True):
        self.root = os.path.abspath(root)
        self.verbose = verbose
        db = os.path.join(self.root, ".texos", "texos.db")
        if not os.path.exists(db):
            raise DryRunError(f"TexoS のプロジェクトではありません: {self.root}")
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        self.cards: dict[str, dict] = {}
        for cid, label, kind, ref, note, cpath, ext, mime in conn.execute(
            "SELECT v.id, v.label, v.kind, v.filter_ref, v.note_text, v.committed_path,"
            " COALESCE(v.ext, r.ext), COALESCE(v.mime_type, r.mime_type)"
            " FROM virtual_bodies v LEFT JOIN real_bodies r ON r.id = v.real_id"
            " WHERE v.deleted_at IS NULL"
        ):
            spec = {}
            if ref:
                try:
                    spec = json.loads(ref)
                except ValueError:
                    spec = {}
            # v1.9.1 以降の TexoS はプロジェクト内のパスを root 基準の相対で保存する
            # （開いた時に絶対→相対へ直す）。ここで root に繋いで読む。
            if cpath and not os.path.isabs(cpath):
                cpath = os.path.join(self.root, cpath)
            self.cards[cid] = dict(label=label or "", kind=kind or "", spec=spec,
                                   note=note, path=cpath, ext=ext, mime=mime)
        self.edges: list[tuple[str, str, str | None]] = []
        for f, t, attrs in conn.execute(
            "SELECT from_id, to_id, attrs FROM relations"
            " WHERE kind='flow' AND deleted_at IS NULL ORDER BY created_at"
        ):
            role = None
            if attrs:
                try:
                    role = (json.loads(attrs) or {}).get("role")
                except ValueError:
                    role = None
            self.edges.append((f, t, role))
        conn.close()
        self._cache: dict[str, list[Payload]] = {}
        self._stack: list[str] = []
        self.notes: list[str] = []          # スキップ・警告
        self.written: list[str] = []        # 試走で書いたファイル
        self.out_dir = os.path.join(self.root, f"試走結果_{time.strftime('%Y-%m-%d')}")

    # ------------------------------------------------------------------ 情報
    def label(self, cid: str) -> str:
        c = self.cards.get(cid)
        if not c:
            return "?"
        return c["label"] or (c["note"] or "").splitlines()[0][:20] or "(無題)"

    def terminals(self) -> list[str]:
        """実行できて、下流にもう処理カードが無いもの（フィルタ or OUT）。

        「出ていく矢印が無い＝終端」ではない。**1回でも ▶ を押すと、終端カードから
        出来た出力カードへ `flow` の矢印が生える**ので、実行済みのプロジェクトでは
        その判定が必ず 0 枚になる（2026-08-21 実機で踏んだ）。
        見るべきは「下流に処理カード（filter / op）があるか」。
        """
        # v1.8 ④ 反復の戻り矢印（role="previous"）は**下流ではない**。評価では遡らず、
        # 値は反復カードが実行文脈へ置いたものを読むだけ（規約 §3-11）。これを下流と
        # 数えると、反復カードが終端から外れて「終端 0 枚」になる（2026-08-27 実機）。
        consumed = {
            f for f, t, role in self.edges
            if role != "previous"
            and self.cards.get(t, {}).get("kind") in ("filter", "op")
        }
        out = []
        for cid, c in self.cards.items():
            if cid in consumed:
                continue
            if c["kind"] == "filter":
                out.append(cid)
            elif c["kind"] == "op" and c["spec"].get("op") == "out":
                out.append(cid)
        return out

    def find(self, needle: str) -> str:
        for cid, c in self.cards.items():
            if cid == needle or c["label"] == needle:
                return cid
        hits = [cid for cid, c in self.cards.items() if needle in (c["label"] or "")]
        if len(hits) == 1:
            return hits[0]
        if not hits:
            raise DryRunError(f"カードが見つかりません: {needle}")
        raise DryRunError(f"カード名が曖昧です: {[self.label(h) for h in hits]}")

    # ------------------------------------------------------------------ 評価
    def eval(self, cid: str) -> list[Payload]:
        if cid in self._cache:
            return self._cache[cid]
        if cid in self._stack:
            raise DryRunError("フローに循環があります: " + " → ".join(self.label(c) for c in self._stack + [cid]))
        self._stack.append(cid)
        try:
            out = self._eval(cid)
        finally:
            self._stack.pop()
        self._cache[cid] = out
        return out

    def _role_text(self, cid: str, role: str) -> str | None:
        for f, t, r in self.edges:
            if t == cid and r == role:
                ups = self.eval(f)
                return ups[0].text if ups else ""
        return None

    def _inputs(self, cid: str, accept: str) -> list[Payload]:
        """通常入力（role 付きは入力に数えない）。接続順のまま、型が合わないものは飛ばす。"""
        got: list[Payload] = []
        for f, t, r in self.edges:
            if t != cid or r in ("system", "answer", "previous"):
                continue
            for pl in self.eval(f):
                if mime_matches(accept, pl.mime):
                    got.append(pl)
                else:
                    self.notes.append(
                        f"スキップ: [{self.label(f)}]（{pl.mime}）→ [{self.label(cid)}]"
                        f" は {accept} しか受けません（§3-7）")
        return got

    def _eval(self, cid: str) -> list[Payload]:
        c = self.cards.get(cid)
        if not c:
            raise DryRunError(f"カードがありません: {cid}")
        kind = c["kind"]

        if kind == "file":
            path = c["path"]
            if not path or not os.path.exists(path):
                raise DryRunError(f"[{self.label(cid)}] 実ファイルがありません: {path}")
            with open(path, "rb") as fh:
                data = fh.read()
            ext = (c["ext"] or os.path.splitext(path)[1].lstrip(".") or "bin").lower()
            stem = os.path.splitext(os.path.basename(path))[0]
            return [Payload(data, ext, c["mime"] or "application/octet-stream", stem)]

        if kind == "note":
            return [Payload((c["note"] or "").encode("utf-8"), "txt", "text/plain",
                            self.label(cid))]

        if kind == "drop":
            # flow_run.rs:1399 と同じ契約 —— 未投入のドロップポイントは
            # エラーにせず「スキップ」（no-data ではない＝下流は止まらない）。
            self.notes.append(f"スキップ: [{self.label(cid)}]（まだファイルが投入されていません）")
            return []

        if kind == "op":
            op = c["spec"].get("op")
            if op in ("in", "out"):
                return self._inputs(cid, "*/*")     # 素通し
            raise DryRunError(f"[{self.label(cid)}] op:{op} は試走の対象外です")

        if kind != "filter":
            raise DryRunError(f"[{self.label(cid)}] kind={kind} は流せません")

        spec = c["spec"]
        b = spec.get("builtin")
        if b in UNSUPPORTED:
            raise DryRunError(
                f"[{self.label(cid)}] {UNSUPPORTED[b]} は試走スクリプトでは再現しません。"
                f"TexoS を起動して ▶ で確かめてください")
        accept = spec.get("accept_mime") or "*/*"
        ins = self._inputs(cid, accept)
        t0 = time.time()

        if b == "tap":
            for i, pl in enumerate(ins):
                name = spec.get("prefix") or self.label(cid)
                if i:
                    name = f"{name}_{i + 1}"
                self._write(f"{name}.{pl.ext}", pl.data)
            self._log(cid, t0, ins)
            return ins

        if b == "ai":
            role = self._role_text(cid, "system")
            system = role if role is not None else (spec.get("system") or "")
            outs = []
            for pl in ins:                          # 集約ではない＝入力ごとに1回（§3-1）
                text = self._call_ai(cid, spec, system, pl.text)
                outs.append(Payload(text.encode("utf-8"), "md", "text/markdown", pl.stem))
            outs = _apply_prefix(spec, outs)
            self._log(cid, t0, outs)
            return outs

        if b in AGGREGATE_BUILTINS:
            data, ext, mime = self._builtin_agg(b, spec, ins, cid)
            outs = _apply_prefix(spec, [Payload(data, ext, mime,
                                                ins[0].stem if ins else "output")])
            self._log(cid, t0, outs)
            return outs

        if b:
            outs = []
            for pl in ins:
                data, ext, mime = self._builtin(b, spec, pl, cid)
                outs.append(Payload(data, ext, mime, pl.stem))
            outs = _apply_prefix(spec, outs)
            self._log(cid, t0, outs)
            return outs

        outs = _apply_prefix(spec, self._external(cid, spec, ins))
        self._log(cid, t0, outs)
        return outs

    # ------------------------------------------------------- 内蔵フィルタ
    def _builtin_agg(self, name, spec, ins, cid):
        texts = [p.text for p in ins]
        if name == "text_concat":
            sep = spec.get("separator") or "newline"
            out = ""
            for i, t in enumerate(texts):
                if i and sep != "none":
                    if out and not out.endswith("\n"):
                        out += "\n"
                    if sep == "blank":
                        out += "\n"
                    elif sep == "rule":
                        out += "\n---\n\n"
                out += t
            return out.encode("utf-8"), "txt", "text/plain"
        if name == "text_diff":
            if len(texts) != 2:
                raise DryRunError(f"[{self.label(cid)}] diff は入力ちょうど2本（現在 {len(texts)}）")
            n = int(spec.get("lines") or 3)
            out = "".join(difflib.unified_diff(
                texts[0].splitlines(True), texts[1].splitlines(True),
                "入力1（旧）", "入力2（新）", n=n))
            return out.encode("utf-8"), "diff", "text/plain"
        if name == "text_paste":
            sep = {"tab": "\t", "": "\t", "comma": ",", "space": " "}.get(
                spec.get("delimiter") or "tab", spec.get("delimiter") or "\t")
            cols = [t.splitlines() for t in texts]
            rows = max((len(c) for c in cols), default=0)
            out = "\n".join(sep.join(c[i] if i < len(c) else "" for c in cols) for i in range(rows))
            return out.encode("utf-8"), "txt", "text/plain"
        raise DryRunError(f"未知の集約ビルトイン: {name}")

    def _builtin(self, name, spec, pl, cid):
        text = pl.text
        plain = ("txt", "text/plain")
        if name == "text_grep":
            pat = spec.get("pattern") or ""
            rx = re.compile(pat)
            keep = [ln for ln in text.splitlines() if bool(rx.search(ln)) != bool(spec.get("invert"))]
            return ("\n".join(keep)).encode("utf-8"), *plain
        if name == "text_replace":
            out = re.sub(spec.get("pattern") or "", spec.get("replace") or "", text)
            return out.encode("utf-8"), *plain
        if name == "text_head":
            n = int(spec.get("lines") or 10)
            return "\n".join(text.splitlines()[:n]).encode("utf-8"), *plain
        if name == "text_tail":
            n = int(spec.get("lines") or 10)
            return "\n".join(text.splitlines()[-n:]).encode("utf-8"), *plain
        if name == "text_sort":
            lines = text.splitlines()
            if (spec.get("compare") or "text") == "number":
                def key(s):
                    m = re.match(r"\s*(-?\d+(?:\.\d+)?)", s)
                    return float(m.group(1)) if m else float("inf")
                lines.sort(key=key)
            else:
                lines.sort()
            if (spec.get("order") or "asc") == "desc":
                lines.reverse()
            if (spec.get("unique") or "keep") == "remove":
                seen, uniq = set(), []
                for ln in lines:
                    if ln not in seen:
                        seen.add(ln)
                        uniq.append(ln)
                lines = uniq
            return "\n".join(lines).encode("utf-8"), *plain
        if name == "text_count":
            lines, words, chars = len(text.splitlines()), len(text.split()), len(text)
            mode = spec.get("mode") or "all"
            out = {"lines": str(lines), "words": str(words), "chars": str(chars)}.get(
                mode, f"{lines} {words} {chars}")
            return out.encode("utf-8"), *plain
        if name == "text_cut":
            delim = {"comma": ",", "space": None}.get(spec.get("delimiter") or "tab", "\t")
            want = [int(x) for x in re.findall(r"\d+", str(spec.get("fields") or "1"))]
            rows = []
            for ln in text.splitlines():
                parts = ln.split() if delim is None else ln.split(delim)
                rows.append((delim or "\t").join(
                    parts[i - 1] for i in want if 0 < i <= len(parts)))
            return "\n".join(rows).encode("utf-8"), *plain
        if name == "md_codeblock":
            want = (spec.get("lang") or "").strip().lower() or None
            in_block, cur_lang, cur, found = False, "", [], None
            for line in text.splitlines():
                t = line.lstrip()
                if not in_block:
                    if t.startswith("```"):
                        in_block, cur_lang, cur = True, t[3:].strip().lower(), []
                elif t.startswith("```"):
                    in_block = False
                    if found is None and (want is None or cur_lang == want):
                        found = (cur_lang, "\n".join(cur))
                else:
                    cur.append(line)
            if found is None:
                raise DryRunError(
                    f"[{self.label(cid)}] "
                    + (f"`{want}` のコードブロックが見つかりません" if want
                       else "コードブロック(```...```)が見つかりません"))
            lang, code = found
            if code and not code.endswith("\n"):
                code += "\n"
            ext = (spec.get("output_ext") or "").strip().lstrip(".") or _lang_ext(lang)
            return code.encode("utf-8"), ext, "text/plain"
        if name == "text_convert":
            # nkf 相当（→UTF-8）。auto は BOM → UTF-8厳密 → 推定。ここでは chardetng の
            # 代わりに cp932/euc-jp を順に試す（本物とズレる可能性がある簡易版）。
            enc = (spec.get("encoding") or "auto").strip().lower()
            raw = pl.data
            if enc in ("", "auto"):
                for cand in ("utf-8-sig", "utf-8", "cp932", "euc_jp"):
                    try:
                        out = raw.decode(cand)
                        break
                    except UnicodeDecodeError:
                        continue
                else:
                    out = raw.decode("utf-8", "replace")
            else:
                out = raw.decode({"shift_jis": "cp932"}.get(enc, enc), "replace")
            return out.encode("utf-8"), *plain
        if name in ("img_resize", "img_convert"):
            try:
                from PIL import Image        # 任意依存（無ければ ▶ で確かめる）
            except ImportError:
                raise DryRunError(
                    f"[{self.label(cid)}] 画像フィルタの試走には Pillow が要ります"
                    f"（pip install pillow）。TexoS 本体には不要です")
            import io as _io
            im = Image.open(_io.BytesIO(pl.data))
            if name == "img_resize":
                w = int(spec.get("width") or 800)
                if im.width:
                    im = im.resize((w, max(1, round(im.height * w / im.width))))
                ext, fmt = pl.ext, (im.format or "PNG")
            else:
                fmt = (spec.get("format") or "png").lower()
                ext = {"jpeg": "jpg"}.get(fmt, fmt)
                if fmt in ("jpeg", "jpg") and im.mode in ("RGBA", "P"):
                    im = im.convert("RGB")
            buf = _io.BytesIO()
            im.save(buf, format={"jpg": "JPEG"}.get(str(fmt).lower(), str(fmt).upper()))
            return buf.getvalue(), ext, _mime_for_ext(ext)
        raise DryRunError(
            f"[{self.label(cid)}] 内蔵フィルタ {name} は試走スクリプトが未対応です"
            f"（TexoS では動くかもしれません。flow.rs: run_builtin を見て足すか、▶ で確かめてください）")

    # ------------------------------------------------------- 外部プログラム
    def _external(self, cid, spec, ins):
        cmd_t = spec.get("cmd") or ""
        name = (spec.get("name") or "").strip()
        exe = None
        if name:
            exe = _resolve_program(name, spec.get("path"),
                                   os.path.join(self.root, ".texos", "filters"))
            if not exe:
                raise DryRunError(
                    f"[{self.label(cid)}] プログラム「{name}」が見つかりません"
                    f"（指定パス → .texos/filters → PATH の順で探索。"
                    f"Windows は拡張子まで書くこと＝§3-13）")
        agg = bool(_AGG_CMD.search(cmd_t))
        groups = [ins] if agg else [[p] for p in ins]
        outs = []
        for grp in groups:
            outs.append(self._run_once(cid, spec, exe, cmd_t, grp))
        return [o for o in outs if o is not None]

    def _run_once(self, cid, spec, exe, cmd_t, grp):
        out_ext = (spec.get("output_ext") or (grp[0].ext if grp else "bin")).lstrip(".")
        tmp = os.path.join(tempfile.gettempdir(), "texos_dryrun")
        os.makedirs(tmp, exist_ok=True)
        tag = str(time.time_ns())
        in_paths = []
        for i, pl in enumerate(grp):
            p = os.path.join(tmp, f"in_{tag}_{i + 1}.{pl.ext}")
            with open(p, "wb") as fh:
                fh.write(pl.data)
            in_paths.append(p)
        out_path = os.path.join(tmp, f"out_{tag}.{out_ext}")
        cmd = cmd_t.replace("{out_noext}", os.path.splitext(out_path)[0])
        cmd = cmd.replace("{out}", out_path)
        if "{in*}" in cmd:
            cmd = cmd.replace("{in*}", " ".join(f'"{p}"' for p in in_paths))
        for i, p in enumerate(in_paths):
            cmd = cmd.replace("{in%d}" % (i + 1), p)
        if in_paths:
            cmd = cmd.replace("{in}", in_paths[0])
        if re.search(r"\{in\d*\}", cmd):
            raise DryRunError(f"[{self.label(cid)}] 入力の矢印が足りません: {cmd_t}")
        full = f'"{exe}" {cmd}'.strip() if exe else cmd
        r = subprocess.run(full, shell=True, cwd=self.root, capture_output=True)
        if not os.path.exists(out_path):
            err = (r.stderr or b"").decode("utf-8", "replace").strip()[:600]
            if "{out" not in cmd_t:
                return None                     # {out} を書かない自己完結コマンド
            raise DryRunError(
                f"[{self.label(cid)}] コマンドが出力を作りませんでした"
                f"（exit {r.returncode}）\n{err}")
        with open(out_path, "rb") as fh:
            data = fh.read()
        return Payload(data, out_ext, _mime_for_ext(out_ext),
                       grp[0].stem if grp else "output")

    # ------------------------------------------------------------------ AI
    def _call_ai(self, cid, spec, system, user):
        model = (spec.get("model") or "").strip()
        if not model:
            raise DryRunError(f"[{self.label(cid)}] AIカードに model がありません（§2-1）")
        if not user.strip():
            raise DryRunError(f"[{self.label(cid)}] 入力テキストが空です")
        provider = spec.get("provider") or "ollama"
        openai_like = provider in ("openai", "lmstudio")
        endpoint = (spec.get("endpoint") or "").strip().rstrip("/") or (
            "http://localhost:1234/v1" if openai_like else "http://localhost:11434")
        messages = ([{"role": "system", "content": system}] if system.strip() else []) + \
                   [{"role": "user", "content": user}]
        if openai_like:
            url, body = f"{endpoint}/chat/completions", {
                "model": model, "messages": messages, "stream": False}
            if spec.get("temperature") is not None:
                body["temperature"] = spec["temperature"]
        else:
            opts = {}
            if spec.get("temperature") is not None:
                opts["temperature"] = spec["temperature"]
            url, body = f"{endpoint}/api/chat", {
                "model": model, "messages": messages, "stream": False,
                "keep_alive": "5m", "options": opts}
        req = urllib.request.Request(
            url, data=json.dumps(body).encode("utf-8"),
            headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=spec.get("timeout_secs") or 1800) as resp:
                v = json.loads(resp.read().decode("utf-8"))
        except urllib.error.URLError as e:
            raise DryRunError(
                f"[{self.label(cid)}] {provider} に繋がりません（{endpoint}）: {e}\n"
                f"  Ollama なら `ollama serve` が動いているか、"
                f"モデル {model} が `ollama list` にあるか確かめてください（§2-1）")
        if openai_like:
            return v["choices"][0]["message"]["content"]
        if "error" in v:
            raise DryRunError(f"[{self.label(cid)}] AI APIエラー: {v['error']}")
        return v["message"]["content"]

    # ------------------------------------------------------------- 書き出し
    def _write(self, name: str, data: bytes) -> str:
        os.makedirs(self.out_dir, exist_ok=True)
        path = os.path.join(self.out_dir, name)
        with open(path, "wb") as fh:
            fh.write(data)
        self.written.append(path)
        return path

    def _log(self, cid, t0, outs):
        if not self.verbose:
            return
        dt = time.time() - t0
        size = sum(len(p.data) for p in outs)
        n = f" ×{len(outs)}" if len(outs) != 1 else ""
        print(f"  {self.label(cid)[:28]:30} {dt:5.1f}秒 {size:>7,}バイト{n}")

    # ---------------------------------------------------------------- 実行
    def run(self, cid: str) -> list[str]:
        c = self.cards[cid]
        spec = c["spec"]
        print(f"▶ {self.label(cid)}")
        outs = self.eval(cid)
        made = []
        for i, pl in enumerate(outs):
            pre = spec.get("prefix")
            if pre:
                name = f"{pre}{i + 1}.{pl.ext}" if (spec.get("seq") or len(outs) > 1) else f"{pre}.{pl.ext}"
            else:
                # flow_run.rs: (None,_) => "{stem}_{終端カードのラベル}.{ext}"
                name = f"{pl.stem}_{safe_label(self.label(cid))}.{pl.ext}"
            made.append(self._write(name, pl.data))
        return made


def _apply_prefix(spec: dict, outs: list) -> list:
    """prefix はファイル名だけでなく **stem そのもの**を差し替える（flow_run.rs:1055）。
    下流の終端が prefix を持たないとき、この stem が出力名の前半になる。"""
    name = (spec.get("prefix") or "").strip()
    if not name:
        return outs
    stem, ext = split_output_name(name)
    for pl in outs:
        pl.stem = stem
        if ext:
            pl.ext = ext
    return outs


def split_output_name(name: str) -> tuple[str, str | None]:
    """flow.rs: split_output_name。"fibo.exe" → ("fibo","exe")／"名前." → ("名前.", None)。"""
    if "." in name:
        stem, _, ext = name.rpartition(".")
        if stem and 1 <= len(ext) <= 8 and ext.isascii() and ext.isalnum():
            return stem, ext
    return name, None


def safe_label(label: str) -> str:
    """出力ファイル名に使えるラベル（flow_run.rs の safe_flabel と同じ置換）。"""
    return "".join("_" if (c in '\/:*?"<>|' or ord(c) < 32) else c for c in label)


def _lang_ext(lang: str) -> str:
    return {"c": "c", "cc": "cc", "cpp": "cc", "java": "java", "kotlin": "kt", "kt": "kt",
            "python": "py", "py": "py", "ruby": "rb", "rb": "rb", "rust": "rs", "rs": "rs",
            "typescript": "ts", "ts": "ts", "javascript": "js", "js": "js",
            "go": "go", "golang": "go", "powershell": "ps1", "ps1": "ps1",
            "bash": "sh", "sh": "sh", "zsh": "zsh", "sql": "sql", "toml": "toml",
            "yaml": "yml", "yml": "yml", "json": "json", "html": "html",
            "css": "css", "xml": "xml", "markdown": "md", "md": "md"}.get(lang, "txt")


def _mime_for_ext(ext: str) -> str:
    """util.rs: mime_for_ext の要点だけ（試走で下流の型チェックを再現するため）。"""
    e = ext.lower()
    if e in ("txt", "log", "ini", "diff", "patch"):
        return "text/plain"
    if e == "md":
        return "text/markdown"
    if e == "json":
        return "application/json"
    if e == "csv":
        return "text/csv"
    if e in ("c", "cc", "cpp", "cxx", "h", "hpp"):
        return "text/x-c"
    if e in ("py", "java", "kt", "go", "rb", "rs", "ts", "js", "sh", "ps1", "sql",
             "html", "css", "yml", "yaml", "toml", "lua", "cs", "swift", "hs"):
        return "text/x-" + e
    return "application/octet-stream"


def _resolve_program(name: str, path: str | None, filters_dir: str) -> str | None:
    """flow.rs: resolve_program と同じ探し方（**拡張子は補わない**＝§3-13）。"""
    if path and path.strip():
        p = path.strip()
        if os.path.isfile(p):
            return p
        j = os.path.join(p, name)
        if os.path.isfile(j):
            return j
    bundled = os.path.join(filters_dir, name)
    if os.path.isfile(bundled):
        return bundled
    for d in os.environ.get("PATH", "").split(os.pathsep):
        if d and os.path.isfile(os.path.join(d, name)):
            return os.path.join(d, name)
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description="TexoS のフローを起動せずに1周流す（試走）")
    ap.add_argument("root", help="プロジェクトのフォルダ")
    ap.add_argument("--card", help="流す終端カード（ラベルの一部でよい）。省略すると全部")
    ap.add_argument("--list", action="store_true", help="終端の一覧だけ表示して終わる")
    a = ap.parse_args()
    try:
        dr = DryRun(a.root)
        terms = dr.terminals()
        if a.list or not terms:
            print(f"終端カード（{len(terms)}枚）:")
            for t in terms:
                print(f"  ▶ {dr.label(t)}")
            return 0 if terms else 1
        targets = [dr.find(a.card)] if a.card else terms
        print(f"試走: {dr.root}")
        for t in targets:
            dr.run(t)
        print()
        if dr.notes:
            for n in dict.fromkeys(dr.notes):
                print(f"! {n}")
            print()
        print(f"出力 → {dr.out_dir}")
        for p in dr.written:
            print(f"  {os.path.basename(p)}  ({os.path.getsize(p):,} バイト)")
        print("\n※ これは flow_run.rs の写しであって TexoS 本体ではない。"
              "最終確認は TexoS の ▶ で。")
        return 0
    except DryRunError as e:
        print(f"試走で止まりました:\n  {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
