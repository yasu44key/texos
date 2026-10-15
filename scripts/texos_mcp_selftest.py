#!/usr/bin/env python3
"""TexoS MCP 接続の通しテスト（実機）。

起動中の TexoS（MCP 接続オン・**検証用の空プロジェクトを開いた状態**）に対して、
Claude Code と同じ経路（stdio で texos_mcp.py を起動）で一連の操作を流す。
ユーザーのプロジェクトでは実行しないこと — 付箋・カード・出力ファイルが増える。

    python scripts/texos_mcp_selftest.py --project "F:\\...\\TexoS_MCP検証" [--model 名前]

--model を省くと list_ai_models の結果から選ぶ（名前を決め打ちしない）。
検証する流れ:
  1 接続して現在のプロジェクトを取得  2 付箋と AI 変換カードを作って接続（役割も付箋で）
  3（画面反映は目視／スクリーンショット）  4 モデルを実機から選ぶ  5 実行→ジョブ完了
  6 出力本文を取得して期待語を確認  7 付箋を書き換えて再実行
  8 異常系: プロジェクト不一致・不正 ID・不正設定・実行失敗・キャンセル・冪等再送
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent


class Mcp:
    def __init__(self) -> None:
        self.p = subprocess.Popen(
            [sys.executable, str(HERE / "texos_mcp.py")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr,
            encoding="utf-8", bufsize=1)
        self.n = 0
        init = self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                       "clientInfo": {"name": "selftest", "version": "1"}})
        assert init["serverInfo"]["name"] == "texos"
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")

    def rpc(self, method: str, params: dict) -> dict:
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method,
                                       "params": params}, ensure_ascii=False) + "\n")
        self.p.stdin.flush()
        resp = json.loads(self.p.stdout.readline())
        if "error" in resp:
            raise RuntimeError(resp["error"])
        return resp["result"]

    def tool(self, _tool: str, **args) -> tuple[bool, dict]:
        r = self.rpc("tools/call", {"name": _tool, "arguments": args})
        return (not r["isError"]), r["structuredContent"]

    def ok(self, _tool: str, **args) -> dict:
        good, v = self.tool(_tool, **args)
        if not good:
            raise AssertionError(f"{_tool} failed: {json.dumps(v, ensure_ascii=False)}")
        return v

    def err(self, _tool: str, _code: str, **args) -> dict:
        good, v = self.tool(_tool, **args)
        got = v.get("error", {}).get("code")
        if good or got != _code:
            raise AssertionError(f"{_tool}: expected error {_code}, got {json.dumps(v, ensure_ascii=False)}")
        return v

    def close(self) -> None:
        self.p.stdin.close()
        self.p.wait(timeout=10)


RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, cond: bool, detail: str = "") -> None:
    RESULTS.append((name, cond, detail))
    print(("PASS " if cond else "FAIL ") + name + (f" — {detail}" if detail else ""), flush=True)


def pick_model(models: list[str]) -> str:
    """埋め込み専用・巨大そうなモデルを避けつつ、一覧から1つ選ぶ（名前は決め打ちしない）。"""
    def score(m: str) -> tuple:
        low = m.lower()
        bad = any(w in low for w in ("embed", "vision", "ocr", "whisper"))
        size = 999.0
        import re
        for tok in re.findall(r"(\d+(?:\.\d+)?)b", low):
            size = min(size, float(tok))
        return (bad, size if size != 999.0 else 50.0, low)
    return sorted(models, key=score)[0]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--project", required=True)
    ap.add_argument("--model")
    ap.add_argument("--timeout", type=float, default=300)
    a = ap.parse_args()

    m = Mcp()
    try:
        # 1 接続・現在のプロジェクト
        st = m.ok("status")
        check("status", st.get("mcp_enabled") is True, f"TexoS {st.get('texos_version')}")
        gp = m.ok("get_project")
        proj = gp["project"]["path"]
        same = Path(proj).resolve() == Path(a.project).resolve()
        check("get_project が検証用プロジェクト", same, proj)
        if not same:
            print("検証用プロジェクトを開いてから実行してください。中止します。")
            return 2

        # 4 モデル（実機から）
        models = m.ok("list_ai_models")["models"]
        check("list_ai_models", len(models) > 0, f"{len(models)} 件")
        model = a.model or pick_model(models)
        check("モデル選択", model in models, model)

        # 2 付箋と AI カードを作り、接続（役割は role=system の付箋）
        rid = "selftest-" + uuid.uuid4().hex
        n1 = m.ok("create_note", project=proj, text="りんご", x=40, y=40, request_id=rid)
        n1b = m.ok("create_note", project=proj, text="りんご", x=40, y=40, request_id=rid)
        check("冪等: 同じ request_id は作り直さない", n1b["card_id"] == n1["card_id"] and n1b.get("replayed") is True)
        # 同じ request_id を別の内容・別のプロジェクトに使い回したら、成功（replayed）にしない（v1.11.0 検証で発見）。
        m.err("create_note", "request_id_conflict", project=proj, text="別の内容", x=40, y=40, request_id=rid)
        check("冪等: 同じ request_id で別の本文は request_id_conflict", True)
        m.err("create_note", "request_id_conflict", project=str(Path(proj).parent / "存在しない別プロジェクト"),
              text="りんご", x=40, y=40, request_id=rid)
        check("冪等: 同じ request_id で別のプロジェクトは request_id_conflict", True)
        n1c = m.ok("create_note", project=proj.replace("\\", "/") + "/", text="りんご", x=40, y=40, request_id=rid)
        check("冪等: パスの書き方違いは同じ要求として再送扱い", n1c["card_id"] == n1["card_id"] and n1c.get("replayed") is True)
        role = m.ok("create_note", project=proj, x=40, y=200,
                    text="役割（ここを書き換える）\n入力された日本語の単語を英語に訳し、英単語を大文字で1語だけ出力する。"
                         "説明・記号・引用符は書かない。")
        ai = m.ok("create_card", project=proj, type="ai", label="英訳", x=380, y=40,
                  settings={"model": model, "temperature": 0, "prefix": "MCP検証_出力"})
        e1 = m.ok("connect", project=proj, from_id=n1["card_id"], to_id=ai["card_id"])
        e2 = m.ok("connect", project=proj, from_id=role["card_id"], to_id=ai["card_id"], role="system")
        check("connect flow", e1["kind"] == "flow", f"relation {e1['relation_id']}")
        card = m.ok("get_card", project=proj, card_id=ai["card_id"])
        roles = [r["attrs"].get("role") if r["attrs"] else None for r in card["inputs"]]
        check("connect role=system", "system" in roles, str(roles))
        e1b = m.ok("connect", project=proj, from_id=n1["card_id"], to_id=ai["card_id"])
        check("同じ矢印は重複しない", e1b["already_existed"] and e1b["relation_id"] == e1["relation_id"])
        m.ok("move_card", project=proj, card_id=ai["card_id"], x=400, y=60)

        # 5 実行 → 完了
        run = m.ok("run_card", project=proj, card_id=ai["card_id"])
        job = m.ok("get_job", job_id=run["job_id"], wait_seconds=60)
        t0 = time.time()
        while job["status"] in ("running", "cancelling") and time.time() - t0 < a.timeout:
            job = m.ok("get_job", job_id=run["job_id"], wait_seconds=30)
        check("実行完了", job["status"] == "succeeded", job["status"])
        outs = job.get("output_cards", [])
        check("出力カード", len(outs) == 1, json.dumps(outs, ensure_ascii=False))

        # 6 出力本文
        text = m.ok("read_card_text", project=proj, card_id=outs[0]["card_id"])["text"]
        check("出力に APPLE", "APPLE" in text.upper(), repr(text[:80]))

        # 7 付箋を更新して再実行
        m.ok("update_note", project=proj, card_id=n1["card_id"], text="ねこ")
        run2 = m.ok("run_card", project=proj, card_id=ai["card_id"])
        job2 = m.ok("get_job", job_id=run2["job_id"], wait_seconds=60)
        while job2["status"] in ("running", "cancelling") and time.time() - t0 < a.timeout * 2:
            job2 = m.ok("get_job", job_id=run2["job_id"], wait_seconds=30)
        out2 = job2.get("output_cards", [])
        text2 = m.ok("read_card_text", project=proj, card_id=out2[0]["card_id"])["text"] if out2 else ""
        check("再実行（同じ prefix は同じカードを更新）",
              job2["status"] == "succeeded" and out2 and out2[0]["card_id"] == outs[0]["card_id"]
              and out2[0]["created"] is False, job2["status"])
        check("再実行の出力に CAT", "CAT" in text2.upper(), repr(text2[:80]))

        # 8 異常系
        wrong = str(Path(proj).parent / "存在しない別プロジェクト")
        v = m.err("create_note", "project_mismatch", project=wrong, text="x", x=0, y=0)
        check("プロジェクト不一致は拒否", v["error"].get("details", {}).get("current") is not None)
        m.err("run_card", "project_mismatch", project=wrong, card_id=ai["card_id"])
        check("実行もプロジェクト不一致は拒否", True)
        m.err("create_note", "invalid_params", text="x", x=0, y=0)
        check("project 無しの変更は拒否", True)
        m.err("get_card", "not_found", card_id="no-such-card")
        check("不正なカード ID", True)
        m.err("update_card", "unsupported_setting", project=proj, card_id=ai["card_id"],
              settings={"cmd": "del *"})
        check("不正な設定キーは拒否", True)
        m.err("create_card", "invalid_spec", project=proj, type="ai", x=0, y=0, settings={})
        check("model 無しの AI カードは拒否", True)
        m.err("create_card", "unsupported_card_type", project=proj, type="builtin", name="t2i", x=0, y=0)
        check("対応外の内蔵カードは拒否", True)
        m.err("run_card", "not_runnable", project=proj, card_id=n1["card_id"])
        check("付箋は終端にできない", True)
        m.err("get_job", "unknown_job", job_id=987654321)
        check("不明なジョブ", True)

        # 実行失敗（存在しないモデル）→ 終端は完走しても失敗が判別できる
        bad = m.ok("create_card", project=proj, type="ai", label="失敗する AI", x=380, y=420,
                   settings={"model": "no-such-model-" + uuid.uuid4().hex[:6], "prefix": "MCP検証_失敗"})
        m.ok("connect", project=proj, from_id=n1["card_id"], to_id=bad["card_id"])
        jb = m.ok("run_card", project=proj, card_id=bad["card_id"])
        jbv = m.ok("get_job", job_id=jb["job_id"], wait_seconds=60)
        check("実行失敗が判別できる", jbv["status"] in ("failed", "completed_with_errors"),
              f"{jbv['status']} / {(jbv.get('errors') or [jbv.get('error')])[:1]}")

        # 冪等: run_card の再送はジョブを増やさない
        rk = "selftest-run-" + uuid.uuid4().hex
        r1 = m.ok("run_card", project=proj, card_id=ai["card_id"], request_id=rk)
        r2 = m.ok("run_card", project=proj, card_id=ai["card_id"], request_id=rk)
        check("冪等: run_card の再送は同じジョブ", r1["job_id"] == r2["job_id"] and r2.get("replayed") is True)
        m.ok("get_job", job_id=r1["job_id"], wait_seconds=60)

        # キャンセル（長い生成を始めて止める）
        long_note = m.ok("create_note", project=proj, x=40, y=600,
                         text="日本の歴史を縄文時代から現代まで、時代ごとに3000字程度で書いてください。")
        slow = m.ok("create_card", project=proj, type="ai", label="長い生成（中断用）", x=380, y=600,
                    settings={"model": model, "prefix": "MCP検証_中断"})
        m.ok("connect", project=proj, from_id=long_note["card_id"], to_id=slow["card_id"])
        js = m.ok("run_card", project=proj, card_id=slow["card_id"])
        busy_ok, busy = m.tool("run_card", project=proj, card_id=slow["card_id"])
        check("同じ終端の多重起動は busy", (not busy_ok) and busy["error"]["code"] == "busy")
        time.sleep(3)
        c0 = time.time()
        m.ok("cancel_job", job_id=js["job_id"])
        jsv = m.ok("get_job", job_id=js["job_id"], wait_seconds=5)
        check("キャンセル要求が受理される", jsv["status"] in ("cancelling", "cancelled"), jsv["status"])
        # AI 変換は受信をストリームにしてあり、中断要求から数秒で止まる（応答の完走を待たない）。
        while jsv["status"] in ("running", "cancelling") and time.time() - c0 < 600:
            jsv = m.ok("get_job", job_id=js["job_id"], wait_seconds=30)
        check("キャンセルで終わり、出力カードを作らない",
              jsv["status"] == "cancelled" and not jsv.get("output_cards"),
              f"{jsv['status']} / 中断要求から {time.time() - c0:.0f} 秒")
        check("中断要求から 10 秒以内に止まる", time.time() - c0 < 10, f"{time.time() - c0:.1f} 秒")


        lc = m.ok("list_cards", project=proj, limit=2)
        check("list_cards のページング", lc["limit"] == 2 and (lc["total"] <= 2 or lc["next_offset"] == 2),
              f"total={lc['total']}")
    finally:
        m.close()

    failed = [r for r in RESULTS if not r[1]]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
