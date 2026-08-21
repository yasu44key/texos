# -*- coding: utf-8 -*-
"""レシピ集を組む — 過去のプロジェクトで実際に動いていた仕組みを、重複を落として並べる。

出来るもの: `F:\\yoshi\\Documents\\TexoS\\レシピ集`

素材は 11 個の実プロジェクト（TexoS / TexoSMain / TexoSLib / TexoS画像生成 / TexoSSound /
TexoS紹介動画制作 / TexoS1.4テスト …）の棚卸し。フロー矢印の連結成分で「ひとつの仕組み」を
切り出したところ **84 個・署名で 41 種類**あり、`t2i` だけで 10 回、`ai` 単体が 9 回、
`launcher` が 7 回と重複が大きかった。ここではそれを**目的が違うものだけ**に畳んである。

外部プログラムの `cmd` は**実物からそのまま写した**（推測で書くと動かないレシピになる）。
出典は各レシピの見出し付箋に書いてある。

小さなサンプル素材（sample.png / sample.wav / sample.mp4）を同梱するので、
道具さえ入っていれば **▶ を押せばそのまま動く**。

    python scripts/build_recipe_book.py
"""
import os
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from texos_flow import Project  # noqa: E402

ROOT = r"F:\yoshi\Documents\TexoS\レシピ集"
MODEL = "gemma4:latest"          # §2-1: この環境の ollama list で確認したもの
WHISPER = os.path.join(os.environ["APPDATA"], "com.yoshi.texos", "whisper")

COL = 2100          # レシピ1つぶんの横幅
ROW = 900           # 縦の間隔


