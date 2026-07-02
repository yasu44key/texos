import { useEffect, useMemo, useRef, useState } from "react";
import type { VirtualBodyInfo, NoteInfo } from "../model";
import { isTextMime } from "../model";

// v1.4.19 カードを検索（Ctrl+F）。ラベル・付箋本文・メモ・拡張子・種別・URLをライブ検索し、
// 行クリック＝そのカードへフォーカス／「一致をすべて選択」＝複数選択してまとめてフォーカス。
// フォーカスの様子が見えるように、画面を暗くしない右側フローティング（非モーダル）にする。
// 種別チップ（複数選択可）は「名前を思い出せない」ブラウズ用: 文字なし＋種別だけでも一覧できる。
// チップはプロジェクト内のカードから動的に生成するので、将来 kind が増えても自動で並ぶ。

interface Props {
  objects: VirtualBodyInfo[];
  notes: NoteInfo[];
  /** 選択を置き換えてフォーカスする（App.focusCards）。 */
  onFocus: (cardIds: string[], noteIds: string[]) => void;
  onClose: () => void;
}

interface Hit {
  id: string;
  kind: "card" | "note";
  icon: string;
  label: string;
  detail: string | null;
  catKey: string;
}

interface Cat {
  key: string;
  icon: string;
  label: string;
}

// カードの分類（チップと行アイコンの共通語彙）。既知の変種＋メディア別。
// 将来の新 kind はハードコード漏れでも最後の分岐で「kind名そのまま」のチップになる＝埋もれない。
function catOf(o: VirtualBodyInfo): Cat {
  if (o.project_ref) return { key: "project", icon: "📁", label: "プロジェクト" };
  if (o.url_ref) return { key: "url", icon: "🔗", label: "URL" };
  if (o.folder_ref) return { key: "folder", icon: "📂", label: "フォルダ" };
  switch (o.kind) {
    case "launcher":
      return { key: "launcher", icon: "🚀", label: "ランチャー" };
    case "drop":
      return { key: "drop", icon: "📥", label: "ドロップ" };
    case "filter":
      return { key: "filter", icon: "⚗", label: "フィルタ" };
  }
  if (o.mime_type.startsWith("image/")) return { key: "image", icon: "🖼", label: "画像" };
  if (o.mime_type.startsWith("video/")) return { key: "video", icon: "🎬", label: "動画" };
  if (o.mime_type.startsWith("audio/")) return { key: "audio", icon: "🎵", label: "音声" };
  if (o.mime_type === "application/pdf" || (o.ext ?? "").toLowerCase() === "pdf")
    return { key: "pdf", icon: "📕", label: "PDF" };
  if (isTextMime(o.mime_type, o.ext)) return { key: "text", icon: "📄", label: "テキスト" };
  if (o.kind && o.kind !== "file") return { key: `kind:${o.kind}`, icon: "🧩", label: o.kind };
  return { key: "file", icon: "🗂", label: "その他ファイル" };
}

const NOTE_CAT: Cat = { key: "note", icon: "🗒", label: "コメント" };

