import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

// v1.5.2 WS-C 背景ジョブパネル（凍結仕様 §8）: 「フロー実行中…」1行パネルを実行中ジョブの
// 一覧へ。各行 = 種別アイコン／ラベル／経過／進捗（note・percent）／停止（record 対応の
// フローのみ・stdin へ停止コマンド）／中断（job.cancel）。最下部に Stop-all（中央 kill）。
// カードが画面外・プロジェクト切替後でもここに残る＝「最後の物理停止」（自律安全 §6.3）。
// テーマは --panel-* 変数追従（ビュワー系＝2026-06-26 方針。設定ダイアログの暗色固定とは別）。

export type JobRow = {
  id: number;
  kind: string; // "flow" | "chat" | "setup"
  label: string;
  startedAt: number; // unix ms
  note?: string;
  percent?: number;
  /// 録音など stdin 停止対応のフロー（true のとき「停止」を出す）
  record?: boolean;
  stopCmd?: string;
};

const KIND_ICON: Record<string, string> = {
  flow: "▶",
  chat: "💬",
  setup: "⬇",
};

function elapsedText(startedAt: number, now: number): string {
  const s = Math.max(0, Math.floor((now - startedAt) / 1000));
  const m = Math.floor(s / 60);
  const sec = s % 60;
  if (m >= 60) {
    const h = Math.floor(m / 60);
    return `${h}:${String(m % 60).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
  }
  return `${m}:${String(sec).padStart(2, "0")}`;
}

export function JobsPanel({ jobs }: { jobs: JobRow[] }) {
  // 経過時間の再描画（1秒刻み。パネルが出ている間だけ）。
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  if (jobs.length === 0) return null;

  const btn = (bg: string, border?: string): React.CSSProperties => ({
    padding: "4px 12px",
    background: bg,
    color: "#ffffff",
    border: border ? `1px solid ${border}` : "none",
    borderRadius: 6,
    cursor: "pointer",
    fontSize: 12,
    fontFamily: "sans-serif",
    whiteSpace: "nowrap",
  });

  return (
    <div
      style={{
        position: "fixed",
        bottom: 24,
        left: "50%",
        transform: "translateX(-50%)",
        zIndex: 2500,
        display: "flex",
        flexDirection: "column",
        gap: 6,
        minWidth: 420,
        maxWidth: 640,
        background: "var(--panel-bg, #1c2130)",
        border: "1px solid var(--panel-border, #38445e)",
        borderRadius: 10,
        padding: "10px 14px",
        color: "var(--panel-text, #d6dded)",
        fontFamily: "sans-serif",
        fontSize: 13,
        boxShadow: "0 8px 32px #000000aa",
      }}
    >
      {jobs.map((j) => (
        <div key={j.id} style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <span title={j.kind} style={{ width: 20, textAlign: "center" }}>
            {KIND_ICON[j.kind] ?? "•"}
          </span>
          <span
            style={{
              flex: 1,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
            title={j.label}
          >
            {j.label}
          </span>
          <span style={{ color: "var(--panel-muted, #7a8aa6)", fontVariantNumeric: "tabular-nums" }}>
            {elapsedText(j.startedAt, now)}
          </span>
          {(j.note || j.percent != null) && (
            <span
              style={{
                color: "var(--panel-muted, #7a8aa6)",
                maxWidth: 160,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
              title={j.note}
            >
              {j.percent != null ? `${j.percent}% ` : ""}
              {j.note ?? ""}
            </span>
          )}
          {j.kind === "flow" && j.record && (
            <button
              title={`stdin に "${j.stopCmd ?? "q"}" を送って正常終了させます（録音は WAV を確定）`}
              onClick={() =>
                void invoke("flow_send_stdin", { jobId: j.id, text: j.stopCmd ?? "q" }).catch(
                  () => {}
                )
              }
              style={btn("#2f6f4f")}
            >
              ⏹ 停止
            </button>
          )}
          <button
            title="このジョブを中断します（プロセスツリーを強制終了・出力は破棄）"
            onClick={() => void invoke("break_flow", { jobId: j.id }).catch(() => {})}
            style={btn("#b03a3a")}
          >
            ■ 中断
          </button>
        </div>
      ))}
      {/* 中央 kill スイッチ: 全ジョブ（フロー＋チャット＋セットアップ＋t2i 生成）を止める */}
      <div
        style={{
          display: "flex",
          justifyContent: "flex-end",
          borderTop: "1px solid var(--panel-border, #38445e)",
          paddingTop: 6,
          marginTop: 2,
        }}
      >
        <button
          title="実行中のすべてのジョブを停止します"
          onClick={() => void invoke("cancel_all").catch(() => {})}
          style={btn("#7a3a3a", "#a05050")}
        >
          ■ Stop-all
        </button>
      </div>
    </div>
  );
}
