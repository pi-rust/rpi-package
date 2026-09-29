# rpi-voice

Hands-free voice conversation for the rpi Rust agent — assistant replies can be
spoken aloud (TTS), and you can talk to the agent instead of typing (STT).
Auto-TTS is opt-in and starts disabled.

Unlike a standalone voice-assistant binary, this is an **rpi extension**: it
loads into the running TUI, so the normal text conversation stays intact and
voice is layered on top.

## What it does

- **Auto-TTS** — every assistant reply is synthesized with Microsoft Edge TTS
  (free, no API key) and played through the default output device. Replies are
  queued on one worker thread, so a multi-message turn is spoken in order
  rather than dropped while audio is playing.
- **Voice input** — `/voice` records the microphone, stops automatically after a
  short trailing silence, transcribes with a Whisper-compatible API, and injects
  the text into the conversation as a user message.
- **Push-to-talk** — `/voice ptt` dedicates a key (default space) to talking:
  hold it and the footer counts down, then becomes a live mic level meter
  (`🎤 listening █████░░░ 1.1s`); release and the utterance is transcribed and
  sent. The key is only claimed while the input box is empty.
- **Markdown-aware speech** — code fences, backticks, links, headings and
  emphasis markers are stripped before synthesis, so the voice never reads
  `**bold**` or a URL.
- **Status in the TUI** — the footer shows `voice: 🎤 recording…`,
  `voice: 📝 transcribing…`, the injected text preview, and any error.

## Commands

| Command | Effect |
| --- | --- |
| `/voice` | Record → transcribe → editable draft in the input box |
| `/voice auto [on\|off]` | Continuous conversation: reply, then listen again |
| `/voice on` / `/voice off` | Enable / disable auto-TTS of replies |
| `/voice ptt [on|off]` | Push-to-talk: hold the key, release to deliver |
| `/voice stop` | Stop the reply that is being read aloud |
| `/voice output [draft|send]` | Where a transcription goes (default `draft`) |
| `/voice status` | Auto-TTS state, current voice, recording/playing, STT key |
| `/voice set <name>` | Set the TTS voice for this session |
| `/voice model` | Show STT engine + local model status |
| `/voice model download` | Pre-fetch the embedded SenseVoice model |
| `/voice help` | Usage |

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `RPI_VOICE` | `zh-CN-XiaoxiaoNeural` | TTS voice name |
| `RPI_VOICE_AUTO_TTS` | `off` | Set `on` to start speaking assistant replies automatically |
| `OPENAI_API_KEY` | — | STT key (only required for the hosted OpenAI endpoint) |
| `RPI_STT_API_KEY` | — | STT key; takes precedence over `OPENAI_API_KEY` |
| `RPI_STT_API_BASE` | `https://api.openai.com/v1` | OpenAI-compatible STT base URL |
| `RPI_STT_MODEL` | `whisper-1` | STT model name (API backend) |
| `RPI_STT_ENGINE` | `local` | `local` \| `auto` \| `api`; local SenseVoice is the default |
| `RPI_STT_MODEL_DIR` | `~/.rpi/agent/models/sense-voice` | Embedded model dir (`model.int8.onnx` + `tokens.txt`) |
| `RPI_VOICE_STT_LANG` | auto | ISO-639-1 language hint for STT (e.g. `zh`) |
| `RPI_VOICE_RECORD_MS` | `20000` | Hard cap on a single recording |
| `RPI_VOICE_SILENCE_MS` | `1200` | Trailing silence that ends recording (`0` disables) |
| `RPI_VOICE_MIN_SPEECH_MS` | `500` | Speech required before silence auto-stop |
| `RPI_VOICE_PTT_KEY` | `space` | Key that drives `/voice ptt` |
| `RPI_VOICE_PTT_HOLD_MS` | `600` | Hold time before the mic opens; shorter holds are ignored |
| `RPI_VOICE_PTT_MAX_MS` | `60000` | Safety cap on one push-to-talk recording |
| `RPI_VOICE_OUTPUT` | `draft` | `draft` (input box) \| `send` (straight to the model) |
| `RPI_VOICE_DRAFT_MS` | `2000` | Auto-send countdown for a draft; `0` waits for a manual Enter |
| `RPI_VOICE_NO_SPEECH_MS` | unset | Optional continuous-mode speech-start timeout; auto waits like PTT by default |
| `RPI_VOICE_WARMUP_MS` | `4000` | Continuous mode: let a Bluetooth microphone wake before the speech window starts |
| `RPI_VOICE_AUTO_EMPTY_MAX` | `3` | Continuous mode: empty turns before it stops itself |
| `RPI_VOICE_INPUT_DEVICE` | system default | Case-insensitive substring of the capture device to use |
| `RPI_VOICE_INPUT_GAIN` | `1.0` | Software input gain (e.g. `20` for a very quiet mic), capped at 100 |