export function SearchDialog({ objects, notes, onFocus, onClose }: Props) {
  const [query, setQuery] = useState("");
  // 種別フィルタ（複数選択可）。空 = すべての種別。
  const [cats, setCats] = useState<Set<string>>(new Set());
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // プロジェクト内に実在する種別だけを件数つきで並べる（動的生成）。
  const availableCats = useMemo(() => {
    const m = new Map<string, Cat & { count: number }>();
    const add = (c: Cat) => {
      const cur = m.get(c.key);
      if (cur) cur.count += 1;
      else m.set(c.key, { ...c, count: 1 });
    };
    objects.forEach((o) => add(catOf(o)));
    notes.forEach(() => add(NOTE_CAT));
    return [...m.values()].sort((a, b) => b.count - a.count);
  }, [objects, notes]);

  const toggleCat = (key: string) => {
    setCats((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  // スペース区切り＝AND（すべての語を含む）。大文字小文字は区別しない。
  // 文字なしでも種別チップが選ばれていればその種別の全カードを一覧する（ブラウズモード）。
  const hits = useMemo<Hit[]>(() => {
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    const catFilter = cats.size ? cats : null;
    if (!terms.length && !catFilter) return [];
    const matches = (hay: string) => terms.every((t) => hay.includes(t));
    const out: Hit[] = [];
    for (const o of objects) {
      const c = catOf(o);
      if (catFilter && !catFilter.has(c.key)) continue;
      const hay = [o.label, o.memo ?? "", o.ext ?? "", o.kind ?? "", o.url_ref ?? "", o.folder_ref ?? ""]
        .join("\n")
        .toLowerCase();
      if (!terms.length || matches(hay)) {
        out.push({
          id: o.id,
          kind: "card",
          icon: c.icon,
          label: o.label || "(無題)",
          detail: o.ext ? `.${o.ext}` : c.label,
          catKey: c.key,
        });
      }
    }
    if (!catFilter || catFilter.has(NOTE_CAT.key)) {
      for (const n of notes) {
        if (!terms.length || matches(n.text.toLowerCase())) {
          const first = n.text.split("\n")[0].trim();
          out.push({
            id: n.id,
            kind: "note",
            icon: NOTE_CAT.icon,
            label: first || "(コメント)",
            detail: "コメント",
            catKey: NOTE_CAT.key,
          });
        }
      }
    }
    return out.slice(0, 200);
  }, [query, cats, objects, notes]);

  const focusAll = () => {
    onFocus(
      hits.filter((h) => h.kind === "card").map((h) => h.id),
      hits.filter((h) => h.kind === "note").map((h) => h.id)
    );
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") onClose();
    // Enter = 一致をすべて選択してフォーカス（1件ならそれ単体と同じ）。
    if (e.key === "Enter" && hits.length) focusAll();
  };

  const active = query.trim().length > 0 || cats.size > 0;

  return (
    <div style={panel} onMouseDown={(e) => e.stopPropagation()}>
      <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 8 }}>
        <span style={{ fontSize: 13, fontWeight: 600, flex: 1 }}>🔍 カードを検索</span>
        <button style={hdrBtn} onClick={onClose} title="閉じる（Esc）">
          ✕
        </button>
      </div>
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={onKey}
        placeholder="ラベル・コメント本文・メモ・拡張子（スペース区切り=AND）"
        style={inp}
      />
      {/* 種別チップ: プロジェクト内に実在する種別を件数つきで（複数選択・空=すべて）。
          名前を思い出せないときは、文字なしで種別だけ選んで一覧できる。 */}
      <div style={{ display: "flex", flexWrap: "wrap", gap: 4, margin: "8px 0 2px" }}>
        <button
          style={chip(cats.size === 0)}
          onClick={() => setCats(new Set())}
          title="種別で絞り込まない"
        >
          すべて
        </button>
        {availableCats.map((c) => (
          <button
            key={c.key}
            style={chip(cats.has(c.key))}
            onClick={() => toggleCat(c.key)}
            title={`${c.label}だけを対象にする（複数選択可）`}
          >
            {c.icon} {c.label} {c.count}
          </button>
        ))}
      </div>
      <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", margin: "6px 0" }}>
        {active
          ? `${hits.length}件${hits.length >= 200 ? "以上" : ""}が一致`
          : "クリック=フォーカス ／ Enter=一致をすべて選択"}
      </div>
      <div style={{ flex: 1, minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column", gap: 3 }}>
        {hits.map((h) => (
          <div
            key={`${h.kind}:${h.id}`}
            style={row}
            title="クリックでこのカードにフォーカス"
            onClick={() => onFocus(h.kind === "card" ? [h.id] : [], h.kind === "note" ? [h.id] : [])}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--panel-btn-bg, #222a3c)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "var(--panel-input-bg, #0e121c)")}
          >
            <span style={{ flex: "none" }}>{h.icon}</span>
            <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {h.label}
            </span>
            {h.detail && (
              <span style={{ flex: "none", fontSize: 10, color: "var(--panel-muted, #8a93a8)" }}>{h.detail}</span>
            )}
          </div>
        ))}
      </div>
      {hits.length > 1 && (
        <button style={allBtn} onClick={focusAll} title="一致した全カードを選択して全体が入るようにフォーカスします（Enter）">
          一致をすべて選択してフォーカス（{hits.length}件）
        </button>
      )}
    </div>
  );
}

const panel: React.CSSProperties = {
  position: "fixed",
  top: 60,
  right: 16,
  width: 340,
  maxHeight: "70vh",
  display: "flex",
  flexDirection: "column",
  background: "var(--panel-bg, #1c2130)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 10,
  padding: 12,
  boxShadow: "0 8px 28px #000000aa",
  fontFamily: "sans-serif",
  color: "var(--panel-text, #d6dded)",
  fontSize: 13,
  zIndex: 1500,
};

const inp: React.CSSProperties = {
  width: "100%",
  padding: "7px 9px",
  background: "var(--panel-input-bg, #0e121c)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 6,
  color: "var(--panel-text, #eef2f9)",
  fontSize: 13,
  outline: "none",
  boxSizing: "border-box",
};

function chip(selected: boolean): React.CSSProperties {
  return {
    padding: "3px 8px",
    fontSize: 11,
    borderRadius: 12,
    cursor: "pointer",
    fontFamily: "sans-serif",
    background: selected ? "#3a6fb0" : "var(--panel-btn-bg, #2a3346)",
    color: selected ? "#fff" : "var(--panel-text, #cdd6e6)",
    border: `1px solid ${selected ? "#5a8fd0" : "var(--panel-border, #3a4660)"}`,
  };
}

const row: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: 6,
  padding: "5px 8px",
  background: "var(--panel-input-bg, #0e121c)",
  border: "1px solid var(--panel-border, #2a3346)",
  borderRadius: 6,
  cursor: "pointer",
  fontSize: 12,
};

const hdrBtn: React.CSSProperties = {
  padding: "2px 8px",
  fontSize: 11,
  color: "var(--panel-text, #d6dded)",
  background: "var(--panel-btn-bg, #2a3346)",
  border: "1px solid var(--panel-border, #3a4660)",
  borderRadius: 5,
  cursor: "pointer",
  flex: "none",
};

const allBtn: React.CSSProperties = {
  marginTop: 8,
  padding: "7px 12px",
  fontSize: 12,
  fontWeight: 600,
  color: "#fff",
  background: "#3a6fb0",
  border: "none",
  borderRadius: 6,
  cursor: "pointer",
  fontFamily: "sans-serif",
};
