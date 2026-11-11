#![allow(unused_imports)]
use crate::*;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 新規プロジェクトの初期カード（Blender の起動時の立方体と同じ考え）
// 空のプロジェクトを新規作成したときだけ、小さな出発点を置く:
//   案内の付箋 1 ／ 素材の実身カード「はじめに.md」1 ／ 内蔵フィルタ「文字数を数える」1 ／
//   素材 → フィルタ の flow 矢印 1。結果カードは置かない（▶ で生まれるのを体験してもらう）。
// 入口は project.rs の create_new_project_at（new_project コマンド）だけ。既存を開く・
// フォーク・内包サブプロジェクト・テスト用 db::open からは呼ばない。
// ---------------------------------------------------------------------------

pub(crate) const STARTER_FILE: &str = "はじめに.md";

const STARTER_NOTE: &str = "ここからスタート
① 右の『文字数を数える』カードの ▶ を押すと、結果のカードが生まれます
② 何もないところを右クリックすると、カードを置くメニューが出ます
③ カードの右端からドラッグすると、矢印でつなげます
このまとまりは不要になったら選んで削除してかまいません";

const STARTER_TEXT: &str = "# はじめに
TexoS では、カード1枚が1つのファイルです。
カードどうしをつなぐ矢印は、データの流れを表します。
フィルタカードの ▶ を押すと流れが実行され、結果は新しいカードとして生まれます。
元のファイルは書き換えられないので、気軽に試してかまいません。
";

/// 内蔵「行数・単語数・文字数」（フロントの addFilter("text_count") と同じ形）。
const STARTER_FILTER_SPEC: &str = r#"{"builtin":"text_count","accept_mime":"text/*"}"#;

// 配置（キャンバス左上付近。カードは 150x115、付箋は幅 380 で 7 行前後＝約 170px）。
const NOTE_POS: (f64, f64) = (40.0, 40.0);
const FILE_POS: (f64, f64) = (60.0, 260.0);
const FILTER_POS: (f64, f64) = (300.0, 260.0);

/// 初期カードを置く（best-effort）。`はじめに.md` が既にあれば何も置かない。
/// 途中で失敗したら DB はロールバックし、今書いた `はじめに.md` も消す。
pub(crate) fn place_starter_cards(p: &Project) -> Result<(), String> {
    let file = p.root.join(STARTER_FILE);
    if file.exists() {
        return Ok(());
    }
    std::fs::write(&file, STARTER_TEXT).map_err(|e| e.to_string())?;
    let res = place_in_tx(p, &file);
    if res.is_err() {
        let _ = std::fs::remove_file(&file);
    }
    res
}

fn place_in_tx(p: &Project, file: &Path) -> Result<(), String> {
    let tx = p.conn.unchecked_transaction().map_err(|e| e.to_string())?;

    // 1. 素材の実身カード（プロジェクト内のファイル＝committed_path つきで取り込まれる）
    let file_id = import_file_into(
        p,
        file.to_string_lossy().to_string(),
        String::new(),
        FILE_POS.0,
        FILE_POS.1,
    )?;
    // import は kind を空のまま入れ、次回 open の migrate で 'file' に補正される。
    // 初回表示から確定させておく（既存の補正と同じ値）。
    p.conn
        .execute(
            "UPDATE virtual_bodies SET kind = 'file' WHERE id = ?1 AND kind IS NULL",
            rusqlite::params![file_id],
        )
        .map_err(|e| e.to_string())?;

    // 2. 内蔵フィルタカード
    let filter_id = create_filter_in(
        p,
        "文字数を数える".to_string(),
        STARTER_FILTER_SPEC.to_string(),
        FILTER_POS.0,
        FILTER_POS.1,
    )?;

    // 3. 素材 → フィルタ の flow 矢印
    create_relation_in(p, file_id, filter_id, Some("flow".to_string()))?;

    // 4. 案内の付箋（幅を広げ、先頭行を見出しに）
    let note_id = create_note_in(p, STARTER_NOTE.to_string(), NOTE_POS.0, NOTE_POS.1)?;
    let style: NoteStyle =
        serde_json::from_value(serde_json::json!({ "width": 380.0, "title_size": 16.0 }))
            .map_err(|e| e.to_string())?;
    write_note_style(p, &note_id, &style)?;

    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("texos_starter_{}_{}_{}", tag, db::now_ms(), uuid()))
    }

    fn count(p: &Project, sql: &str) -> i64 {
        p.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn counts(p: &Project) -> (i64, i64, i64, i64) {
        (
            count(p, "SELECT COUNT(*) FROM virtual_bodies WHERE kind = 'note' AND deleted_at IS NULL"),
            count(
                p,
                "SELECT COUNT(*) FROM virtual_bodies
                 WHERE kind = 'file' AND committed_path IS NOT NULL AND deleted_at IS NULL",
            ),
            count(p, "SELECT COUNT(*) FROM virtual_bodies WHERE kind = 'filter' AND deleted_at IS NULL"),
            count(p, "SELECT COUNT(*) FROM relations WHERE kind = 'flow' AND deleted_at IS NULL"),
        )
    }

    #[test]
    fn new_project_gets_starter_cards_once() {
        let root = temp_root("new");
        {
            let p = create_new_project_at(&root, true).unwrap();
            assert_eq!(counts(&p), (1, 1, 1, 1));
            assert!(root.join(STARTER_FILE).is_file());
            // 矢印は 素材 → フィルタ
            let (from_kind, to_kind): (String, String) = p
                .conn
                .query_row(
                    "SELECT a.kind, b.kind FROM relations r
                     JOIN virtual_bodies a ON a.id = r.from_id
                     JOIN virtual_bodies b ON b.id = r.to_id
                     WHERE r.kind = 'flow'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!((from_kind.as_str(), to_kind.as_str()), ("file", "filter"));
            let spec: String = p
                .conn
                .query_row("SELECT filter_ref FROM virtual_bodies WHERE kind = 'filter'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(flow::parse_spec(&spec).unwrap().builtin.as_deref(), Some("text_count"));
        }
        // 2 回目: 既存を開いても、同じ入口をもう一度通っても増えない
        {
            let p = open_project_at(&root).unwrap();
            assert_eq!(counts(&p), (1, 1, 1, 1));
        }
        {
            let p = create_new_project_at(&root, true).unwrap();
            assert_eq!(counts(&p), (1, 1, 1, 1));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn starter_cards_disabled_places_nothing() {
        let root = temp_root("off");
        let p = create_new_project_at(&root, false).unwrap();
        assert_eq!(counts(&p), (0, 0, 0, 0));
        assert!(!root.join(STARTER_FILE).exists());
        drop(p);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_starter_file_is_not_overwritten() {
        let root = temp_root("exists");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(STARTER_FILE), "ユーザーのメモ").unwrap();
        let p = create_new_project_at(&root, true).unwrap();
        assert_eq!(counts(&p), (0, 0, 0, 0));
        assert_eq!(std::fs::read_to_string(root.join(STARTER_FILE)).unwrap(), "ユーザーのメモ");
        drop(p);
        let _ = std::fs::remove_dir_all(&root);
    }
}
