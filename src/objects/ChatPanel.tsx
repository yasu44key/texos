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
  // v1.7.3 "lmstudio" も OpenAI準拠側 — ここを外すと LM Studio カードに Ollama の
  // 導入ボタンが出てしまう（案内先が違うので実害）。
  const isOllama = !["openai", "lmstudio"].includes(provider ?? "ollama");
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
  // v1.8.6 Thinking: 思考は「表示するだけ」。履歴にも MDログにも残らない（backend が
  // 本文だけを保存する）ので、ここでもターンが確定したら捨てる＝画面にも残さない。
  const [thinking, setThinking] = useState<string | null>(null);
  const [thinkOpen, setThinkOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [input, setInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [size, setSize] = useState({ w: PANEL_W, h: 420 }); // v1.4.1 リサイズ可能
  const [contextLabels, setContextLabels] = useState<
    { label: string; kind: string }[]
  >([]); // v1.4.4 接続資料（kind: image|text|other）
  const scrollRef = useRef<HTMLDivElement>(null);
  // v1.8.9 抜粋の書き出し: 吹き出しのチェック（履歴の何番目か）と「You:/AI: 見出しを付ける」。
  // 選択はパネルを閉じれば消える（書き出した MD が残る）。見出しの有無だけ覚えておく。
  const [selected, setSelected] = useState<Set<number>>(() => new Set());
  const [withLabels, setWithLabels] = useState(() => {
    try {
      return localStorage.getItem("texos-chat-export-labels") !== "0";
    } catch {
      return true;
    }
  });
  const [exportMsg, setExportMsg] = useState<string | null>(null);
  // v1.5.1 WS-B: 実行中ターンの job_id（job:started の card=このカードで受領）。
  // 停止ボタンはこれを ai_chat_cancel に渡す（未着なら null＝Chat 全体キャンセルで代用）。
  const jobIdRef = useRef<number | null>(null);

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
    listen<{ id: string; delta: string; thinking?: boolean }>("ai-chat-token", (e) => {
      if (e.payload.id !== objId) return;
      if (e.payload.thinking) setThinking((prev) => (prev ?? "") + e.payload.delta);
      else setStreaming((prev) => (prev ?? "") + e.payload.delta);
    }).then(keep);
    listen<{ id: string; error: string | null }>("ai-chat-done", (e) => {
      if (e.payload.id !== objId) return;
      setBusy(false);
      setStreaming(null);
      setThinking(null); // 思考は残さない（保存もしない）
      jobIdRef.current = null;
      if (e.payload.error) setError(e.payload.error);
      reload();
    }).then(keep);
    // v1.5.1 WS-B: このカードのターンの job_id を受領（中断ボタンの job_id 化）。
    listen<{ job_id: number; kind: string; card?: string }>("job:started", (e) => {
      if (e.payload.kind === "chat" && e.payload.card === objId) {
        jobIdRef.current = e.payload.job_id;
      }
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
  }, [history, streaming, thinking, thinkOpen]);

  const send = useCallback(() => {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setError(null);
    setBusy(true);
    setStreaming("");
    setThinking(null);
    setHistory((prev) => [...prev, { role: "user", content: text }]); // 楽観表示
    invoke<string>("ai_chat_send", { id: objId, message: text }).catch((e) => {
      setError(String(e));
      setBusy(false);
      setStreaming(null);
      setThinking(null);
    });
  }, [input, busy, objId]);

  const stop = () => {
    // job_id 未着（job:started 前の停止連打）は null＝バックエンドが Chat ジョブ全部に効かせる。
    invoke("ai_chat_cancel", { jobId: jobIdRef.current }).catch(() => {});
  };

  const clear = () => {
    invoke("ai_chat_clear", { id: objId })
      .then(() => {
        setHistory([]);
        setStreaming(null);
        setThinking(null);
        setError(null);
        setSelected(new Set()); // 番号が指す発言が無くなる
        setExportMsg(null);
      })
      .catch(() => {});
  };

  // v1.8.9 選択の操作。役割ボタンは「その役割が全部選ばれていれば外す／でなければ全部選ぶ」。
  const toggleOne = (i: number) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  const toggleRole = (role: "user" | "assistant" | null) => {
    const idx = history.map((m, i) => (role == null || m.role === role ? i : -1)).filter((i) => i >= 0);
    setSelected((prev) => {
      const allOn = idx.length > 0 && idx.every((i) => prev.has(i));
      const next = new Set(prev);
      idx.forEach((i) => (allOn ? next.delete(i) : next.add(i)));
      return next;
    });
  };
  const roleAllOn = (role: "user" | "assistant" | null) => {
    const idx = history.map((m, i) => (role == null || m.role === role ? i : -1)).filter((i) => i >= 0);
    return idx.length > 0 && idx.every((i) => selected.has(i));
  };
  const changeLabels = (v: boolean) => {
    setWithLabels(v);
    try {
      localStorage.setItem("texos-chat-export-labels", v ? "1" : "0");
    } catch {
      /* 保存できなくても動作は変わらない */
    }
  };
  const exportSelected = () => {
    const indices = [...selected].filter((i) => i < history.length).sort((a, b) => a - b);
    if (indices.length === 0) return;
    setExportMsg(null);
    invoke<string>("ai_chat_export", { id: objId, indices, withLabels })
      .then((f) => setExportMsg(`「${f}」に書き出しました（${indices.length}件）`))
      .catch((e) => setError(String(e)));
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
      {/* v1.8.9 抜粋の書き出し（選んだ発言だけを別の MD に。毎ターンのログとは別） */}
      {history.length > 0 && (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 4,
            flexWrap: "wrap",
            padding: "4px 8px",
            fontSize: 11,
            color: "var(--panel-muted, #9fc9b3)",
            background: "var(--panel-header-bg, #0f1b15)",
            borderBottom: "1px solid var(--panel-border, #233a2e)",
          }}
        >
          <button
            onClick={() => toggleRole(null)}
            title="全部の発言を選ぶ（もう一度押すと全部外す）"
            style={selBtn(roleAllOn(null))}
          >
            全部
          </button>
          <button
            onClick={() => toggleRole("user")}
            title="自分（You）の発言をまとめて選ぶ／外す"
            style={selBtn(roleAllOn("user"))}
          >
            You
          </button>
          <button
            onClick={() => toggleRole("assistant")}
            title="AI の発言をまとめて選ぶ／外す"
            style={selBtn(roleAllOn("assistant"))}
          >
            AI
          </button>
          <label
            title={"オン: ログと同じ「You:／AI:」の見出し付き\nオフ: 内容のみ（本文を空行区切りで並べる）"}
            style={{ display: "flex", alignItems: "center", gap: 3, marginLeft: 4, cursor: "pointer" }}
          >
            <input type="checkbox" checked={withLabels} onChange={(e) => changeLabels(e.target.checked)} />
            見出し
          </label>
          <span style={{ flex: 1 }} />
          <button
            onClick={exportSelected}
            disabled={selected.size === 0}
            title={
              "チェックした発言だけを新しい MD カードに書き出します（「<タイトル>の抜粋.md」・既存は上書きせず連番）。\n" +
              "毎ターン自動で更新されるログとは別です。思考は含まれません。"
            }
            style={{ ...hdrBtn(), opacity: selected.size === 0 ? 0.45 : 1, cursor: selected.size ? "pointer" : "default" }}
          >
            📝 書き出し{selected.size > 0 ? `（${selected.size}）` : ""}
          </button>
        </div>
      )}
      {exportMsg && (
        <div
          onClick={() => setExportMsg(null)}
          style={{ padding: "3px 8px", fontSize: 11, color: "#9fd8b8", background: "#12261c", cursor: "pointer" }}
        >
          {exportMsg}
        </div>
      )}
      {/* messages */}
      <div ref={scrollRef} style={{ flex: 1, overflowY: "auto", padding: 8, display: "flex", flexDirection: "column", gap: 8 }}>
        {history.length === 0 && streaming == null && (
          <div style={{ color: "var(--panel-muted, #6b7686)", fontSize: 12, textAlign: "center", marginTop: 16 }}>
            メッセージを入力して会話を始めてください。
          </div>
        )}
        {history.map((m, i) => (
          <Bubble
            key={i}
            role={m.role}
            content={m.content}
            checked={selected.has(i)}
            onToggle={busy && i === history.length - 1 ? undefined : () => toggleOne(i)}
          />
        ))}
        {thinking != null && (
          <ThinkingBox
            text={thinking}
            open={thinkOpen}
            onToggle={() => setThinkOpen((v) => !v)}
            live={busy && !streaming}
          />
        )}
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

/// v1.8.6 思考の折りたたみ表示。既定は閉じたまま（本文の邪魔をしない）で、
/// 生成中は最後の一行だけを覗かせて「止まっていない」ことを示す。
/// 会話が確定すると親が捨てるので、この箱はそのターンの間しか存在しない。
function ThinkingBox({
  text,
  open,
  onToggle,
  live,
}: {
  text: string;
  open: boolean;
  onToggle: () => void;
  live: boolean;
}) {
  const lastLine = text.trimEnd().split("\n").pop() ?? "";
  return (
    <div
      style={{
        border: "1px dashed var(--panel-border, #3a5a68)",
        borderRadius: 8,
        background: "var(--panel-header-bg, #121c22)",
        fontSize: 12,
        color: "var(--panel-muted, #8fb0ca)",
      }}
    >
      <div
        onClick={onToggle}
        title="モデルの思考（表示のみ — 会話履歴にもログにも保存されません）"
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          padding: "4px 8px",
          cursor: "pointer",
          userSelect: "none",
        }}
      >
        <span>{open ? "▼" : "▶"}</span>
        <span>💭 思考{live ? "中…" : ""}</span>
        {!open && (
          <span
            style={{
              flex: 1,
              minWidth: 0,
              opacity: 0.7,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {lastLine}
          </span>
        )}
      </div>
      {open && (
        <div
          style={{
            padding: "0 8px 6px",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            lineHeight: 1.5,
            maxHeight: 180,
            overflowY: "auto",
            userSelect: "text",
            WebkitUserSelect: "text",
          }}
        >
          {text}
        </div>
      )}
    </div>
  );
}

function Bubble({
  role,
  content,
  checked,
  onToggle,
}: {
  role: string;
  content: string;
  /** v1.8.9 書き出しの選択。onToggle が無い吹き出し（生成中・送信直後）はチェック欄なし */
  checked?: boolean;
  onToggle?: () => void;
}) {
  const isUser = role === "user";
  const box = onToggle && (
    <input
      type="checkbox"
      checked={!!checked}
      onChange={onToggle}
      title="書き出しに含める"
      style={{ flex: "none", alignSelf: "flex-start", marginTop: 8, cursor: "pointer" }}
    />
  );
  return (
    <div style={{ display: "flex", gap: 4, justifyContent: isUser ? "flex-end" : "flex-start" }}>
      {!isUser && box}
      <div
        style={{
          maxWidth: "85%",
          padding: "6px 10px",
          borderRadius: 8,
          fontSize: 13,
          lineHeight: 1.5,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          // v1.4.13 Canvas の userSelect:none を打ち消し、応答を選択コピー可能に。
          userSelect: "text",
          WebkitUserSelect: "text",
          cursor: "text",
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
      {isUser && box}
    </div>
  );
}

/// v1.8.9 選択ボタン（全部／You／AI）。その範囲が全部選ばれているとき点灯。
function selBtn(on: boolean): React.CSSProperties {
  return {
    ...hdrBtn(),
    padding: "1px 7px",
    background: on ? "#2f6a4a" : "var(--panel-btn-bg, #1c2a22)",
  };
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
