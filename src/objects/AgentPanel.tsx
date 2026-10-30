import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// V1.13 エージェントカード（docs/V1.13_エージェントカード計画.md §4）: カードの下の会話の欄。
// 頼むと agent_send → backend（agent.rs）が一手ずつ道具を使い、"agent:event" で行を送ってくる。
// 初期表示は agent_history（backend の控え）から — 画面の再読込やパネルの付け直しで消えない。
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

// おしゃべりモードの状態の1行（agent.rs vad_state）。
const VAD_LABEL: Record<string, string> = {
  listening: "👂 聞いています — 話し終えて少し黙ると頼みます",
  speech: "● 話しています…",
  transcribing: "✍ 文字にしています…",
  speaking: "🔊 返事を読み上げ中（その間は聞いていません）",
};
const VAD_COLOR: Record<string, string> = {
  listening: "#3f9a6a",
  speech: "#d04040",
  transcribing: "#c08a20",
  speaking: "#3f7a9a",
};

// レベルの尺度は音声カードと同じ（RMS 0〜0.08 を平方根で広げる）。縦線＝区切りのしきい値（agent.rs VAD_THRESHOLD）。
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

/** 入力欄の高さ（文字に合わせて伸びる・上限あり）。 */
const INPUT_MIN = 36;
const INPUT_MAX = 120;

