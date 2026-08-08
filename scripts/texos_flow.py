# -*- coding: utf-8 -*-
"""TexoS プロジェクト（カード＋矢印のフロー）を外から組み立てるための最小ビルダー。

用途: AI エージェントに「TexoS で○○する仕組みを作って」と頼むときの足場。
規約の正本は `docs/AIエージェント向け_フロー構築規約.md`。

    from texos_flow import Project

    p = Project(r"F:\\...\\新プロジェクト")
    src  = p.file("議題.md", "# 議題\\n...", 0, 300)
    knob = p.note("重み付け\\n...", 0, 500)
    cat  = p.cat("束 材料", 400, 400, prefix="05_材料")
    ai   = p.ai("判定", 700, 400, system="...", prefix="判定")
    p.flow(src, cat); p.flow(knob, cat); p.flow(cat, ai)
    p.save()          # 検証して初めて書き込む

save() は「よくある壊し方」を例外にする。特に **AIカードに複数入力を直結する**
（＝TexoS は入力ごとに別々に実行する＝ファンアウト）を弾く。

スキーマは `src-tauri/src/db.rs` の migrate() が正本。ここが古くても、TexoS が
プロジェクトを開いたときに best-effort ALTER で不足列を足すので、**このビルダーは
自分が書き込む列さえ正しければよい**。列を増やしたときだけ追随すればよい。
"""
from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import time
import uuid

FILTER_SENTINEL = "filter:sentinel"
NOTE_SENTINEL = "note:sentinel"

_SCHEMA = """
CREATE TABLE IF NOT EXISTS real_bodies (
    id TEXT PRIMARY KEY, mime_type TEXT NOT NULL, size INTEGER NOT NULL,
    created_at INTEGER NOT NULL, codec_ref TEXT, tags TEXT NOT NULL DEFAULT '[]', ext TEXT);
CREATE TABLE IF NOT EXISTS virtual_bodies (
    id TEXT PRIMARY KEY, real_id TEXT NOT NULL REFERENCES real_bodies(id),
    label TEXT NOT NULL DEFAULT '', pos_x REAL NOT NULL DEFAULT 0, pos_y REAL NOT NULL DEFAULT 0,
    desktop_id TEXT, created_at INTEGER NOT NULL, etag TEXT, last_modified TEXT,
    committed_path TEXT, committed_mtime INTEGER, project_ref TEXT,
    expanded INTEGER NOT NULL DEFAULT 0, url_ref TEXT, memo TEXT,
    z_order INTEGER NOT NULL DEFAULT 0, folder_ref TEXT, origin_path TEXT,
    panel_w REAL, panel_h REAL, deleted_at INTEGER, kind TEXT, filter_ref TEXT,
    last_run TEXT, web_meta TEXT, ext TEXT, mime_type TEXT, window_of TEXT,
    note_text TEXT, style_ref TEXT);
CREATE TABLE IF NOT EXISTS desktops (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS diff_stack (
    id INTEGER PRIMARY KEY AUTOINCREMENT, virtual_id TEXT NOT NULL REFERENCES virtual_bodies(id),
    seq INTEGER NOT NULL, op TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS relations (
    id INTEGER PRIMARY KEY AUTOINCREMENT, from_id TEXT NOT NULL, to_id TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'link', created_at INTEGER NOT NULL,
    attrs TEXT, deleted_at INTEGER);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS shapes (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL DEFAULT 'rect',
    pos_x REAL NOT NULL DEFAULT 0, pos_y REAL NOT NULL DEFAULT 0,
    width REAL NOT NULL DEFAULT 320, height REAL NOT NULL DEFAULT 220,
    rotation REAL NOT NULL DEFAULT 0, fill TEXT NOT NULL DEFAULT '#4a90d9',
    opacity REAL NOT NULL DEFAULT 0.2, z_order INTEGER NOT NULL DEFAULT 0,
    label TEXT NOT NULL DEFAULT '', created_at INTEGER NOT NULL,
    locked INTEGER NOT NULL DEFAULT 0, members TEXT, deleted_at INTEGER);
CREATE INDEX IF NOT EXISTS idx_vb_desktop ON virtual_bodies(desktop_id);
CREATE INDEX IF NOT EXISTS idx_diff_virtual ON diff_stack(virtual_id, seq);
CREATE INDEX IF NOT EXISTS idx_rel_from ON relations(from_id);
"""