### STT backends

STT uses the embedded SenseVoice model locally by default. On the first voice
input, missing model files are downloaded automatically to
`~/.rpi/agent/models/sense-voice/` (or
`RPI_CODING_AGENT_DIR/models/sense-voice/`). Use `/voice model download` to
pre-fetch the model before recording.

Select another backend with `RPI_STT_ENGINE`:

| `RPI_STT_ENGINE` | Behaviour |
| --- | --- |
| `local` (default) | Use the embedded engine and download missing model files automatically |
| `auto` | Use local SenseVoice when available; fall back to the API if unavailable |
| `api` | Force the OpenAI-compatible endpoint |

#### Embedded offline model — SenseVoice (recommended)

The default build includes the embedded engine and runs entirely on-device via
[sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) + **SenseVoice** (中文/英/日/韩/粤,
auto language detection, punctuation). No key, no server, no network, no runtime
DLL (the native libs are linked statically).

Model files live in `~/.rpi/agent/models/sense-voice/` (`$RPI_CODING_AGENT_DIR/models`
if set) and are downloaded automatically on first use — or up front with
`/voice model download`. ≈240 MB.

```bash
# build (the native libs are fetched at build time)
cargo build -p rpi-voice --release --features local-stt
```

Building needs a C++ toolchain with **cmake** and **libclang** (bindgen) — build-time
only, nothing at runtime. Cheapest way to get libclang without installing full LLVM:

```bash
pip install libclang
# then point LIBCLANG_PATH at clang/native in site-packages
```

The `sherpa-rs` cdylib also needs the Windows system lib at link time:

```bash
# Windows
export LIBCLANG_PATH="$(python -c 'import clang,os;print(os.path.dirname(clang.__file__))')/native"
export RUSTFLAGS="-C link-arg=advapi32.lib"
cargo build -p rpi-voice --release --features local-stt

# Linux (static sherpa also needs this)
export RUSTFLAGS="-C relocation-model=dynamic-no-pic"
cargo build -p rpi-voice --release --features local-stt
```

`RPI_STT_MODEL_DIR` overrides the model directory (must contain `model.int8.onnx`
and `tokens.txt`).

#### OpenAI-compatible endpoint

The client speaks the OpenAI `/audio/transcriptions` shape, so any compatible
server works. A key is **only** required for the hosted OpenAI endpoint — a local
server needs none:

```bash
# Free, fully local (faster-whisper): docker one-liner
docker run -p 8000:8000 ghcr.io/speaches-ai/speaches:latest-cpu
export RPI_STT_ENGINE=api
export RPI_STT_API_BASE=http://localhost:8000/v1
export RPI_STT_MODEL=Systran/faster-whisper-small
```

Groq's free tier also works: `RPI_STT_API_BASE=https://api.groq.com/openai/v1`,
`RPI_STT_MODEL=whisper-large-v3`, plus a key.

Common Chinese voices: `zh-CN-XiaoxiaoNeural` (female, default),
`zh-CN-YunxiNeural` (male), `zh-CN-XiaoyiNeural` (lively).

```bash
# Windows
set OPENAI_API_KEY=sk-xxx
# macOS / Linux
export OPENAI_API_KEY=sk-xxx
```

## Build

```bash
# API-only (default; no native STT deps)
cargo build -p rpi-voice --release

# with embedded offline STT (see "Embedded offline model" above for env vars)
cargo build -p rpi-voice --release --features local-stt
```

The built `rpi_voice.dll`/`.so`/`.dylib` is dropped into `.rpi/extensions/` by
`task install` (or `rpi-package-install`) and loads automatically on the next
rpi start.

## Push-to-talk

`/voice ptt` turns the configured key (default: **space**) into a hold-to-talk
button:

```
/voice ptt          # on
/voice ptt off      # off
```

- Hold the key → the footer counts down (`voice: ⏳ hold ▰▰▰▱▱▱ 0.4s to talk`),
  so you can see the press registered and know to keep holding.
- Once `RPI_VOICE_PTT_HOLD_MS` (default **0.6s**) elapses, the mic opens and the
  footer becomes a live level meter driven by your voice:
  `voice: 🎤 listening █████░░░░░░░ 1.1s — release to send`. A bar that never
  moves means the mic isn't hearing you.
