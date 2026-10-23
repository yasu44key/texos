import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// v1.10 音声反射カード（docs/V1.10_音声反射カード計画.md）: 稼働中だけカードの下に出す状態パネル。
// backend（voice.rs）が中継する "voice:event"（音声プロセスの JSON 行＋card）を自分の id で拾う。
// 初期表示は voice_status（backend の控え）から — 画面の再読込やパネルの付け直しで嘘をつかない。

type VoiceEvent = {
  card: string;
  ev: string;
  state?: string;
  message?: string;
  rms?: number;
  text?: string;
  kind?: string;
  label?: string;
  content?: string;
  /** utterance の出どころ（mic / text=パネルの欄 / card=会話へ送るカード）。 */
  source?: string;
  /** v1.12 thinker: 思考役の頭（Strata / LM Studio / 空＝使えない）・状態・順番待ち・考え中の件数。 */
  head?: string;
  queued?: number;
  busy?: number;
  error?: boolean;
};

// v1.12 思考役の状態の1行（voice.rs thinker_line と同じ文言）。
const THINKER_STATE: Record<string, string> = {
  idle: "待機中",
  ready: "待機中",
  reading: "読み込み中",
  generating: "生成中",
};
function thinkerLine(v: VoiceEvent): string {
  if (!v.head) return "🧠 思考役なし（Strata も LM Studio も使えません）";
  let line = `🧠 ${v.head}：${THINKER_STATE[v.state ?? ""] ?? v.state ?? ""}`;
  if (v.queued) line += `（${v.queued}件待ち）`;
  if (v.busy) line += `・考え中 ${v.busy}件`;
  return line;
}

const STATE_LABEL: Record<string, string> = {
  starting: "起動中…",
  preparing: "準備中…（初回は音声認識モデルを取得）",
  listening: "聞き取り中",
  recording: "● 録音中",
  transcribing: "文字起こし中…",
  paused: "録音カードにマイクを譲っています",
  stopped: "停止",
  error: "エラー",
  no_env: "音声環境がありません",
};

const STATE_COLOR: Record<string, string> = {
  listening: "#7fd8b0",
  recording: "#ff8080",
  transcribing: "#f0c070",
  error: "#ff8080",
  no_env: "#ff8080",
};

// レベルバーの尺度（RMS 0〜0.08 を平方根で広げる＝小さいしきい値付近を見やすく）。
const LEVEL_MAX = 0.08;
const toPct = (v: number) => Math.min(100, Math.round(Math.sqrt(Math.max(0, v) / LEVEL_MAX) * 100));

