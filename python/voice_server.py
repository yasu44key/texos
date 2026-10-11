"""TexoS 音声反射カードの音声プロセス（v1.10・docs/V1.10_音声反射カード計画.md）。

TexoS（音声カード op:"voice" の稼働中だけ）が子プロセスとして起動する。マイクと話者の
持ち主はこのプロセス1つ（調停に「ユーザーが話し中」を使うため）。

取り決め（§3.5）: 標準入出力に JSON を1行ずつ。stdout は JSON 行専用・人向けログは stderr。
  → TexoS: ready / state{state} / level{rms} / speech_start / utterance{text}
           / speaking{id, text} / spoken{id, interrupted} / error{message}
  ← TexoS: say{id, text, priority} / hush / pause / resume / stop
stdin が閉じたら（TexoS が終了・異常終了したら）自分も終わる＝孤児にならない。

素材: VoiceAssistantAgent（record の無音区切り・割り込み検出・冒頭保持、speak.ps1）。
会話（Ollama）は持たない — 会話は大脳側（AI カード）。
"""
import argparse
from collections import deque
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time

RATE = 16000
BLOCK = 800  # 50ms
BLOCK_SEC = BLOCK / RATE
LEVEL_EVERY = 0.2  # level イベントの間引き（秒）

_out_lock = threading.Lock()


def emit(ev, **kw):
    kw["ev"] = ev
    line = json.dumps(kw, ensure_ascii=False)
    with _out_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def log(msg):
    print(msg, file=sys.stderr, flush=True)


# Windows: WinRT の日本語音声で合成し同期再生する（VoiceAssistantAgent の speak.ps1）。
# 中断はこのプロセスを止めることで行う。
SPEAK_PS1 = r"""param([Parameter(Mandatory=$true)][string]$TextPath)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Media.SpeechSynthesis.SpeechSynthesizer, Windows.Media.SpeechSynthesis, ContentType=WindowsRuntime]
$synth = New-Object Windows.Media.SpeechSynthesis.SpeechSynthesizer
$voice = [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::AllVoices | Where-Object { $_.Language -eq 'ja-JP' } | Select-Object -First 1
if (-not $voice) { throw 'Japanese TTS voice missing. Install Japanese speech in Windows Settings > Time & language > Speech.' }
$synth.Voice = $voice
$text = [IO.File]::ReadAllText($TextPath, [Text.Encoding]::UTF8)
$operation = $synth.SynthesizeTextToStreamAsync($text)
$asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.IsGenericMethod -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' } | Select-Object -First 1
$null = [Windows.Media.SpeechSynthesis.SpeechSynthesisStream, Windows.Media.SpeechSynthesis, ContentType=WindowsRuntime]
$task = $asTask.MakeGenericMethod([Windows.Media.SpeechSynthesis.SpeechSynthesisStream]).Invoke($null, @($operation))
$stream = $task.GetAwaiter().GetResult()
$reader = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($stream)
$memory = New-Object IO.MemoryStream
try {
    $reader.CopyTo($memory)
    $memory.Position = 0
    $player = New-Object System.Media.SoundPlayer
    $player.Stream = $memory
    $player.PlaySync()
    $player.Dispose()
} finally {
    $memory.Dispose()
    $reader.Dispose()
    $stream.Dispose()
    $synth.Dispose()
}
"""

PRIORITY_RANK = {"urgent": 0, "normal": 1, "low": 2}


