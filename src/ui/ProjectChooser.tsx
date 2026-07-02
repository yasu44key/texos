import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export interface ProjectInfo {
  path: string;
  name: string;
}

interface Props {
  onChosen: (info: ProjectInfo) => void;
  onCancel?: () => void; // null when no project is open yet (cannot cancel)
}

export function ProjectChooser({ onChosen, onCancel }: Props) {
  const [recent, setRecent] = useState<ProjectInfo[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<ProjectInfo[]>("list_recent_projects").then(setRecent).catch(() => {});
  }, []);

  const createNew = async () => {
    const dir = await open({
      directory: true,
      title: "新規プロジェクトの場所を選択（空の専用フォルダを推奨）",
    });
    if (!dir || Array.isArray(dir)) return;
    try {
      const info = await invoke<ProjectInfo>("new_project", { path: dir });
      onChosen(info);
    } catch (e) {
      setError(String(e));
    }
  };

  const openExisting = async () => {
    const dir = await open({ directory: true, title: "TexoSプロジェクトフォルダを開く" });
    if (!dir || Array.isArray(dir)) return;
    try {
      const info = await invoke<ProjectInfo>("open_project", { path: dir });
      onChosen(info);
    } catch (e) {
      setError(String(e));
    }
  };

  const openRecent = async (path: string) => {
    try {
      const info = await invoke<ProjectInfo>("open_project", { path });
      onChosen(info);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    // v1.4.18: テーマ追従 — 全面はキャンバス色、行/ボタンは --panel-*（ビュワー系と同じ変数体系）。
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "var(--canvas-bg, #12121e)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 3000,
        fontFamily: "sans-serif",
        color: "var(--panel-text, #d6dded)",
      }}
    >
      <div style={{ width: 420, maxWidth: "90vw" }}>
        <div style={{ fontSize: 26, fontWeight: 700, marginBottom: 4, letterSpacing: 1 }}>
          TexoS
        </div>
        <div style={{ fontSize: 12, color: "var(--panel-muted, #7a8aa6)", marginBottom: 24 }}>
          織り紡ぐ者たち — プロジェクトを選択
        </div>

        <div style={{ display: "flex", gap: 10, marginBottom: 20 }}>
          <button onClick={createNew} style={primaryBtn}>
            ＋ 新規プロジェクト
          </button>
          <button onClick={openExisting} style={secondaryBtn}>
            プロジェクトを開く...
          </button>
        </div>

        {recent.length > 0 && (
          <div>
            <div style={{ fontSize: 11, color: "var(--panel-muted, #7a8aa6)", marginBottom: 8 }}>最近使ったプロジェクト</div>
            <div style={{ display: "flex", flexDirection: "column", gap: 4, maxHeight: "40vh", overflowY: "auto" }}>
              {buildTree(recent).map(({ info, depth }) => (
                <div
                  key={info.path}
                  onClick={() => openRecent(info.path)}
                  style={{ ...recentRow, marginLeft: depth * 18 }}
                  onMouseEnter={(e) => (e.currentTarget.style.background = "var(--panel-btn-bg, #222a3c)")}
                  onMouseLeave={(e) => (e.currentTarget.style.background = "var(--panel-input-bg, #1a2030)")}
                >
                  <div style={{ fontSize: 14 }}>
                    {depth > 0 && <span style={{ color: "var(--panel-muted, #5a6478)" }}>↳ </span>}
                    {info.name}
                  </div>
                  <div style={{ fontSize: 10, color: "var(--panel-muted, #6a7488)" }}>{info.path}</div>
                </div>
              ))}
            </div>
          </div>
        )}

        {error && (
          <div style={{ marginTop: 16, color: "#ff7b7b", fontSize: 12 }}>{error}</div>
        )}

        {onCancel && (
          <div style={{ marginTop: 20, textAlign: "right" }}>
            <button onClick={onCancel} style={secondaryBtn}>
              キャンセル
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

const primaryBtn: React.CSSProperties = {
  flex: 1,
  padding: "12px 16px",
  background: "#3a6fb0",
  color: "#fff",
  border: "none",
  borderRadius: 8,
  cursor: "pointer",
  fontSize: 14,
  fontFamily: "sans-serif",
};

const secondaryBtn: React.CSSProperties = {
  padding: "12px 16px",
  background: "var(--panel-btn-bg, #2a3346)",
  color: "var(--panel-text, #cdd6e6)",
  border: "1px solid var(--panel-border, transparent)",
  borderRadius: 8,
  cursor: "pointer",
  fontSize: 14,
  fontFamily: "sans-serif",
};

// Normalize separators for prefix comparison.
function norm(p: string): string {
  return p.replace(/\\/g, "/").replace(/\/+$/, "");
}

function isAncestor(parent: string, child: string): boolean {
  const a = norm(parent);
  const b = norm(child);
  return b !== a && b.startsWith(a + "/");
}

/** Order recent projects as a tree (parent before children) with indent depth. */
function buildTree(items: ProjectInfo[]): { info: ProjectInfo; depth: number }[] {
  const parentOf = new Map<string, string | null>();
  for (const it of items) {
    let best: string | null = null;
    for (const other of items) {
      if (other.path === it.path) continue;
      if (isAncestor(other.path, it.path)) {
        if (best === null || norm(other.path).length > norm(best).length) best = other.path;
      }
    }
    parentOf.set(it.path, best);
  }
  const childrenOf = new Map<string, ProjectInfo[]>();
  for (const it of items) {
    const key = parentOf.get(it.path) ?? "__root__";
    if (!childrenOf.has(key)) childrenOf.set(key, []);
    childrenOf.get(key)!.push(it);
  }
  const out: { info: ProjectInfo; depth: number }[] = [];
  const walk = (list: ProjectInfo[] | undefined, depth: number) => {
    for (const it of list ?? []) {
      out.push({ info: it, depth });
      walk(childrenOf.get(it.path), depth + 1);
    }
  };
  walk(childrenOf.get("__root__"), 0);
  return out;
}

const recentRow: React.CSSProperties = {
  background: "var(--panel-input-bg, #1a2030)",
  border: "1px solid var(--panel-border, #2a3346)",
  borderRadius: 6,
  padding: "8px 12px",
  cursor: "pointer",
};
