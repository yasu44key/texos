import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

// エージェントカード（声で壁打ちする相手・agent.rs）のパネル。カードをダブルクリックで開閉する。
// 頼むと agent_send → backend が返事を出た端から "agent:event"（draft）で送り、書き終わると行（agent）になる。
// 聞き取り（💬 おしゃべり）は backend の常駐プロセスが持つ — パネルを閉じても続く（カードに印が出る）。
// 初期表示は agent_history / agent_state（backend の控え）から — 開き直しても消えない。
// 配色はビュワー系パネルと AI 対話の吹き出しの変数（--panel-* / --chat-bubble-*）＝ UI テーマに追従。

type Line = { role: string; text: string };

const bubble = (who: "user" | "ai"): React.CSSProperties => ({
  background: `var(--chat-bubble-${who}-bg)`,
  color: `var(--chat-bubble-${who}-text)`,
  border: `1px solid var(--chat-bubble-${who}-border)`,
  borderRadius: 8,
  padding: "4px 8px",
  maxWidth: "88%",
  alignSelf: who === "user" ? "flex-end" : "flex-start",
});

const ROLE_STYLE: Record<string, React.CSSProperties> = {
  user: bubble("user"),
  agent: bubble("ai"),
  // 道具を使った記録は吹き出しにしない（会話の脇の小さな行）。
  tool: {
    fontSize: 11,
    color: "var(--panel-muted)",
    borderLeft: "2px solid #7a5ab0",
    paddingLeft: 6,
    alignSelf: "stretch",
  },
  error: { color: "var(--panel-danger)", alignSelf: "stretch" },
};

// おしゃべりの状態の1行（agent.rs vad_state）。
const VAD_LABEL: Record<string, string> = {
  preparing: "⏳ 聞き取りの準備中…（初回はモデルの読み込みに少しかかります）",
  listening: "👂 聞いています — 話し終えて少し黙ると頼みます",
  speech: "● 話しています…",
  transcribing: "✍ 文字にしています…",
  speaking: "🔊 返事を読み上げ中（話しかけると止まります）",
  paused: "⏸ マイクを譲っています",
};
const VAD_SHORT: Record<string, string> = {
  preparing: "⏳ 準備中",
  listening: "👂 聞いています",
  speech: "● 話しています",
  transcribing: "✍ 文字にしています",
  speaking: "🔊 読み上げ中",
};
const VAD_COLOR: Record<string, string> = {
  preparing: "#8a8a8a",
  listening: "#3f9a6a",
  speech: "#d04040",
  transcribing: "#c08a20",
  speaking: "#3f7a9a",
};

// レベルの尺度は音声カードと同じ（RMS 0〜0.08 を平方根で広げる）。縦線＝声とみなすしきい値（voice_server.py の既定）。
const LEVEL_MAX = 0.08;
const THRESHOLD = 0.012;
const toPct = (v: number) => Math.min(100, Math.round(Math.sqrt(Math.max(0, v) / LEVEL_MAX) * 100));

function LevelMeter({ level }: { level: number }) {
  return (
    <div
      title={`入力レベル（縦線＝声とみなす大きさ ${THRESHOLD}）。話してもバーが縦線を超えないときはマイクの音量を上げてください`}
      style={{ position: "relative", height: 10, marginTop: 6, background: "var(--panel-input-bg)", border: "1px solid var(--panel-border)", borderRadius: 5, overflow: "hidden" }}
    >
      <div
        style={{
          width: `${toPct(level)}%`,
          height: "100%",
          background: level >= THRESHOLD ? "#e06060" : "#7a5ab0",
          transition: "width 0.1s",
        }}
      />
      <div style={{ position: "absolute", left: `${toPct(THRESHOLD)}%`, top: 0, width: 2, height: "100%", background: "var(--panel-text)", opacity: 0.6 }} />
    </div>
  );
}

// v1.13.1 返事の Markdown（AI は **太字**・箇条書き・番号・見出しで返す）。吹き出しで使う分だけの小さな表示で、
// HTML として流し込まない（要素を組み立てる＝返事の中身がそのまま実行されることはない）。
const CODE_STYLE: React.CSSProperties = {
  fontFamily: "Consolas, monospace",
  fontSize: 11,
  background: "var(--panel-input-bg)",
  borderRadius: 3,
  padding: "0 3px",
};