class Speaker:
    """報告の読み上げと調停（§3.4）。

    urgent = 低い優先度の読み上げを差し替えて、ユーザーの発話が終わり次第すぐ話す。
    normal = ユーザーが話し終えて report_gap 秒静かになってから話す。
    low    = 溜めておく（今は話さない。聞かれたら話す仕組みは後段）。
    """

    def __init__(self, home, report_gap):
        self.home = home
        self.report_gap = report_gap
        self.queue = []  # (rank, seq, id, text)
        self.seq = 0
        self.proc = None
        self.cur = None  # (rank, id, textfile)
        self.script = None
        if os.name == "nt":
            self.script = home / "speak.ps1"
            self.script.write_text(SPEAK_PS1, encoding="utf-8-sig")

    def busy(self):
        return self.proc is not None and self.proc.poll() is None

    def say(self, sid, text, priority):
        rank = PRIORITY_RANK.get(priority, 1)
        self.seq += 1
        self.queue.append((rank, self.seq, sid, text))
        self.queue.sort()
        # urgent は今の（より低い優先度の）読み上げを差し替える。
        if rank == 0 and self.busy() and self.cur and self.cur[0] > 0:
            self.stop(interrupted=True)

    def hush(self):
        for _, _, sid, _ in self.queue:
            emit("spoken", id=sid, interrupted=True)
        self.queue.clear()
        self.stop(interrupted=True)

    def stop(self, interrupted):
        p = self.proc
        if p is not None and p.poll() is None:
            p.terminate()
            try:
                p.wait(timeout=2)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait()
        self._finish(interrupted)

    def _finish(self, interrupted):
        if self.cur is not None:
            _, sid, path = self.cur
            emit("spoken", id=sid, interrupted=interrupted)
            try:
                os.remove(path)
            except OSError:
                pass
        self.proc = None
        self.cur = None

    def tick(self, recording, quiet_for):
        """毎ブロック呼ぶ。終わった読み上げを片付け、次を出せるなら出す。"""
        if self.proc is not None and self.proc.poll() is not None:
            if self.proc.returncode not in (0, None):
                log(f"読み上げプロセスが異常終了しました（{self.proc.returncode}）")
            self._finish(False)
        if self.busy() or not self.queue or recording:
            return
        rank = self.queue[0][0]
        if rank >= 2:
            return
        if rank == 1 and quiet_for < self.report_gap:
            return
        _, _, sid, text = self.queue.pop(0)
        self._start(rank, sid, text)

    def _start(self, rank, sid, text):
        fd, path = tempfile.mkstemp(prefix="texos-voice-", suffix=".txt")
        with os.fdopen(fd, "w", encoding="utf-8-sig") as f:
            f.write(text)
        if os.name == "nt":
            cmd = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass",
                   "-File", str(self.script), "-TextPath", path]
            flags = 0x08000000  # CREATE_NO_WINDOW
        elif sys.platform == "darwin":
            cmd, flags = ["say", "-f", path], 0
        else:
            cmd, flags = ["espeak", "-f", path], 0
        try:
            self.proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                         stderr=subprocess.DEVNULL, creationflags=flags)
        except OSError as e:
            emit("error", message=f"読み上げを起動できません: {e}")
            emit("spoken", id=sid, interrupted=True)
            os.remove(path)
            return
        self.cur = (rank, sid, path)
        emit("speaking", id=sid, text=text)


def stdin_reader(cmds):
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            cmds.put(json.loads(line))
        except json.JSONDecodeError:
            log(f"不正な命令行を無視しました: {line[:80]}")
    cmds.put({"cmd": "stop"})  # TexoS が居なくなった


def transcriber(model, jobs, hotwords):
    while True:
        audio = jobs.get()
        if audio is None:
            return
        emit("state", state="transcribing")
        try:
            # hotwords: コマンドの言い回しへ認識を寄せる（「メモして」→「目もして」の聞き違い対策）。
            segments, _ = model.transcribe(audio, language="ja", beam_size=3, vad_filter=True,
                                           condition_on_previous_text=False,
                                           hotwords=hotwords or None)
            text = "".join(s.text for s in segments).strip()
        except Exception as e:  # 1発話の失敗でプロセスを落とさない
            emit("error", message=f"文字起こしに失敗しました: {e}")
            text = ""
        if text:
            emit("utterance", text=text)
        emit("state", state="listening")