- The same meter is shown for **every** recording path — one-shot `/voice` and
  hands-free turns too (they end on silence, so their hint reads `silence ends
  the turn` instead of `release to send`).
- Release → recording stops, the audio is transcribed, and the text is
  delivered according to `/voice output` (see **Where the text goes**); the
  reply is then spoken by auto-TTS.
- A hold shorter than the threshold never touches the microphone and leaves no
  trace in the footer.

## Continuous conversation (`/voice auto`)

Hands-free turn-taking, in the style of a voice assistant: you talk, it answers
aloud, then it listens again. No key, no `/voice` — just keep talking.

```
/voice auto         # on (starts listening immediately)
/voice auto off     # off
```

```
you:  why is this request failing?
   🔊 voice: ♪ ▂▅▇▅▂▁▃ playing 3.4s      ← reply spoken aloud
   voice: 🎧 listening…                   ← mic reopens by itself
you:  what if I make it synchronous?
   …
```

The loop is closed by **playback finishing**, never by a timer: the microphone
opens only after the speakers go quiet, so the assistant can never transcribe
its own voice (the classic hands-free echo bug).

### Getting out

| How | What happens |
| --- | --- |
| **Start typing** | The mode pauses itself — you've taken the keyboard. (Type to resume: `/voice auto`.) |
| `/voice auto off` | Exits now, aborting the listen in flight |
| Silence | After `RPI_VOICE_AUTO_EMPTY_MAX` (3) empty turns it stops on its own |

### When it says "nothing heard"

The status carries the **measured input peak**, so the two very different causes
are distinguishable at a glance:

```
voice: 🎧 nothing heard (peak 0.180) — still listening (1/3)   ← mic works, no speech recognized
voice: 🎧 mic is silent (peak 0.000) — check the input device (1/3)   ← nothing reached the mic
```

A peak at (or near) `0.000` means the microphone captured nothing at all — a
muted device, or the wrong input selected. A healthy peak with an empty
transcription means the audio arrived but STT found no words in it.

#### 1. Which microphone is it even using?

`/voice status` names the active capture endpoint:

```
🎤 rpi-voice
  ...
  Input device: 麦克风 (USB Audio Device)
```

A machine with a headset *and* a webcam has several capture endpoints, and the
Windows default is often **not** the one you are talking into — so you get a
recording of the room while you speak into a device nobody is listening to. Pin
it explicitly (substring match, case-insensitive):

```bash
RPI_VOICE_INPUT_DEVICE=EDIFIER rpi     # talk into the headset
RPI_VOICE_INPUT_DEVICE="USB Audio" rpi
```

#### 2. Is the microphone actually delivering audio?

```bash
cargo test -p rpi-voice --lib -- --ignored --nocapture mic_probe
```

Lists every capture endpoint with its format, records 4 seconds from the one
that would be used, and reports what arrived — through the exact code path the
extension uses. **Speak while it runs.** It prints `peak` / `rms` / `speech` and
an explicit verdict.

#### 3. Is the level simply too low?

A microphone can be perfectly healthy and still deliver almost nothing: a Windows
input level left near the bottom, or a mic sitting across the desk. Measured in
the wild — a working USB microphone whose *speech* peaked at `0.011` of full
scale (≈ -39 dBFS), which is 10–40× below a healthy capture. At that level the
voice never rises above the room, so the VAD says "nobody is talking" and STT
has almost nothing to work with.

Check the per-second level profile first (`mic_probe_compares` below). If every
second is ≈ `0.01` or less, either raise the level in Windows
(**Settings → System → Sound → Input →** your mic **→ Volume**, plus any
"Microphone boost") or apply software gain:

```bash
RPI_VOICE_INPUT_GAIN=20 rpi
```

The boost is applied where the samples enter the recorder, so the meter, the VAD
and the STT input all see the same corrected signal — boosting after the VAD
would still leave the turn ending early. `/voice status` reports the gain that
was applied, so a level reading can always be read back against the raw capture.

#### 4. Bluetooth headsets need ~4 seconds to wake up

A Bluetooth headset's microphone is a **different profile (HFP)** from its music
playback (A2DP), and Windows takes **3–4 seconds** to switch when an app starts
capturing. A short probe therefore measures a device that has not woken yet and
wrongly concludes it is dead.

Measured on an EDIFIER W820NB with a 200-second capture, per second:

```
second:  1     2     3     4     5     6   ...  11    12    13    14
peak:   0.00  0.00  0.00  0.72  0.68  0.44 ...  0.73  0.76  0.75  0.74
```

