# Web検索カード（web_search）× SearXNG 計画

2026-09-30 起票。V2.0課題.md §7（v1.7 §7-1 で見送った Web検索コネクタ）の再浮上。
**検索バックエンドは SearXNG 一本**・**セットアップはフルオート**（ユーザー決定 2026-09-30）。

## 0. 位置づけ

- 形は「方式A＝検索カード」。AIカードは改修しない。検索語は人が書いても（A1）AIが書いても（A2）よい。
  - A1: `[検索語] → web_search → AI(課題・役割は赤線) → レポート`
  - A2: `[課題] → AI(検索語を発行) → web_search → AI(レポート) → レポート`（課題は最後のAIにも赤線）
- 起動は既存の pull（eval_filter の再帰）に乗る。検索専用の起動機構は作らない。
- tools（方式B＝AIが自分で検索するループ）は V2.0。同じ検索実装を tool として見せるだけにする。

## 1. 実機スパイクの結果（2026-09-30・Windows 11・Python 3.13.15）

SearXNG master（a9d9900・2026-09-30）を **Docker なし・venv で直接起動し、JSON 検索まで成功**。

- **git clone は失敗する**: `utils/templates/...searxng.conf:socket` 等、`:` を含むファイル名が4つ（Linux サーバー用テンプレート・動作に無関係）。
  → GitHub の tarball を取得し、Windows で無効な名前を飛ばして展開すれば足りる。
- **`import pwd` で起動失敗**: `searx/valkeydb.py` の1か所のみ（valkey 接続失敗時のエラー文言用）。
  → **上流を書き換えず、シムで外から差す**: `pwd.py`（getpwuid スタブ）＋`sitecustomize.py`（os.getuid 補完）を PYTHONPATH に置く。
- **tzdata が無い**: Windows の Python はタイムゾーン DB を持たない（bilibili エンジン等が ZoneInfo を要求）。
  → `tzdata` を追加で入れる。
- **ポート 8888 は Windows に予約されていた**（`netsh ... excludedportrange` に 8888、System PID 4 が LISTEN）。
  → 固定ポートにしない。TexoS が空きポートを選んで settings.yml に書く。
- **Tor 系エンジン（ahmia / torch）は読み込み失敗**。無害だがログが汚れるので無効化する。
- **venv の python.exe は中継役**: 実体は子プロセスとして起動する（親 5MB → 子 94MB）。
  → プロセスツリーごと管理する（Job Object・KILL_ON_JOB_CLOSE）。
- 数値の目安:
  - tarball 6.5MB
  - searx/ 21MB
  - venv 90MB
  - 起動 1.7秒
  - 常駐 約95MB
- 検索結果:
  - `language=ja` で日本語の結果が返る。
  - `categories=news` と `time_range=week` で Bing News の今週の記事が返る。
  - `engines=google,bing` で両方の結果が統合され、`engines` 欄に出典が付く。
  - Brave は初回から「too many requests」で自動休止した（unresponsive_engines に理由付きで出る）。

## 2. フルオート・セットアップ

目標は「**同意1クリック → 以後は意識しない**」。ダウンロードを伴うので、初回の同意ダイアログ1回だけは残す（ゼロクリックにはしない）。

置き場所は本体の外で、app_data/searxng/ の下に置く。

- `bin/uv.exe`: GitHub Releases から単体の zip を取得する（winget に依存しない）。
- `python/`: `uv python install` で入れる、TexoS 専用の Python（版を固定）。
- `src/<pin>/`: SearXNG のソース。固定コミットの tarball を展開したもの。
- `venv/`: `uv venv` で作り、`uv pip install -r requirements.txt tzdata` で依存を入れる。
- `shim/`: TexoS が所有する `pwd.py` と `sitecustomize.py`（TexoS の版と一緒に管理する）。
- `settings.yml`: TexoS が生成する。
- `state.json`: 導入した pin・ポート・最後の健康確認の結果。

手順（ステートマシン: 未導入 → 導入中 → 停止 → 起動中 → 稼働 / 故障）:

1. **uv の取得**: uv.exe が無ければダウンロードする。システムの Python には触れない。
2. **Python の取得**: `uv python install 3.13`（スパイクで動作確認した版）。
3. **ソースの取得**: `codeload.github.com/searxng/searxng/tar.gz/<pin>` を取得し、Rust 側（tar + flate2）で展開する。
   - 取り出すのは `searx/` と `requirements.txt` だけ。Windows で無効な名前は飛ばす。
   - SearXNG には正式なリリース版が無く、常に更新され続ける方式なので、**TexoS が検証済みのコミットを pin として持つ**。
4. **依存の導入**: `uv pip install`。進捗は ollama-pull と同じくイベントで UI に流す。
5. **シムと設定の生成**:
   - `use_default_settings: true`
   - `secret_key`: ランダムに生成
   - `bind_address: 127.0.0.1`
   - `port`: 空きポートを探して決める
   - `limiter: false`
   - `formats: [html, json]`
   - `valkey.url: false`
   - Tor 系エンジンを無効化
   - 既定の検索元（§3）
