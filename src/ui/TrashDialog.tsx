import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// v1.4.19 ゴミ箱を開く — 削除済み（ソフトデリート）カード・コメント・図形の一覧。
// 復活の呪文（Ctrl+Z）は UNDO 履歴の寿命に縛られるが、ここは deleted_at 行を直接読むので
// 履歴を掃除した後でも「復活」できる。「完全に削除／空にする」は元に戻せない（行＋trash
// ファイルを物理削除）。空にした後は「孤立データを削除（正規化）」で実身 CAS を回収できる。

interface DeletedItem {
  id: string;
  kind: "card" | "note" | "shape";
  label: string;
  detail: string | null;
  deleted_at: number;
}

interface PurgeReport {
  cards: number;
  shapes: number;
  relations: number;
  files: number;
  bytes: number;
}

interface Props {
  onClose: () => void;
  /** 削除（完全削除）後にキャンバスを更新する（App.refresh）。 */
  onChanged: () => void;
  /** 復活後: 更新して該当カードへフォーカスする（App.handleTrashRestored）。 */
  onRestored: (id: string, kind: DeletedItem["kind"]) => void;
}

const KIND_ICON: Record<DeletedItem["kind"], string> = {
  card: "🗂",
  note: "🗒",
  shape: "⬜",
};

const RESTORE_CMD: Record<DeletedItem["kind"], string> = {
  card: "restore_object",
  note: "restore_note",
  shape: "restore_shape",
};

export function TrashDialog({ onClose, onChanged, onRestored }: Props) {
  const [items, setItems] = useState<DeletedItem[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  // 危険操作は二度押し（armed）方式: 1回目で赤くなり、2回目で実行。
  const [armed, setArmed] = useState<string | null>(null); // item.id または "__all__"

  const reload = useCallback(() => {
    invoke<DeletedItem[]>("list_deleted")
      .then(setItems)
      .catch((e) => setStatus(String(e)));
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  const restore = async (it: DeletedItem) => {
    setStatus(null);
    try {
      await invoke(RESTORE_CMD[it.kind], { id: it.id });
      setStatus(`「${it.label || "(無題)"}」を復活しました（キャンバスでフォーカスしています）。`);
      reload();
      onRestored(it.id, it.kind); // 更新＋該当カードへフォーカス
    } catch (e) {
      setStatus("復活できませんでした: " + String(e));
    }
  };

  const purge = async (ids: string[] | null) => {
    setArmed(null);
    setStatus(null);
    try {
      const r = await invoke<PurgeReport>("purge_deleted", { ids });
      const n = r.cards + r.shapes;
      const mb = (r.bytes / 1048576).toFixed(1);
      setStatus(
        `${n}件を完全に削除しました（退避ファイル ${r.files}件・${mb}MB）。` +
          (n > 0 ? "実身データ(CAS)の容量回収は「孤立データを削除（正規化）」で行えます。" : "")
      );
      reload();
      onChanged();
    } catch (e) {
      setStatus("削除できませんでした: " + String(e));
    }
  };

  const fmtDate = (ms: number) => {
    const d = new Date(ms);
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };

  return (
    <div style={overlay} onMouseDown={onClose}>
      <div onMouseDown={(e) => e.stopPropagation()} style={panel}>
        <div style={{ fontSize: 15, fontWeight: 600, marginBottom: 4, textAlign: "center" }}>
          🗑 ゴミ箱
        </div>
        <div style={{ fontSize: 11, color: "var(--panel-muted, #9aa3b8)", marginBottom: 12, textAlign: "center" }}>
          削除したカード・コメント・図形の一覧。「復活」はいつでも可能（UNDO履歴を掃除した後でも）。
          「完全に削除」「空にする」は元に戻せません。
        </div>

        {items.length === 0 ? (
          <div style={{ padding: 24, textAlign: "center", color: "var(--panel-muted, #6b7686)", fontSize: 13 }}>
            ゴミ箱は空です。
          </div>
        ) : (
          <div style={{ maxHeight: "50vh", overflowY: "auto", display: "flex", flexDirection: "column", gap: 4 }}>
            {items.map((it) => (
              <div key={`${it.kind}:${it.id}`} style={rowStyle}>
                <span style={{ flex: "none" }}>{KIND_ICON[it.kind]}</span>
                <span
                  style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
                  title={it.label}
                >
                  {it.label || "(無題)"}
                  {it.detail && (
                    <span style={{ color: "var(--panel-muted, #8a93a8)", fontSize: 11 }}> ・{it.detail}</span>
                  )}
                </span>
                <span style={{ flex: "none", fontSize: 11, color: "var(--panel-muted, #8a93a8)" }}>
                  {fmtDate(it.deleted_at)}
                </span>
                <button style={smallBtn} onClick={() => void restore(it)} title="キャンバスに戻します（矢印も一緒に戻ります）">
                  復活
                </button>
                <button
                  style={armed === it.id ? dangerArmedBtn : dangerBtn}
                  onClick={() => (armed === it.id ? void purge([it.id]) : setArmed(it.id))}
                  title="行と退避ファイルを物理削除します（元に戻せません）。もう一度押すと実行。"
                >
                  {armed === it.id ? "⚠ 確定" : "完全に削除"}
                </button>
              </div>
            ))}
          </div>
        )}

        {status && (
          <div style={{ marginTop: 10, fontSize: 12, color: "var(--panel-muted, #9fc9b3)", whiteSpace: "pre-wrap" }}>
            {status}
          </div>
        )}

        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 14 }}>
          <button
            style={armed === "__all__" ? dangerArmedBtn : dangerBtn}
            disabled={items.length === 0}
            onClick={() => (armed === "__all__" ? void purge(null) : setArmed("__all__"))}
            title="ゴミ箱の全項目と退避ファイルを物理削除します（元に戻せません）。もう一度押すと実行。"
          >
            {armed === "__all__" ? "⚠ 元に戻せません — もう一度押すと実行" : "ゴミ箱を空にする..."}
          </button>
          <span style={{ flex: 1 }} />
          <button style={ghostBtn} onClick={onClose}>
            閉じる
          </button>
        </div>
      </div>
    </div>
  );
}

const overlay: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "#00000066",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2000,
};

