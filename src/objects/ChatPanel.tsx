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
  /** v1.4.6 Ollama セットアップ案内のゲート用 */
  provider?: string;
  endpoint?: string;
}

const PANEL_W = 380;

export function ChatPanel({ objId, label, top, onClose, provider, endpoint }: Props) {
  // v1.4.6 Ollama 未導入PC向けのセットアップ案内（OpenAI準拠カードはスキップ）。
  const isOllama = (provider ?? "ollama") !== "openai";
  const [ollama, setOllama] = useState<"checking" | "ready" | "setup">(
    isOllama ? "checking" : "ready"
  );
  const [setupInfo, setSetupInfo] = useState({ installed: false, canWinget: false });
  const [setupMsg, setSetupMsg] = useState<string | null>(null);
  const [setupBusy, setSetupBusy] = useState(false);

  const ep = () => endpoint?.trim() || null;

  const checkOllama = useCallback(() => {
    if (!isOllama) {
      setOllama("ready");
      return;
    }
    setOllama("checking");
    invoke<{ running: boolean; installed: boolean; can_winget: boolean }>("ai_ollama_status", {
      endpoint: endpoint?.trim() || null,
    })
      .then((st) => {
        if (st.running) setOllama("ready");
        else {
          setSetupInfo({ installed: st.installed, canWinget: st.can_winget });
          setOllama("setup");
        }
      })
      .catch(() => setOllama("ready")); // 判定不能時は通常表示（送信エラーで気づける）
  }, [isOllama, endpoint]);

  useEffect(() => {
    checkOllama();
  }, [checkOllama]);

  // モデル取得の進捗（自動セットアップ中）。
  useEffect(() => {
    let alive = true;
    let un: (() => void) | undefined;
    listen<{ status: string; percent: number }>("ollama-pull", (e) => {
      const p = e.payload.percent ? ` ${e.payload.percent}%` : "";
      setSetupMsg(`既定モデル gemma3 を取得中… ${e.payload.status}${p}`);
    }).then((u) => {
      if (alive) un = u;
      else u();
    });
    return () => {
      alive = false;
      un?.();
    };
  }, []);

  const runSetup = async (mode: "install" | "start") => {
    setSetupBusy(true);
    setSetupMsg(
      mode === "install"
        ? "Ollama をインストール中…（数分かかることがあります）"
        : "Ollama を起動中…"
    );
    try {
      if (mode === "install") await invoke("ai_install_ollama");
      await invoke("ai_start_ollama").catch(() => {});
      setSetupMsg("サーバの起動を待っています…");
      let up = false;
      for (let i = 0; i < 25 && !up; i++) {
        await new Promise((r) => setTimeout(r, 1000));
        const st = await invoke<{ running: boolean }>("ai_ollama_status", {
          endpoint: ep(),
        }).catch(() => ({ running: false }));
        up = st.running;
      }
      if (!up) {
        setSetupMsg("起動を確認できませんでした。少し待ってから「再確認」を押してください。");
        setSetupBusy(false);
        return;
      }
      setSetupMsg("既定モデル gemma3 を取得中…（初回は数分）");
      await invoke("ai_pull_model", { model: "gemma3:latest", endpoint: ep() });
      setSetupMsg(null);
      setSetupBusy(false);
      setOllama("ready");
    } catch (e) {
      setSetupMsg("失敗しました: " + String(e));
      setSetupBusy(false);
    }
  };

  const openDownload = () => {
    invoke("open_with_default", { path: "https://ollama.com/download" }).catch(() => {});
  };

  const [history, setHistory] = useState<Msg[]>([]);
  const [streaming, setStreaming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [input, setInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [size, setSize] = useState({ w: PANEL_W, h: 420 }); // v1.4.1 リサイズ可能
  const [contextLabels, setContextLabels] = useState<
    { label: string; kind: string }[]
  >([]); // v1.4.4 接続資料（kind: image|text|other）
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
    invoke<{ label: string; kind: string }[]>("ai_context_labels", { id: objId })
      .then(setContextLabels)
      .catch(() => setContextLabels([]));
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
        background: "var(--panel-bg, #11161f)",
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
          background: "var(--panel-header-bg, #14281e)",
          borderBottom: "1px solid #2f6a4a",
          fontSize: 12,
          color: "var(--panel-header-text, #7fd8b0)",
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

      {/* v1.4.4 接続中の文脈カード（矢印を引いた順 = 資料の順番） */}
      {contextLabels.length > 0 && (
        <div
          title="このカードへ矢印で繋いだカードを資料として参照します（接続順）"
          style={{
            padding: "4px 8px",
            fontSize: 11,
            color: "var(--panel-muted, #9fc9b3)",
            background: "var(--panel-header-bg, #0f1b15)",
            borderBottom: "1px solid var(--panel-border, #233a2e)",
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
          }}
        >
          📎 資料:{" "}
          {contextLabels
            .map((c) => `${c.kind === "image" ? "🖼" : c.kind === "text" ? "📄" : "❔"}${c.label}`)
            .join(" → ")}
        </div>
      )}

      {/* v1.4.6 Ollama 状態確認中 */}
      {ollama === "checking" && (
        <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--panel-muted, #6b7686)", fontSize: 12 }}>
          ローカルAI（Ollama）の状態を確認中…
        </div>
      )}

      {/* v1.4.6 Ollama 未起動 → セットアップ案内 */}
      {ollama === "setup" && (
        <div style={{ flex: 1, overflowY: "auto", padding: 16, display: "flex", flexDirection: "column", gap: 12, justifyContent: "center" }}>
          <div style={{ fontSize: 13, color: "var(--panel-text, #d6e8dd)", lineHeight: 1.6 }}>
            🦙 ローカルAI「Ollama」が見つかりません。
            <br />
            {setupInfo.installed
              ? "インストール済みですが、サーバが起動していないようです。"
              : "AI対話にはローカルで動く Ollama が必要です。"}
          </div>
          {setupMsg && (
            <div style={{ fontSize: 12, color: "var(--panel-muted, #9fc9b3)", whiteSpace: "pre-wrap" }}>{setupMsg}</div>
          )}
          {!setupBusy && (
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              {setupInfo.installed ? (
                <button onClick={() => void runSetup("start")} style={setupBtn("#2f6a4a")}>
                  Ollama を起動して使う
                </button>
              ) : setupInfo.canWinget ? (
                <button onClick={() => void runSetup("install")} style={setupBtn("#2f6a4a")}>
                  自動インストール（gemma3 も取得）
                </button>
              ) : null}
              <button onClick={openDownload} style={setupBtn("#2a3346")}>
                ダウンロードページを開く（手動）
              </button>
              <button onClick={checkOllama} style={setupBtn("#2a3346")}>
                ↻ 再確認
              </button>
            </div>
          )}
          {setupBusy && (
            <div style={{ fontSize: 12, color: "var(--panel-muted, #9fc9b3)" }}>処理中… しばらくお待ちください</div>
          )}
        </div>
      )}

      {ollama === "ready" && (
        <>
      {/* messages */}
      <div ref={scrollRef} style={{ flex: 1, overflowY: "auto", padding: 8, display: "flex", flexDirection: "column", gap: 8 }}>
        {history.length === 0 && streaming == null && (
          <div style={{ color: "var(--panel-muted, #6b7686)", fontSize: 12, textAlign: "center", marginTop: 16 }}>
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
      <div style={{ display: "flex", gap: 6, padding: 8, borderTop: "1px solid var(--panel-border, #243043)", background: "var(--panel-header-bg, #0e121c)" }}>
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          placeholder="メッセージ（Enter送信 / Shift+Enter改行）"
          rows={2}
          style={{
            flex: 1,
            resize: "none",
            background: "var(--panel-input-bg, #0a0e16)",
            border: "1px solid var(--panel-border, #2a3346)",
            borderRadius: 6,
            color: "var(--panel-text, #e6ecf6)",
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
        </>
      )}

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
          background: isUser
            ? "var(--chat-bubble-user-bg, #1e3a5a)"
            : "var(--chat-bubble-ai-bg, #1a2620)",
          color: isUser
            ? "var(--chat-bubble-user-text, #cfe3fb)"
            : "var(--chat-bubble-ai-text, #d6e8dd)",
          border: `1px solid ${
            isUser ? "var(--chat-bubble-user-border, #2f5a8a)" : "var(--chat-bubble-ai-border, #2f6a4a)"
          }`,
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
    color: "#fff",
    background: "var(--panel-btn-bg, #1c2a22)",
    border: "1px solid var(--panel-border, #2f6a4a)",
    borderRadius: 5,
    cursor: "pointer",
    flex: "none",
  };
}

function setupBtn(bg: string): React.CSSProperties {
  return {
    padding: "9px 14px",
    fontSize: 13,
    fontWeight: 600,
    color: "#fff",
    background: bg,
    border: "1px solid #2f6a4a",
    borderRadius: 7,
    cursor: "pointer",
    fontFamily: "sans-serif",
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
