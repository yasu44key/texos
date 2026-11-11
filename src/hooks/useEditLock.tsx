// v2.1 編集権と「表示のみ」（バックエンドは src-tauri/src/editlock.rs）。
//
// 同じプロジェクトを複数の PC で開いたままにし、座った PC で作業するための画面側。
// - この PC を操作したら edit_touch を呼ぶ（取れるなら編集権を取る・生存の合図）。
// - 「表示のみ」の間はパンとズームだけできる（Canvas の readOnly）。キー操作は通さない。
// - 編集するには上部の「編集権を取得」を押す（相手が操作中なら「引き継ぐ」）。
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type EditStatus = {
  /** none=プロジェクトなし / edit=編集中 / idle=持ち主だが手放している / view=表示のみ */
  mode: "none" | "edit" | "idle" | "view";
  holder: string | null;
  holder_active: boolean;
  me: string;
  took: boolean;
};

/** 編集中に生存の合図を送る間隔（ms）。 */
const PING_MS = 10_000;

export function useEditLock(opts: {
  projectPath: string | null;
  /** 他の PC の変更を読み直す。 */
  refresh: () => void | Promise<void>;
  /** 編集権を新しく取った（最新のカードとアンドゥ履歴を読み直す）。 */
  onTook: () => void;
}) {
  const [status, setStatus] = useState<EditStatus | null>(null);
  const statusRef = useRef<EditStatus | null>(null);
  const inflight = useRef(false);
  const lastTouch = useRef(0);
  const optsRef = useRef(opts);
  optsRef.current = opts;

  const apply = useCallback((s: EditStatus) => {
    statusRef.current = s;
    setStatus(s);
    if (s.took) optsRef.current.onTook();
  }, []);

  const touch = useCallback(
    async (force = false) => {
      if (inflight.current) return;
      inflight.current = true;
      lastTouch.current = Date.now();
      try {
        apply(await invoke<EditStatus>(force ? "edit_takeover" : "edit_touch"));
      } catch (e) {
        console.error("edit_touch failed", e);
      } finally {
        inflight.current = false;
      }
    },
    [apply]
  );

  // プロジェクトが替わったら状態を読み直す（開いた時点でバックエンドが取得を試みている）。
  useEffect(() => {
    invoke<EditStatus>("edit_status")
      .then(apply)
      .catch(() => {});
  }, [opts.projectPath, apply]);

  // バックエンドからの通知: 状態の変化と、他の PC による変更。
  useEffect(() => {
    let alive = true;
    const uns: (() => void)[] = [];
    void listen<EditStatus>("edit:state", (e) => apply(e.payload)).then((u) =>
      alive ? uns.push(u) : u()
    );
    void listen("edit:changed", () => {
      if (statusRef.current?.mode === "view") void optsRef.current.refresh();
    }).then((u) => (alive ? uns.push(u) : u()));
    return () => {
      alive = false;
      uns.forEach((u) => u());
    };
  }, [apply]);

  // この PC への操作を拾う。
  useEffect(() => {
    const onInput = (e: Event) => {
      const s = statusRef.current;
      if (!s || s.mode === "none") return;
      if (s.mode === "view" && e.type === "keydown") {
        // 表示のみの間、キー操作は通さない（引き継ぎボタンへの操作だけは通す）。
        const t = e.target as HTMLElement | null;
        if (!t?.closest?.(".edit-lock-banner")) {
          e.preventDefault();
          e.stopPropagation();
        }
      }
      // 表示のみの間は自動では取らない（「編集権を取得」を押してから）。
      if (s.mode === "view") return;
      const gap = s.mode === "edit" ? PING_MS : 0;
      if (Date.now() - lastTouch.current >= gap) void touch();
    };
    const evs = ["pointerdown", "keydown", "wheel", "pointermove"] as const;
    for (const ev of evs) window.addEventListener(ev, onInput, { capture: true, passive: ev !== "keydown" });
    return () => {
      for (const ev of evs) window.removeEventListener(ev, onInput, { capture: true });
    };
  }, [touch]);

  return { status, takeover: () => touch(true) };
}

/** 「表示のみ」の覆いと表示。App の最後に置く。 */
export function EditLockLayer(props: {
  status: EditStatus | null;
  takeover: () => void;
}) {
  const s = props.status;
  if (!s || s.mode === "none" || s.mode === "edit") return null;
  const pill: React.CSSProperties = {
    position: "fixed",
    top: 8,
    left: "50%",
    transform: "translateX(-50%)",
    zIndex: 99991,
    display: "flex",
    alignItems: "center",
    gap: 10,
    padding: "5px 14px",
    borderRadius: 16,
    fontSize: 12,
    background: "var(--chrome-bg)",
    border: "1px solid var(--chrome-border)",
    color: "var(--chrome-text)",
    boxShadow: "var(--card-shadow)",
    userSelect: "none",
  };
  if (s.mode === "idle") {
    // 持ち主のまま手放している。操作すればそのまま続けられるので、覆いは置かない。
    return (
      <div className="edit-lock-banner" style={{ ...pill, pointerEvents: "none", opacity: 0.8 }}>
        👁 表示のみ（操作すると編集に戻ります）
      </div>
    );
  }
  const busy = s.holder_active && !!s.holder;
  return (
    <div className="edit-lock-banner" style={pill} onMouseDown={(e) => e.stopPropagation()}>
      <span>
        {busy
          ? `✋ ${s.holder} で編集中です（この PC は表示のみ）`
          : `👁 表示のみ${s.holder ? `（編集権: ${s.holder}）` : ""}`}
      </span>
      <button
        onClick={props.takeover}
        style={{
          padding: "2px 10px",
          borderRadius: 10,
          border: "1px solid var(--accent)",
          background: "transparent",
          color: "var(--chrome-text)",
          cursor: "pointer",
          fontSize: 12,
        }}
        title="この PC で編集を始めます。最新の内容を読み直してから切り替わり、相手の PC は数秒で「表示のみ」になります。"
      >
        {busy ? "引き継ぐ" : "編集権を取得"}
      </button>
    </div>
  );
}