def parse_device(s):
    if s is None or str(s).strip() == "":
        return None
    s = str(s).strip()
    return int(s) if s.isdigit() else s


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--home", required=True, help="作業フォルダ（models/・speak.ps1 を置く）")
    ap.add_argument("--whisper-model", default="base")
    ap.add_argument("--device", default=None, help="入力デバイス番号または名前の一部")
    ap.add_argument("--threshold", type=float, default=0.012)
    ap.add_argument("--interrupt-threshold", type=float, default=0.04)
    ap.add_argument("--silence", type=float, default=1.2)
    ap.add_argument("--max-seconds", type=float, default=30)
    ap.add_argument("--report-gap", type=float, default=1.0)
    ap.add_argument("--hotwords", default="", help="聞き取りを寄せる語（コマンドの言い回し等・空白区切り）")
    a = ap.parse_args()

    home = Path(a.home)
    home.mkdir(parents=True, exist_ok=True)
    emit("state", state="preparing")
    try:
        import numpy as np
        import sounddevice as sd
        from faster_whisper import WhisperModel
    except ImportError as e:
        emit("error", message=f"音声ライブラリが未導入です（環境を構築してください）: {e}")
        return 2
    try:
        model = WhisperModel(a.whisper_model, device="cpu", compute_type="int8",
                             download_root=str(home / "models"))
    except Exception as e:
        emit("error", message=f"音声認識モデルを準備できません（初回はネット接続が必要）: {e}")
        return 1

    cmds = queue.Queue()
    threading.Thread(target=stdin_reader, args=(cmds,), daemon=True).start()
    tjobs = queue.Queue()
    threading.Thread(target=transcriber, args=(model, tjobs, a.hotwords.strip()), daemon=True).start()
    speaker = Speaker(home, a.report_gap)
    audio = queue.Queue()

    def on_audio(indata, frames, t, status):
        if status:
            log(f"録音の状態: {status}")
        audio.put(indata[:, 0].copy())

    device = parse_device(a.device)
    paused = False
    stream = None

    def open_stream():
        s = sd.InputStream(samplerate=RATE, channels=1, dtype="float32", blocksize=BLOCK,
                           device=device, callback=on_audio)
        s.start()
        return s

    def close_stream(s):
        if s is not None:
            s.stop()
            s.close()
        while not audio.empty():  # 閉じる前の残りは捨てる
            audio.get_nowait()

    try:
        stream = open_stream()
    except Exception as e:
        emit("error", message=f"マイクを開けません: {e}")
        return 1
    emit("ready", model=a.whisper_model)
    emit("state", state="listening")

    pre = deque(maxlen=6)  # 冒頭の取りこぼし防止（最大300ms）
    chunks = []
    recording = False
    voiced_blocks = 0
    quiet = 0.0
    speech_at = 0.0
    last_voice = time.monotonic()
    last_level = 0.0
    try:
        while True:
            # 1) 命令
            while True:
                try:
                    c = cmds.get_nowait()
                except queue.Empty:
                    break
                kind = c.get("cmd")
                if kind == "stop":
                    return 0
                if kind == "say":
                    speaker.say(c.get("id", ""), str(c.get("text", "")), c.get("priority", "normal"))
                elif kind == "hush":
                    speaker.hush()
                elif kind == "pause" and not paused:
                    # 録音カードがマイクを使う（§5-4 録音カード優先）。話者は止めない。
                    close_stream(stream)
                    stream = None
                    paused = True
                    recording = False
                    chunks = []
                    pre.clear()
                    emit("state", state="paused")
                elif kind == "resume" and paused:
                    try:
                        stream = open_stream()
                    except Exception as e:
                        emit("error", message=f"マイクを開き直せません: {e}")
                        return 1
                    paused = False
                    last_voice = time.monotonic()
                    emit("state", state="listening")
            if paused:
                speaker.tick(False, time.monotonic() - last_voice)
                time.sleep(0.05)
                continue
            # 2) 音声ブロック
            try:
                data = audio.get(timeout=0.1)
            except queue.Empty:
                speaker.tick(recording, time.monotonic() - last_voice)
                continue
            now = time.monotonic()
            level = float(np.sqrt(np.mean(data ** 2)))
            if now - last_level >= LEVEL_EVERY:
                emit("level", rms=round(level, 4))
                last_level = now
            speaking = speaker.busy()
            thr = a.interrupt_threshold if speaking and not recording else a.threshold
            voiced = level >= thr
            if voiced:
                last_voice = now
            if not recording:
                pre.append(data)
                voiced_blocks = voiced_blocks + 1 if voiced else 0
                # 読み上げ中は自分の声を拾わないよう 3 ブロック（約150ms）続いたら割り込みとみなす。
                if voiced and (not speaking or voiced_blocks >= 3):
                    if speaking:
                        speaker.stop(interrupted=True)
                    recording = True
                    speech_at = now
                    quiet = 0.0
                    chunks = list(pre)
                    pre.clear()
                    emit("speech_start")
                    emit("state", state="recording")
            else:
                chunks.append(data)
                quiet = 0.0 if voiced else quiet + BLOCK_SEC
                if quiet >= a.silence or now - speech_at >= a.max_seconds:
                    recording = False
                    voiced_blocks = 0
                    tjobs.put(np.concatenate(chunks))
                    chunks = []
            speaker.tick(recording, now - last_voice)
    finally:
        speaker.hush()
        close_stream(stream)
        tjobs.put(None)


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8")
        sys.stderr.reconfigure(encoding="utf-8")
    except AttributeError:
        pass
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
    except Exception as exc:
        emit("error", message=f"音声プロセスが停止しました: {exc}")
        sys.exit(1)
