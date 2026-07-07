import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

// v1.5.3 WS-P1（オペ層設計 §5.4.3 案①の第1段）: カードへ入る flow 矢印の順序を
// 一覧＋上下ボタンで並べ替える。保存で attrs.port = 1..N を書き戻す（set_input_order）。
// {in2} 等の位置指定・セレクタの first-match 採用順がこの並びに従う。
// 第2段（キャンバス上の入りポート・矢印端点のドラッグ）は WS-P2 以降。

interface FlowInput {
  relation_id: number;
  label: string;
  port: number | null;
  when: string | null;
}

interface InputOrderDialogProps {
  cardId: string;
  cardLabel: string;
  onSaved: () => void;
  onCancel: () => void;
}

const WHEN_LABEL: Record<string, string> = {
  success: "成功時",
  failure: "失敗時",
};

export function InputOrderDialog({ cardId, cardLabel, onSaved, onCancel }: InputOrderDialogProps) {
  const [items, setItems] = useState<FlowInput[] | null>(null);

  useEffect(() => {
    invoke<FlowInput[]>("list_flow_inputs", { id: cardId })
      .then(setItems)
      .catch(() => setItems([]));
  }, [cardId]);

  const move = (i: number, dir: -1 | 1) => {
    setItems((prev) => {
      if (!prev) return prev;
      const j = i + dir;
      if (j < 0 || j >= prev.length) return prev;
      const next = [...prev];
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
  };

  const save = async () => {
    if (!items) return;
    try {
      await invoke("set_input_order", { relationIds: items.map((it) => it.relation_id) });
      onSaved();
    } catch (e) {
      console.error("set_input_order failed", e);
      onCancel();
    }
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
      onMouseDown={onCancel}
    >
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: "var(--panel-bg, #1c2130)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 10,
          padding: 20,
          minWidth: 380,
          maxWidth: 520,
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
        }}
      >
        <div style={{ fontSize: 14, marginBottom: 4 }}>入力の順序 — 「{cardLabel}」</div>
        <div style={{ fontSize: 12, color: "var(--panel-muted, #7a8aa6)", marginBottom: 12 }}>
          上から {"{in1}"}, {"{in2}"}, … の順になります（セレクタはこの順で最初の有データを採用）。
        </div>
        {items === null ? (
          <div style={{ fontSize: 13, color: "var(--panel-muted, #7a8aa6)" }}>読み込み中…</div>
        ) : items.length === 0 ? (
          <div style={{ fontSize: 13, color: "var(--panel-muted, #7a8aa6)" }}>
            入力の矢印がありません（データカードからこのカードへ矢印を引いてください）
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            {items.map((it, i) => (
              <div
                key={it.relation_id}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "6px 8px",
                  background: "var(--panel-input-bg, #0e121c)",
                  border: "1px solid var(--panel-border, #38445e)",
                  borderRadius: 6,
                }}
              >
                <span style={{ width: 40, color: "var(--panel-muted, #7a8aa6)", fontSize: 12 }}>
                  {`{in${i + 1}}`}
                </span>
                <span
                  style={{ flex: 1, fontSize: 13, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
                  title={it.label}
                >
                  {it.label}
                  {it.when && WHEN_LABEL[it.when] && (
                    <span style={{ color: "var(--panel-muted, #7a8aa6)", fontSize: 11 }}>
                      {`（${WHEN_LABEL[it.when]}）`}
                    </span>
                  )}
                </span>
                <button onClick={() => move(i, -1)} disabled={i === 0} style={arrowBtn(i === 0)}>
                  ↑
                </button>
                <button
                  onClick={() => move(i, 1)}
                  disabled={i === items.length - 1}
                  style={arrowBtn(i === items.length - 1)}
                >
                  ↓
                </button>
              </div>
            ))}
          </div>
        )}
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 16 }}>
          <button
            onClick={onCancel}
            style={btnStyle("var(--panel-btn-bg, #2a3346)", "var(--panel-text, #aeb8cc)")}
          >
            キャンセル
          </button>
          <button
            onClick={() => void save()}
            disabled={!items || items.length === 0}
            style={btnStyle("#3a6fb0", "#ffffff")}
          >
            この順で確定
          </button>
        </div>
      </div>
    </div>
  );
}

function arrowBtn(disabled: boolean): React.CSSProperties {
  return {
    padding: "2px 10px",
    background: "var(--panel-btn-bg, #2a3346)",
    color: disabled ? "var(--panel-muted, #566079)" : "var(--panel-text, #d6dded)",
    border: "none",
    borderRadius: 4,
    cursor: disabled ? "default" : "pointer",
    fontSize: 13,
  };
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