const panel: React.CSSProperties = {
  background: "var(--panel-bg, #1c2130)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 10,
  padding: 20,
  width: 560,
  maxWidth: "92vw",
  boxShadow: "0 8px 32px #000000aa",
  fontFamily: "sans-serif",
  color: "var(--panel-text, #d6dded)",
  fontSize: 13,
};

const rowStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: 8,
  padding: "6px 8px",
  background: "var(--panel-input-bg, #0e121c)",
  border: "1px solid var(--panel-border, #2a3346)",
  borderRadius: 6,
};

const smallBtn: React.CSSProperties = {
  padding: "4px 10px",
  fontSize: 12,
  background: "#2f6a4a",
  color: "#fff",
  border: "none",
  borderRadius: 5,
  cursor: "pointer",
  flex: "none",
  fontFamily: "sans-serif",
};

const dangerBtn: React.CSSProperties = {
  padding: "4px 10px",
  fontSize: 12,
  background: "var(--panel-btn-bg, #2a3346)",
  color: "var(--panel-danger, #e08a8a)",
  border: "1px solid var(--panel-border, #3a4660)",
  borderRadius: 5,
  cursor: "pointer",
  flex: "none",
  fontFamily: "sans-serif",
};

const dangerArmedBtn: React.CSSProperties = {
  ...dangerBtn,
  background: "#6e2e2e",
  color: "#fff",
  border: "1px solid #a14a4a",
};

const ghostBtn: React.CSSProperties = {
  padding: "6px 14px",
  fontSize: 12,
  background: "var(--panel-btn-bg, #2a3346)",
  color: "var(--panel-text, #d6dded)",
  border: "1px solid var(--panel-border, #3a4660)",
  borderRadius: 6,
  cursor: "pointer",
  fontFamily: "sans-serif",
};