export function VoiceStatus({
  id,
  top,
  threshold,
}: {
  id: string;
  top: number;
  threshold: number;
}) {
  const [state, setState] = useState<string>("starting");
  const [message, setMessage] = useState<string>("");
  const [level, setLevel] = useState(0);
  const [last, setLast] = useState<string>("");
  const [speaking, setSpeaking] = useState(false);
  const [build, setBuild] = useState<string | null>(null);
  const [thinker, setThinker] = useState<string>("");
  // v1.10: テキスト欄（話しかけたのと同じ扱いで会話へ入れる・voice_send_text）。
  const [draft, setDraft] = useState("");
  const [sendErr, setSendErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void invoke<{ state: string; message: string; last: string; thinker: string } | null>("voice_status", {
      card: id,
    }).then((s) => {
      if (!alive || !s) return;
      if (s.state) setState(s.state);
      setMessage(s.message);
      setLast(s.last);
      setThinker(s.thinker);
    });
    const un = listen<VoiceEvent>("voice:event", (e) => {
      const v = e.payload;
      if (v.card !== id) return;
      switch (v.ev) {
        case "state":
          setState(v.state ?? "");
          setMessage(v.message ?? "");
          if (v.state !== "no_env") setBuild(null);
          if (v.state === "stopped") setLevel(0);
          break;
        case "error":
          setMessage(v.message ?? "");
          break;
        case "thinker":
          setThinker(thinkerLine(v));
          break;
        case "level":
          setLevel(v.rms ?? 0);
          break;
        case "utterance":
          setLast(`${v.source === "text" ? "⌨" : v.source === "card" ? "📄" : ""}「${v.text ?? ""}」`);
          break;
        case "speaking":
          setSpeaking(true);
          setLast(`🔊 ${v.text ?? ""}`);
          break;
        case "spoken":
          setSpeaking(false);
          break;
        case "reflex":
          if (v.kind === "fire") setLast(`→ ${v.label}（${v.content ?? ""}）`);
          else if (v.kind === "classifying") setLast(`「${v.text ?? ""}」→ 考え中…`);
          else if (v.kind === "reply") setLast(`「${v.text ?? ""}」`);
          else if (v.kind === "think") setLast(`🧠 考え中：${v.text ?? ""}`);
          else if (v.kind === "thought") setLast(`🧠 ${v.text ?? ""}`);
          else if (v.kind === "reset") setLast("（会話をリセット）");
          else if (v.kind === "nomatch") setLast(`「${v.text ?? ""}」（該当なし）`);
          else if (v.kind === "hush") setLast("（読み上げ停止）");
          else if (v.kind === "cancel") setLast("（実行を中止）");
          break;
      }
    });
    const unEnv = listen<{ env?: string; status?: string; percent?: number }>("python-env", (e) => {
      if (e.payload.env !== "voice") return;
      setBuild(`${e.payload.percent ?? 0}% ${e.payload.status ?? ""}`);
    });
    return () => {
      alive = false;
      void un.then((f) => f());
      void unEnv.then((f) => f());
    };
  }, [id]);

  const buildEnv = async () => {
    setBuild("0% 開始…");
    try {
      await invoke("python_build_env", { env: "voice" });
      setBuild("環境の準備ができました。聞き取りを始めます…");
      await invoke("voice_retry"); // 再起動待ちを解いて次の tick ですぐ起動
    } catch (e) {
      setBuild(null);
      setMessage(String(e));
    }
  };

  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  const live = state === "listening" || state === "recording";
  // 音声プロセスが動いている間は打ち込める（録音カードにマイクを譲っている間も）。
  const canType = live || state === "transcribing" || state === "paused";
  const sendText = async () => {
    const t = draft.trim();
    if (!t) return;
    try {
      await invoke("voice_send_text", { card: id, text: t });
      setDraft("");
      setSendErr(null);
    } catch (e) {
      setSendErr(String(e));
    }
  };
  const label = STATE_LABEL[state] ?? state;
  return (
    <div
      onMouseDown={stop}
      onDoubleClick={stop}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: 240,
        padding: "6px 8px",
        background: "var(--panel-bg, #101820)",
        border: "1px solid var(--panel-border, #2a3a5a)",
        borderRadius: 8,
        boxShadow: "0 2px 8px #00000088",
        fontSize: 12,
        lineHeight: 1.45,
        color: "var(--panel-text, #d8e0ea)",
        fontFamily: "sans-serif",
        zIndex: 3,
        cursor: "default",
      }}
    >
      <div style={{ fontWeight: 600, color: STATE_COLOR[state] ?? "inherit" }}>
        🎙 {label}
        {speaking && <span style={{ color: "#f0c070", marginLeft: 6 }}>🔊 報告中</span>}
      </div>
      {live && (
        <div
          title={`入力レベル（縦線＝聞き取りのしきい値 ${threshold}）`}
          style={{
            position: "relative",
            height: 6,
            margin: "4px 0 2px",
            background: "rgba(255,255,255,0.12)",
            borderRadius: 3,
          }}
        >
          <div
            style={{
              width: `${toPct(level)}%`,
              height: "100%",
              background: level >= threshold ? "#e06060" : "#7a5ab0",
              borderRadius: 3,
              transition: "width 0.15s",
            }}
          />
          <div
            style={{
              position: "absolute",
              left: `${toPct(threshold)}%`,
              top: -2,
              width: 2,
              height: 10,
              background: "#f0f0f0",
            }}
          />
        </div>
      )}
      {state === "no_env" && !build && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            void buildEnv();
          }}
          title="音声認識（faster-whisper）とマイク入力のライブラリを TexoS の管理環境へ導入します（数百MB）"
          style={{
            marginTop: 4,
            padding: "3px 12px",
            fontSize: 12,
            fontWeight: 600,
            color: "#fff",
            background: "#7a5ab0",
            border: "none",
            borderRadius: 5,
            cursor: "pointer",
          }}
        >
          音声環境を構築
        </button>
      )}
      {build && <div style={{ wordBreak: "break-all" }}>{build}</div>}
      {message && (state === "error" || state === "no_env") && !build && (
        <div style={{ color: "#ff9a9a", wordBreak: "break-all" }}>{message}</div>
      )}
      {last && <div style={{ wordBreak: "break-all", opacity: 0.9 }}>{last}</div>}
      {thinker && (
        <div
          title="思考役（じっくり考える問いを回す先）。Strata → LM Studio の順に使えるものを選びます"
          style={{ opacity: 0.75, fontSize: 11 }}
        >
          {thinker}
        </div>
      )}
      {canType && (
        <div style={{ display: "flex", gap: 4, marginTop: 4 }}>
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation();
              // 日本語入力の確定 Enter では送らない。
              if (e.key === "Enter" && !e.nativeEvent.isComposing) void sendText();
            }}
            placeholder="文字で話しかける（Enter で送信）"
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: 12,
              padding: "2px 6px",
              background: "var(--input-bg, #0b0f16)",
              color: "inherit",
              border: "1px solid var(--panel-border, #2a3a5a)",
              borderRadius: 4,
            }}
          />
          <button
            onClick={() => void sendText()}
            disabled={!draft.trim()}
            style={{
              fontSize: 12,
              padding: "2px 8px",
              color: "#fff",
              background: "#7a5ab0",
              border: "none",
              borderRadius: 4,
              cursor: draft.trim() ? "pointer" : "default",
              opacity: draft.trim() ? 1 : 0.5,
            }}
          >
            送る
          </button>
        </div>
      )}
      {sendErr && <div style={{ color: "#ff9a9a", wordBreak: "break-all" }}>{sendErr}</div>}
    </div>
  );
}