Seconds 1–3 are digital silence; the microphone comes alive at second 4. The
same speaker, captured simultaneously on a laptop's webcam microphone, never
exceeded `0.18` — a fifth of the level, because it sits across the room.

So a Bluetooth headset is usually the **best** input, provided the wait is
tolerated: the microphone first gets a wake-up grace period (`RPI_VOICE_WARMUP_MS`,
default 4s), then auto keeps the stream open like PTT until speech is detected
and trailing silence ends the turn. Set `RPI_VOICE_NO_SPEECH_MS` only when an
explicit speech-start timeout is desired.

#### 5. Speech detection adapts to your mic

The VAD threshold is **not** a fixed level. It measures the ambient floor from
quiet frames and requires speech to be ~3× that (≈ +10 dB SNR), with a low
absolute minimum. A fixed threshold is what once made a perfectly good quiet
microphone (ambient RMS ~30 against a hard floor of 260) look completely dead:
every turn reported "heard nothing" while the hardware worked fine.

Push-to-talk keeps working while continuous mode is on. So does barge-in —
speaking over a reply or typing cuts the audio.

> Continuous mode needs replies to be **spoken** (that's the turn hand-off), so
> `/voice auto` turns auto-TTS back on if it was muted, and says so.

## Interrupting speech (barge-in)

Nothing is worse than an assistant that keeps reading while you talk over it,
so any sign that you are taking the floor cuts playback off instantly:

- **Start typing** — the moment the prompt draft changes, the voice stops. This
  needs no recording and is the usual way out of a long reply.
- **Hold the PTT key** — the hold threshold stops playback *before* the mic
  opens, so the speakers can never feed back into the recording.
- **`/voice`** — starting a dictation also stops playback, as do `/voice off`
  (mute) and `/voice stop`.

While speech plays, `voice:` in the footer becomes a music-style equalizer:

```
voice: ♪ ▂▅▇▅▂▁▃ playing 1.2s
```

The bar heights follow the **real audio envelope** — playback publishes the RMS
of every ~60ms chunk it hands to the audio device — so the bars move with what
you are actually hearing, and the crest ripples left → right like a level meter.

> Barge-in on typing relies on the host emitting an `editorChange` event. On an
> older host you still get push-to-talk and `/voice stop`; typing simply won't
> interrupt.

## Where the text goes

By default a transcription is **not** sent straight to the model — it is placed
in the prompt editor as an editable *draft*, with a countdown in the footer:

```
input: 你好，帮我看看这个 bug █
footer: Auto-send in 1.4s · any key to edit
```

- Leave it alone and it is submitted automatically after `RPI_VOICE_DRAFT_MS`
  (default **2s**).
- **Type anything** to cancel the countdown — the draft stays put and the key
you pressed becomes your first edit, so you can fix a mis-recognized word
  before it ever becomes a message. (Editing at all — typing, pasting,
  Backspace — also cancels it.)
- Press `Enter` to send early.
- Set `RPI_VOICE_DRAFT_MS=0` to disable the countdown entirely and always wait
  for `Enter`.

Because the draft is submitted through the editor's normal path, it appears in
the transcript as a genuine **user message**. The old immediate path
(`SendUserMessage`) did not render a user bubble at all.

Use `/voice output send` (or `RPI_VOICE_OUTPUT=send`) when you would rather skip
the draft entirely and send the moment you release the key — faster, but with
no chance to correct a mis-recognition.

The key is only claimed while the **input box is empty**, so space still types
normally whenever you have a draft. Chords (`Ctrl+Space`, `Alt+Space`) are left
to the editor. When PTT is off the key is not claimed at all.

This needs a host that routes keys to extensions (rpi's `register_shortcut` +
`Input` events); on an older host the command still works but the key is never
delivered, so use `/voice` instead.

## Notes / limitations

- Push-to-talk relies on the host delivering key **release** events, which the
  terminal must report (true for Windows conhost and modern terminal emulators).
  If presses register but releasing never ends the recording, the
  `RPI_VOICE_PTT_MAX_MS` cap still stops it.
- Interrupting on *typing* needs a host that emits `editorChange`; on an older
  host use push-to-talk or `/voice stop` instead.
- STT needs a reachable OpenAI-compatible endpoint: the hosted API (with a key),
  a free local server, or the embedded offline engine (`--features local-stt`).
- Edge TTS is an undocumented Microsoft endpoint; if it changes, synthesis can
  break and the failure is logged to stderr.
