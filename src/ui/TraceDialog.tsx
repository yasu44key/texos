// v1.8.7 実行トレース: カードの直近の実行記録（last_trace）を表示するダイアログ。
// フローは中間結果をメモリで渡すので、ここが「このカードが何を受け取り、何を出したか」を
// 後から確かめる唯一の窓になる。全ノード分は .texos/logs/ の Markdown（「ログ全体を開く」）。
import { invoke } from "@tauri-apps/api/core";

export interface TraceData {
  name: string;
  mime: string;
  bytes: number;
  sha256?: string;
  text?: string;
  truncated: boolean;
}

export interface TraceEntry {
  order: number;
  seq: number;
  card: string;
  label: string;
  kind: string;
  round: string;
  started_at: number;
  ms: number;
  status: "ok" | "error" | "nodata" | "reused" | "unfinished" | "info" | string;
  exit: number | null;
  inputs: TraceData[];
  outputs: TraceData[];
  errors: string[];
  skipped: number;
  notes: [string, string][];
}

export interface CardTrace {
  log: string;
  job_label: string;
  job_id: number;
  at: number;
  detail: boolean;
  entries: TraceEntry[];
}

interface Props {
  label: string;
  /** null = このカードはまだ実行記録がない。 */
  trace: CardTrace | null;
  debugMode: boolean;
  onClose: () => void;
}