# 集約ノード（全入力を1回で受け取る）。これ以外の builtin は入力ごとにファンアウトする。
AGGREGATE_BUILTINS = {"text_concat", "text_diff", "text_paste"}
# 素通し系（入力を何本受けてもファンアウトしない）。
PASSTHROUGH_BUILTINS = {"tap", "loop"}

MIME_BY_EXT = {
    "md": "text/markdown", "txt": "text/plain", "csv": "text/csv",
    "json": "application/json", "html": "text/html",
}


class BuildError(Exception):
    pass


class Project:
    def __init__(self, root: str, model: str = "gemma4:latest", provider: str = "ollama"):
        self.root = os.path.abspath(root)
        self.model = model
        self.provider = provider
        self._cards: dict[str, dict] = {}   # id -> {label, kind, spec}
        self._edges: list[tuple[str, str, str | None]] = []  # (from, to, role)
        self._shapes: list[tuple] = []
        self._files: list[tuple[str, bytes, str]] = []   # (path, bytes, card_id)
        self._view = None

    # ---------------------------------------------------------------- カード
    def file(self, name: str, text: str, x: float, y: float, memo: str | None = None) -> str:
        """プロジェクト直下に実ファイルを作り、コミット済み実身カードとして置く。"""
        cid = uuid.uuid4().hex
        path = os.path.join(self.root, name)
        self._files.append((path, text.encode("utf-8"), cid))
        self._cards[cid] = dict(label=name, kind="file", spec=None, x=x, y=y,
                                memo=memo, path=path,
                                ext=name.rsplit(".", 1)[-1] if "." in name else None)
        return cid

    def note(self, text: str, x: float, y: float, width: float = 300,
             font_size: float = 13, title_size: float | None = None,
             color: str | None = None) -> str:
        """コメント付箋。フローに繋ぐと本文が text/plain の定数として流れる（定数カード）。"""
        cid = uuid.uuid4().hex
        style = {"width": width, "font_size": font_size}
        if title_size:
            style["title_size"] = title_size
        if color:
            style["color"] = color
        self._cards[cid] = dict(label=text.splitlines()[0][:40] if text.strip() else "",
                                kind="note", spec=None, x=x, y=y, memo=None,
                                note_text=text, style=style)
        return cid

    def ai(self, label: str, x: float, y: float, *, system: str,
           prefix: str | None = None, temperature: float | None = None,
           model: str | None = None, provider: str | None = None,
           endpoint: str | None = None, memo: str | None = None) -> str:
        """AIフィルタカード（builtin:"ai" transform）。

        **通常入力は1本にすること**（複数繋ぐと入力ごとにファンアウトする → cat() で束ねる）。
        `system` は役割ポート（`flow(付箋, ai, role="system")`）で上書きできる。
        役割を付箋にすると、担当が何者なのかがキャンバスから読めて、その場で直せる。
        """
        spec = {"builtin": "ai", "provider": provider or self.provider,
                "mode": "transform", "model": model or self.model,
                "system": system, "accept_mime": "text/*", "output_ext": "md"}
        if temperature is not None:
            spec["temperature"] = temperature
        if endpoint:
            spec["endpoint"] = endpoint
        if prefix:
            spec["prefix"] = prefix
        return self._filter(label, spec, x, y, memo)

    def cat(self, label: str, x: float, y: float, *, separator: str = "rule",
            prefix: str | None = None, memo: str | None = None) -> str:
        """連結（集約）カード。複数の資料を1本にまとめてAIに渡すための「束」。

        separator: "rule"（--- を挟む・既定）| "blank" | "newline" | "none"
        """
        spec = {"builtin": "text_concat", "separator": separator,
                "accept_mime": "text/*", "output_ext": "md"}
        if prefix:
            spec["prefix"] = prefix
        return self._filter(label, spec, x, y, memo)

    def tap(self, label: str, x: float, y: float, *, prefix: str | None = None,
            memo: str | None = None) -> str:
        """記録カード。入力をそのまま下流へ通しつつ、通った中身をファイルに残す。

        フローの途中の結果はふつう残らない（メモリ上のペイロードとして消える）。
        1周流したあとに「何がどう変わったか」を読み返したい所に置く。
        素通しなので入力は何本でもよく、ファンアウトも起こさない。
        """
        spec = {"builtin": "tap", "accept_mime": "*/*", "prefix": prefix or label}
        return self._filter(label, spec, x, y, memo)

    def loop(self, label: str, x: float, y: float, *, max_rounds: int = 3,
             until_contains: str | None = None, memo: str | None = None) -> str:
        """反復カード。上流のサブグラフを、条件を満たすまで引き直す。

        入力にはループさせたい流れの出口を繋ぐ。前周の結果を上流へ戻すには、
        このカードから戻したいカードへ `flow(loop_id, 戻し先, role="previous")`。
        戻り矢印は**評価では遡らない**ので、絵は輪に見えても pull は DAG のまま。

        max_rounds は暴走止め（ヒューズ・1〜50）。until_contains を指定すると、
        その周の出力にその語が現れた時点で止まる。
        """
        n = max(1, min(50, int(max_rounds)))
        spec: dict = {"builtin": "loop", "accept_mime": "*/*", "max_rounds": n}
        if until_contains:
            spec["until"] = "contains"
            spec["pattern"] = until_contains
        return self._filter(label, spec, x, y, memo)

    def builtin(self, label: str, x: float, y: float, *, name: str,
                memo: str | None = None, **params) -> str:
        """任意の内蔵フィルタ（text_grep / text_sort / md_codeblock ...）。"""
        spec = {"builtin": name, **params}
        return self._filter(label, spec, x, y, memo)

    def _filter(self, label, spec, x, y, memo) -> str:
        cid = uuid.uuid4().hex
        self._cards[cid] = dict(label=label, kind="filter", spec=spec,
                                x=x, y=y, memo=memo)
        return cid

    # ---------------------------------------------------------------- 配線
    def flow(self, src: str, dst: str, *, role: str | None = None) -> None:
        """実行の矢印。集約カードへの入力順は、この flow() を呼んだ順になる。

        role="system" を付けると、上流の中身が **AIカードのシステムプロンプト**として
        渡される（役割ポート）。役割つきの矢印は入力に数えないので、
        **ファンアウトを起こさない**＝AIカードの前に束を置かなくてよい。
        役割は付箋で書くのが基本（キャンバスから読めて、その場で直せる）。
        """
        for c in (src, dst):
            if c not in self._cards:
                raise BuildError(f"未知のカードid: {c}")
        if role is not None and role not in ("system", "previous"):
            raise BuildError(f'role は "system" / "previous" のみ対応しています（指定: {role}）')
        self._edges.append((src, dst, role))

    def shape(self, x, y, w, h, label="", fill="#4a90d9", opacity=0.15) -> None:
        self._shapes.append((x, y, w, h, label, fill, opacity))

    def view(self, x=60.0, y=60.0, scale=0.6) -> None:
        self._view = {"x": x, "y": y, "scale": scale}

    # ---------------------------------------------------------------- 検証
    def _inputs_of(self) -> dict[str, list[str]]:
        """入力に数える矢印。

        role="system" は「設定」であって入力ではない（数に入れない）。
        role="previous"（反復の戻り）は**材料なので入力に数える** — 数えないと
        「AIカードに前周と資料の2本が入る＝ファンアウト」を見逃す。
        """
        d: dict[str, list[str]] = {}
        for f, t, role in self._edges:
            if role != "system":
                d.setdefault(t, []).append(f)
        return d

    def _all_targets(self) -> dict[str, list[tuple[str, str | None]]]:
        d: dict[str, list[tuple[str, str | None]]] = {}
        for f, t, role in self._edges:
            d.setdefault(t, []).append((f, role))
        return d

    def validate(self) -> list[str]:
        """壊れた配線を例外に、もったいない配線を警告に。"""
        errs, warns = [], []
        ins = self._inputs_of()
        targets = self._all_targets()
        outs = {f for f, _, _ in self._edges}

        # 役割つきの矢印は AI カードにだけ効く
        for cid, c in self._cards.items():
            roles = [f for f, r in targets.get(cid, []) if r == "system"]
            if not roles:
                continue
            if c["kind"] != "filter" or c["spec"].get("builtin") != "ai":
                errs.append(f"[{c['label']}] 役割（system）の矢印は AIカードにだけ繋げます")
            elif len(roles) > 1:
                errs.append(f"[{c['label']}] 役割（system）の矢印が {len(roles)} 本あります（1本にしてください）")

        for cid, c in self._cards.items():
            if c["kind"] != "filter":
                continue
            spec, n = c["spec"], len(ins.get(cid, []))
            b = spec.get("builtin")
            if b == "text_diff" and n != 2:
                errs.append(f"[{c['label']}] diff は入力ちょうど2本が必要（現在 {n} 本）")
            elif b not in AGGREGATE_BUILTINS and b not in PASSTHROUGH_BUILTINS and n > 1:
                errs.append(
                    f"[{c['label']}] 入力が {n} 本あります。集約カード以外は"
                    f"**入力ごとに別々に実行**されます（ファンアウト）。"
                    f"まとめて1回で読ませたいなら cat() を前に挟んでください"
                )
            if b == "ai" and not spec.get("model"):
                errs.append(f"[{c['label']}] AIカードに model がありません")
            if b == "ai" and n == 0:
                errs.append(f"[{c['label']}] AIカードに入力がありません")
            if cid not in outs and not spec.get("prefix"):
                warns.append(
                    f"[{c['label']}] 終端カードに prefix がありません。"
                    f"出力名が「入力名_フィルタ名」になります"
                )
            if cid not in outs and cid not in ins:
                warns.append(f"[{c['label']}] どこにも繋がっていません")

        # 反復（v1.8 ④）: 戻り矢印は反復カードからしか出せない。ここを間違えると
        # 「前周の結果」が永遠に空のまま静かに回り続ける（気づけない失敗になる）。
        for f, t, role in self._edges:
            if role != "previous":
                continue
            src = self._cards[f]
            if src["kind"] != "filter" or src["spec"].get("builtin") != "loop":
                errs.append(
                    f"[{src['label']}] 役割（previous）の矢印は反復カードからだけ引けます"
                )
        for cid, c in self._cards.items():
            if c["kind"] != "filter" or c["spec"].get("builtin") != "loop":
                continue
            if not any(f == cid and r == "previous" for f, _, r in self._edges):
                warns.append(
                    f"[{c['label']}] 反復カードから戻り矢印（role=\"previous\"）が出ていません。"
                    f"前周の結果が上流に届かないので、同じ処理を繰り返すだけになります"
                )

        # 循環（pull 評価が無限再帰する）。戻り矢印は評価で遡らないので辿らない
        # ＝反復は循環として検出されない（これが「DAG を保つ反復」の意味）。
        back = {(f, t) for f, t, r in self._edges if r == "previous"}
        color: dict[str, int] = {}

        def walk(n: str) -> bool:
            color[n] = 1
            for m in ins.get(n, []):
                if (m, n) in back:
                    continue
                if color.get(m) == 1 or (color.get(m, 0) == 0 and walk(m)):
                    return True
            color[n] = 2
            return False

        for cid in self._cards:
            if color.get(cid, 0) == 0 and walk(cid):
                errs.append("矢印が循環しています（pull 評価が回り続けます）")
                break

        if errs:
            raise BuildError("配線エラー:\n  - " + "\n  - ".join(errs))
        return warns

    def terminals(self) -> list[tuple[str, str]]:
        """(ラベル, 実行すると出来るファイル名) の一覧。"""
        outs = {f for f, _, _ in self._edges}
        r = []
        for cid, c in self._cards.items():
            if c["kind"] == "filter" and cid not in outs:
                spec = c["spec"]
                pre = spec.get("prefix")
                ext = spec.get("output_ext", "md")
                r.append((c["label"], f"{pre}.{ext}" if pre else "（入力名_フィルタ名）"))
        return r

    # ---------------------------------------------------------------- 書き出し
    def save(self, verbose: bool = True) -> None:
        warns = self.validate()
        texos = os.path.join(self.root, ".texos")
        cas = os.path.join(texos, "cas")
        os.makedirs(cas, exist_ok=True)
        conn = sqlite3.connect(os.path.join(texos, "texos.db"))
        conn.executescript("PRAGMA journal_mode=WAL;")
        conn.executescript(_SCHEMA)
        t = _now()
        for rid, mime in ((FILTER_SENTINEL, "application/vnd.texos.filter"),
                          (NOTE_SENTINEL, "text/vnd.texos.note")):
            conn.execute("INSERT OR IGNORE INTO real_bodies (id,mime_type,size,created_at)"
                         " VALUES (?,?,0,?)", (rid, mime, t))

        # 実ファイル → CAS → 実身カード（import_file と同じ形にする＝
        # committed_path と origin_path の両方を実体へ、mtime も記録）
        written = {}
        for path, data, cid in self._files:
            with open(path, "wb") as f:
                f.write(data)
            h = hashlib.sha256(data).hexdigest()
            dest = os.path.join(cas, h[:2], h[2:])
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            if not os.path.exists(dest):
                with open(dest, "wb") as f:
                    f.write(data)
            written[cid] = (h, path, len(data))

        for cid, c in self._cards.items():
            if c["kind"] == "file":
                h, path, size = written[cid]
                ext = c["ext"]
                mime = MIME_BY_EXT.get(ext or "", "application/octet-stream")
                conn.execute("INSERT OR IGNORE INTO real_bodies"
                             " (id,mime_type,size,created_at,ext) VALUES (?,?,?,?,?)",
                             (h, mime, size, _now(), ext))
                conn.execute(
                    "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,"
                    " origin_path,committed_path,committed_mtime,ext,mime_type,kind,memo)"
                    " VALUES (?,?,?,?,?,?,?,?,?,?,?,'file',?)",
                    (cid, h, c["label"], c["x"], c["y"], _now(), path, path,
                     int(os.path.getmtime(path) * 1000), ext, mime, c["memo"]))
            elif c["kind"] == "note":
                conn.execute(
                    "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,"
                    " kind,note_text,style_ref) VALUES (?,?,?,?,?,?,'note',?,?)",
                    (cid, NOTE_SENTINEL, c["label"], c["x"], c["y"], _now(),
                     c["note_text"], json.dumps(c["style"], ensure_ascii=False)))
            else:
                conn.execute(
                    "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,"
                    " kind,filter_ref,memo) VALUES (?,?,?,?,?,?,'filter',?,?)",
                    (cid, FILTER_SENTINEL, c["label"], c["x"], c["y"], _now(),
                     json.dumps(c["spec"], ensure_ascii=False), c["memo"]))

        # 集約の入力順 = created_at 昇順。flow() を呼んだ順が保たれるよう明示的に振る。
        base = _now()
        for i, (f, t_, role) in enumerate(self._edges):
            attrs = json.dumps({"role": role}) if role else None
            conn.execute("INSERT INTO relations (from_id,to_id,kind,created_at,attrs)"
                         " VALUES (?,?,'flow',?,?)", (f, t_, base + i, attrs))
        for x, y, w, h, label, fill, op in self._shapes:
            conn.execute(
                "INSERT INTO shapes (id,kind,pos_x,pos_y,width,height,rotation,fill,"
                "opacity,z_order,label,created_at) VALUES (?,'rect',?,?,?,?,0,?,?,0,?,?)",
                (uuid.uuid4().hex, x, y, w, h, fill, op, label, _now()))
        if self._view:
            conn.execute("INSERT OR REPLACE INTO meta (key,value) VALUES ('view_transform',?)",
                         (json.dumps(self._view),))
        conn.commit()
        conn.close()

        if verbose:
            print(f"作成: {self.root}")
            print(f"  カード {len(self._cards)} 枚 / 矢印 {len(self._edges)} 本")
            for label, out in self.terminals():
                print(f"  ▶ {label} → {out}")
            for w in warns:
                print(f"  警告: {w}")


def _now() -> int:
    return int(time.time() * 1000)
