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
import re
import shutil
import sqlite3
import sys
import time
import uuid

FILTER_SENTINEL = "filter:sentinel"
NOTE_SENTINEL = "note:sentinel"
DROP_SENTINEL = "drop:empty"
LAUNCHER_SENTINEL = "launcher:sentinel"
OP_SENTINEL = "op:empty"
# 発行（リクエスト）側の op。flow ではなく request() で繋ぐ。
REQUEST_OPS = ("trigger", "timer", "watch", "voice")
# 出力カードを作らない内蔵（flow.rs is_self_contained と同じ）。
NO_OUTPUT_BUILTINS = ("speak", "import_new", "voice_send")

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
# t2i / t23d も集約（flow_run.rs: T2i/T23d は全入力を集めて1回＝テキスト→プロンプト・画像→元画像）。
AGGREGATE_BUILTINS = {"text_concat", "text_diff", "text_paste", "t2i", "t23d"}
# 素通し系（入力を何本受けてもファンアウトしない）。
PASSTHROUGH_BUILTINS = {"tap", "loop", "ask"}

# 拡張子 → MIME。**正本は `src-tauri/src/util.rs: mime_for_ext`** で、ここはその写し。
# ここに無い拡張子は application/octet-stream になり、`accept_mime="text/*"` の
# カード（AI・連結など）に**エラーも出さず黙って読み飛ばされる**（規約 §3-7）。
# ソースコードを流すフローで踏み抜くので、テキスト系は広めに持っておく。
MIME_BY_EXT = {
    "txt": "text/plain", "log": "text/plain", "ini": "text/plain", "diff": "text/plain",
    "patch": "text/plain", "md": "text/markdown", "png": "image/png", "jpg": "image/jpeg",
    "jpeg": "image/jpeg", "gif": "image/gif", "webp": "image/webp", "bmp": "image/bmp",
    "svg": "image/svg+xml", "glb": "model/gltf-binary", "gltf": "model/gltf+json", "mp4": "video/mp4",
    "webm": "video/webm", "avi": "video/x-msvideo", "mov": "video/quicktime", "mkv": "video/x-matroska",
    "mp3": "audio/mpeg", "wav": "audio/wav", "flac": "audio/flac", "ogg": "audio/ogg",
    "m4a": "audio/mp4", "json": "application/json", "csv": "text/csv", "tsv": "text/tab-separated-values",
    "xml": "application/xml", "html": "text/html", "htm": "text/html", "css": "text/css",
    "py": "text/x-python", "js": "text/javascript", "ts": "text/javascript", "rs": "text/x-rust",
    "c": "text/x-c", "cpp": "text/x-c", "cc": "text/x-c", "cxx": "text/x-c", "h": "text/x-c",
    "hpp": "text/x-c", "java": "text/x-java", "kt": "text/x-kotlin", "kts": "text/x-kotlin",
    "go": "text/x-go", "rb": "text/x-ruby", "sh": "text/x-shellscript", "bash": "text/x-shellscript",
    "zsh": "text/x-shellscript", "ps1": "text/x-powershell", "psm1": "text/x-powershell",
    "bat": "text/x-bat", "cmd": "text/x-bat", "sql": "text/x-sql", "lua": "text/x-lua",
    "pl": "text/x-perl", "php": "text/x-php", "swift": "text/x-swift", "cs": "text/x-csharp",
    "hs": "text/x-haskell", "r": "text/x-r", "jsx": "text/javascript", "tsx": "text/javascript",
    "tex": "text/x-tex", "yaml": "text/yaml", "yml": "text/yaml", "toml": "text/x-toml",
    "pdf": "application/pdf", "doc": "application/msword", "docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "xls": "application/vnd.ms-excel", "xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "ppt": "application/vnd.ms-powerpoint", "pptx": "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "odt": "application/vnd.oasis.opendocument.text", "ods": "application/vnd.oasis.opendocument.spreadsheet",
    "odp": "application/vnd.oasis.opendocument.presentation", "rtf": "application/rtf",
    "epub": "application/epub+zip", "zip": "application/zip", "7z": "application/x-7z-compressed",
    "rar": "application/vnd.rar", "gz": "application/gzip", "tar": "application/x-tar",
}

