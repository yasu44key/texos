import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// v1.4.1 AIチャット（builtin:"ai" mode:"chat"）の会話パネル。
// 履歴は backend の .texos/chats/<id>.json が source of truth。送信→
// "ai-chat-token" イベントで応答を逐次表示、"ai-chat-done" で確定して再読込。

interface Msg {
  role: string;
  content: string;
}

interface Props {
  objId: string;
  label: string;
  /** anchored offset from the card top (other panels use CARD_H + 8) */
  top: number;
  onClose: () => void;
}

const PANEL_W = 380;

export function ChatPanel({ objId, label, top, onClose }: Props) {
  const [history, setHistory] = useState<Msg[]>([]);
  const [streaming, setStreaming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [input, setInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [size, setSize] = useState({ w: PANEL_W, h: 420 }); // v1.4.1 リサイズ可能
  const scrollRef = useRef<HTMLDivElement>(null);

  const startResize = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const sx = e.clientX;
    const sy = e.clientY;
    const sw = size.w;
    const sh = size.h;
    const onMove = (me: globalThis.MouseEvent) => {
      setSize({
        w: Math.max(300, sw + (me.clientX - sx)),
        h: Math.max(260, sh + (me.clientY - sy)),
      });
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const reload = useCallback(() => {
    invoke<Msg[]>("ai_chat_history", { id: objId })
      .then(setHistory)
      .catch(() => {});
  }, [objId]);

  useEffect(() => {
    reload();
  }, [reload]);

  // ストリーミングイベント購読（このカード id 宛だけ拾う）。
  useEffect(() => {
    let alive = true;
    const unlisteners: Array<() => void> = [];
    const keep = (u: () => void) => {
      if (alive) unlisteners.push(u);
      else u();
    };
    listen<{ id: string; delta: string }>("ai-chat-token", (e) => {
      if (e.payload.id !== objId) return;
      setStreaming((prev) => (prev ?? "") + e.payload.delta);
    }).then(keep);
    listen<{ id: string; error: string | null }>("ai-chat-done", (e) => {
      if (e.payload.id !== objId) return;
      setBusy(false);
      setStreaming(null);
      if (e.payload.error) setError(e.payload.error);
      reload();
    }).then(keep);
    return () => {
      alive = false;
      unlisteners.forEach((u) => u());
    };
  }, [objId, reload]);

  // 末尾へ自動スクロール。
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [history, streaming]);

  const send = useCallback(() => {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setError(null);
    setBusy(true);
    setStreaming("");
    setHistory((prev) => [...prev, { role: "user", content: text }]); // 楽観表示
    invoke<string>("ai_chat_send", { id: objId, message: text }).catch((e) => {
      setError(String(e));
      setBusy(false);
      setStreaming(null);
    });
  }, [input, busy, objId]);

  const stop = () => {
    invoke("ai_chat_cancel").catch(() => {});
  };

  const clear = () => {
    invoke("ai_chat_clear", { id: objId })
      .then(() => {
        setHistory([]);
        setStreaming(null);
        setError(null);
      })
      .catch(() => {});
  };

  const onKey = (e: React.KeyboardEvent) => {
    // Enter=送信 / Shift+Enter=改行（チャットの定石）。
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    <div
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        top,
        left: 0,
        width: size.w,
        height: size.h,
        display: "flex",
        flexDirection: "column",
        background: "#11161f",
        border: "1px solid #2f6a4a",
        borderRadius: 8,
        boxShadow: "0 8px 28px #000000aa",
        zIndex: 10,
        overflow: "hidden",
        cursor: "default",
      }}
    >
      {/* header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          padding: "6px 8px",
          background: "#14281e",
          borderBottom: "1px solid #2f6a4a",
          fontSize: 12,
          color: "#7fd8b0",
        }}
      >
        <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          🤖 {label}
        </span>
        <button onClick={clear} title="会話をリセット" style={hdrBtn()}>
          履歴消去
        </button>
        <button onClick={onClose} title="閉じる" style={hdrBtn()}>
          ✕
        </button>
      </div>

      {/* messages */}
      <div ref={scrollRef} style={{ flex: 1, overflowY: "auto", padding: 8, display: "flex", flexDirection: "column", gap: 8 }}>
        {history.length === 0 && streaming == null && (
          <div style={{ color: "#6b7686", fontSize: 12, textAlign: "center", marginTop: 16 }}>
            メッセージを入力して会話を始めてください。
          </div>
        )}
        {history.map((m, i) => (
          <Bubble key={i} role={m.role} content={m.content} />
        ))}
        {streaming != null && <Bubble role="assistant" content={streaming + "▋"} />}
      </div>

      {error && (
        <div style={{ padding: "4px 8px", fontSize: 11, color: "#e89a9a", background: "#2a1414", borderTop: "1px solid #5a3a3a" }}>
          {error}
        </div>
      )}

      {/* input */}
      <div style={{ display: "flex", gap: 6, padding: 8, borderTop: "1px solid #243043", background: "#0e121c" }}>
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          placeholder="メッセージ（Enter送信 / Shift+Enter改行）"
          rows={2}
          style={{
            flex: 1,
            resize: "none",
            background: "#0a0e16",
            border: "1px solid #2a3346",
            borderRadius: 6,
            color: "#e6ecf6",
            fontSize: 13,
            padding: "6px 8px",
            outline: "none",
            fontFamily: "inherit",
          }}
        />
        {busy ? (
          <button onClick={stop} title="生成を停止" style={sendBtn("#a14a4a")}>
            停止
          </button>
        ) : (
          <button onClick={send} disabled={!input.trim()} title="送信（Enter）" style={sendBtn(input.trim() ? "#2f6a4a" : "#243043")}>
            送信
          </button>
        )}
      </div>

      {/* リサイズハンドル（右下） */}
      <div
        onMouseDown={startResize}
        title="ドラッグでパネルサイズ変更"
        style={{
          position: "absolute",
          right: 0,
          bottom: 0,
          width: 16,
          height: 16,
          cursor: "nwse-resize",
          background:
            "linear-gradient(135deg, transparent 0 50%, #4a6a58 50% 60%, transparent 60% 70%, #4a6a58 70% 80%, transparent 80%)",
        }}
      />
    </div>
  );
}

function Bubble({ role, content }: { role: string; content: string }) {
  const isUser = role === "user";
  return (
    <div style={{ display: "flex", justifyContent: isUser ? "flex-end" : "flex-start" }}>
      <div
        style={{
          maxWidth: "85%",
          padding: "6px 10px",
          borderRadius: 8,
          fontSize: 13,
          lineHeight: 1.5,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          background: isUser ? "#1e3a5a" : "#1a2620",
          color: isUser ? "#cfe3fb" : "#d6e8dd",
          border: `1px solid ${isUser ? "#2f5a8a" : "#2f6a4a"}`,
        }}
      >
        {content}
      </div>
    </div>
  );
}

function hdrBtn(): React.CSSProperties {
  return {
    padding: "2px 8px",
    fontSize: 11,
    color: "#aeb8cc",
    background: "#1c2a22",
    border: "1px solid #2f6a4a",
    borderRadius: 5,
    cursor: "pointer",
    flex: "none",
  };
}

function sendBtn(bg: string): React.CSSProperties {
  return {
    padding: "0 14px",
    fontSize: 13,
    fontWeight: 600,
    color: "#fff",
    background: bg,
    border: "none",
    borderRadius: 6,
    cursor: "pointer",
    flex: "none",
    fontFamily: "sans-serif",
  };
}