function inline(text: string): React.ReactNode[] {
  return text.split(/(\*\*[^*\n]+\*\*|`[^`\n]+`)/g).map((p, i) => {
    if (p.length > 4 && p.startsWith("**") && p.endsWith("**")) return <strong key={i}>{p.slice(2, -2)}</strong>;
    if (p.length > 2 && p.startsWith("`") && p.endsWith("`"))
      return (
        <code key={i} style={CODE_STYLE}>
          {p.slice(1, -1)}
        </code>
      );
    return p;
  });
}

function MiniMd({ text }: { text: string }) {
  const out: React.ReactNode[] = [];
  let code: string[] | null = null;
  text.split("\n").forEach((raw, i) => {
    if (raw.trim().startsWith("```")) {
      if (code) {
        out.push(
          <pre key={i} style={{ ...CODE_STYLE, padding: "4px 6px", margin: "2px 0", whiteSpace: "pre-wrap" }}>
            {code.join("\n")}
          </pre>
        );
        code = null;
      } else code = [];
      return;
    }
    if (code) {
      code.push(raw);
      return;
    }
    const indent = Math.min(3, Math.floor((raw.length - raw.trimStart().length) / 2));
    const line = raw.trim();
    let m: RegExpMatchArray | null;
    if (!line) out.push(<div key={i} style={{ height: 4 }} />);
    else if ((m = line.match(/^#{1,6}\s+(.*)$/)))
      out.push(
        <div key={i} style={{ fontWeight: 700, marginTop: 2 }}>
          {inline(m[1])}
        </div>
      );
    else if ((m = line.match(/^([-*・])\s+(.*)$/)))
      out.push(
        <div key={i} style={{ display: "flex", gap: 4, paddingLeft: indent * 12 }}>
          <span>•</span>
          <span>{inline(m[2])}</span>
        </div>
      );
    else if ((m = line.match(/^(\d+)[.)．]\s+(.*)$/)))
      out.push(
        <div key={i} style={{ display: "flex", gap: 4, paddingLeft: indent * 12 }}>
          <span>{m[1]}.</span>
          <span>{inline(m[2])}</span>
        </div>
      );
    else out.push(<div key={i}>{inline(line)}</div>);
  });
  if (code) out.push(<pre key="tail" style={{ ...CODE_STYLE, padding: "4px 6px", whiteSpace: "pre-wrap" }}>{(code as string[]).join("\n")}</pre>);
  return <>{out}</>;
}

/** 道具の1行。ウェブ検索は、検索語の下に出典（agent.rs sources_of の `  - [タイトル](URL)`）が並ぶ —
 *  押すと既定のブラウザで開く。 */
