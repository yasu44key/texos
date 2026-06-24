import { useEffect, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import * as XLSX from "xlsx";
import mammoth from "mammoth";

// HTML 化（第1階層）で扱う拡張子。それ以外の doc 系は LibreOffice→PDF（第2階層）。
const HTML_EXTS = ["docx", "xlsx", "xls", "ods", "csv", "tsv"];

interface Props {
  objId: string;
  label: string;
  /** 小文字の拡張子（docx / xlsx / xls / csv / tsv / ods）。レンダラ選択に使う。 */
  ext: string;
  /** vertical offset from the card origin (card height + gap) */
  top: number;
  onClose: () => void;
}

/**
 * v1.3.11 #13: オフィス系ファイルのインライン（DOM）プレビュー。承認ロードマップの
 * 第1階層「HTML 化」: docx → mammoth、xlsx/xls/ods/csv/tsv → SheetJS（→HTMLテーブル）。
 * すべて読取専用・非破壊（get_media_data_url で中身を取得するだけ。CAS には焼かない）。
 * DOM に描くので他のプレビュー同様カードと一緒に動く。未対応や変換失敗は「一時的に閲覧」へ。
 */
export function DocPreview({ objId, label, ext, top, onClose }: Props) {
  const [html, setHtml] = useState<string | null>(null);
  const [pdfSrc, setPdfSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const isHtmlMode = HTML_EXTS.includes(ext);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        if (isHtmlMode) {
          // 第1階層: get_media_data_url を fetch で ArrayBuffer 化し DOM に描く。
          const dataUrl = await invoke<string>("get_media_data_url", { id: objId });
          const buf = await (await fetch(dataUrl)).arrayBuffer();
          if (cancelled) return;
          if (ext === "docx") {
            const res = await mammoth.convertToHtml({ arrayBuffer: buf });
            if (!cancelled) setHtml(res.value || "<p>（空のドキュメント）</p>");
          } else {
            // SheetJS: 先頭シートを HTML テーブルに。csv/tsv/ods も読める。
            const wb = XLSX.read(new Uint8Array(buf), { type: "array" });
            const first = wb.SheetNames[0];
            const table = first
              ? XLSX.utils.sheet_to_html(wb.Sheets[first])
              : "<p>（シートがありません）</p>";
            if (!cancelled) setHtml(table);
          }
        } else {
          // 第2階層: LibreOffice headless で PDF 化して既存の PDF ビューアで見せる
          // （pptx/ppt/odp/doc/odt 等。要 LibreOffice）。
          const pdfPath = await invoke<string>("convert_office_to_pdf", { id: objId });
          if (!cancelled) setPdfSrc(convertFileSrc(pdfPath));
        }
      } catch (e) {
        console.error("DocPreview failed", objId, e);
        if (!cancelled) setFailed(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [objId, ext, isHtmlMode]);

  return (
    <div
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: 520,
        background: "#0d0f16",
        border: "1px solid #2a3a5a",
        borderRadius: 8,
        boxShadow: "0 4px 20px #000000aa",
        overflow: "hidden",
        zIndex: 5,
        fontFamily: "sans-serif",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "6px 8px",
          background: "#161b26",
          fontSize: 11,
          color: "#c7d2e6",
        }}
      >
        <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          📄 {label}（読み取り専用プレビュー）
        </span>
        <button
          onClick={onClose}
          style={{
            padding: "3px 9px",
            background: "#3a4658",
            color: "#fff",
            border: "none",
            borderRadius: 5,
            cursor: "pointer",
            fontSize: 11,
          }}
        >
          ✕
        </button>
      </div>
      {failed ? (
        <div style={{ color: "#ff7b7b", fontSize: 12, padding: 20, lineHeight: 1.5 }}>
          プレビューを生成できませんでした。「一時的に閲覧」や「既定のアプリで開く」をお試しください。
          <div style={{ color: "#9a6a6a", fontSize: 11, marginTop: 8 }}>{failed}</div>
        </div>
      ) : pdfSrc ? (
        <iframe
          src={pdfSrc}
          title={label}
          style={{ display: "block", width: "100%", height: 600, border: "none", background: "#fff" }}
        />
      ) : html == null ? (
        <div style={{ color: "#7a8aa6", fontSize: 12, padding: 30 }}>
          {isHtmlMode ? "読み込み中..." : "PDFに変換中...（LibreOffice）"}
        </div>
      ) : (
        <div
          // 変換結果は読取専用表示。スクロールできる白背景パネル。
          style={{
            maxHeight: 560,
            overflow: "auto",
            background: "#fff",
            color: "#111",
            padding: 14,
            fontSize: 13,
            lineHeight: 1.5,
          }}
          dangerouslySetInnerHTML={{ __html: html }}
        />
      )}
    </div>
  );
}