const STATUS: Record<string, { text: string; fg: string; bg: string }> = {
  ok: { text: "✓ 成功", fg: "#7fd692", bg: "#2f5d3a66" },
  error: { text: "✗ エラー", fg: "#d97f7f", bg: "#5d2f2f66" },
  nodata: { text: "― no-data（材料なし・実行せず）", fg: "#aeb8cc", bg: "#3a425566" },
  reused: { text: "↺ 再利用（同じ実行内の結果）", fg: "#8fb8e8", bg: "#2f456066" },
  unfinished: { text: "⚠ 未完了（中断／構造エラー）", fg: "#d9a76a", bg: "#5d4a2f66" },
  info: { text: "ℹ 情報", fg: "#aeb8cc", bg: "#3a425566" },
};

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function fmtTime(ms: number): string {
  const d = new Date(ms);
  const p = (x: number) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

const preStyle: React.CSSProperties = {
  margin: "4px 0 6px",
  padding: 8,
  maxHeight: 240,
  overflow: "auto",
  whiteSpace: "pre-wrap",
  overflowWrap: "anywhere",
  background: "var(--panel-input-bg, #0e121c)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 6,
  fontFamily: "Consolas, monospace",
  fontSize: 12,
  lineHeight: 1.45,
};

function DataList({ title, items, detail }: { title: string; items: TraceData[]; detail: boolean }) {
  return (
    <div style={{ marginTop: 6 }}>
      <div style={{ color: "#8f9bb3", fontSize: 12 }}>
        {title}（{items.length}）
      </div>
      {items.length === 0 && <div style={{ fontSize: 12, opacity: 0.7, marginLeft: 10 }}>なし</div>}
      {items.map((d, i) => (
        <div key={i} style={{ marginLeft: 10 }}>
          {d.text !== undefined ? (
            <details open={items.length <= 2}>
              <summary style={{ cursor: "pointer", fontSize: 12.5 }}>
                <code>{d.name}</code>　{d.mime}・{fmtBytes(d.bytes)}
              </summary>
              <pre style={preStyle}>{d.text || "（空）"}</pre>
              {d.truncated && (
                <div style={{ fontSize: 11, color: "#d9a76a" }}>先頭 4KB で打ち切り（全文は記録していません）</div>
              )}
            </details>
          ) : (
            <div style={{ fontSize: 12.5 }}>
              <code>{d.name}</code>　{d.mime}・{fmtBytes(d.bytes)}
              {d.sha256 && <span style={{ opacity: 0.6 }}>　sha256:{d.sha256.slice(0, 16)}…</span>}
              {!detail && <span style={{ opacity: 0.55 }}>　（中身はデバッグモードで記録）</span>}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

export function TraceDialog({ label, trace, debugMode, onClose }: Props) {
  const openLog = () => {
    if (trace?.log) void invoke("open_with_default", { path: trace.log }).catch(() => {});
  };
  const revealLog = () => {
    if (trace?.log) void invoke("reveal_in_dir", { path: trace.log }).catch(() => {});
  };
  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "#00000066",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 2000,
      }}
      onMouseDown={onClose}
    >
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: "var(--panel-bg, #1c2130)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 10,
          padding: 20,
          width: 760,
          maxWidth: "94vw",
          maxHeight: "88vh",
          display: "flex",
          flexDirection: "column",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
          fontSize: 13,
        }}
      >
        <div style={{ fontSize: 15, marginBottom: 4 }}>実行ログ: {label}</div>
        {trace ? (
          <div style={{ fontSize: 12, color: "#8f9bb3", marginBottom: 10 }}>
            {fmtTime(trace.at)}　実行「{trace.job_label}」（ジョブ #{trace.job_id}）　記録:{" "}
            {trace.detail ? "詳細（デバッグモード）" : "要約"}
          </div>
        ) : null}
        <div style={{ flex: 1, overflow: "auto", minHeight: 80 }}>
          {!trace && (
            <div style={{ lineHeight: 1.7 }}>
              このカードの実行記録はまだありません。
              <br />
              フローを実行すると（トリガ発火を含む）、通ったカードごとに記録されます。
              <br />
              トリガ・タイマ・ウォッチャ自身は実行ノードではないので、発行先のカードのログを見てください。
            </div>
          )}
          {trace?.entries.map((e, i) => {
            const st = STATUS[e.status] ?? STATUS.info;
            return (
              <div
                key={i}
                style={{
                  border: "1px solid var(--panel-border, #38445e)",
                  borderLeft: `4px solid ${st.fg}`,
                  borderRadius: 6,
                  padding: "8px 10px",
                  marginBottom: 8,
                }}
              >
                <div style={{ display: "flex", gap: 10, alignItems: "baseline", flexWrap: "wrap" }}>
                  <span
                    style={{ fontSize: 12, padding: "2px 8px", borderRadius: 10, background: st.bg, color: st.fg }}
                  >
                    {st.text}
                  </span>
                  <span style={{ fontSize: 12, opacity: 0.8 }}>
                    #{e.order}
                    {e.round && `　周回 ${e.round}`}
                    {e.kind && `　${e.kind}`}
                    {e.status !== "reused" && e.status !== "info" && `　${e.ms} ms`}
                    {e.exit !== null && `　終了コード ${e.exit}`}
                  </span>
                </div>
                {e.status !== "reused" && e.status !== "info" && (
                  <>
                    <DataList title="入力" items={e.inputs} detail={trace.detail} />
                    <DataList title="出力" items={e.outputs} detail={trace.detail} />
                  </>
                )}
                {e.skipped > 0 && (
                  <div style={{ fontSize: 12, color: "#d9a76a", marginTop: 4 }}>
                    型不一致でスキップした入力: {e.skipped} 件
                  </div>
                )}
                {e.errors.map((err, j) => (
                  <pre key={j} style={{ ...preStyle, color: "#e6a0a0" }}>
                    {err}
                  </pre>
                ))}
                {e.notes.map(([k, v], j) => (
                  <div key={j} style={{ marginTop: 4 }}>
                    <span style={{ color: "#8f9bb3", fontSize: 12 }}>{k}: </span>
                    {v.includes("\n") ? (
                      <pre style={preStyle}>{v}</pre>
                    ) : (
                      <code style={{ fontSize: 12, overflowWrap: "anywhere" }}>{v}</code>
                    )}
                  </div>
                ))}
              </div>
            );
          })}
        </div>
        {!debugMode && (
          <div style={{ fontSize: 11.5, color: "#d9a76a", marginTop: 8 }}>
            デバッグモードはオフです（中身・stderr は記録されません）。キャンバス右クリック「表示・設定」からオンにできます。
          </div>
        )}
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 12 }}>
          {trace && (
            <>
              <button onClick={revealLog} style={btnStyle("#2a3346", "#aeb8cc")}>
                ログフォルダを表示
              </button>
              <button onClick={openLog} style={btnStyle("#2a3346", "#aeb8cc")}>
                この実行のログ全体を開く
              </button>
            </>
          )}
          <button onClick={onClose} style={btnStyle("#3a6fb0", "#ffffff")}>
            閉じる
          </button>
        </div>
      </div>
    </div>
  );
}

function btnStyle(bg: string, color: string): React.CSSProperties {
  return {
    padding: "7px 16px",
    background: bg,
    color,
    border: "none",
    borderRadius: 6,
    cursor: "pointer",
    fontSize: 13,
    fontFamily: "sans-serif",
  };
}