def make_media() -> str:
    """ffmpeg で小さな見本（画像・音声・動画）を作る。無ければレシピは素材なしで組む。"""
    d = os.path.join(tempfile.gettempdir(), "texos_recipe_media")
    os.makedirs(d, exist_ok=True)
    jobs = [
        ("sample.png", ["-f", "lavfi", "-i", "testsrc=size=480x360:rate=1:duration=1",
                        "-frames:v", "1"]),
        ("sample.wav", ["-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                        "-ar", "16000", "-ac", "1"]),
        ("sample.mp4", ["-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=2",
                        "-pix_fmt", "yuv420p"]),
    ]
    for name, args in jobs:
        path = os.path.join(d, name)
        if os.path.exists(path):
            continue
        try:
            subprocess.run(["ffmpeg.exe", "-hide_banner", "-loglevel", "error", "-y",
                            *args, path], check=True, capture_output=True)
        except (OSError, subprocess.CalledProcessError):
            return ""
    return d


class Book:
    def __init__(self, p: Project, media: str):
        self.p, self.media = p, media
        self.i = 0

    def head(self, title, what, needs, source) -> tuple[float, float]:
        """レシピの枠と見出し。戻り値は中身を置く左上。"""
        col, rowi = self.i % 2, self.i // 2
        x, y = col * COL, rowi * ROW
        self.i += 1
        self.p.shape(x - 40, y - 60, COL - 120, ROW - 80, title)
        self.p.note(
            f"【{title}】\n\n{what}\n\n要るもの: {needs}\n出典: {source}",
            x, y - 30, width=330, title_size=15)
        return x + 380, y

    def media_card(self, name, x, y, memo=None):
        return self.p.copy_file(os.path.join(self.media, name), x, y, memo=memo)


def build(p: Project, media: str) -> None:
    b = Book(p, media)

    # ---------------------------------------------------------------- 1
    x, y = b.head("AIに聞く", "いちばん小さい形。付箋に書いた問いをAIに渡して答えを受け取る。",
                  "Ollama（または LM Studio）", "TexoSMain の AI対話（9個あった同型を1つに）")
    q = p.note("問い（ここを書き換える）\n\n日本の四季を一文で説明してください。", x, y, width=300)
    ai = p.ai("AI", x + 380, y, prefix="AIの答え")
    r = p.note("役割：簡潔なアシスタント\n\nあなたは親切で簡潔なアシスタントです。",
               x + 380, y + 240, width=300)
    p.flow(q, ai)
    p.flow(r, ai, role="system")

    # ---------------------------------------------------------------- 2
    x, y = b.head("AIの答えを Word にする",
                  "AIの出力（Markdown）を pandoc で .docx に変換する。報告書づくりの定番。",
                  "pandoc.exe", "TexoS / TexoSLib / TexoSMain（3プロジェクトで同型）")
    q2 = p.note("お題（ここを書き換える）\n\n在宅勤務の利点と欠点を、見出し付きで300字。", x, y, width=300)
    ai2 = p.ai("AI（Markdownで書かせる）", x + 380, y,
               system="Markdown で、見出しと箇条書きを使って書いてください。")
    doc = p.program("Word に変換（pandoc）", x + 760, y, name="pandoc.exe",
                    cmd='"{in}" -o "{out}"', output_ext="docx", accept_mime="*/*",
                    prefix="報告書", timeout_secs=60)
    p.flow(q2, ai2)
    p.flow(ai2, doc)

    # ---------------------------------------------------------------- 3
    x, y = b.head("AIにコードを書かせて実行する",
                  "AIの答えから ``` の中身だけを取り出し、そのまま python に食わせる。\n"
                  "コンパイル言語なら gcc.exe に差し替える（レシピ「多言語で実行」も参照）。",
                  "python.exe（または gcc.exe）", "TexoSMain の『フィボナッチプログラム』一式")
    q3 = p.note("注文（ここを書き換える）\n\nフィボナッチ数列を20項まで表示する Python を書いて。",
                x, y, width=300)
    ai3 = p.ai("AI（コードを書く）", x + 380, y,
               system="要求されたプログラムだけを ``` で囲んで出力してください。説明は不要。")
    ext = p.builtin("コード抽出（python）", x + 760, y, name="md_codeblock",
                    lang="python", accept_mime="text/*",
                    memo="``` の中身だけを取り出す。lang を変えれば c / java などに")
    run = p.program("Python 実行", x + 1140, y, name="python.exe",
                    cmd='"{in}" > "{out}"', output_ext="txt", accept_mime="text/*",
                    prefix="実行結果", timeout_secs=60)
    p.flow(q3, ai3)
    p.flow(ai3, ext)
    p.flow(ext, run)

    # ---------------------------------------------------------------- 4
    x, y = b.head("多視点AI（賢者・イノベーター → 蒸留 → 判定）",
                  "同じ問いを**別々のカード**で聞くから、視点が本当に割れる（1枚のプロンプトで\n"
                  "「4つの視点で」と頼むと必ず穏当に収束する）。温度も役割ごとに変える。",
                  "Ollama", "TexoS紹介動画制作（AI×6の合議）")
    odai = p.note("課題（ここを書き換える）\n\n紙の手帳が生き残っている理由は何か。", x, y - 40, width=300)
    kenja = p.ai("AI 賢者（temperature 0）", x + 380, y - 140, temperature=0,
                 system="あなたは非常に優れた賢者です。物事を冷静に眺め、大きな視点で判断し答えを出します。")
    inov = p.ai("AI イノベーター（temperature 1）", x + 380, y + 120, temperature=1,
                system="あなたは特異な発想を持つイノベーターです。突拍子もない考えでもどんどん出していきます。")
    taba = p.cat("束（2つの意見）", x + 800, y - 20, separator="rule", prefix="材料")
    hantei = p.ai("AI 判定者", x + 1180, y - 20, temperature=0, prefix="判定",
                  system="2つの意見を読み、それぞれの強い点と弱い点を挙げたうえで、"
                         "採るべき案とその理由を述べてください。確信度と、その理由も一文で。")
    p.flow(odai, kenja)
    p.flow(odai, inov)
    p.flow(kenja, taba)      # ← 束の入力順は繋いだ順
    p.flow(inov, taba)
    p.flow(taba, hantei)

    # ---------------------------------------------------------------- 5
    x, y = b.head("人の判断を途中に挟む",
                  "AIの下書きを読んでから人が決める。答えの付箋に書いて ▶ を押し直すと先へ進む。\n"
                  "「答えが入ったら上流を動かさない」を入れないと、読んだ案と別物が下流へ行く。",
                  "Ollama", "v1.8 サンプル_人の判断を挟む を圧縮")
    q5 = p.note("相談ごと（ここを書き換える）\n\n社内勉強会のテーマを3案ください。", x, y, width=300)
    ai5 = p.ai("AI 下書き", x + 380, y, system="案を3つ、それぞれ1行で。")
    ask = p.ask("人に聞く", x + 760, y, question="どの案で進めますか",
                skip_upstream=True, prefix="決定",
                memo="skip_upstream=on: 答えが入っている間は上流のAIを動かさない")
    ans = p.note("（ここに答えを書く）\n> 上の下書きを見て、選んだ案を書いてください",
                 x + 760, y + 250, width=300)
    p.flow(q5, ai5)
    p.flow(ai5, ask)
    p.flow(ans, ask, role="answer")

    # ---------------------------------------------------------------- 6
    x, y = b.head("納得するまで練り直す（反復）",
                  "反復カードが上流を引き直す。戻り矢印は**束**に戻すこと（AIに直接戻すと\n"
                  "資料と合わせて2本＝ファンアウトする）。max_rounds はヒューズ。",
                  "Ollama", "v1.8 カード実験_反復 を圧縮")
    odai6 = p.note("推敲のお題（ここを書き換える）\n\n「当システムにおきましては、"
                   "利用者様がファイルを保存された場合において、当該ファイルは自動的に"
                   "バックアップが取得される仕組みとなっております。」", x, y - 40, width=300)
    yaku6 = p.note("役割：推敲者\n\n前回の案があれば1点だけ直して全文を出す。無ければお題を推敲する。\n"
                   "もう直すところが無いときだけ、最後の行に「合格」と書く。", x, y + 260, width=300)
    taba6 = p.cat("束（お題＋前周）", x + 380, y, separator="rule", prefix="材料")
    ai6 = p.ai("AI 推敲", x + 760, y)
    tap6 = p.tap("各周の記録", x + 1140, y, prefix="各周")
    loop6 = p.loop("反復（最大3周）", x + 1500, y, max_rounds=3, until_contains="合格")
    p.flow(odai6, taba6)
    p.flow(loop6, taba6, role="previous")
    p.flow(taba6, ai6)
    p.flow(yaku6, ai6, role="system")
    p.flow(ai6, tap6)
    p.flow(tap6, loop6)

    # ---------------------------------------------------------------- 7
    x, y = b.head("Webページを読み込んでAIに要約させる",
                  "URLカードから本文を Markdown で取り、そのままAIに渡す。",
                  "ネット接続 / Ollama", "TexoSMain の web_fetch + to_url（39枚の大物を圧縮）")
    urls = p.note("URL（ここを書き換える）\n\nhttps://ja.wikipedia.org/wiki/情報カード",
                  x, y, width=300)
    tourl = p.builtin("URLカード化", x + 380, y, name="to_url", accept_mime="text/*",
                      memo="テキスト中のURLを1枚ずつURLカードにする")
    fetch = p.builtin("Webページ取得", x + 760, y, name="web_fetch", accept_mime="text/*",
                      memo="URLカードを受けて本文を Markdown の実身にする")
    ai7 = p.ai("AI 要約", x + 1140, y, prefix="要約",
               system="この記事を箇条書き5点で要約してください。")
    p.flow(urls, tourl)
    p.flow(tourl, fetch)
    p.flow(fetch, ai7)

    # ---------------------------------------------------------------- 8
    x, y = b.head("フィードを取って、欲しい記事だけ拾う",
                  "フィード取得 → grep で絞る → 置換で整える → URLカード化。\n"
                  "毎朝みたいに回したいならタイマカードから発行する（カタログの「自動発行」）。",
                  "ネット接続", "TexoSMain のフィード監視（17枚）")
    feed = p.builtin("フィード取得", x, y, name="web_feed", accept_mime="text/*",
                     url="https://news.yahoo.co.jp/rss/topics/top-picks.xml",
                     memo="URL はカードの設定で書き換える")
    grep8 = p.builtin("grep: 関心のある語", x + 380, y, name="text_grep",
                      pattern="AI|半導体", accept_mime="text/*",
                      memo="pattern を書き換えると拾う記事が変わる")
    tourl8 = p.builtin("URLカード化", x + 760, y, name="to_url", accept_mime="text/*",
                       prefix="拾った記事")
    p.flow(feed, grep8)
    p.flow(grep8, tourl8)

    # ---------------------------------------------------------------- 9
    x, y = b.head("音声を文字にする",
                  "音声ファイルを 16kHz mono に揃えてから whisper に渡す。\n"
                  "マイクから録るなら、左を『マイク録音』カード（ジェネレータ）に差し替える。",
                  "ffmpeg.exe / whisper-cli.exe＋モデル", "TexoS / TexoSLib / TexoSMain（3件で同型）")
    if media:
        wav = b.media_card("sample.wav", x, y, memo="見本の音（440Hz 2秒）。本物の録音に差し替える")
    else:
        wav = p.note("（ここに音声の実身カードを繋ぐ）", x, y, width=300)
    conv = p.program("Whisper用WAVに変換", x + 380, y, name="ffmpeg.exe",
                     cmd="-y -i {in} -ar 16000 -ac 1 -vn {out}",
                     accept_mime="audio/*", output_ext="wav", timeout_secs=600)
    wh = p.program("Whisper 文字起こし", x + 760, y, name="whisper-cli.exe",
                   path=os.path.join(WHISPER, "bin", "whisper-cli.exe"),
                   cmd='-m "' + os.path.join(WHISPER, "models", "ggml-small.bin")
                       + '" -f {in} -otxt -of "{out_noext}"',
                   output_ext="txt", accept_mime="audio/*", prefix="文字起こし",
                   timeout_secs=600,
                   memo="-of は拡張子を自分で付けるので {out_noext} を渡す")
    rec = p.program("（差し替え用）マイク録音", x + 380, y + 300, name="ffmpeg.exe",
                    mode="generator", output_ext="wav", accept_mime="audio/*",
                    cmd='-hide_banner -f dshow -i "audio=マイク (USB MICROPHONE)"'
                        ' -ar 16000 -ac 1 -y {out}',
                    memo="ジェネレータ（入力なし）。デバイス名は環境ごとに違う。停止は q")
    p.flow(wav, conv)
    p.flow(conv, wh)

    # ---------------------------------------------------------------- 10
    x, y = b.head("読み上げる",
                  "テキストを OS の読み上げに渡すだけ。**出力カードを作らない**"
                  "（呼んで終わるカード）。AIの答えを耳で聞きたいときに末尾へ足す。",
                  "OS の読み上げ機能", "TexoSLib / TexoSMain の『テキストの読み上げ』")
    t10 = p.note("読み上げる文（ここを書き換える）\n\nレシピ集の読み上げ試験です。", x, y, width=300)
    sp = p.builtin("読み上げ", x + 380, y, name="speak", accept_mime="text/*")
    p.flow(t10, sp)

    # ---------------------------------------------------------------- 11
    x, y = b.head("プロンプトをAIに書かせて画像を作る",
                  "日本語の思いつき → AIが英語のプロンプトに → 画像生成。\n"
                  "AIが余計な前置きを書くので grep で `Prompt:` の行だけ抜くのが実地の知恵。",
                  "Ollama / t2i デーモン＋GPU", "TexoS画像生成・TexoSMain（同型が複数）")
    seed = p.note("作りたいもの（ここを書き換える）\n\n夕暮れの図書館、窓辺に猫", x, y, width=300)
    ai11 = p.ai("AI プロンプト作成", x + 380, y,
                system="入力を英語の画像生成プロンプトにしてください。"
                       "出力は `Prompt: ` で始まる1行だけ。説明は書かないこと。")
    grep11 = p.builtin("grep: Prompt 行", x + 760, y, name="text_grep",
                       pattern="^Prompt:", accept_mime="text/*")
    rep11 = p.builtin("置換: Prompt: を外す", x + 1140, y, name="text_replace",
                      pattern="^Prompt:\\s*", replace="", accept_mime="text/*")
    t2i = p.builtin("画像生成", x + 1520, y, name="t2i", accept_mime="text/*",
                    model="sdxl", steps=25, guidance=7.5, width=1024, height=1024,
                    negative="blurry, low quality, bad anatomy, extra limbs, text, watermark",
                    prefix="生成画像")
    p.flow(seed, ai11)
    p.flow(ai11, grep11)
    p.flow(grep11, rep11)
    p.flow(rep11, t2i)

    # ---------------------------------------------------------------- 12
    x, y = b.head("画像を直す（リサイズ・変換・回転）",
                  "内蔵のリサイズ／形式変換で足りるものは内蔵で。回転など細かいことは magick に渡す。",
                  "（回転のみ）magick.exe", "TexoS1.4テスト / TexoSMain の画像処理")
    if media:
        img = b.media_card("sample.png", x, y, memo="見本の画像。差し替えて使う")
    else:
        img = p.note("（ここに画像の実身カードを繋ぐ）", x, y, width=300)
    rsz = p.builtin("リサイズ 幅400px", x + 380, y, name="img_resize", width=400,
                    accept_mime="image/*", prefix="縮小")
    cnv = p.builtin("JPEG に変換", x + 380, y + 240, name="img_convert", format="jpeg",
                    accept_mime="image/*", prefix="変換後")
    rot = p.program("90度回転（magick）", x + 760, y, name="magick.exe",
                    cmd='"{in}" -rotate 90 "{out}"', output_ext="jpg",
                    accept_mime="*/*", prefix="回転", timeout_secs=60)
    p.flow(img, rsz)
    p.flow(img, cnv)
    p.flow(rsz, rot)

    # ---------------------------------------------------------------- 13
    x, y = b.head("画像から3Dモデルを作る",
                  "1枚の画像から glb を作る。出来た glb はカードの上でそのまま回せる。",
                  "Hunyuan3D-2 デーモン＋GPU", "TexoSMain の t23d")
    if media:
        img13 = b.media_card("sample.png", x, y, "見本の画像。被写体がはっきりした写真ほどよい")
    else:
        img13 = p.note("（ここに画像の実身カードを繋ぐ）", x, y, width=300)
    t23d = p.builtin("3D生成", x + 380, y, name="t23d", accept_mime="image/*",
                     prefix="3Dモデル",
                     memo="Hunyuan3D の glb は metallic が既定1.0。暗く見えたら環境光の問題")
    p.flow(img13, t23d)

    # ---------------------------------------------------------------- 14
    x, y = b.head("音を切り出す・整える",
                  "頭出しと長さを指定して切る／音量を揃える。ffmpeg の引数をそのまま書く。",
                  "ffmpeg.exe", "TexoSSound の音声クリップ / TexoSMain の音声ノーマライズ")
    if media:
        wav14 = b.media_card("sample.wav", x, y, memo="見本の音。差し替えて使う")
    else:
        wav14 = p.note("（ここに音声の実身カードを繋ぐ）", x, y, width=300)
    clip = p.program("切り出し 0.2秒目から0.4秒", x + 380, y, name="ffmpeg.exe",
                     cmd="-y -ss 0.2 -i {in} -t 0.4 {out}", output_ext="mp3",
                     accept_mime="audio/*", prefix="切り出し", timeout_secs=600)
    norm = p.program("音量を揃える（loudnorm）", x + 380, y + 240, name="ffmpeg.exe",
                     cmd="-y -i {in} -af loudnorm {out}", output_ext="mp3",
                     accept_mime="audio/*", prefix="正規化", timeout_secs=600)
    p.flow(wav14, clip)
    p.flow(wav14, norm)

    # ---------------------------------------------------------------- 15
    x, y = b.head("動画を GIF にする・切り出す",
                  "説明用の短い GIF を作る。長さを切るときは -c copy を付けると再圧縮しない。",
                  "ffmpeg.exe", "TexoSMain の『mp4 2 gif』『動画クリップ』")
    if media:
        mp4 = b.media_card("sample.mp4", x, y, memo="見本の動画。差し替えて使う")
    else:
        mp4 = p.note("（ここに動画の実身カードを繋ぐ）", x, y, width=300)
    gif = p.program("GIF に変換", x + 380, y, name="ffmpeg.exe",
                    cmd='-i "{in}" "{out}"', output_ext="gif", accept_mime="*/*",
                    prefix="説明用", timeout_secs=60)
    cut = p.program("先頭10秒を切り出す", x + 380, y + 240, name="ffmpeg.exe",
                    cmd="-y -ss 0 -i {in} -t 10 -c copy {out}", output_ext="mp4",
                    accept_mime="video/*", prefix="切り出し", timeout_secs=600)
    p.flow(mp4, gif)
    p.flow(mp4, cut)

    # ---------------------------------------------------------------- 16
    x, y = b.head("テキストを数える・並べる・比べる",
                  "Unix の道具立てをそのままカードにしたもの。連結・差分・横結合だけが\n"
                  "**複数入力を1回で受ける**（それ以外は入力ごとに別々に走る）。",
                  "なし（内蔵だけ）", "TexoSMain の標準フィルタ見本（13枚）")
    a16 = p.note("入力1\n赤\n青\n黄\n", x, y, width=200)
    b16 = p.note("入力2\n赤\n緑\n黄\n", x, y + 190, width=200)
    cat16 = p.cat("連結", x + 300, y, separator="rule", prefix="連結結果")
    diff16 = p.builtin("差分", x + 300, y + 250, name="text_diff", lines=3,
                       accept_mime="text/*", prefix="差分")
    cnt16 = p.builtin("行数・単語数・文字数", x + 680, y, name="text_count", mode="all",
                      accept_mime="text/*", prefix="集計")
    srt16 = p.builtin("並べ替え（重複を除く）", x + 680, y + 250, name="text_sort",
                      order="asc", compare="text", unique="remove",
                      accept_mime="text/*", prefix="整列")
    p.flow(a16, cat16)
    p.flow(b16, cat16)
    p.flow(a16, diff16)
    p.flow(b16, diff16)
    p.flow(cat16, cnt16)
    p.flow(cat16, srt16)

    # ---------------------------------------------------------------- 17
    x, y = b.head("アプリをそのまま起動する（ランチャー）",
                  "TexoS が抱え込まない統合。作業フォルダはプロジェクト直下になり、\n"
                  "環境変数 TEXOS_PROJECT_DIR にもその場所が入るので、アプリ側が書き戻せる。",
                  "起動したいアプリ", "TexoSLib / TexoSSound / TexoSProject2（7件あった同型）")
    p.launcher("メモ帳を開く", x, y, name="notepad.exe",
               memo="name を差し替えるだけ。Audacity・draw.io・自作 Python など何でも")
    p.note("ランチャーには入出力が無い（フローの矢印は繋がない）。\n"
           "『このプロジェクトで作業するための道具』を面に置いておく使い方。",
           x + 380, y, width=320)

    # ---------------------------------------------------------------- 18
    x, y = b.head("文字コードを直す",
                  "Shift_JIS などの古いテキストを UTF-8 に揃える。\n"
                  "文字化けしたまま下流へ行くと、AIも grep も静かに間違える。",
                  "なし（内蔵だけ）", "TexoSLib の『UTF-8へ変換』")
    src18 = p.note("（ここに文字コードの怪しい実身カードを繋ぐ）\n"
                   "この付箋はすでに UTF-8 なので、変換しても中身は変わりません。",
                   x, y, width=320)
    conv18 = p.builtin("文字コード変換（→UTF-8）", x + 400, y, name="text_convert",
                       accept_mime="*/*", prefix="UTF8化")
    p.flow(src18, conv18)


def main() -> int:
    media = make_media()
    p = Project(ROOT, model=MODEL)
    p.note(
        "レシピ集 — 実際に動いていた仕組みを、目的ごとに1つずつ\n\n"
        "・枠ひとつが1レシピ。左の付箋に「何ができるか／要るもの／出典」。\n"
        "・**右端のカードの ▶ を押せば動きます**（見本の素材を同梱してあります）。\n"
        "・使うときは、このプロジェクトからカードをコピーして自分の面に貼るか、\n"
        "  同じ形を組み直してください。\n\n"
        "元にしたのは11個の実プロジェクト。フロー矢印の連結成分で数えると仕組みは84個・\n"
        "41種類ありましたが、目的が重なるもの（画像生成が10回、AI単体が9回…）を\n"
        "畳んで18本にしています。外部プログラムの引数は実物からそのまま写しました。",
        60, -420, width=680, title_size=18)
    # 道具の入手先は、道具が要ると気づく場所（レシピの面）に置いておく。
    # 正本は docs/外部依存プログラム一覧.md で、ここへは組むたびに写しを持ってくる。
    guide = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                         "docs", "外部依存プログラム一覧.md")
    if os.path.exists(guide):
        p.copy_file(guide, 60, -180, name="道具のインストールガイド.md",
                    memo="各レシピの「要るもの」の入手先と注意点。"
                         "正本は texos の docs/外部依存プログラム一覧.md")

    build(p, media)
    p.view(0, -380, 0.28)
    p.save(reset=True)
    p.link_into(os.path.dirname(ROOT))
    if not media:
        print("  ※ ffmpeg.exe が見つからず、見本の素材は同梱していません")
    return 0


if __name__ == "__main__":
    sys.exit(main())
