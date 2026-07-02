#!/usr/bin/env node
// TexoS リリーススクリプト（v1.4.17〜）。手作業だった「リリース手順（毎回共通）」を1コマンドに:
//   1. バージョン3か所更新（package.json / src-tauri/tauri.conf.json / src-tauri/Cargo.toml。
//      Cargo.lock は次の cargo 実行で自動追従）
//   2. npx tsc --noEmit（型チェック）
//   3. cargo test --lib（Rust 単体テスト）
//   4. npm run tauri build（NSIS インストーラ）
//   5. インストーラを I:\ へコピー
//   6. docs/更新履歴.md の先頭にエントリ雛形を挿入（記入は人間）
//
// 使い方:
//   npm run release -- 1.4.17            … フル実行
//   npm run release -- 1.4.17 --dry-run  … 何をするか表示のみ（ファイル変更なし）
//   npm run release -- 1.4.17 --no-build … バージョン更新＋チェックのみ（ビルド/コピーなし）
//
// 注意: git のコミット/タグ/push は行わない（意図的・人間の判断で）。dev サーバーは
// 停止不要（release/debug 別プロファイルで並行ビルド可＝v1.4.16 で実績）。

import { readFileSync, writeFileSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { execSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const version = args.find((a) => !a.startsWith("--"));
const dryRun = args.includes("--dry-run");
const noBuild = args.includes("--no-build");
const DEST_DIR = "I:\\"; // インストーラの配布先（従来の手順どおり）

if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error("使い方: npm run release -- <x.y.z> [--dry-run] [--no-build]");
  process.exit(1);
}

const log = (msg) => console.log(`\x1b[36m[release]\x1b[0m ${msg}`);
const run = (cmd, cwd = root) => {
  log(`$ ${cmd}${cwd !== root ? `  (in ${cwd})` : ""}`);
  if (!dryRun) execSync(cmd, { stdio: "inherit", cwd });
};

// --- 1. バージョン3か所 ------------------------------------------------------
// JSON は parse/stringify で整形が変わるため、regex でその場だけ書き換える。
function patchFile(rel, pattern, replacement) {
  const path = join(root, rel);
  const src = readFileSync(path, "utf8");
  if (!pattern.test(src)) {
    console.error(`エラー: ${rel} にバージョン行が見つかりません（パターン: ${pattern}）`);
    process.exit(1);
  }
  const out = src.replace(pattern, replacement);
  log(`version → ${version}: ${rel}`);
  if (!dryRun) writeFileSync(path, out);
}

patchFile("package.json", /("version":\s*")[\d.]+(")/, `$1${version}$2`);
patchFile("src-tauri/tauri.conf.json", /("version":\s*")[\d.]+(")/, `$1${version}$2`);
patchFile("src-tauri/Cargo.toml", /(^version\s*=\s*")[\d.]+(")/m, `$1${version}$2`);

// --- 2〜3. チェック -----------------------------------------------------------
run("npx tsc --noEmit");
run("cargo test --lib", join(root, "src-tauri"));

// --- 4〜5. ビルド＆配布コピー --------------------------------------------------
if (noBuild) {
  log("--no-build 指定のためビルド/コピーをスキップします。");
} else {
  run("npm run tauri build");
  const installer = join(
    root,
    "src-tauri", "target", "release", "bundle", "nsis",
    `texos_${version}_x64-setup.exe`
  );
  if (!dryRun) {
    if (!existsSync(installer)) {
      console.error(`エラー: インストーラが見つかりません: ${installer}`);
      process.exit(1);
    }
    if (existsSync(DEST_DIR)) {
      copyFileSync(installer, join(DEST_DIR, `texos_${version}_x64-setup.exe`));
      log(`コピー完了: ${DEST_DIR}texos_${version}_x64-setup.exe`);
    } else {
      log(`警告: ${DEST_DIR} が見つからないためコピーをスキップしました（手動でコピーしてください）`);
    }
  } else {
    log(`copy ${installer} → ${DEST_DIR}`);
  }
}

// --- 6. 更新履歴の雛形 ---------------------------------------------------------
const histPath = join(root, "docs", "更新履歴.md");
const hist = readFileSync(histPath, "utf8");
if (hist.includes(`## v${version}`)) {
  log(`更新履歴: v${version} のエントリは既にあります（挿入スキップ）`);
} else {
  const today = new Date().toISOString().slice(0, 10);
  const stub = `## v${version}（${today}）\n\n- （ここに変更点を記入）\n\n---\n\n`;
  // 先頭のイントロ直後（最初の "---" 行の後）に挿入＝最新が上の既存形式を保つ。
  const m = hist.match(/^---\s*$/m);
  const at = m ? m.index + m[0].length + 1 : 0;
  const out = hist.slice(0, at) + "\n" + stub + hist.slice(at).replace(/^\n+/, "");
  log(`更新履歴: v${version} の雛形を挿入（内容の記入を忘れずに）`);
  if (!dryRun) writeFileSync(histPath, out);
}

log("完了。残りの人間の仕事: ①更新履歴の記入 ②実機確認 ③git commit & push");