export function AgentPanel({ id, top }: { id: string; top: number }) {
  const [lines, setLines] = useState<Line[]>([]);
  const [status, setStatus] = useState("");
  const [draft, setDraft] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const logRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    let alive = true;
    void invoke<Line[]>("agent_history", { card: id }).then((h) => {
      if (alive) setLines(h);
    });
    const un = listen<Line & { card: string }>("agent:event", (e) => {
      const v = e.payload;
      if (v.card !== id) return;
      if (v.role === "status") setStatus(v.text);
      else setLines((ls) => [...ls, { role: v.role, text: v.text }].slice(-80));
    });
    return () => {
      alive = false;
      void un.then((f) => f());
    };
  }, [id]);

  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines, status]);

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(INPUT_MAX, Math.max(INPUT_MIN, el.scrollHeight))}px`;
  }, [draft]);

  const busy = status !== "";
  const sendText = async (t: string) => {
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
  const send = () => sendText(draft.trim());

  // 🎙 押して話す → もう一度押すと止めて文字にし、そのまま頼む（agent.rs agent_mic_*）。
  // 💬 おしゃべり（auto）: 黙ったところで区切って頼み、終わったらまた聞く。返事を声で読み上げる選択つき。
  // mic: null=待機 / "starting" / マイク名（録音中）。
  const [mic, setMic] = useState<string | null>(null);
  const [auto, setAuto] = useState(false);
  const [speakReply, setSpeakReply] = useState(true);
  const [transcribing, setTranscribing] = useState(false);
  const [level, setLevel] = useState(0);
  // おしゃべりの状態（listening / speech / transcribing / speaking / off）。
  const [vad, setVad] = useState("off");
  const recording = mic !== null && mic !== "starting";
  useEffect(() => {
    const unL = listen<{ card: string; rms: number }>("agent:level", (e) => {
      if (e.payload.card === id) setLevel(e.payload.rms);
    });
    const unV = listen<{ card: string; state: string }>("agent:vad", (e) => {
      if (e.payload.card === id) setVad(e.payload.state);
    });
    return () => {
      void unL.then((f) => f());
      void unV.then((f) => f());
    };
  }, [id]);
  const startMic = async (chat: boolean) => {
    setErr(null);
    setAuto(chat);
    setMic("starting");
    try {
      setMic(await invoke<string>("agent_mic_start", { card: id, auto: chat, speak: chat && speakReply }));
    } catch (e) {
      setMic(null);
      setErr(String(e));
    }
  };
  const stopMic = async () => {
    setMic(null);
    setLevel(0);
    if (auto) {
      // おしゃべりを終える（文字にしている途中の分は最後まで頼む）。
      await invoke("agent_mic_stop", { card: id }).catch((e) => setErr(String(e)));
      return;
    }
    setTranscribing(true);
    try {
      const text = await invoke<string>("agent_mic_stop", { card: id });
      setTranscribing(false);
      await sendText(text.trim());
    } catch (e) {
      setTranscribing(false);
      setErr(String(e));
    }
  };
  const cancelMic = () => {
    setMic(null);
    void invoke("agent_mic_cancel", { card: id });
  };
  // パネルが消えたら録音も止める（カード削除・プロジェクト切替）。
  useEffect(() => () => void invoke("agent_mic_cancel", { card: id }), [id]);
  const reset = async () => {
    await invoke("agent_reset", { card: id });
    setLines([]);
  };

  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  // 録音・止める は大きく（押し間違えない・離れた所からでも押せる）。
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
  return (
    <div
      onMouseDown={stop}
      onDoubleClick={stop}
      onWheel={stop}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: 340,
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
            頼みごとをどうぞ。答える・手元の資料を調べる・ファイルを読む・カードのフローを組んで動かす、ができます。
          </div>
        )}
        {lines.map((l, i) => (
          <div
            key={i}
            style={{
              whiteSpace: l.role === "agent" ? "normal" : "pre-wrap",
              wordBreak: "break-word",
              ...(ROLE_STYLE[l.role] ?? ROLE_STYLE.tool),
            }}
          >
            {l.role === "agent" ? <MiniMd text={l.text} /> : l.role === "error" ? `⚠ ${l.text}` : l.text}
          </div>
        ))}
        {busy && <div style={{ color: "var(--panel-muted)", fontStyle: "italic" }}>🤖 {status}</div>}
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
          placeholder="ここに頼む（Enter で送信・Shift+Enter で改行）"
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
        <button
          onClick={() => void send()}
          disabled={busy || !draft.trim()}
          style={btn("#7a5ab0", !busy && !!draft.trim())}
        >
          送る
        </button>
      </div>
      {/* 大きなボタンの列: 待機中＝🎙 話して頼む・💬 おしゃべり／録音中＝■ 止めて頼む／
          おしゃべり中＝■ おしゃべりを終える（考え中でも出す）／考え中＝■ 止める。 */}
      <div style={{ display: "flex", gap: 6, marginTop: 8 }}>
        {recording && auto ? (
          <>
            <button
              title="聞き取りを終えます（文字にしている途中の分は最後まで頼みます）"
              onClick={() => void stopMic()}
              style={bigBtn("#c03838")}
            >
              ■ おしゃべりを終える
            </button>
            {busy && (
              <button
                title="いまの頼みと、ここから動かしているフローを止めます"
                onClick={() => void invoke("agent_stop", { card: id })}
                style={{ ...bigBtn("#7a3a3a"), flex: "0 0 96px", fontSize: 13 }}
              >
                ■ 頼みを止める
              </button>
            )}
          </>
        ) : busy ? (
          <button
            title="この頼みと、ここから動かしているフローを止めます"
            onClick={() => void invoke("agent_stop", { card: id })}
            style={bigBtn("#b04848")}
          >
            ■ 止める
          </button>
        ) : recording ? (
          <>
            <button
              title="録音を止めて文字にし、そのまま頼みます"
              onClick={() => void stopMic()}
              style={bigBtn("#c03838")}
            >
              ■ 止めて頼む
            </button>
            <button
              title="録音をやめます（頼みません）"
              onClick={cancelMic}
              style={{ ...bigBtn("#5a6678"), flex: "0 0 72px", fontSize: 13 }}
            >
              やめる
            </button>
          </>
        ) : (
          <>
            <button
              title="押してから話し、話し終えたら「■ 止めて頼む」を押します（Whisper で文字にします）"
              onClick={() => void startMic(false)}
              disabled={mic === "starting" || transcribing}
              style={bigBtn("#7a5ab0", mic === null && !transcribing)}
            >
              {transcribing ? "文字にしています…" : mic === "starting" && !auto ? "開いています…" : "🎙 話して頼む"}
            </button>
            <button
              title="話しかけるだけで進むモード。1.2 秒黙ったところで区切って頼み、終わったらまた聞きます"
              onClick={() => void startMic(true)}
              disabled={mic === "starting" || transcribing}
              style={bigBtn("#3f7a9a", mic === null && !transcribing)}
            >
              {mic === "starting" && auto ? "開いています…" : "💬 おしゃべり"}
            </button>
          </>
        )}
      </div>
      {!recording && mic === null && (
        <label
          title="おしゃべりモードで、返事の最初の段落を Windows の声で読み上げます（読み上げ中は聞き取りを止めます）"
          style={{ display: "flex", alignItems: "center", gap: 4, marginTop: 4, fontSize: 11, color: "var(--panel-muted)", cursor: "pointer" }}
        >
          <input type="checkbox" checked={speakReply} onChange={(e) => setSpeakReply(e.target.checked)} />
          🔊 おしゃべりの返事を声で読み上げる
        </label>
      )}
      {recording && (
        <>
          <LevelMeter level={level} />
          <div style={{ marginTop: 2, fontWeight: 600, color: auto ? VAD_COLOR[vad] ?? "var(--panel-text)" : "#d04040" }}>
            {auto ? `${VAD_LABEL[vad] ?? "👂 聞いています"}（${mic}）` : `● 録音中（${mic}）— 話し終えたら「■ 止めて頼む」`}
          </div>
        </>
      )}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 4 }}>
        <span style={{ fontSize: 10, color: "var(--panel-muted)" }}>記録: エージェント/（カード名）の会話.md</span>
        <button
          title="会話を忘れて最初からにします（会話ログ.md は残ります）"
          onClick={() => void reset()}
          disabled={busy || lines.length === 0}
          style={{
            fontSize: 10,
            padding: 0,
            background: "none",
            border: "none",
            color: "var(--panel-muted)",
            textDecoration: "underline",
            cursor: !busy && lines.length > 0 ? "pointer" : "default",
            opacity: !busy && lines.length > 0 ? 1 : 0.45,
          }}
        >
          会話をリセット
        </button>
      </div>
      {err && <div style={{ color: "var(--panel-danger)", wordBreak: "break-all" }}>{err}</div>}
    </div>
  );
}
