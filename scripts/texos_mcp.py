#!/usr/bin/env python3
"""TexoS MCP サーバー（stdio）— 起動中の TexoS を AI エージェントから操作する口。

    Claude Code などの MCP クライアント ──stdio(JSON-RPC)──▶ このスクリプト
        ──名前付きパイプ（Windows）/ Unix ソケット＋トークン──▶ 起動中の TexoS

* 標準ライブラリだけで動く（pip 不要）。標準出力は MCP 通信専用、ログは標準エラーへ。
* TexoS 側で「キャンバス右クリック → 表示・設定 → AIエージェント接続（MCP）を受け付ける」を
  オンにしておくこと（既定オフ）。オンにすると app_data に mcp.json（口の名前とトークン）ができ、
  このスクリプトはそれを読んで接続する。
* 操作はすべて TexoS 本体の処理（UI と同じ関数）で行われ、画面にその場で反映される。
  SQLite へ直接は書かない。

使い方は docs/MCP接続.md。
"""
from __future__ import annotations

import json
import os
import socket
import sys
import time
from pathlib import Path

SERVER_NAME = "texos"
SERVER_VERSION = "1.0.0"
SUPPORTED_PROTOCOLS = ("2025-06-18", "2025-03-26", "2024-11-05")
APP_ID = "com.yoshi.texos"
VERBOSE = "--verbose" in sys.argv or os.environ.get("TEXOS_MCP_VERBOSE") == "1"