function ToolLine({ text }: { text: string }) {
  const [head, ...rest] = text.split("\n");
  return (
    <>
      {head}
      {rest.map((line, i) => {
        const m = line.match(/^\s*-\s*\[(.*)\]\((https?:\/\/[^)\s]+)\)\s*$/);
        if (!m) return <div key={i}>{line}</div>;
        let host = "";
        try {
          host = new URL(m[2]).hostname.replace(/^www\./, "");
        } catch {
          /* 読めない URL はそのまま出す */
        }
        return (
          <div key={i} style={{ paddingLeft: 10, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
            <a
              href={m[2]}
              title={`${m[1]}\n${m[2]}\n（押すとブラウザで開きます）`}
              onClick={(e) => {
                e.preventDefault();
                void openUrl(m[2]).catch(() => {});
              }}
              style={{ color: "var(--panel-link, #6aa8e0)", textDecoration: "underline", cursor: "pointer" }}
            >
              {m[1]}
            </a>
            {host && <span style={{ opacity: 0.8 }}> — {host}</span>}
          </div>
        );
      })}
    </>
  );
}

/** 入力欄の高さ（文字に合わせて伸びる・上限あり）。 */
const INPUT_MIN = 36;
const INPUT_MAX = 120;
/** パネルの幅（App.tsx の整列・空き探しも同じ値を使う）。 */
export const AGENT_PANEL_W = 360;

type AgentState = { status: string; listening: boolean; vad: string; speak: boolean; web: boolean };

/** パネルを閉じている間のカードの印: 聞き取り中・考え中が分かる（聞き取りは閉じても続く）。 */
export function AgentBadge({ id, top }: { id: string; top: number }) {
  const [vad, setVad] = useState("off");
  const [status, setStatus] = useState("");
  useEffect(() => {
    let alive = true;
    void invoke<AgentState>("agent_state", { card: id }).then((s) => {
      if (!alive) return;
      setVad(s.vad);
      setStatus(s.status);
    });
    const unV = listen<{ card: string; state: string }>("agent:vad", (e) => {
      if (e.payload.card === id) setVad(e.payload.state);
    });
    const unE = listen<Line & { card: string }>("agent:event", (e) => {
      if (e.payload.card === id && e.payload.role === "status") setStatus(e.payload.text);
    });
    return () => {
      alive = false;
      void unV.then((f) => f());
      void unE.then((f) => f());
    };
  }, [id]);
  if (vad === "off" && !status) return null;
  return (
    <div
      title="ダブルクリックでパネルを開きます"
      style={{
        position: "absolute",
        left: 0,
        top,
        padding: "1px 8px",
        fontSize: 11,
        fontFamily: "sans-serif",
        whiteSpace: "nowrap",
        color: "#fff",
        background: status ? "#7a5ab0" : VAD_COLOR[vad] ?? "#3f9a6a",
        borderRadius: 9,
        boxShadow: "0 1px 4px #00000055",
        zIndex: 3,
        pointerEvents: "none",
      }}
    >
      {status ? `🤖 ${status}` : VAD_SHORT[vad] ?? "👂 聞いています"}
    </div>
  );
}

export function AgentPanel({ id, label, top, onClose }: { id: string; label: string; top: number; onClose: () => void }) {
  const [lines, setLines] = useState<Line[]>([]);
  const [status, setStatus] = useState("");
  // 書きかけの返事（出た端から届く）。
  const [writing, setWriting] = useState("");
  const [draft, setDraft] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const logRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // 💬 おしゃべり: 黙ったところで区切って頼み、返事を声で返す。聞き取りは backend が持つ。
  const [listening, setListening] = useState(false);
  const [starting, setStarting] = useState(false);
  const [vad, setVad] = useState("off");
  const [level, setLevel] = useState(0);
  const [speak, setSpeak] = useState(true);
  // 聞き取りを動かしている場所（「GPU・base」）。
  const [ear, setEar] = useState("");
  const [web, setWeb] = useState(false);
  // 音声環境が無い → 構築の案内。build＝構築の進み具合。
  const [noEnv, setNoEnv] = useState(false);
  const [build, setBuild] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void invoke<Line[]>("agent_history", { card: id }).then((h) => {
      if (alive) setLines(h);
    });
    void invoke<AgentState>("agent_state", { card: id }).then((s) => {
      if (!alive) return;
      setStatus(s.status);
      setListening(s.listening);
      setVad(s.vad);
      setSpeak(s.speak);
      setWeb(s.web);
    });
    const uns = [
      listen<Line & { card: string }>("agent:event", (e) => {
        const v = e.payload;
        if (v.card !== id) return;
        if (v.role === "status") setStatus(v.text);
        else if (v.role === "draft") setWriting(v.text);
        else {
          if (v.role !== "tool") setWriting("");
          setLines((ls) => [...ls, { role: v.role, text: v.text }].slice(-80));
        }
      }),
      listen<{ card: string; state: string }>("agent:vad", (e) => {
        if (e.payload.card !== id) return;
        setVad(e.payload.state);
        setListening(e.payload.state !== "off");
        if (e.payload.state === "off") setLevel(0);
      }),
      listen<{ card: string; rms: number }>("agent:level", (e) => {
        if (e.payload.card === id) setLevel(e.payload.rms);
      }),
      listen<{ card: string; model: string; device: string }>("agent:ear", (e) => {
        if (e.payload.card === id) setEar(`${e.payload.device === "cuda" ? "GPU" : "CPU"}・${e.payload.model}`);
      }),
      listen<{ env?: string; status?: string; percent?: number }>("python-env", (e) => {
        if (e.payload.env === "voice") setBuild(`${e.payload.percent ?? 0}% ${e.payload.status ?? ""}`);
      }),
    ];
    return () => {
      alive = false;
      uns.forEach((u) => void u.then((f) => f()));
    };
  }, [id]);

  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines, status, writing]);

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(INPUT_MAX, Math.max(INPUT_MIN, el.scrollHeight))}px`;
  }, [draft]);

  const busy = status !== "";
  const send = async () => {
    const t = draft.trim();
    if (!t || busy) return;
    try {
      setStatus("考え中…");
      await invoke("agent_send", { card: id, text: t });
      setDraft("");
      setErr(null);
    } catch (e) {
      setStatus("");
      setErr(String(e));
    }
  };

  const startListen = async () => {
    setErr(null);
    setStarting(true);
    try {
      await invoke("agent_listen_start", { card: id, speak });
      setNoEnv(false);
      setListening(true);
      setVad((v) => (v === "off" ? "preparing" : v));
    } catch (e) {
      if (String(e) === "no_env") setNoEnv(true);
      else setErr(String(e));
    } finally {
      setStarting(false);
    }
  };
  const stopListen = async () => {
    setListening(false);
    setLevel(0);
    await invoke("agent_listen_stop", { card: id }).catch((e) => setErr(String(e)));
  };
  const buildEnv = async () => {
    setBuild("0% 開始…");
    try {
      await invoke("python_build_env", { env: "voice" });
      setBuild(null);
      await startListen();
    } catch (e) {
      setBuild(null);
      setErr(String(e));
    }
  };
  const toggleSpeak = (on: boolean) => {
    setSpeak(on);
    void invoke("agent_set_speak", { card: id, on });
  };
  const toggleWeb = async () => {
    try {
      await invoke("agent_set_web", { card: id, on: !web });
      setWeb(!web);
    } catch (e) {
      setErr(String(e));
    }
  };
  // 📎 返事をカードにする。吹き出しの中で文字を選んでいれば、その部分だけ。
  const clip = async (text: string, bubbleEl: Element | null) => {
    const sel = window.getSelection();
    const inside =
      sel && !sel.isCollapsed && bubbleEl && bubbleEl.contains(sel.anchorNode) && bubbleEl.contains(sel.focusNode);
    try {
      await invoke("agent_clip", { card: id, text: inside ? sel.toString() : text });
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  };
  const summarize = async () => {
    try {
      setStatus("まとめています…");
      await invoke("agent_summarize", { card: id });
      setErr(null);
    } catch (e) {
      setStatus("");
      setErr(String(e));
    }
  };
  const reset = async () => {
    await invoke("agent_reset", { card: id });
    setLines([]);
  };

  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  const bigBtn = (bg: string, enabled = true): React.CSSProperties => ({
    flex: 1,
    height: 40,
    fontSize: 15,
    fontWeight: 700,
    color: "#fff",
    background: bg,
    border: "none",
    borderRadius: 6,
    cursor: enabled ? "pointer" : "default",
    opacity: enabled ? 1 : 0.45,
    whiteSpace: "nowrap",
    minWidth: 0,
    padding: "0 4px",
    overflow: "hidden",
  });
  const btn = (bg: string, enabled: boolean): React.CSSProperties => ({
    fontSize: 12,
    padding: "3px 10px",
    color: "#fff",
    background: bg,
    border: "none",
    borderRadius: 4,
    cursor: enabled ? "pointer" : "default",
    opacity: enabled ? 1 : 0.45,
    whiteSpace: "nowrap",
  });
  const linkBtn = (enabled = true): React.CSSProperties => ({
    fontSize: 10,
    padding: 0,
    background: "none",
    border: "none",
    color: "var(--panel-muted)",
    textDecoration: "underline",
    cursor: enabled ? "pointer" : "default",
    opacity: enabled ? 1 : 0.45,
  });
  const canSummarize = !busy && lines.some((l) => l.role === "agent");
  return (
    <div
      onMouseDown={stop}
      onDoubleClick={stop}
      onWheel={stop}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: AGENT_PANEL_W,
        padding: 8,
        background: "var(--panel-bg)",
        border: "1px solid #7a5ab0",
        borderRadius: 8,
        boxShadow: "0 2px 8px #00000055",
        fontSize: 12,
        lineHeight: 1.5,
        color: "var(--panel-text)",
        fontFamily: "sans-serif",
        zIndex: 3,
        cursor: "default",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 6 }}>
        <span style={{ flex: 1, fontWeight: 600, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          🤖 {label}
        </span>
        <button
          title={
            web
              ? "ウェブ検索: 入（必要なときに、この PC で動く検索エンジンで調べます。外へ出るのは検索語だけで、使った検索語は記録に残ります）。押すと切ります"
              : "ウェブ検索: 切（手元の資料と AI の知識だけで話します）。押すと入れます"
          }
          onClick={() => void toggleWeb()}
          style={{
            fontSize: 11,
            padding: "1px 8px",
            borderRadius: 9,
            border: `1px solid ${web ? "#3f7a9a" : "var(--panel-border)"}`,
            background: web ? "#3f7a9a" : "transparent",
            color: web ? "#fff" : "var(--panel-muted)",
            cursor: "pointer",
            whiteSpace: "nowrap",
          }}
        >
          🌐 ウェブ {web ? "入" : "切"}
        </button>
        <button
          title="パネルを閉じます（聞き取り中なら、閉じても聞き続けます）"
          onClick={onClose}
          style={{ fontSize: 14, lineHeight: 1, padding: "0 4px", background: "none", border: "none", color: "var(--panel-muted)", cursor: "pointer" }}
        >
          ×
        </button>
      </div>
      <div
        ref={logRef}
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 5,
          maxHeight: 320,
          overflowY: "auto",
          userSelect: "text",
          scrollbarWidth: "thin",
          scrollbarColor: "var(--panel-border) transparent",
        }}
      >
        {lines.length === 0 && !busy && (
          <div style={{ color: "var(--panel-muted)" }}>
            壁打ちの相手です。考えていることを話してください — カードの構成や進め方を一緒に固めます。
            必要なら手元の資料{web ? "やウェブ" : ""}を調べます。まとまったら「📝 まとめてカードに」で次へ渡せます。
          </div>
        )}
        {lines.map((l, i) =>
          l.role === "agent" ? (
            <div key={i} style={{ alignSelf: "flex-start", maxWidth: "88%", display: "flex", flexDirection: "column", alignItems: "flex-start" }}>
              <div style={{ ...ROLE_STYLE.agent, maxWidth: "100%", alignSelf: "auto", wordBreak: "break-word" }}>
                <MiniMd text={l.text} />
              </div>
              <button
                title="この返事を1枚のカードにします（文字を選んでから押すと、その部分だけ）。カードは次のフローへつなげます"
                onMouseDown={(e) => e.preventDefault()}
                onClick={(e) => void clip(l.text, e.currentTarget.previousElementSibling)}
                style={linkBtn()}
              >
                📎 カードにする
              </button>
            </div>
          ) : (
            <div key={i} style={{ whiteSpace: "pre-wrap", wordBreak: "break-word", ...(ROLE_STYLE[l.role] ?? ROLE_STYLE.tool) }}>
              {l.role === "error" ? `⚠ ${l.text}` : <ToolLine text={l.text} />}
            </div>
          )
        )}
        {writing && (
          <div style={{ ...ROLE_STYLE.agent, wordBreak: "break-word", opacity: 0.85 }}>
            <MiniMd text={writing} />
          </div>
        )}
        {busy && !writing && <div style={{ color: "var(--panel-muted)", fontStyle: "italic" }}>🤖 {status}</div>}
      </div>
      <div style={{ display: "flex", gap: 6, marginTop: 8, alignItems: "flex-end" }}>
        <textarea
          ref={inputRef}
          value={draft}
          rows={1}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            // Enter で送る（Shift+Enter は改行・日本語入力の確定 Enter では送らない）。
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void send();
            }
          }}
          placeholder="ここに書いて話す（Enter で送信・Shift+Enter で改行）"
          style={{
            flex: 1,
            minWidth: 0,
            height: INPUT_MIN,
            resize: "none",
            fontSize: 12,
            lineHeight: 1.5,
            fontFamily: "inherit",
            padding: "4px 6px",
            boxSizing: "border-box",
            background: "var(--panel-input-bg)",
            color: "var(--panel-text)",
            border: "1px solid var(--panel-border)",
            borderRadius: 4,
            outline: "none",
          }}
        />
        <button onClick={() => void send()} disabled={busy || !draft.trim()} style={btn("#7a5ab0", !busy && !!draft.trim())}>
          送る
        </button>
      </div>
      {/* 大きなボタンの列: 💬 おしゃべり（聞き取りの入り切り）・考え中は ■ 止める・📝 まとめてカードに。 */}
      <div style={{ display: "flex", gap: 6, marginTop: 8 }}>
        {listening ? (
          <button
            title="聞き取りを終えます（処理中の頼みは最後まで進みます）"
            onClick={() => void stopListen()}
            style={bigBtn("#c03838")}
          >
            ■ おしゃべりを終える
          </button>
        ) : (
          <button
            title="話しかけるだけで進みます。少し黙ったところで区切って頼み、返事を声で返します。読み上げ中に話しかけると止まります"
            onClick={() => void startListen()}
            disabled={starting || build !== null}
            style={bigBtn("#3f7a9a", !starting && build === null)}
          >
            {starting ? "開いています…" : "💬 おしゃべり"}
          </button>
        )}
        {/* 右の枠は1つだけ（3つ並べるとパネルからはみ出した）: 考え中は ■ 止める、ふだんは 📝 まとめてカードに。 */}
        {busy ? (
          <button
            title="いまの頼み（まとめも）と読み上げを止めます"
            onClick={() => void invoke("agent_stop", { card: id })}
            style={{ ...bigBtn("#7a3a3a"), flex: "0 0 124px", fontSize: 13 }}
          >
            ■ 止める
          </button>
        ) : (
          <button
            title="ここまでの会話を文書にまとめて、1枚のカードにします（目的・決まったこと・カードの構成の案・次にやること）。コンストラクタにつなぐと、その構成を組めます"
            onClick={() => void summarize()}
            disabled={!canSummarize}
            style={{ ...bigBtn("#7a5ab0", canSummarize), flex: "0 0 124px", fontSize: 12 }}
          >
            📝 まとめてカードに
          </button>
        )}
      </div>
      {noEnv && (
        <div style={{ marginTop: 6, color: "var(--panel-muted)" }}>
          声で話すには音声環境が要ります（数百MB・この PC の中だけで聞き取ります）。
          <button onClick={() => void buildEnv()} disabled={build !== null} style={{ ...btn("#3f7a9a", build === null), marginLeft: 6 }}>
            音声環境を構築
          </button>
        </div>
      )}
      {build !== null && <div style={{ marginTop: 4, color: "var(--panel-muted)" }}>構築中… {build}</div>}
      <label
        title="返事を Windows の声で読み上げます（文ごとに・長い返事は途中まで）。読み上げ中に話しかけると止まります"
        style={{ display: "flex", alignItems: "center", gap: 4, marginTop: 4, fontSize: 11, color: "var(--panel-muted)", cursor: "pointer" }}
      >
        <input type="checkbox" checked={speak} onChange={(e) => toggleSpeak(e.target.checked)} />
        🔊 おしゃべりの返事を声で読み上げる
      </label>
      {listening && (
        <>
          <LevelMeter level={level} />
          <div style={{ marginTop: 2, fontWeight: 600, color: VAD_COLOR[vad] ?? "var(--panel-text)" }}>
            {VAD_LABEL[vad] ?? "👂 聞いています"}
            {ear && <span style={{ fontWeight: 400, color: "var(--panel-muted)" }}>（{ear}）</span>}
          </div>
        </>
      )}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 4, gap: 8 }}>
        <span
          title="会話は毎回ここへ追記し、右隣のカードにします。切り出しとまとめは、その下に別のカードとして並びます"
          style={{ fontSize: 10, color: "var(--panel-muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
        >
          記録: エージェント/{label}の会話.md
        </span>
        <button
          title="会話を忘れて最初からにします（会話.md と、カードにした切り出し・まとめは残ります）"
          onClick={() => void reset()}
          disabled={busy || lines.length === 0}
          style={{ ...linkBtn(!busy && lines.length > 0), whiteSpace: "nowrap" }}
        >
          会話をリセット
        </button>
      </div>
      {err && <div style={{ color: "var(--panel-danger)", wordBreak: "break-all" }}>{err}</div>}
    </div>
  );
}