6. **起動**:
   - 環境変数: `PYTHONPATH=shim`、`SEARXNG_SETTINGS_PATH`、`PYTHONUTF8=1`
   - CREATE_NO_WINDOW で起動し、Job Object に入れる。
   - `/healthz` が 200 を返せば稼働。
7. **自己診断**: `search?q=test&format=json` を1回投げる。
   - 403 が返る → JSON 無効。設定を再生成する。
   - 結果が0件で全エンジンが unresponsive → ネットワーク側の問題として案内する。

運用:

- **遅延起動**: 最初の web_search 実行（pull）で起動する。起動は2秒弱なので、アプリ起動時の常駐は不要。
- **停止**: TexoS 終了時に Job Object ごと落とす。一定時間使われなければ停止する（既定10分）。
- **ポート衝突**: 起動時に bind が失敗したら、ポートを選び直して設定を再生成し、もう一度起動する。
- **更新**: TexoS のリリースで pin を上げる（検証済みのものだけ）。
  - 新しい版は `src/<新pin>` と venv を**並べて作ってから切り替える**。古い版は1世代残し、戻せるようにする（水泡に帰さない）。
  - 検索元サイトの変更で結果が取れなくなったら、unresponsive の理由を添えて「更新があります」と案内する。
- **アンインストール**: app_data/searxng/ を消すだけ。
- **ライセンス**: AGPL-3.0 の SearXNG は同梱しない。ユーザーの操作で取得し、手元で動かす（改変もしない＝シムは TexoS 側のファイル）。

## 3. web_search カードの仕様

- **入力**: text。空でない1行を1つの検索語として扱う。行頭の `-`・`1.`・引用符などの装飾は除く。
- **ヒューズ**:
  - `max_queries`: 1回の実行で検索する行数の上限。既定3。超えた分は捨て、出力の末尾に注記する。
  - 検索と検索の間は1秒あける。SearXNG 自身の自動休止と合わせて二重にする。
- **設定**（カードに持つ。検索語のテキストには混ぜない）:
  - `engines`: チェックボックス。既定は google / bing / duckduckgo / wikipedia。
    - Brave はスパイクで即座に休止したので既定から外す。
  - `categories`: general / news / it / science。
  - `language`: 既定 ja。
  - `time_range`: なし / day / week / month / year。
  - `max_results`: 1検索語あたりの件数。既定10。
- **出力**: md 1枚。検索語ごとに節を立て、各結果は次の形にする。

  ```
  ### タイトル
  URL
  抜粋
  （出典: google, bing ／ 日付）
  ```

  - URL は裸で書く。to_url がそのまま拾えるようにするため。
  - 末尾に unresponsive だったエンジンと理由を注記する。
- **結果が0件のとき**: no-data として下流を止める（v1.5 の no-data 一級化に従う）。
- **安全**:
  - `fetch_url` の `allow_private` は、**TexoS が起動した SearXNG の URL にだけ**立てる。ユーザーが入力した URL には立てない。
  - SearXNG の待ち受けは 127.0.0.1 限定。

## 4. 号割り（案）

1. **P1 デーモン管理**: 導入・起動・停止・健康確認・Job Object、preflight/modules に状態表示、同意ダイアログと進捗表示。
2. **P2 web_search ビルトイン**: 3点チェックリスト（run_builtin＋単体テスト / BUILTIN_SCHEMAS / メニュー「Web ...」）。
3. **P3 レシピ**: A1・A2 をサンプルフローとユーザーズチップスに加える。A2 ではキーワードを受けるカードを挟み、人が確認できる形にする。
4. **P4（任意）**: 既出 URL を除くオプション（web_feed の既読台帳と同じ考え方）。タイマー起動の定期レポートで重複を防ぐ。

## 5. 決着・未決

- ✅ Python は **3.13 固定**（2026-09-30 ユーザー決定）。t2i の管理 env（3.12）とは別に持つ。
- ✅ 号割りは §4 のとおり（2026-09-30 ユーザー決定）。
- ✅ 最初の pin = `a9d99003344ac3879ec45c56c661283482a92a39`（2026-09-30・スパイクで動作確認したもの）。
- ✅ tarball の完全性確認: GitHub の tarball は pax ヘッダの comment にコミットハッシュを持つ。
  展開前にこれが pin と一致することを確かめる（バイト列のハッシュは不変保証が無いので使わない）。
- ✅ プロセスの後始末: venv の python.exe（中継）は自分の子を「中継が死ねば子も死ぬ」Job に入れている
  （スパイクで中継を止めると子も消えることを確認）。TexoS は中継を KILL_ON_JOB_CLOSE の Job に入れるだけで、
  TexoS の異常終了時も含めて木ごと片付く。
- 未決: macOS / Linux では SearXNG が公式に対応しているので、シムが不要な分だけ単純になる見込み。実機は未検証。