def log(*a) -> None:
    if VERBOSE:
        print("[texos-mcp]", *a, file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# TexoS との接続
# ---------------------------------------------------------------------------

def discovery_candidates() -> list[Path]:
    env = os.environ.get("TEXOS_MCP_DISCOVERY")
    if env:
        return [Path(env)]
    home = Path.home()
    if sys.platform == "win32":
        base = Path(os.environ.get("APPDATA", home / "AppData" / "Roaming"))
        return [base / APP_ID / "mcp.json"]
    if sys.platform == "darwin":
        return [home / "Library" / "Application Support" / APP_ID / "mcp.json"]
    xdg_data = Path(os.environ.get("XDG_DATA_HOME", home / ".local" / "share"))
    xdg_conf = Path(os.environ.get("XDG_CONFIG_HOME", home / ".config"))
    return [xdg_data / APP_ID / "mcp.json", xdg_conf / APP_ID / "mcp.json"]


class Unavailable(Exception):
    """TexoS に繋がらない（起動していない・MCP 接続がオフ）。"""


class TexosConnection:
    def __init__(self) -> None:
        self.info: dict | None = None
        self.raw = None      # Windows: パイプのファイル
        self.sock = None     # Unix: ソケット
        self.buf = b""
        self.next_id = 1

    def _load(self) -> dict:
        for p in discovery_candidates():
            if p.is_file():
                try:
                    return json.loads(p.read_text(encoding="utf-8"))
                except (OSError, ValueError) as e:
                    raise Unavailable(f"{p} を読めません: {e}")
        raise Unavailable(
            "TexoS の MCP 接続が見つかりません。TexoS を起動し、キャンバス右クリック →"
            "「表示・設定」→「AIエージェント接続（MCP）を受け付ける」をオンにしてください。"
        )

    def close(self) -> None:
        for h in (self.raw, self.sock):
            try:
                if h:
                    h.close()
            except OSError:
                pass
        self.raw = self.sock = None
        self.buf = b""

    def _connect(self) -> None:
        self.close()
        self.info = self._load()
        ep = self.info.get("endpoint", "")
        if sys.platform == "win32":
            deadline = time.time() + 5
            while True:
                try:
                    self.raw = open(ep, "r+b", buffering=0)
                    break
                except FileNotFoundError:
                    raise Unavailable(
                        "TexoS が応答しません（起動していないか、MCP 接続がオフです）。"
                    )
                except OSError as e:
                    # ERROR_PIPE_BUSY（231）: 空きインスタンス待ち。
                    if getattr(e, "winerror", None) == 231 and time.time() < deadline:
                        time.sleep(0.1)
                        continue
                    raise Unavailable(f"TexoS に接続できません: {e}")
        else:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                s.connect(ep)
            except OSError as e:
                raise Unavailable(f"TexoS に接続できません: {e}")
            self.sock = s
        log("connected", ep)

    def _send(self, data: bytes) -> None:
        if self.raw is not None:
            self.raw.write(data)
        else:
            self.sock.sendall(data)

    def _recv_line(self) -> bytes:
        while b"\n" not in self.buf:
            chunk = self.raw.read(65536) if self.raw is not None else self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("TexoS が接続を閉じました")
            self.buf += chunk
        line, _, self.buf = self.buf.partition(b"\n")
        return line

    def call(self, method: str, params: dict) -> dict:
        """1要求を送り、応答（{ok, result|error}）を返す。切れていたら1回だけ繋ぎ直す。"""
        last: Exception | None = None
        for attempt in range(2):
            try:
                if self.raw is None and self.sock is None:
                    self._connect()
                rid = self.next_id
                self.next_id += 1
                req = {"token": self.info.get("token", ""), "id": rid,
                       "method": method, "params": params}
                self._send((json.dumps(req, ensure_ascii=False) + "\n").encode("utf-8"))
                resp = json.loads(self._recv_line().decode("utf-8"))
                if (not resp.get("ok") and attempt == 0
                        and resp.get("error", {}).get("code") == "unauthorized"):
                    # TexoS 側でトークンが作り直された（再起動・MCP の入れ直し）。読み直して再送。
                    self.close()
                    continue
                return resp
            except Unavailable:
                raise
            except (OSError, ValueError, ConnectionError) as e:
                last = e
                log("connection error", e)
                self.close()
        raise Unavailable(f"TexoS との通信に失敗しました: {last}")


# ---------------------------------------------------------------------------
# ツール定義
# ---------------------------------------------------------------------------

PROJECT = {"type": "string",
           "description": "対象プロジェクトのフォルダパス（get_project が返す project.path をそのまま渡す）。"
                          "TexoS で開いているプロジェクトと違えば project_mismatch で拒否される。"}
PROJECT_OPT = dict(PROJECT, description=PROJECT["description"] + "（読み取りでは省略可＝現在のプロジェクト）")
REQUEST_ID = {"type": "string",
              "description": "冪等キー（任意）。同じ request_id で再送すると、作り直さず1回目の結果を "
                             "replayed:true で返す（通信の再試行で二重にしない）。TexoS 再起動で消える。"}
CARD_ID = {"type": "string", "description": "カード ID（list_cards / 作成系の結果の card_id）。"}
XY = {"type": "number"}

AI_SETTINGS = {
    "type": "object",
    "description": (
        "AI 変換カードの設定。model は必須（list_ai_models で実機のモデルを確認して指定。"
        "マニュアル等のモデル名を写さない）。"
        "キー: model, provider(ollama|lmstudio|openai・既定 ollama), endpoint, system(役割の文), "
        "temperature, prefix(終端にしたときの出力名), seq, think(off|on|low|medium|high), "
        "num_ctx, gpu(auto|max|cpu), send_reasoning, note。update_card では null でキー削除。"
    ),
}
BUILTIN_SETTINGS = {
    "type": "object",
    "description": (
        "内蔵カードの設定。キー: pattern, replacement, lines, order, compare, unique, delimiter, "
        "fields, encoding, separator(text_concat: newline|none|blank|rule), lang(md_codeblock), "
        "prefix, seq, output_ext, accept_mime, note。update_card では null でキー削除。"
    ),
}

TOOLS: list[dict] = [
    {
        "name": "status",
        "description": "TexoS との接続確認。TexoS のバージョン・MCP 接続の状態・いま開いているプロジェクトを返す。",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "get_project",
        "description": "いま TexoS で開いているプロジェクト（path/name）とカード・付箋・矢印の件数、"
                       "実行中ジョブを返す。変更・実行の前に必ず呼び、返った path を以後の project に渡す。",
        "inputSchema": {"type": "object", "properties": {"project": PROJECT_OPT}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "list_cards",
        "description": "カード（付箋を含む）の一覧。id・kind（note/file/filter/op/url/project/folder…）・"
                       "type（ai / builtin:text_concat / op:out / program など）・ラベル・座標・"
                       "直近の実行結果（last_run）・mcp_editable を返す。設定や本文の全体は含まない"
                       "（get_card / read_card_text で個別に取る）。既定100件・最大500件、next_offset でページング。",
        "inputSchema": {"type": "object", "properties": {
            "project": PROJECT_OPT,
            "kinds": {"type": "array", "items": {"type": "string"},
                      "description": "kind で絞る（例 [\"note\",\"filter\"]）。"},
            "limit": {"type": "integer", "minimum": 1, "maximum": 500},
            "offset": {"type": "integer", "minimum": 0},
        }},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "get_card",
        "description": "カード1枚の詳細: 設定（filter_ref の JSON。秘密らしいキーは *** に伏せ、長い文字列は8000字で切る）・"
                       "メモ・直近の実行結果・入ってくる矢印（inputs）と出ていく矢印（outputs）。",
        "inputSchema": {"type": "object", "required": ["card_id"],
                        "properties": {"project": PROJECT_OPT, "card_id": CARD_ID}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "list_relations",
        "description": "矢印の一覧（id・from_id・to_id・kind・attrs）。kind=flow はデータの接続（pull 評価で遡る）、"
                       "req はトリガ／タイマ等からの発行、link はデータの流れない関連線。card_id で絞り込める。",
        "inputSchema": {"type": "object", "properties": {
            "project": PROJECT_OPT, "card_id": CARD_ID,
            "limit": {"type": "integer", "minimum": 1}, "offset": {"type": "integer", "minimum": 0}}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "read_card_text",
        "description": "付箋の本文、またはテキスト系ファイルカード（実行結果の .md など）の中身を返す。"
                       "既定 20000 字・最大 200000 字。続きは next_offset を offset に渡して読む。"
                       "画像・PDF などバイナリは not_text で断る。",
        "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
            "project": PROJECT_OPT, "card_id": CARD_ID,
            "max_chars": {"type": "integer", "minimum": 1, "maximum": 200000},
            "offset": {"type": "integer", "minimum": 0}}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "list_ai_models",
        "description": "ローカルの LLM サーバーにあるモデル名の一覧（Ollama 既定 http://localhost:11434 / "
                       "LM Studio http://localhost:1234/v1）。AI カードの model はここから選ぶこと。",
        "inputSchema": {"type": "object", "properties": {
            "provider": {"type": "string", "enum": ["ollama", "lmstudio", "openai"]},
            "endpoint": {"type": "string"}}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "create_note",
        "description": "付箋を作る。付箋をフローに繋ぐと本文が text/plain の定数として流れる"
                       "（課題・パラメータ・役割の置き場）。画面にその場で出て、Ctrl+Z で消せる。",
        "inputSchema": {"type": "object", "required": ["project", "text", "x", "y"], "properties": {
            "project": PROJECT, "text": {"type": "string"}, "x": XY, "y": XY, "request_id": REQUEST_ID}},
    },
    {
        "name": "update_note",
        "description": "付箋の本文を書き換える（UNDO の対象外＝UI の本文編集と同じ）。",
        "inputSchema": {"type": "object", "required": ["project", "card_id", "text"], "properties": {
            "project": PROJECT, "card_id": CARD_ID, "text": {"type": "string"}}},
    },
    {
        "name": "create_card",
        "description": (
            "処理カードを作る。type: "
            "\"ai\"＝AI 変換カード（builtin:ai・transform。settings.model 必須）／"
            "\"builtin\"＝内蔵カード（name に text_concat, text_diff, text_paste, text_grep, text_replace, "
            "text_head, text_tail, text_sort, text_count, text_cut, text_convert, md_codeblock, tap）／"
            "\"out\"＝OUT カード（▶で実行できる素通しの出口・settings.prefix で出力名）／\"in\"＝IN カード。"
            "外部プログラム・シェル・画像生成などは MCP からは作れない。"
            "注意: AI カードは通常入力ごとに別々に実行される（ファンアウト）。複数資料を1回で読ませるなら"
            "前に text_concat を挟む。役割（システムプロンプト）は付箋から role=system の矢印で渡せる。"
        ),
        "inputSchema": {"type": "object", "required": ["project", "type", "x", "y"], "properties": {
            "project": PROJECT,
            "type": {"type": "string", "enum": ["ai", "builtin", "out", "in"]},
            "name": {"type": "string", "description": "type=builtin のときの内蔵カード名。"},
            "label": {"type": "string"},
            "x": XY, "y": XY,
            "settings": {"type": "object", "description": AI_SETTINGS["description"] + " / " +
                         BUILTIN_SETTINGS["description"]},
            "request_id": REQUEST_ID}},
    },
    {
        "name": "update_card",
        "description": "AI 変換カード・対応する内蔵カード・OUT カードの設定を部分更新する（settings のキーをマージ、"
                       "null はキー削除）。種別そのもの（builtin/mode）は変えられない。UNDO の対象外。",
        "inputSchema": {"type": "object", "required": ["project", "card_id", "settings"], "properties": {
            "project": PROJECT, "card_id": CARD_ID, "settings": {"type": "object"}}},
    },
    {
        "name": "move_card",
        "description": "カード・付箋の位置を変える（Ctrl+Z で元の位置へ戻せる）。",
        "inputSchema": {"type": "object", "required": ["project", "card_id", "x", "y"], "properties": {
            "project": PROJECT, "card_id": CARD_ID, "x": XY, "y": XY}},
    },
    {
        "name": "connect",
        "description": (
            "矢印を引く（from → to）。処理カード（filter/OUT/IN）が絡めば kind=flow（データの接続）、"
            "トリガ・タイマ等が絡めば req（発行）、それ以外は link。同じ矢印が既にあればそれを返す（already_existed）。"
            "集約カード（text_concat 等）への入力順は矢印を作った順。"
            "role: system＝上流の中身を AI カードの役割（システムプロンプト）にする（入力に数えない・AI カード宛のみ）／"
            "previous＝反復カードからの戻り矢印／answer＝付箋から問い合わせカードへの答え。"
            "when: always|success|failure、carry: input|none は条件付きの矢印。"
        ),
        "inputSchema": {"type": "object", "required": ["project", "from_id", "to_id"], "properties": {
            "project": PROJECT, "from_id": CARD_ID, "to_id": CARD_ID,
            "role": {"type": "string", "enum": ["system", "previous", "answer"]},
            "when": {"type": "string", "enum": ["always", "success", "failure"]},
            "carry": {"type": "string", "enum": ["input", "none"]},
            "request_id": REQUEST_ID}},
    },
    {
        "name": "update_relation",
        "description": "矢印の属性を変える（role / when / carry / port）。空文字でその属性を外す。UNDO の対象外。",
        "inputSchema": {"type": "object", "required": ["project", "relation_id"], "properties": {
            "project": PROJECT, "relation_id": {"type": "integer"},
            "role": {"type": "string"}, "when": {"type": "string"}, "carry": {"type": "string"},
            "port": {"type": "integer", "description": "入力の順番（1始まり・0以下で外す）。"}}},
    },
    {
        "name": "disconnect",
        "description": "矢印を外す（ソフト削除。画面の Ctrl+Z で戻せる）。",
        "inputSchema": {"type": "object", "required": ["project", "relation_id"], "properties": {
            "project": PROJECT, "relation_id": {"type": "integer"}}},
    },
    {
        "name": "import_file",
        "description": "既存のファイル（絶対パス）をカードとして取り込む。プロジェクト内のファイルは実身、"
                       "外のファイルは窓（仮身）になる（UI のドロップと同じ）。フォルダは不可。",
        "inputSchema": {"type": "object", "required": ["project", "path", "x", "y"], "properties": {
            "project": PROJECT, "path": {"type": "string"}, "label": {"type": "string"},
            "x": XY, "y": XY, "request_id": REQUEST_ID}},
    },
    {
        "name": "run_card",
        "description": (
            "終端カード（処理カードか OUT カード）を実行する（UI の ▶ と同じ）。すぐに job_id を返し、"
            "結果は get_job で取る。実行は終端から矢印を遡る pull 評価で、必要な上流だけが走る。"
            "ファイルになるのは終端の出力だけ（中間はメモリ上）。"
            "⚠ 上流に外部プログラム・シェル・Web 取得などがあれば、それらも実行される（副作用あり）。"
            "同じ終端が実行中なら busy。終端の完了は上流すべての成功を意味しない — get_job の status・errors・nodes を見ること。"
        ),
        "inputSchema": {"type": "object", "required": ["project", "card_id"], "properties": {
            "project": PROJECT, "card_id": CARD_ID, "request_id": REQUEST_ID}},
    },
    {
        "name": "get_job",
        "description": (
            "ジョブの状態。status: running | cancelling | succeeded | completed_with_errors（どこかの段が失敗。"
            "出力は欠けている可能性）| nodata（材料が無く何も作らなかった）| failed（実行自体が失敗）| cancelled。"
            "完了時は output_cards（card_id・label・path・created）、errors、skipped、nodes（段ごとの status=ok/error/nodata/reused）を返す。"
            "出力の中身は read_card_text で読んで確認すること。"
        ),
        "inputSchema": {"type": "object", "required": ["job_id"], "properties": {
            "job_id": {"type": "integer"},
            "include_nodes": {"type": "boolean", "description": "段ごとの記録を含めるか（既定 true）。"},
            "wait_seconds": {"type": "number", "minimum": 0, "maximum": 60,
                             "description": "実行中なら最大この秒数だけ完了を待ってから返す（既定 0）。"}}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "cancel_job",
        "description": "実行中のジョブを中断する（UI の中断ボタンと同じ。子プロセスはツリーごと止まる）。",
        "inputSchema": {"type": "object", "required": ["job_id"], "properties": {"job_id": {"type": "integer"}}},
    },
    {
        "name": "list_jobs",
        "description": "実行中のジョブ（UI・トリガ起動を含む）と、MCP から起動した直近のジョブの状態。",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
    },
]
TOOL_NAMES = {t["name"] for t in TOOLS}

INSTRUCTIONS = """TexoS（カードと矢印で処理を組むツール）を、いま開いている画面と同じ状態で操作する。
手順: get_project で project.path を得る → 以後の変更・実行には必ず project にそれを渡す。
実行モデル（変えられない約束）:
- 実行は終端カードから矢印を遡る pull 評価。run_card は終端を指定する。中間の出力はファイルにならない。
- 矢印 kind=flow はデータの接続、req はトリガ等からの発行。
- AI 変換カードは通常入力ごとに別々に実行される。複数資料をまとめて読ませるなら text_concat を挟む。
- role=system の矢印は付箋の中身を AI の役割にする（入力に数えない）。
- 終端が完了しても上流がすべて成功したとは限らない。get_job の status/errors/nodes を見て、
  出力は read_card_text で中身を読んで確認する。
- AI のモデル名は list_ai_models で実機を確認してから決める。
詳しい規約は TexoS リポジトリの docs/AIエージェント向け_フロー構築規約.md、MCP の使い方は docs/MCP接続.md。"""


# ---------------------------------------------------------------------------
# MCP（JSON-RPC over stdio）
# ---------------------------------------------------------------------------

CONN = TexosConnection()


def tool_result(payload: dict, is_error: bool = False) -> dict:
    return {
        "content": [{"type": "text", "text": json.dumps(payload, ensure_ascii=False, indent=1)}],
        "structuredContent": payload,
        "isError": is_error,
    }


def call_tool(name: str, args: dict) -> dict:
    if name not in TOOL_NAMES:
        return tool_result({"error": {"code": "unknown_tool", "message": f"未知のツール: {name}"}}, True)
    wait = 0.0
    if name == "get_job":
        args = dict(args)
        try:
            wait = max(0.0, min(60.0, float(args.pop("wait_seconds", 0) or 0)))
        except (TypeError, ValueError):
            wait = 0.0
    try:
        deadline = time.time() + wait
        while True:
            resp = CONN.call(name, args)
            if (name == "get_job" and resp.get("ok")
                    and resp["result"].get("status") in ("running", "cancelling")
                    and time.time() < deadline):
                time.sleep(min(1.0, max(0.1, deadline - time.time())))
                continue
            break
    except Unavailable as e:
        return tool_result({"error": {"code": "texos_unavailable", "message": str(e)}}, True)
    if resp.get("ok"):
        return tool_result(resp.get("result") or {})
    return tool_result({"error": resp.get("error") or {"code": "unknown", "message": "不明なエラー"}}, True)


def handle(msg: dict) -> dict | None:
    method = msg.get("method")
    mid = msg.get("id")
    if mid is None:  # 通知（initialized / cancelled 等）は返事をしない
        return None
    if method == "initialize":
        requested = (msg.get("params") or {}).get("protocolVersion")
        version = requested if requested in SUPPORTED_PROTOCOLS else SUPPORTED_PROTOCOLS[0]
        return {"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "instructions": INSTRUCTIONS,
        }}
    if method == "ping":
        return {"jsonrpc": "2.0", "id": mid, "result": {}}
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS}}
    if method == "tools/call":
        p = msg.get("params") or {}
        args = p.get("arguments") or {}
        if not isinstance(args, dict):
            return {"jsonrpc": "2.0", "id": mid,
                    "error": {"code": -32602, "message": "arguments must be an object"}}
        return {"jsonrpc": "2.0", "id": mid, "result": call_tool(p.get("name", ""), args)}
    return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": f"Method not found: {method}"}}


def main() -> None:
    # 標準入出力は UTF-8 固定（Windows の既定コードページに左右されない）。
    stdin = open(sys.stdin.fileno(), "r", encoding="utf-8", newline="\n", closefd=False)
    stdout = open(sys.stdout.fileno(), "w", encoding="utf-8", newline="\n", closefd=False)
    log("started")
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            out = {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "Parse error"}}
        else:
            try:
                out = handle(msg) if isinstance(msg, dict) else {
                    "jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": "Invalid Request"}}
            except Exception as e:  # 想定外でもサーバーを落とさない
                log("internal error", repr(e))
                out = {"jsonrpc": "2.0", "id": msg.get("id") if isinstance(msg, dict) else None,
                       "error": {"code": -32603, "message": f"Internal error: {e}"}}
        if out is not None:
            stdout.write(json.dumps(out, ensure_ascii=False) + "\n")
            stdout.flush()
    CONN.close()


if __name__ == "__main__":
    main()
