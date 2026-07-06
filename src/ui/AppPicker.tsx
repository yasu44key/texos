import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface App {
  name: string;
  path: string;
}

interface Props {
  onPick: (app: App) => void;
  onCancel: () => void;
}

/**
 * v1.3.11 #A: インストール済みアプリ（スタートメニューの .lnk）を検索して選ぶピッカー。
 * exe をファイルシステムから探さずに、普段見ているアプリ一覧から登録できる。
 */
export function AppPicker({ onPick, onCancel }: Props) {
  const [apps, setApps] = useState<App[] | null>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    invoke<App[]>("list_installed_apps")
      .then(setApps)
      .catch(() => setApps([]));
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!apps) return [];
    if (!q) return apps;
    return apps.filter((a) => a.name.toLowerCase().includes(q));
  }, [apps, query]);

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
          // 2026-07-06: テーマ追従（--panel-* 体系）。暗色固定（2026-06-26決定）は改訂。
          background: "var(--panel-bg, #1c2130)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 10,
          padding: 16,
          width: 460,
          maxHeight: "80vh",
          display: "flex",
          flexDirection: "column",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
          fontSize: 13,
        }}
      >
        <div style={{ fontSize: 15, marginBottom: 10, textAlign: "center" }}>
          インストール済みアプリから選択
        </div>
        <input
          autoFocus
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="アプリ名で検索..."
          style={{
            width: "100%",
            padding: "8px 10px",
            background: "var(--panel-input-bg, #0e121c)",
            border: "1px solid var(--panel-border, #38445e)",
            borderRadius: 6,
            color: "var(--panel-text, #eef2f9)",
            fontSize: 13,
            outline: "none",
            boxSizing: "border-box",
            marginBottom: 10,
          }}
        />
        <div style={{ overflowY: "auto", flex: 1, minHeight: 120 }}>
          {apps == null ? (
            <div style={{ color: "var(--panel-muted, #7a8aa6)", padding: 20, textAlign: "center" }}>読み込み中...</div>
          ) : filtered.length === 0 ? (
            <div style={{ color: "var(--panel-muted, #7a8aa6)", padding: 20, textAlign: "center" }}>
              該当するアプリがありません
            </div>
          ) : (
            filtered.map((a) => (
              <button
                key={a.path}
                onClick={() => onPick(a)}
                title={a.path}
                style={{
                  display: "block",
                  width: "100%",
                  textAlign: "left",
                  padding: "7px 10px",
                  background: "transparent",
                  color: "var(--panel-text, #d6dded)",
                  border: "none",
                  borderRadius: 6,
                  cursor: "pointer",
                  fontSize: 13,
                  fontFamily: "sans-serif",
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--panel-header-bg, #2a3346)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
              >
                🚀 {a.name}
              </button>
            ))
          )}
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 12 }}>
          <button
            onClick={onCancel}
            style={{
              padding: "7px 16px",
              background: "#2a3346",
              color: "#aeb8cc",
              border: "none",
              borderRadius: 6,
              cursor: "pointer",
              fontSize: 13,
              fontFamily: "sans-serif",
            }}
          >
            キャンセル
          </button>
        </div>
      </div>
    </div>
  );
}