# 外部プログラムカードが集約になる条件（flow.rs: cmd_is_aggregate と同じ規則）。
_AGG_CMD = re.compile(r"\{in\*\}|\{in([2-9]|[1-9]\d+)\}")


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
        self._bundles: list[tuple[str, str, tuple[str, ...]]] = []  # 同梱プログラム
        self._reqs: list[tuple[str, str, dict | None]] = []  # リクエスト矢印 (op, 先, attrs)
        self._view = None

    # ---------------------------------------------------------------- カード
    def file(self, name: str, text: str | bytes, x: float, y: float,
             memo: str | None = None) -> str:
        """プロジェクト直下に実ファイルを作り、コミット済み実身カードとして置く。
        text は文字列（UTF-8で書く）でもバイト列（wav などの実体）でもよい。"""
        cid = uuid.uuid4().hex
        path = os.path.join(self.root, name)
        self._files.append((path, text if isinstance(text, bytes) else text.encode("utf-8"), cid))
        # サブフォルダ指定（例 "音声/発話.txt"）可。カード名はファイル名だけ（TexoS の取り込みと同じ）。
        self._cards[cid] = dict(label=os.path.basename(name), kind="file", spec=None, x=x, y=y,
                                memo=memo, path=path,
                                ext=name.rsplit(".", 1)[-1] if "." in name else None)
        return cid

    def copy_file(self, src: str, x: float, y: float, name: str | None = None,
                  memo: str | None = None) -> str:
        """既存のファイル（wav・画像など）をプロジェクトに取り込んで実身カードにする。"""
        with open(src, "rb") as f:
            data = f.read()
        return self.file(name or os.path.basename(src), data, x, y, memo)

    def program(self, label: str, x: float, y: float, *, name: str,
                cmd: str = "", mode: str = "command", path: str | None = None,
                output_ext: str | None = None, accept_mime: str | None = None,
                prefix: str | None = None, timeout_secs: int | None = None,
                out_dir: str | None = None, memo: str | None = None) -> str:
        """外部プログラムのカード（内蔵フィルタではない実プログラムを呼ぶ）。

        name  … プログラムのファイル名。TexoS は「path → .texos/filters → PATH」の
                順に探す。**`name` に `wavmix\\wavmix.cmd` のような相対パスを書くと
                `.texos/filters` 起点で解決される**＝プロジェクトに同梱したまま
                絶対パスなしで配れる（bundle() と対で使う）。
        cmd   … 引数。`{in}`（入力）/ `{out}`（出力）/ `{out*}`（出力フォルダ・複数）。
                `{in*}` か `{in2}` 以降を書けば**集約**（全入力を1回で受ける）、
                `{in}`/`{in1}` だけなら入力ごとにファンアウトする。
        mode  … "command"（引数で受け渡し）/ "filter"（標準入出力）/ "generator"（入力不要）
        """
        if mode not in ("command", "filter", "generator"):
            raise BuildError(f'mode は "command" / "filter" / "generator" のみ（指定: {mode}）')
        spec: dict = {"name": name, "type": mode, "cmd": cmd}
        for k, v in (("path", path), ("output_ext", output_ext),
                     ("accept_mime", accept_mime), ("prefix", prefix),
                     ("out_dir", out_dir)):
            if v:
                spec[k] = v
        if timeout_secs is not None:
            spec["timeout_secs"] = timeout_secs
        return self._filter(label, spec, x, y, memo)

    def launcher(self, label: str, x: float, y: float, *, name: str,
                 path: str | None = None, memo: str | None = None) -> str:
        """ランチャーカード（対話的なアプリをそのまま起動する。TexoS が抱えない統合）。

        起動時、TexoS は作業ディレクトリをプロジェクト直下にし、環境変数
        `TEXOS_PROJECT_DIR` にもその場所を入れる。だからアプリ側が
        「今のプロジェクト」に書き出せる。プログラムの探し方は program() と同じで、
        `arrange\\arrange.cmd` のような相対名なら `.texos/filters` 起点で解決される。
        """
        spec: dict = {"name": name, "type": "launcher"}
        if path:
            spec["path"] = path
        cid = uuid.uuid4().hex
        self._cards[cid] = dict(label=label, kind="launcher", spec=spec,
                                x=x, y=y, memo=memo)
        return cid

    def bundle(self, src_dir: str, name: str | None = None,
               ignore: tuple[str, ...] = ("venv", "__pycache__", ".git", ".claude")) -> str:
        """プログラム一式を `.texos/filters/<name>/` に同梱する（プロジェクトごと配る形）。

        戻り値は program(name=...) に渡す相対名の頭（`<name>`）。同梱したものは
        `.texos/filters` 起点で解決されるので、受け取った人は解凍して開くだけで動く。
        """
        dest_name = name or os.path.basename(os.path.abspath(src_dir))
        self._bundles.append((os.path.abspath(src_dir), dest_name, ignore))
        return dest_name

    def drop(self, label: str = "ドロップポイント", x: float = 0, y: float = 0,
             memo: str | None = None) -> str:
        """ドロップポイント。**人があとからファイルを投げ込む口**（画像・音・PDF など）。

        付箋にも実身にもできない入力はこれ。空の実身を置くと、バイナリを食うカードは
        実行時に「壊れたファイル」として落ちる（実測: 空の .png に magick が
        `improper image header` を返した・2026-08-27）。**投入されるまでは
        「スキップ」**されるので、フローは組んだ状態で置いておける。
        """
        cid = uuid.uuid4().hex
        self._cards[cid] = dict(label=label, kind="drop", spec=None,
                                x=x, y=y, memo=memo)
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

    def ai(self, label: str, x: float, y: float, *, system: str = "",
           prefix: str | None = None, temperature: float | None = None,
           model: str | None = None, provider: str | None = None,
           endpoint: str | None = None, gpu: str | None = None,
           think: str | None = None, num_ctx: int | None = None,
           send_reasoning: bool = False, extra_params: dict | None = None,
           seq: int | None = None, memo: str | None = None) -> str:
        """AIフィルタカード（builtin:"ai" transform）。

        seq: None（既定・prefix の固定名で毎回上書き）| 開始番号（prefix1, prefix2 … と
             実行のたびに新しいカードを作る＝溜まっていく。規約 §3-4）。

        **通常入力は1本にすること**（複数繋ぐと入力ごとにファンアウトする → cat() で束ねる）。

        `system` は**省略できる**（既定は空）。役割を付箋にして
        `flow(付箋, ai, role="system")` で渡すのが基本形で、そのとき system は空のままでよい
        —— 担当が何者なのかがキャンバスから読めて、その場で直せるため。
        逆に**役割の矢印も system も無い AI カードは指示ゼロ**なので save() が警告する。

        gpu: None（既定・Ollama 任せ＝VRAM に載る層は GPU）| "max"（全層を GPU へ強制）|
             "cpu"（GPU を使わない）。Ollama ネイティブのみ有効。
        think: None（既定・思考モード指定なし）| "on" | "low" | "medium" | "high"。
             transform では思考は出力から必ず取り除かれる（下流のフィルタが食べるため）。
             思考非対応のモデルには送られない（実行時に能力を確認）。gpt-oss 系は
             深さ指定のみで、"on" は "medium" として送られる。
        num_ctx: None（既定・Ollama の既定長）| 整数（実行時のコンテキスト長・トークン）。
             長い資料を渡すときに増やす。Ollama ネイティブのみ有効。
        send_reasoning: False（既定）| True。provider="openai"（汎用 OpenAI互換）のときだけ効き、
             think を reasoning_effort として送る（"off"→"none"・"low"/"medium"/"high"→そのまま・
             "on"→送らずサーバ既定）。未知の値で 400 を返すサーバがあるので既定は送らない。
        extra_params: None | dict。リクエスト本体のトップレベルへそのまま加える（全サーバ共通）。
             例: Strata の思考上限 {"reasoning_budget_tokens": 8000}。model/messages/stream/
             temperature/think/reasoning_effort/options は書けない（実行時エラーになる）。
             思考の上限などの API パラメータを system に書いても効かない（本文として送られるだけ）。
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
        if seq is not None:
            spec["seq"] = seq
        if gpu:
            spec["gpu"] = gpu
        if think:
            spec["think"] = think
        if num_ctx:
            spec["num_ctx"] = num_ctx
        if send_reasoning:
            spec["send_reasoning"] = True
        if extra_params:
            spec["extra_params"] = extra_params
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

    def ask(self, label: str, x: float, y: float, *, question: str | None = None,
            prefix: str | None = None, skip_upstream: bool = False,
            memo: str | None = None) -> str:
        """問い合わせカード。実行の途中で人の答えを待つ。

        答えは `role="answer"` で繋いだ付箋に書く。付箋を繋がずに実行すると、
        TexoS が答え箱の付箋を作って矢印まで引く（そこに書いて再実行する）。
        付箋の中の「>」で始まる行は質問の控えで、答えには含まれない。
        答えが空のあいだは no-data ＝下流へ進まない（エラーではない）。

        skip_upstream=True にすると、**答えが入っている限り上流を評価しない**。
        AI に原案を書かせてから聞く形では、これを付けないと2パス目に原案が作り直され、
        人が読んだ案と別物になる（答えを消せばまた上流から動く）。
        """
        spec: dict = {"builtin": "ask", "accept_mime": "*/*"}
        if question:
            spec["question"] = question
        if prefix:
            spec["prefix"] = prefix
        if skip_upstream:
            spec["skip_upstream"] = "on"
        return self._filter(label, spec, x, y, memo)

    def loop(self, label: str, x: float, y: float, *, max_rounds: int = 3,
             until_contains: str | None = None, prefix: str | None = None,
             memo: str | None = None) -> str:
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
        if prefix:
            spec["prefix"] = prefix   # 反復カードを終端にしたときの出力名（他のカードと同じ）
        return self._filter(label, spec, x, y, memo)

    def op_in(self, label: str, x: float, y: float, memo: str | None = None) -> str:
        """IN カード（モジュールの入口）。素通しで、受けたものをそのまま下流へ渡す。

        「差し替えられる口」を1か所に見せるための札。仕組みの入口がどこかを
        キャンバスから一目で分かるようにする以外の働きは無い（評価では素通し）。
        **IN は終端になれない**（▶ で実行するのは OUT か普通のフィルタカード）。

        人が課題や設定を書き込む場所が要るなら、IN ではなく **付箋か実身**を使うこと
        （§3-5 定数カード）。IN は中身を持てない。
        """
        return self._op(label, {"op": "in"}, x, y, memo)

    def op_out(self, label: str, x: float, y: float, memo: str | None = None,
               *, prefix: str | None = None) -> str:
        """OUT カード（モジュールの出口）。**▶ を押せる終端になれる**素通しカード。

        押すと上流をさかのぼって実行し、結果のカードが OUT の横に生まれる。
        「ここが出口です」と示しつつ、何度でも押せる実行口を兼ねる。

        `prefix` を書くと出来るファイルの名前を決められる（規約 §3-15）。
        省略すると `入力名_OUTのラベル.拡張子` になるので、ラベルに案内文を
        入れているとそれがファイル名に化ける。**OUT を終端にするなら prefix を付ける**。
        """
        spec: dict = {"op": "out"}
        if prefix:
            spec["prefix"] = prefix
        return self._op(label, spec, x, y, memo)

    def op(self, label: str, x: float, y: float, *, kind: str,
           memo: str | None = None, **params) -> str:
        """任意の op カード。`kind` は in / out / trigger / timer / watch / voice。

        トリガ・タイマ・ウォッチャ・音声は**発行（リクエスト）側**で、pull の評価には出てこない
        （▶ を押す代わりに下流のフィルタを走らせる仕掛け）。フローの矢印では繋がず、
        `request()` で繋ぐ。タイマ・ウォッチャ・音声は**停止状態で作る**（paused=True。
        TexoS の UI と同じ＝開いた途端に発火・マイクを開かない。人が「▶ 稼働開始」を押す）。
        """
        if kind not in REQUEST_OPS + ("in", "out"):
            raise BuildError(f'op の kind は in/out/trigger/timer/watch/voice のみ（指定: {kind}）')
        spec = {"op": kind, **params}
        if kind in ("timer", "watch", "voice"):
            spec.setdefault("paused", True)
        return self._op(label, spec, x, y, memo)

    def voice(self, label: str, x: float, y: float, *, mode: str = "command",
              say_out: str = "音声/発話.txt", memo: str | None = None, **params) -> str:
        """音声反射カード（v1.10・docs/V1.10_音声反射カード計画.md）。

        稼働中はマイクで聞き取り、`request()` で繋いだ先のフィルタを実行して声で報告する。
        - mode="command": 発話がカード名（または request の say=）を含むとき、そのカードだけ。
          言い回しを除いた中身が say_out へ入る（「メモして 牛乳」→「牛乳」）。
        - mode="dictation": 発話のたびに全文を say_out へ入れ、繋いだ先を全部実行する。
        フロー側は **say_out の実身カード**（`file(say_out, "", ...)` で作る）を入力にする。
        """
        if mode not in ("command", "dictation"):
            raise BuildError(f'voice の mode は command / dictation のみ（指定: {mode}）')
        return self.op(label, x, y, kind="voice", memo=memo, mode=mode, say_out=say_out, **params)

    def _op(self, label, spec, x, y, memo) -> str:
        cid = uuid.uuid4().hex
        self._cards[cid] = dict(label=label, kind="op", spec=spec, x=x, y=y, memo=memo)
        return cid

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
        if role is not None and role not in ("system", "previous", "answer"):
            raise BuildError(
                f'role は "system" / "previous" / "answer" のみ対応しています（指定: {role}）'
            )
        self._edges.append((src, dst, role))

    def request(self, src: str, dst: str, *, say: str | list[str] | None = None) -> None:
        """リクエスト矢印（kind='req'・破線）。発行側の op（トリガ/タイマ/ウォッチャ/音声）から、
        実行させたいフィルタカードへ。データは運ばない（▶ を代わりに押すだけ）。

        say= は音声カード用の言い回し（省略時は先のカード名が言い回し）。

        音声カードからファイルカード（file()）へ引くと「書き出し」の矢印になり、
        **それが発話の中身の受け皿**になる（v1.10・voice.rs inbox_path）。
        """
        for c in (src, dst):
            if c not in self._cards:
                raise BuildError(f"未知のカードid: {c}")
        sc, dc = self._cards[src], self._cards[dst]
        if sc["kind"] != "op" or sc["spec"].get("op") not in REQUEST_OPS:
            raise BuildError(f"[{sc['label']}] リクエスト矢印の元は トリガ/タイマ/ウォッチャ/音声 だけです")
        is_voice = sc["spec"].get("op") == "voice"
        if dc["kind"] == "file" and is_voice:
            if say is not None:
                raise BuildError("受け皿（ファイル）への矢印に say= は付けられません")
            self._reqs.append((src, dst, {"role": "inbox"}))
            return
        if dc["kind"] != "filter":
            raise BuildError(f"[{dc['label']}] リクエスト矢印の先は処理カード（filter）だけです"
                             "（音声カードからはファイルカード＝受け皿も可）")
        if say is not None and not is_voice:
            raise BuildError("say= は音声カードからの矢印にだけ付けられます")
        self._reqs.append((src, dst, {"say": [say] if isinstance(say, str) else say} if say else None))

    def shape(self, x, y, w, h, label="", fill="#4a90d9", opacity=0.15) -> None:
        self._shapes.append((x, y, w, h, label, fill, opacity))

    def view(self, x=60.0, y=60.0, scale=0.6) -> None:
        self._view = {"x": x, "y": y, "scale": scale}

    # ---------------------------------------------------------------- 検証
    def _inputs_of(self) -> dict[str, list[str]]:
        """入力に数える矢印。

        role="system"（AIの役割）と role="answer"（問い合わせの答え箱）は「設定」で
        あって入力ではない（数に入れない）。
        role="previous"（反復の戻り）は**材料なので入力に数える** — 数えないと
        「AIカードに前周と資料の2本が入る＝ファンアウト」を見逃す。
        """
        d: dict[str, list[str]] = {}
        for f, t, role in self._edges:
            if role not in ("system", "answer"):
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
        # 発行側の op はデータレーン（flow）に出てこない — 繋ぐのは request()。
        for f, t, _r in self._edges:
            for c in (f, t):
                cc = self._cards[c]
                if cc["kind"] == "op" and cc["spec"].get("op") in REQUEST_OPS:
                    errs.append(f"[{cc['label']}] 発行側の op は flow() では繋げません（request() を使う）")
        for cid, c in self._cards.items():
            if c["kind"] == "op" and c["spec"].get("op") == "voice" and not any(
                    f == cid for f, _t, _s in self._reqs):
                warns.append(f"[{c['label']}] 音声カードからのリクエスト矢印がありません（話しても何も起きない）")
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
            if b is None:  # 外部プログラムカード（内蔵ではない）
                cmd, mode = spec.get("cmd") or "", spec.get("type")
                if not spec.get("name") and not cmd:
                    errs.append(f"[{c['label']}] プログラム名も cmd もありません")
                # 規約 §3-13（2026-08-27 実機）: cmd は引数だけ。先頭にプログラム名を書くと
                # 「…\gcc.exe gcc.exe …」になり、エラーにならず 0 バイトの出力が静かに出来る。
                # 2026-10-23 コンストラクタ（Strata）も踏んだので検査で弾く。
                nm = os.path.basename(str(spec.get("name") or "")).lower()
                head = cmd.strip().split(" ", 1)[0].strip('"').replace("\\", "/").rsplit("/", 1)[-1].lower()
                if nm and head and head in (nm, os.path.splitext(nm)[0]):
                    errs.append(
                        f"[{c['label']}] cmd の先頭にプログラム名（{head}）があります。"
                        f"cmd に書くのは**引数だけ**です（name={spec.get('name')} が実行されます）。"
                        f"例: name=\"gcc.exe\", cmd='\"{{in}}\" -o \"{{out}}\"'"
                    )
                if not _AGG_CMD.search(cmd) and "{in" in cmd and n > 1:
                    errs.append(
                        f"[{c['label']}] 入力が {n} 本ありますが cmd は `{{in}}` だけです。"
                        f"**入力ごとに別々に実行**されます（ファンアウト）。"
                        f"1回でまとめて受けるなら cmd を `{{in*}}` にしてください"
                    )
                if "{in" in cmd and n == 0:
                    errs.append(f"[{c['label']}] cmd が入力を要求していますが、入力の矢印がありません")
                if mode == "generator" and n > 0:
                    warns.append(
                        f"[{c['label']}] ジェネレータ（入力不要）に入力が {n} 本繋がっています"
                    )
                # 2026-09-06 実機: ここは終端のときしか見ていなかったが、**途中のカードも
                # {out} に書かなければ下流へ何も流れない**。しかも「出力を作らない
                # 自己完結コマンド」とみなされてエラーにならず、静かに 0 枚になる（§3-14）。
                # 下流がある／出力名を決めている＝出力を期待している、とみなして弾く。
                if "{out" not in cmd:
                    期待 = ("下流のカードがあります" if cid in outs else
                            "出力名（output_ext / prefix）を決めています" 
                            if spec.get("output_ext") or spec.get("prefix") else None)
                    if 期待:
                        errs.append(
                            f"[{c['label']}] {期待}が、cmd に `{{out}}` がありません。"
                            f"コマンドの標準出力は捨てられるので、**結果が1枚も出来ません**"
                            f"（エラーにもなりません）。`... > \"{{out}}\"` を足してください"
                        )
                    else:
                        warns.append(
                            f"[{c['label']}] cmd に `{{out}}` も `{{out*}}` もありません"
                            f"（成果物を取り込まない実行になります）"
                        )
                elif cid not in outs and "{out}" in cmd and not spec.get("prefix"):
                    warns.append(
                        f"[{c['label']}] 終端カードに prefix がありません。"
                        f"出力名が「入力名_フィルタ名」になります"
                    )
                continue
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
            if b == "ai" and not (spec.get("system") or "").strip() and not any(
                t == cid and r == "system" for _, t, r in self._edges
            ):
                warns.append(
                    f"[{c['label']}] AIカードに指示がありません"
                    f"（system= を書くか、役割の付箋を role=\"system\" で繋いでください）"
                )
            # 出力カードを作らない内蔵（読み上げ・新規カード登録・会話へ送る）は名前が要らない。
            if cid not in outs and not spec.get("prefix") and b not in NO_OUTPUT_BUILTINS:
                warns.append(
                    f"[{c['label']}] 終端カードに prefix がありません。"
                    f"出力名が「入力名_フィルタ名」になります"
                )
            if cid not in outs and cid not in ins:
                warns.append(f"[{c['label']}] どこにも繋がっていません")

        # IN/OUT（v1.5.6 WS-M M1）。IN は終端になれない＝▶ を押す先が無いフローになる。
        for cid, c in self._cards.items():
            if c["kind"] != "op":
                continue
            op = c["spec"].get("op")
            if op == "in" and cid not in outs:
                errs.append(
                    f"[{c['label']}] IN カードが終端になっています。IN は ▶ で実行できません"
                    f"（出口には OUT か普通のフィルタカードを置いてください）"
                )
            if op == "in" and cid not in ins:
                warns.append(f"[{c['label']}] IN カードに入力がありません（口として空回りします）")

        # 問い合わせ（v1.8 ③）: 答え箱は「付箋 → 問い合わせカード」。向きや相手を
        # 間違えると、答えたつもりでいつまでも no-data で止まる（黙って進まない失敗）。
        for f, t, role in self._edges:
            if role != "answer":
                continue
            src, dst = self._cards[f], self._cards[t]
            if src["kind"] != "note":
                errs.append(f"[{src['label']}] 役割（answer）の矢印は付箋からだけ引けます")
            if dst["kind"] != "filter" or dst["spec"].get("builtin") != "ask":
                errs.append(
                    f"[{dst['label']}] 役割（answer）の矢印は問い合わせカードにだけ繋げます"
                )
        for cid, c in self._cards.items():
            if c["kind"] != "filter" or c["spec"].get("builtin") != "ask":
                continue
            if not any(t == cid and r == "answer" for _, t, r in self._edges):
                warns.append(
                    f"[{c['label']}] 答えの付箋が繋がっていません"
                    f"（最初の実行で自動生成されますが、先に置いておくほうが面から読めます）"
                )

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
        # 反復の戻り矢印は下流ではない（評価で遡らない）。数えると反復カードが
        # 終端から外れる — texos_dryrun.terminals() と同じ規則にそろえる。
        outs = {f for f, _, role in self._edges if role != "previous"}
        r = []
        for cid, c in self._cards.items():
            if c["kind"] == "op" and cid not in outs and c["spec"].get("op") == "out":
                pre = c["spec"].get("prefix")
                r.append((c["label"], f"{pre}.（上流の拡張子）" if pre
                          else "（入力名_OUTのラベル）"))
                continue
            if c["kind"] == "filter" and cid not in outs:
                spec = c["spec"]
                pre = spec.get("prefix")
                b = spec.get("builtin")
                # flow.rs の run_builtin が返す拡張子に合わせる（text_* は txt・AI は md）
                ext = spec.get("output_ext") or {
                    "t2i": "png", "t23d": "glb", "text_diff": "diff",
                    "web_capture": "png", "img_convert": spec.get("format") or "png",
                    "ai": "md", "web_fetch": "md", "web_search": "md", "md_codeblock": "txt",
                }.get(b) or ("txt" if (b or "").startswith("text_") else
                             "md" if b else "bin")
                if b in NO_OUTPUT_BUILTINS:
                    r.append((c["label"], "（出力カードなし）"))
                elif "{out*}" in (spec.get("cmd") or ""):
                    r.append((c["label"], f"（書き出したファイル名のまま複数）"))
                else:
                    r.append((c["label"], f"{pre}.{ext}" if pre else "（入力名_フィルタ名）"))
        return r

    # ---------------------------------------------------------------- 書き出し
    def save(self, verbose: bool = True, reset: bool = False) -> None:
        """検証してからプロジェクトに書き出す。

        既にカードのあるプロジェクトには**追記しない**（同じスクリプトを2回流すと
        カードが二重になるため）。作り直すときは reset=True — `.texos/texos.db` を
        捨てて組み直す（プロジェクト直下のファイルはそのまま／同名なら上書き）。
        """
        warns = self.validate()
        texos = os.path.join(self.root, ".texos")
        cas = os.path.join(texos, "cas")
        os.makedirs(cas, exist_ok=True)
        db_path = os.path.join(texos, "texos.db")
        locked = False   # TexoS が開いていて DB ファイルを消せなかった
        if reset:
            for suffix in ("", "-wal", "-shm"):
                try:
                    os.remove(db_path + suffix)
                except FileNotFoundError:
                    pass
                except OSError:
                    # Windows は他プロセスが開いているファイルを消せない。
                    # ここを黙って握りつぶすと「reset したのに古いカードが残る」になる（実機で踏んだ）。
                    locked = True
        conn = sqlite3.connect(db_path)
        conn.executescript("PRAGMA journal_mode=WAL;")
        conn.executescript(_SCHEMA)
        if reset and locked:
            # 消せないなら中身を空にして組み直す。TexoS 側は自分の接続を持っているので
            # **開き直すまで面には出ない**（規約 §3-8）。
            for tbl in ("relations", "diff_stack", "shapes", "virtual_bodies",
                        "real_bodies", "meta", "desktops"):
                conn.execute(f"DELETE FROM {tbl}")
            conn.commit()
        n = conn.execute("SELECT COUNT(*) FROM virtual_bodies").fetchone()[0]
        if n:
            conn.close()
            raise BuildError(
                f"このプロジェクトには既に {n} 枚のカードがあります（{self.root}）。"
                f"追記するとカードが二重になります。組み直すなら save(reset=True) を、"
                f"別のプロジェクトから入口だけ張るなら link_into() を、"
                f"別物として作るなら別のフォルダを指定してください"
            )
        t = _now()
        for rid, mime in ((FILTER_SENTINEL, "application/vnd.texos.filter"),
                          (NOTE_SENTINEL, "text/vnd.texos.note"),
                          (LAUNCHER_SENTINEL, "application/vnd.texos.launcher"),
                          (OP_SENTINEL, "application/vnd.texos.op"),
                          (DROP_SENTINEL, "application/vnd.texos.drop")):
            conn.execute("INSERT OR IGNORE INTO real_bodies (id,mime_type,size,created_at)"
                         " VALUES (?,?,0,?)", (rid, mime, t))

        # 同梱プログラム → .texos/filters/<name>/（受け手は解凍して開くだけで動く）
        for src_dir, dest_name, ignore in self._bundles:
            dest = os.path.join(texos, "filters", dest_name)
            shutil.rmtree(dest, ignore_errors=True)
            shutil.copytree(src_dir, dest, ignore=shutil.ignore_patterns(*ignore))

        # 実ファイル → CAS → 実身カード（import_file と同じ形にする＝
        # committed_path と origin_path の両方を実体へ、mtime も記録）
        written = {}
        for path, data, cid in self._files:
            os.makedirs(os.path.dirname(path), exist_ok=True)
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
            elif c["kind"] == "op":
                conn.execute(
                    "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,"
                    " kind,filter_ref,memo) VALUES (?,?,?,?,?,?,'op',?,?)",
                    (cid, OP_SENTINEL, c["label"], c["x"], c["y"], _now(),
                     json.dumps(c["spec"], ensure_ascii=False), c["memo"]))
            elif c["kind"] == "drop":
                conn.execute(
                    "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,"
                    " kind,memo) VALUES (?,?,?,?,?,?,'drop',?)",
                    (cid, DROP_SENTINEL, c["label"], c["x"], c["y"], _now(), c["memo"]))
            elif c["kind"] == "launcher":
                conn.execute(
                    "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,"
                    " kind,filter_ref,memo) VALUES (?,?,?,?,?,?,'launcher',?,?)",
                    (cid, LAUNCHER_SENTINEL, c["label"], c["x"], c["y"], _now(),
                     json.dumps(c["spec"], ensure_ascii=False), c["memo"]))
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
        # リクエスト矢印（kind='req'）。発行順は矢印を引いた順（ops.rs fire_prepare と同じ）。
        base2 = base + len(self._edges)
        for i, (f, t_, a_) in enumerate(self._reqs):
            attrs = json.dumps(a_, ensure_ascii=False) if a_ else None
            conn.execute("INSERT INTO relations (from_id,to_id,kind,created_at,attrs)"
                         " VALUES (?,?,'req',?,?)", (f, t_, base2 + i, attrs))
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
            print(f"  カード {len(self._cards)} 枚 / 矢印 {len(self._edges)} 本"
                  + (f" / リクエスト矢印 {len(self._reqs)} 本" if self._reqs else ""))
            for _src, dest_name, _ig in self._bundles:
                print(f"  同梱: .texos/filters/{dest_name}/")
            for label, out in self.terminals():
                print(f"  ▶ {label} → {out}")
            for w in warns:
                print(f"  警告: {w}")
            if locked:
                print("  ※ TexoS がこのプロジェクトを開いたままです。"
                      "開き直すまで面には出ません（規約 §3-8）")

    # ------------------------------------------------------ 親プロジェクトへの掲示
    def link_into(self, parent: str | None = None, x: float | None = None,
                  y: float | None = None, label: str | None = None,
                  verbose: bool = True) -> str:
        """組んだプロジェクトへの**プロジェクトカード（リンク）**を親に1枚置く。

        既にカードのあるプロジェクトへ、フローそのものを流し込むことはしない
        （save() が拒む＝二重生成と座標衝突の元）。代わりに「別プロジェクトとして組んで、
        いま開いているプロジェクトにその入口を貼る」。人はカードをダブルクリックすれば
        中へ入れるし、親の面は散らからない。

        parent … 親プロジェクトのフォルダ。None なら **いま開いているプロジェクト**
                 （current_project()）。
        x, y  … 置き場所。None なら親の既存カードの下に自動で空けて置く。

        注意: TexoS が親プロジェクトを開いたまま呼ぶと、**開き直すまで面に出ない**
        （規約 §3-8）。開き直すよう最後に案内する。
        """
        root = os.path.abspath(parent) if parent else current_project()
        if not root:
            raise BuildError(
                "親プロジェクトが分かりません（TexoS で一度もプロジェクトを開いていない？）。"
                "link_into(parent=r'...') でフォルダを指定してください"
            )
        db_path = os.path.join(root, ".texos", "texos.db")
        if not os.path.exists(db_path):
            raise BuildError(f"TexoS のプロジェクトではありません（.texos/texos.db が無い）: {root}")
        if os.path.abspath(root) == self.root:
            raise BuildError("自分自身にはリンクできません")

        target = self.root
        real_id = "project:" + hashlib.sha256(target.encode("utf-8")).hexdigest()
        conn = sqlite3.connect(db_path)
        try:
            dup = conn.execute(
                "SELECT id FROM virtual_bodies"
                " WHERE project_ref = ? AND deleted_at IS NULL LIMIT 1", (target,)
            ).fetchone()
            if dup:
                if verbose:
                    print(f"すでにリンク済み: {root} → {os.path.basename(target)}")
                return dup[0]
            if x is None or y is None:
                # 人はプロジェクトカードを一列に並べる。既にあるならその**右隣**に置く
                # （面の隅に一枚だけ飛ぶと、貼ったことに気づかれない）。
                row = conn.execute(
                    "SELECT MAX(pos_x), MIN(pos_y) FROM virtual_bodies"
                    " WHERE kind = 'project' AND deleted_at IS NULL"
                ).fetchone()
                if row and row[0] is not None:
                    ax, ay = row[0] + 178.0, row[1]
                else:
                    row = conn.execute(
                        "SELECT MIN(pos_x), MIN(pos_y) FROM virtual_bodies"
                        " WHERE deleted_at IS NULL"
                    ).fetchone()
                    ax = row[0] if row and row[0] is not None else 0.0
                    ay = (row[1] - 220.0) if row and row[1] is not None else 0.0
                x = ax if x is None else x
                y = ay if y is None else y
            conn.execute(
                "INSERT OR IGNORE INTO real_bodies (id,mime_type,size,created_at)"
                " VALUES (?,'application/vnd.texos.project',0,?)", (real_id, _now()))
            cid = uuid.uuid4().hex
            conn.execute(
                "INSERT INTO virtual_bodies (id,real_id,label,pos_x,pos_y,created_at,"
                " kind,project_ref) VALUES (?,?,?,?,?,?,'project',?)",
                (cid, real_id, label or os.path.basename(target), x, y, _now(), target))
            conn.commit()
        finally:
            conn.close()
        if verbose:
            print(f"リンクを設置: {root}")
            print(f"  「{label or os.path.basename(target)}」（{x:.0f}, {y:.0f}）→ {target}")
            print("  ※ TexoS で開いたままなら、プロジェクトを開き直すと出てきます")
        return cid


def current_project() -> str | None:
    """TexoS が最後に開いたプロジェクト＝「いま開いているプロジェクト」。

    正本は Tauri の app_data_dir にある config.json の "last"
    （`src-tauri/src/project.rs: add_recent`）。エージェントは人にパスを聞かなくてよい。
    """
    ident = "com.yoshi.texos"
    if os.name == "nt":
        base = os.environ.get("APPDATA")
    elif sys.platform == "darwin":
        base = os.path.expanduser("~/Library/Application Support")
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    if not base:
        return None
    cfg = os.path.join(base, ident, "config.json")
    try:
        with open(cfg, encoding="utf-8") as f:
            last = json.load(f).get("last")
    except (OSError, ValueError):
        return None
    if not last or not os.path.exists(os.path.join(last, ".texos", "texos.db")):
        return None
    return os.path.abspath(last)


def _now() -> int:
    return int(time.time() * 1000)
