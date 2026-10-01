//! rpi-voice — Voice conversation extension for rpi.
//!
//! Provides hybrid voice interaction:
//! - **Auto-TTS**: when enabled, assistant replies are queued and spoken aloud
//!   via Edge TTS; it is opt-in and disabled by default.
//! - **Voice input**: `/voice` records the microphone, transcribes via Whisper,
//!   and drops the text into the prompt editor as an editable draft (see
//!   *Output* below)
//! - **Push-to-talk**: `/voice ptt` turns a key (default `space`) into a
//!   hold-to-talk button — hold to open the mic, release to deliver. The host
//!   routes the key here through `register_shortcut` + `EventTag::Input`. The
//!   footer animates while you hold (countdown) and while the mic is open (a
//!   live level meter driven by the input level).
//!
//! ## Output (`SetEditorText` vs `SendUserMessage`)
//!
//! By default a transcription is delivered via the `SetEditorText` runtime
//! action: it lands in the prompt editor as a *draft* and the TUI auto-sends it
//! after a short countdown (`RPI_VOICE_DRAFT_MS`, default 2s). Typing anything
//! cancels the countdown and keeps the draft, so a mis-recognized word can be
//! fixed before the text enters the conversation. Because the draft is
//! submitted through the editor's normal path, it renders as a real user
//! message — which `SendUserMessage` does not (the harness emits no user
//! `message_start` for a directly-sent prompt).
//!
//! Set `RPI_VOICE_OUTPUT=send` (or `/voice output send`) to skip the editor and
//! send immediately instead.
//!
//! ## Continuous conversation (`/voice auto`)
//!
//! Hands-free, turn-based dialogue in the style of a voice assistant: you talk,
//! it answers out loud, then it listens again — no key, no `/voice`.
//!
//! The loop is closed by *playback finishing*, never by a timer:
//!
//! ```text
//! speak ─▶ transcribe ─▶ send ─▶ reply ─▶ speak aloud ─▶ listen ─▶ speak ─▶ …
//! ```
//!
//! That ordering is the whole design: because the microphone only opens after
//! the speakers have gone quiet, the assistant can never transcribe its own
//! voice (the classic hands-free echo bug).
//!
//! Getting out is as important as getting in:
//! - **Start typing** — you have taken the keyboard, so the mode pauses itself
//!   (the host marks such an edit `source:"user"`; the plugin's *own* injected
//!   transcription arrives as `source:"extension"` and must not count, or the
//!   loop would cancel itself every turn).
//! - **`/voice auto off`** — exits immediately, aborting the listen in flight.
//! - **Nothing heard** — a number you read as "still thinking" rather than
//!   "they left": the mic reopens up to `RPI_VOICE_AUTO_EMPTY_MAX` times (3),
//!   then the mode stops on its own rather than listening to an empty room.
//!
//! speech detection threshold is *adaptive*: it learns the ambient floor from
//! quiet frames and requires speech to be ~3× that, so a quiet microphone is not
//! mistaken for a dead one.
//!
//! ## Barge-in (interrupting speech)
//!
//! Nothing is more annoying than an assistant that keeps reading while you are
//! trying to talk over it, so any sign that the user is taking the floor cuts
//! playback off immediately:
//!
//! - **Typing** — the host emits `EventTag::EditorChange` whenever the prompt
//!   draft changes; the handler stops playback. This is the only signal that
//!   does not require actually speaking.
//! - **Push-to-talk** — reaching the hold threshold stops playback *before* the
//!   mic opens, so the speakers never feed back into the recording.
//! - **`/voice`** — starting a dictation stops playback; `/voice off` (mute)
//!   and `/voice stop` stop it too.
//!
//! While speech plays, `voice:` in the footer shows a music-style equalizer
//! (`voice: ♪ ▂▅▇▅▂▁▃ playing 1.2s`) whose bar heights follow the real audio
//! envelope — `player::play_mp3` publishes the RMS of each ~60ms chunk it hands
//! to the sink, so the bars track what is actually being heard rather than a
//! canned animation.
//!
//! Architecture:
//! - Event handler for `MessageEnd` → extract assistant text → sanitize markdown
//!   → enqueue on a single TTS worker thread
//! - Event handler for `Input` → push-to-talk press/release state machine
//! - Event handler for `EditorChange` → barge-in (stop speech when the user
//!   starts composing) + hands-free stand-down when the human takes over
//! - Command handler for `/voice` → record mic (silence auto-stop) → Whisper STT
//!   → `SetEditorText` (draft) or `SendUserMessage` (immediate)
//! - TTS worker → after a reply finishes playing, reopen the mic in
//!   continuous mode (the hand-off that makes a conversation hands-free)
//! - A dedicated worker thread owns playback so consecutive replies queue instead
//!   of being dropped, and event handlers never block the agent loop.
//!
//! Environment:
//! - `RPI_VOICE`               TTS voice name (default `zh-CN-XiaoxiaoNeural`)
//! - `RPI_STT_API_BASE`        Whisper-compatible STT base URL
//! - `OPENAI_API_KEY`          STT API key
//! - `RPI_VOICE_STT_LANG`      Optional ISO-639-1 language hint for STT
//! - `RPI_VOICE_RECORD_MS`     Hard recording cap (default 20000)
//! - `RPI_VOICE_SILENCE_MS`    Trailing silence that ends recording (default 1200, 0 = off)
//! - `RPI_VOICE_MIN_SPEECH_MS` Speech required before silence stop (default 500)
//! - `RPI_VOICE_PTT_KEY`       Push-to-talk key (default `space`)
//! - `RPI_VOICE_PTT_HOLD_MS`   Hold time before the mic opens (default 600)
//! - `RPI_VOICE_PTT_MAX_MS`    Safety cap on one PTT recording (default 60000)
//! - `RPI_VOICE_OUTPUT`        `draft` (default) or `send`
//! - `RPI_VOICE_DRAFT_MS`      Auto-send countdown for a draft (default 2000, 0 = manual)
//! - `RPI_VOICE_NO_SPEECH_MS`  Optional continuous-mode speech-start timeout
//!   (unset by default; auto waits like PTT until its recording cap)
//! - `RPI_VOICE_WARMUP_MS`     Bluetooth microphone wake-up grace period in
//!   continuous mode (default 4000)
//! - `RPI_VOICE_AUTO_EMPTY_MAX` Continuous mode: empty turns before it stops
//!   itself (default 3)
//! - `RPI_VOICE_INPUT_DEVICE`  Substring of the capture device to use (default:
//!   the system default)
//! - `RPI_VOICE_INPUT_GAIN`    Software input gain for a very quiet mic
//!   (default 1.0 = off, capped at 100)

mod edge_tts;
#[cfg(feature = "local-stt")]
mod local_stt;
mod player;
mod spoken_style;
mod recorder;
mod whisper;

use rpi_plugin_sdk::{
    register_entrypoint, EventTag, FreeStringFn, PluginApi, RuntimeActionFn, RuntimeActionId,
    StablePluginEvent, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Runtime context (captured at registration, used by event/command handlers)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct RuntimeContext {
    runtime_action: RuntimeActionFn,
    free_string: FreeStringFn,
    user_data: *mut c_void,
}

// SAFETY: user_data is valid for the session lifetime; the fn pointers have no
// thread affinity. The host may call handlers from any thread.
unsafe impl Send for RuntimeContext {}
unsafe impl Sync for RuntimeContext {}

static RUNTIME_CTX: OnceLock<RuntimeContext> = OnceLock::new();

impl RuntimeContext {
    /// Invoke a host runtime action, always reclaiming the host-owned output.
    fn action(&self, id: RuntimeActionId, args: Value) -> Result<String, String> {
        let args_json = args.to_string();
        let mut output = StbString::empty();
        let rc = (self.runtime_action)(
            id as u32,
            StbStringRef::from_str(&args_json),
            &mut output,
            self.user_data,
        );
        let out_text = output.to_string_lossy();
        (self.free_string)(output);
        if rc != 0 {
            return Err(out_text);
        }
        Ok(out_text)
    }

    /// Publish a short status string for the host TUI footer (best effort).
    fn set_status(&self, value: &str) {
        let _ = self.action(
            RuntimeActionId::SetStatus,
            json!({"key": "voice", "value": value}),
        );
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Auto-TTS enabled flag (default: false; opt in with `/voice on` or `RPI_VOICE_AUTO_TTS=on`)
static AUTO_TTS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Currently playing TTS (to avoid overlapping playback / drive status)
static TTS_PLAYING: AtomicBool = AtomicBool::new(false);

/// Why the last utterance failed to play, if it did.
///
/// Playback failures used to go to `eprintln!`, which is invisible inside the
/// TUI's alternate screen — the user saw "no sound" and nothing else. Keeping
/// the message here lets `/voice status` explain the silence.
static LAST_TTS_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Loudness of the audio currently being played, as `f32` bits (see
/// [`player::play_mp3`]). `0` whenever nothing is playing. Drives the
/// music-style equalizer animation next to `voice:` in the footer.
static PLAYBACK_LEVEL: AtomicU32 = AtomicU32::new(0);

/// Stop flag of the utterance currently on the speakers, so *any* thread can
/// cut it short — this is the handle barge-in pulls. `None` when silent.
static PLAYBACK_STOP: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Bumped by every barge-in, so a queued utterance can tell it was superseded.
///
/// The TTS worker speaks one queued reply at a time, and a reply that arrives
/// while another is being read is *queued*, not dropped. Cutting off only the
/// utterance on the speakers would therefore leave the queue intact and the
/// worker would start the next one a moment later — reading aloud into the very
/// microphone the user just opened. Stamping each job with the generation it was
/// queued in, and skipping jobs from an older one, is what makes a barge-in
/// actually reach the queue.
static PLAYBACK_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Hands-free ("continuous conversation") mode: once a reply finishes playing,
/// the mic reopens on its own, so the user never touches a key.
static AUTO_TALK_ENABLED: AtomicBool = AtomicBool::new(false);

/// Consecutive hands-free turns that heard nothing. Guards against holding the
/// microphone open after the user has walked away.
static AUTO_TALK_EMPTY: AtomicUsize = AtomicUsize::new(0);

/// Stop flag of the in-flight hands-free recording, so `/voice auto off` can
/// cut a listen short instead of waiting out its no-speech window.
static AUTO_TALK_STOP: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Error message used when a recording contained no usable speech. Shared with
/// [`is_empty_turn`] so the two can never drift apart.
const ERR_NO_SPEECH: &str = "no speech detected";
/// Error message for a recording too brief to transcribe.
const ERR_TOO_SHORT: &str = "recording too short";

/// Peak input level of the most recent recording (f32 bits) and how long its
/// VAD classified as speech. Published by [`finish_recording`] so the
/// hands-free loop can say *why* a turn came up empty.
static LAST_PEAK_LEVEL: AtomicU32 = AtomicU32::new(0);
static LAST_SPEECH_MS: AtomicUsize = AtomicUsize::new(0);
/// Length of the last recording in ms. Published alongside the peak because
/// `peak 0.0000` means two completely different things at 0.2s (the turn was cut
/// short) and at the full wait window (the device really did deliver silence).
static LAST_DURATION_MS: AtomicUsize = AtomicUsize::new(0);
/// Software input gain of the last recording (f32 bits), so the level in
/// [`LAST_PEAK_LEVEL`] can be read back against the raw capture.
static LAST_GAIN: AtomicU32 = AtomicU32::new(0);
/// Share of the last recording's samples that the gain pushed past full scale
/// (f32 bits). A high value means the *gain* destroyed the audio, not the mic.
static LAST_CLIPPED: AtomicU32 = AtomicU32::new(0);

/// Below this peak the capture was effectively digital silence — a muted device
/// or the wrong endpoint — rather than merely a quiet microphone.
///
/// Must stay well below a quiet-but-working mic: a real USB microphone idling in
/// a quiet room measures ~0.008–0.02 here. An earlier value of `0.02` sat right
/// on top of that, so a perfectly healthy device was reported as "silent".
const DEAF_PEAK_LEVEL: f32 = 0.0005;

/// Below this the mic is live but has only ever heard the room — i.e. almost
/// certainly *not* the device the user is speaking into. Distinct from both
/// "nothing reached the microphone" and "the mic hears you but STT found no
/// words", because the fix is different in each case.
const ROOM_NOISE_PEAK_LEVEL: f32 = 0.05;

/// Append a line to the hands-free trace when `RPI_VOICE_DEBUG` is set.
///
/// Writes to a file rather than stderr on purpose: the TUI owns an alternate
/// screen, so a stray `eprintln!` from a background voice thread corrupts the
/// display. The path is printed at `/voice auto` time so it is findable.
fn debug_log(message: &str) {
    if std::env::var("RPI_VOICE_DEBUG").is_err() {
        return;
    }
    use std::io::Write;
    let path = std::env::temp_dir().join("rpi-voice-debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{:?} {message}", std::time::SystemTime::now());
    }
}

/// Path of the hands-free trace (so `/voice auto` can report where it went).
fn debug_log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("rpi-voice-debug.log")
}

/// How many empty hands-free turns in a row end the mode (default 3).
fn auto_talk_max_empty() -> usize {
    std::env::var("RPI_VOICE_AUTO_EMPTY_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(3)
}

/// Whether a failed turn should count as "the user wasn't there".
///
/// A turn cut short because we were told to stand down is **not** an empty turn.
/// Counting it would burn a strike — and eventually stop the mode — purely
/// because the user typed something, and the truncated recording (a fraction of
/// a second, peak `0.0000`) would be reported as a microphone fault it cannot
/// possibly diagnose. That false alarm sent a user hunting for a dead device
/// while their microphone was working fine.
fn counts_as_empty_turn(error: &str, asked_to_stop: bool, mode_on: bool) -> bool {
    mode_on && !asked_to_stop && is_empty_turn(error)
}

/// Whether a failed turn failed because the user simply was not there, as
/// opposed to a real error (mic, network, model). Only the former should cost
/// an empty-turn strike.
///
/// Matches on the sentinel *prefix* rather than the whole string: callers decorate
/// the message with diagnostics ("… — gain x20 clipped 40% of the samples"), and an
/// exact match would silently reclassify a retryable empty turn as a fatal error,
/// killing the session instead of trying again. A test pins that.
fn is_empty_turn(error: &str) -> bool {
    error.starts_with(ERR_NO_SPEECH) || error.starts_with(ERR_TOO_SHORT)
}

/// Leave hands-free mode because the user took the keyboard. Silent unless the
/// mode was actually on, so ordinary typing never produces a status line.
/// Returns `true` when the mode was switched off.
fn pause_auto_talk(reason: &str) -> bool {
    if !AUTO_TALK_ENABLED.swap(false, Ordering::Relaxed) {
        return false;
    }
    debug_log(&format!("continuous mode paused: {reason}"));
    // Abort a listen that may be running right now, otherwise the user's typing
    // would be transcribed and delivered after they took over.
    let stop = AUTO_TALK_STOP.lock().unwrap().take();
    if let Some(stop) = stop {
        stop.store(true, Ordering::Relaxed);
    }
    if let Some(runtime) = RUNTIME_CTX.get().copied() {
        runtime.set_status(&format!("voice: ✋ {reason} — continuous mode paused"));
    }
    true
}

/// Interrupt whatever is being spoken **and** discard anything still queued.
/// Safe to call from any thread and when nothing is playing.
///
/// Returns `true` when there was something to stop, which the caller uses to
/// decide whether to report it.
///
/// Draining the queue is the point: a reply that arrived while another was
/// playing sits in the channel, and stopping only the audible utterance would
/// let the worker start the queued one seconds later — audibly ignoring the
/// interrupt. The generation bump is what tells the worker those jobs are stale
/// (see [`PLAYBACK_GENERATION`]); the playing utterance stops via its own flag.
fn stop_playback() -> bool {
    PLAYBACK_GENERATION.fetch_add(1, Ordering::Relaxed);
    let flag = PLAYBACK_STOP.lock().unwrap().take();
    match flag {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            PLAYBACK_LEVEL.store(0f32.to_bits(), Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Currently recording voice input
static RECORDING: AtomicBool = AtomicBool::new(false);

/// Whether the user selected hands-free mode for this session. This is separate
/// from AUTO_TALK_ENABLED: typing can pause the current listener while keeping
/// the long-press resume entry available.
static AUTO_MODE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Push-to-talk mode (`/voice ptt`). While on, the host routes the claimed key
/// (`ptt_key()`) to [`on_input_key`] instead of the editor when the input box is
/// empty, so holding the key records and releasing it sends.
static PTT_ENABLED: AtomicBool = AtomicBool::new(false);

/// Push-to-talk session state (one mutex guards all of it).
struct Ptt {
    /// When the key went down (`None` = not held). Cleared on release, so the
    /// hold-timer thread can tell whether the key is *still* held.
    pressed_at: Option<std::time::Instant>,
    /// The `token` of the press that owns the current `pressed_at`. Lets a
    /// finishing recording thread clear the held-state only when it still
    /// belongs to its own press (an overlapping newer press must be left alone).
    pressed_token: Option<u64>,
    /// Identifies the current press; bumped on release so a pending hold-timer
    /// thread knows its press is over and must not start a recording.
    token: u64,
    /// Stop flag of the in-flight recording (`None` = not recording yet).
    stop: Option<Arc<AtomicBool>>,
    /// The `token` of the press that owns the in-flight recording. Lets a
    /// finishing recording thread detect that a *newer* press has since taken
    /// over, so it does not clear that press's state.
    recording_owner: Option<u64>,
    /// Tokens whose recording should be **sent** on completion (the release
    /// handler adds its press's token). Keyed by press rather than a single
    /// bool, so a release's intent can't leak into the next press when the two
    /// utterances overlap.
    send_tokens: Vec<u64>,
}

static PTT: Mutex<Ptt> = Mutex::new(Ptt {
    pressed_at: None,
    pressed_token: None,
    token: 0,
    stop: None,
    recording_owner: None,
    send_tokens: Vec::new(),
});

/// The key that drives push-to-talk (normalized name, e.g. `space`).
fn ptt_key() -> String {
    std::env::var("RPI_VOICE_PTT_KEY")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "space".to_string())
}

/// Default hold time (ms) before the mic opens. Short enough that speaking
/// feels immediate, long enough that a stray space still counts as typing.
const PTT_HOLD_DEFAULT_MS: u64 = 600;

/// How long the key must be held to enter speaking mode (ms). A shorter hold is
/// treated as ordinary typing and never touches the microphone.
fn ptt_hold_ms() -> u64 {
    std::env::var("RPI_VOICE_PTT_HOLD_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(PTT_HOLD_DEFAULT_MS)
}

/// Hard cap on a single push-to-talk recording (ms) — release normally ends it.
fn ptt_max_ms() -> u64 {
    std::env::var("RPI_VOICE_PTT_MAX_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v >= 1000)
        .unwrap_or(60_000)
}

/// Where a finished transcription goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    /// Drop it into the prompt editor as an *editable draft*. The TUI submits
    /// it once the auto-send countdown elapses untouched, so a mis-recognized
    /// word can still be fixed — and because it is submitted through the
    /// editor's normal path, it renders as a real user message.
    Draft,
    /// Send it straight to the model, no chance to edit.
    Send,
}

/// Session override set by `/voice output …`; `None` defers to the environment.
static OUTPUT_OVERRIDE: Mutex<Option<OutputMode>> = Mutex::new(None);

/// How a transcription is delivered (default: editable draft).
fn output_mode() -> OutputMode {
    if let Some(mode) = *OUTPUT_OVERRIDE.lock().unwrap() {
        return mode;
    }
    match std::env::var("RPI_VOICE_OUTPUT")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("send" | "direct" | "message") => OutputMode::Send,
        _ => OutputMode::Draft,
    }
}

/// Auto-send countdown applied to a draft (ms). `0` leaves it in the editor
/// until the user presses Enter themselves.
fn draft_auto_send_ms() -> u64 {
    std::env::var("RPI_VOICE_DRAFT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(2000)
}

/// Human-readable description of the current output mode (for `/voice`).
fn output_mode_label() -> String {
    match output_mode() {
        OutputMode::Send => "send (straight to the model)".to_string(),
        OutputMode::Draft => {
            let ms = draft_auto_send_ms();
            if ms == 0 {
                "draft (input box, Enter to send)".to_string()
            } else {
                format!("draft (input box, auto-send in {}s)", ms as f64 / 1000.0)
            }
        }
    }
}

/// Session-level TTS voice override (set via `/voice set <name>`)
static VOICE_OVERRIDE: Mutex<Option<String>> = Mutex::new(None);

/// Which STT engine to use: `local` (default), `auto`, or `api`.
fn stt_engine_pref() -> String {
    std::env::var("RPI_STT_ENGINE")
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_string())
}

/// Start loading the local STT model in the background.
///
/// Called whenever voice is switched on (or the user starts talking) so the
/// ~5.5 s model load overlaps with the user speaking rather than following it.
/// No-op when the local engine is not compiled in or an API engine is pinned.
fn preload_stt_model() {
    #[cfg(feature = "local-stt")]
    {
        if stt_engine_pref() != "api" {
            local_stt::preload_in_background();
        }
    }
}

/// Single TTS worker queue — replies enqueue here instead of racing threads.
/// A queued utterance: the text, and the playback generation it was queued in.
///
/// Carrying the generation lets the worker drop replies a barge-in arrived
/// after — see [`PLAYBACK_GENERATION`].
struct SpeechJob {
    text: String,
    generation: u64,
}

static TTS_TX: OnceLock<mpsc::Sender<SpeechJob>> = OnceLock::new();

fn tts_sender() -> &'static mpsc::Sender<SpeechJob> {
    TTS_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<SpeechJob>();
        std::thread::Builder::new()
            .name("rpi-voice-tts".to_string())
            .spawn(move || {
                for job in rx {
                    // A barge-in since this was queued means the user is talking,
                    // or already said something else. Speaking it now would talk
                    // over them (and into the open microphone).
                    let job_generation = job.generation;
                    if job_generation != PLAYBACK_GENERATION.load(Ordering::Relaxed) {
                        debug_log("tts job dropped: superseded by a barge-in");
                        continue;
                    }
                    let text = job.text;
                    if !AUTO_TTS_ENABLED.load(Ordering::Relaxed) {
                        // Muted: nothing to speak, but hands-free mode still runs
                        // on the *voice* switch alone, so keep the loop alive.
                        // Reopen only when nothing was muted *for* this job
                        // (`maybe_auto_listen` checks the mode itself).
                        if let Some(runtime) = RUNTIME_CTX.get().copied() {
                            maybe_auto_listen(runtime);
                        }
                        continue;
                    }
                    // Mic first: if the user is talking, this reply is stale —
                    // speaking it would be read back as their own words.
                    if mic_owns_the_floor() {
                        debug_log("tts job dropped: the microphone owns the floor");
                        continue;
                    }
                    TTS_PLAYING.store(true, Ordering::Relaxed);
                    let outcome = synthesize_and_play(&text);
                    if let Err(e) = outcome {
                        // Silence is the symptom the user actually sees, so say
                        // why: stderr is hidden behind the TUI's alternate screen.
                        eprintln!("[rpi-voice] TTS error: {e}");
                        debug_log(&format!("tts error: {e}"));
                        *LAST_TTS_ERROR.lock().unwrap() = Some(e.clone());
                        if let Some(runtime) = RUNTIME_CTX.get().copied() {
                            runtime.set_status(&format!("voice: ⚠ speech failed — {e}"));
                        }
                    } else {
                        *LAST_TTS_ERROR.lock().unwrap() = None;
                    }
                    TTS_PLAYING.store(false, Ordering::Relaxed);
                    // Reopen the mic only if nothing interrupted this utterance.
                    // A barge-in (a keystroke, or the user taking the key to
                    // talk) bumps the generation and already owns the floor: PTT
                    // opens the mic itself, and typing pauses hands-free mode. A
                    // reopen here would fight either of those, which is the
                    // "interrupt did not take" symptom.
                    let superseded = job_generation != PLAYBACK_GENERATION.load(Ordering::Relaxed);
                    if superseded {
                        debug_log("auto-listen skipped: a barge-in took the floor");
                    } else if let Some(runtime) = RUNTIME_CTX.get().copied() {
                        maybe_auto_listen(runtime);
                    }
                }
            })
            .expect("spawn rpi-voice TTS worker");
        tx
    })
}

/// Current TTS voice: session override, else `RPI_VOICE`, else the default.
fn current_voice() -> String {
    if let Some(v) = VOICE_OVERRIDE.lock().unwrap().as_ref() {
        return v.clone();
    }
    std::env::var("RPI_VOICE").unwrap_or_else(|_| "zh-CN-XiaoxiaoNeural".to_string())
}

// ---------------------------------------------------------------------------
// Event handler: MessageEnd → enqueue auto TTS
// ---------------------------------------------------------------------------

extern "C" fn on_message_end(event: StablePluginEvent, _user_data: *mut c_void) -> i32 {
    if !AUTO_TTS_ENABLED.load(Ordering::Relaxed) {
        return 0;
    }

    // MessageStart/Update/End carry their payload in the `message` union arm
    // (not `data`). Both arms are a single StbString so reading `data` aliases,
    // but the tag-correct field is `message`.
    let message_json = unsafe { event.payload.message.message.to_string_lossy() };
    if message_json.is_empty() {
        return 0;
    }

    let message: Value = match serde_json::from_str(&message_json) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    // Only assistant messages; `role` is the base-message field, `kind` the
    // AgentMessage tag — accept either.
    let role = message
        .get("role")
        .or_else(|| message.get("kind"))
        .and_then(|r| r.as_str())
        .unwrap_or("");
    if role != "assistant" {
        return 0;
    }

    let text = sanitize_for_speech(&extract_message_text(&message));
    if text.chars().count() < 3 {
        return 0;
    }

    // Queue (never drop) — the worker speaks one reply at a time. Stamp it with
    // the current generation so a barge-in arriving first discards it.
    let job = SpeechJob {
        text,
        generation: PLAYBACK_GENERATION.load(Ordering::Relaxed),
    };
    if tts_sender().send(job).is_err() {
        eprintln!("[rpi-voice] TTS worker unavailable");
    }

    0 // Continue event dispatch
}

// ---------------------------------------------------------------------------
// Push-to-talk: Input event handler
// ---------------------------------------------------------------------------

/// Handle a key event routed by the host. Only fires for keys this extension
/// declared via `register_shortcut`, and only while the input box is empty.
///
/// Hold-to-talk model, active while `/voice ptt` is on:
///
/// - **press** — arm a timer and claim the key, so it never reaches the editor.
/// - after `ptt_hold_ms()` still held — open the mic and start recording.
/// - **release** — stop recording and send; a release *before* the threshold
///   sends nothing (the press was claimed, so it is not typed either — while
///   PTT is on the key is dedicated to talking, matching "进入 voice 模式").
///
/// Returns [`EVENT_HANDLER_CLAIMED`] when it took the key, and
/// [`EVENT_HANDLER_CONTINUE`] when PTT is off (or the key is not ours), so a
/// registered-but-disabled shortcut never steals the key from the editor.
///
/// Recording runs on its own thread: the host calls this on the TUI key thread,
/// which must return promptly to stay responsive.
extern "C" fn on_input_key(event: StablePluginEvent, _user_data: *mut c_void) -> i32 {
    const CONTINUE: i32 = rpi_plugin_sdk::EVENT_HANDLER_CONTINUE;

    if !PTT_ENABLED.load(Ordering::Relaxed) && !AUTO_MODE_ENABLED.load(Ordering::Relaxed) {
        return CONTINUE;
    }
    // Payload lives in the `data` union arm for Input events.
    let raw = unsafe { event.payload.data.data.to_string_lossy() };
    handle_ptt_key(&raw, &ptt_key())
}

/// The user is composing ⇒ stop reading the previous answer aloud (barge-in);
/// in hands-free mode, stand down entirely — they have taken the keyboard, so
/// the mic must stop opening on its own.
///
/// A change the TUI made on our behalf (`source == "extension"`: our own
/// injected transcription, or the host clearing the draft after auto-sending
/// it) is deliberately *not* treated as the user taking over — otherwise the
/// hands-free loop would cancel itself on every single turn.
extern "C" fn on_editor_change(event: StablePluginEvent, _user_data: *mut c_void) -> i32 {
    let raw = unsafe { event.payload.data.data.to_string_lossy() };
    handle_editor_change(&raw)
}

/// Pure `EditorChange` policy (parses the payload, so it is unit-testable
/// without an FFI event).
fn handle_editor_change(raw: &str) -> i32 {
    const CONTINUE: i32 = rpi_plugin_sdk::EVENT_HANDLER_CONTINUE;

    // A missing `source` is treated as the user: the conservative reading, since
    // getting it wrong only stops a voice, never edits the draft.
    let parsed = serde_json::from_str::<Value>(raw).ok();
    let from_extension = parsed
        .as_ref()
        .and_then(|v| v.get("source").and_then(Value::as_str))
        .is_some_and(|source| source == "extension");
    if from_extension {
        debug_log("editor change from the extension (ignored)");
        return CONTINUE;
    }
    // Executing a slash command normally clears the editor after the command
    // has been dispatched. That emptying is not the user taking over the
    // conversation; treating it as typing immediately cancels `/voice auto`.
    let editor_empty = parsed
        .as_ref()
        .and_then(|v| v.get("empty").and_then(Value::as_bool))
        .unwrap_or(false);
    if editor_empty {
        debug_log("editor cleared after command (ignored)");
        return CONTINUE;
    }
    debug_log("editor change from the user — barge-in");
    stop_playback();
    pause_auto_talk("you started typing");
    CONTINUE
}

/// Pure push-to-talk key dispatch: parse the host's key payload and act on it.
/// Split out from the FFI handler so the routing rules are unit-testable
/// (no live registry, no recording thread).
///
/// Returns the handler code the host expects
/// ([`rpi_plugin_sdk::EVENT_HANDLER_CLAIMED`] to swallow the key,
/// [`rpi_plugin_sdk::EVENT_HANDLER_CONTINUE`] to pass it to the editor).
fn handle_ptt_key(raw: &str, wanted: &str) -> i32 {
    const CONTINUE: i32 = rpi_plugin_sdk::EVENT_HANDLER_CONTINUE;
    const CLAIMED: i32 = rpi_plugin_sdk::EVENT_HANDLER_CLAIMED;

    if !PTT_ENABLED.load(Ordering::Relaxed) && !AUTO_MODE_ENABLED.load(Ordering::Relaxed) {
        return CONTINUE;
    }
    let Ok(payload) = serde_json::from_str::<Value>(raw) else {
        return CONTINUE;
    };
    if payload.get("type").and_then(Value::as_str) != Some("key") {
        return CONTINUE;
    }
    if payload.get("key").and_then(Value::as_str) != Some(wanted) {
        return CONTINUE;
    }
    // Modifier chords stay available for the editor (Ctrl+Space etc.).
    if payload
        .get("ctrl")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || payload.get("alt").and_then(Value::as_bool).unwrap_or(false)
    {
        return CONTINUE;
    }

    let kind = payload.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "press" => {
            let mut ptt = PTT.lock().unwrap();
            if ptt.pressed_at.is_some() {
                return CLAIMED; // already held (stray duplicate press)
            }
            ptt.pressed_at = Some(std::time::Instant::now());
            let token = next_ptt_token();
            ptt.pressed_token = Some(token);
            ptt.token = token;
            drop(ptt);
            arm_ptt_start(token, ptt_hold_ms());
            CLAIMED
        }
        // Held keys repeat; the timer is already armed, just swallow them.
        "repeat" => CLAIMED,
        "release" => {
            let mut ptt = PTT.lock().unwrap();
            // A release with no matching press (e.g. pressed before PTT was
            // enabled) is not ours.
            if ptt.pressed_at.take().is_none() {
                return CONTINUE;
            }
            // Bump the token so a still-sleeping arm thread won't start a
            // recording after this release. Drawn from the same monotonic
            // counter as presses so the two can never collide.
            let released_token = ptt.token;
            ptt.token = next_ptt_token();
            ptt.pressed_token = None;
            if let Some(stop) = ptt.stop.take() {
                // Record this press's send intent. Keyed by token so it cannot
                // leak into a later press if the teardown overlaps one.
                ptt.send_tokens.push(released_token);
                stop.store(true, Ordering::Relaxed);
            }
            CLAIMED
        }
        _ => CONTINUE,
    }
}

/// Monotonic token identifying the current press. Bumped on release so a
/// pending hold-timer thread knows its press is over.
fn next_ptt_token() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Wait out the hold threshold, then open the mic if the same press is still
/// active. Runs on its own thread so the key handler returns immediately.
///
/// Shows a countdown while the key is held so the user gets immediate feedback
/// that they are entering speaking mode — without it, the 2-second hold is dead
/// air and there is no way to know a press registered at all.
fn arm_ptt_start(token: u64, hold_ms: u64) {
    let Some(runtime) = RUNTIME_CTX.get().copied() else {
        return;
    };
    std::thread::Builder::new()
        .name("rpi-voice-ptt-arm".to_string())
        .spawn(move || {
            let started = std::time::Instant::now();
            // Step the countdown at the host's status-poll interval so every
            // frame has a chance to paint.
            loop {
                let elapsed = started.elapsed().as_millis() as u64;
                // Still held, and still the same press?
                let (ours, nothing_held) = {
                    let ptt = PTT.lock().unwrap();
                    let ours = ptt.pressed_at.is_some() && ptt.token == token && ptt.stop.is_none();
                    (ours, ptt.pressed_at.is_none())
                };
                if !ours {
                    // Released mid-hold, or superseded by a newer press.
                    //
                    // Only clear the footer when nothing is held at all: if a
                    // newer press already took over, its own countdown owns the
                    // status line and this stale thread must not stomp it.
                    if nothing_held {
                        runtime.set_status("voice: idle");
                    }
                    return;
                }
                if elapsed >= hold_ms {
                    break;
                }
                runtime.set_status(&hold_status(elapsed, hold_ms));
                std::thread::sleep(std::time::Duration::from_millis(LISTENING_FRAME_MS));
            }
            start_ptt_recording(token, runtime);
        })
        .ok();
}

/// The "getting ready to listen" line shown while the key is held but the mic
/// is not open yet. A filling bar (plus the remaining tenths) makes the hold
/// feel responsive and tells the user to keep holding.
fn hold_status(elapsed_ms: u64, hold_ms: u64) -> String {
    const WIDTH: usize = 12;
    let progress = if hold_ms == 0 {
        1.0
    } else {
        (elapsed_ms as f32 / hold_ms as f32).clamp(0.0, 1.0)
    };
    let filled = ((progress * WIDTH as f32).round() as usize).min(WIDTH);
    let bar: String = "▰".repeat(filled) + &"▱".repeat(WIDTH - filled);
    let remaining = hold_ms.saturating_sub(elapsed_ms) as f64 / 1000.0;
    format!("voice: ⏳ hold {bar} {remaining:.1}s to talk")
}

/// Open the mic for a push-to-talk utterance. Ends on key release (the release
/// handler sets the stop flag) or at `ptt_max_ms()` as a stuck-key safety cap.
fn start_ptt_recording(token: u64, runtime: RuntimeContext) {
    if RECORDING.load(Ordering::Relaxed) {
        return;
    }
    // The hold threshold was reached: the user is about to speak, so cut off
    // whatever the assistant is still reading aloud (barge-in). A long press
    // also resumes a paused Auto session.
    if AUTO_MODE_ENABLED.load(Ordering::Relaxed) {
        AUTO_TALK_ENABLED.store(true, Ordering::Relaxed);
    }
    preload_stt_model();
    stop_playback();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut ptt = PTT.lock().unwrap();
        // The press ended while we were starting up — don't record at all.
        if ptt.pressed_at.is_none() || ptt.token != token || ptt.stop.is_some() {
            return;
        }
        ptt.stop = Some(stop.clone());
        ptt.recording_owner = Some(token);
    }

    RECORDING.store(true, Ordering::Relaxed);
    // Publish the meter's first frame so the line is correct even if the
    // animation thread fails to spawn below.
    runtime.set_status(&listening_status(0.0, 0, ListenHint::Release, None, 0));
    // Drive a "listening" animation from the live input level. The host repaints
    // only when this status string actually changes, so the watcher steps a bar
    // whose width tracks the microphone — the user sees the meter move as they
    // speak, and a still bar means the mic isn't picking anything up.
    let meter = recorder::LevelMeter::new();
    let animation = start_listening_animation(runtime, meter.clone(), ListenHint::Release, None, 0);

    std::thread::Builder::new()
        .name("rpi-voice-ptt".to_string())
        .spawn(move || {
            let params = recorder::RecordParams {
                // Release normally ends this; the cap only guards a stuck key.
                max_ms: ptt_max_ms(),
                // Never auto-stop on silence: the user decides by releasing.
                silence_ms: 0,
                min_speech_ms: 0,
                // Release is the only stop condition; no grace window applies.
                no_speech_ms: None,
                warmup_ms: 0,
            };
            let result = recorder::record_until_with_level(stop, params, Some(meter));
            RECORDING.store(false, Ordering::Relaxed);
            // Stop the animation before the transcribing status is published, so
            // the two writers can't fight over the key.
            if let Some(animation) = animation {
                animation.stop();
            }

            // Read the intent the release handler recorded, then clear state.
            // Ownership-checked: if a newer press started while this recording
            // was tearing down, leave its state alone.
            let send = ptt_finish(token);

            match result {
                Ok(rec) if send => match finish_recording(&runtime, rec, false) {
                    Ok(()) => set_status_unless_pressed(&runtime, "voice: idle"),
                    Err(e) => {
                        eprintln!("[rpi-voice] PTT error: {e}");
                        set_status_unless_pressed(&runtime, &format!("voice: ⚠ {e}"));
                    }
                },
                // Released under the minimum, or the cap fired: nothing to send.
                Ok(_) => set_status_unless_pressed(&runtime, "voice: idle"),
                Err(e) => {
                    eprintln!("[rpi-voice] PTT recording error: {e}");
                    set_status_unless_pressed(&runtime, &format!("voice: ⚠ {e}"));
                }
            }
        })
        .ok();
}

/// Publish a terminal status only when no press is in flight, so a finishing
/// recording cannot wipe the countdown of a newer press the user started while
/// this one was still tearing down.
fn set_status_unless_pressed(runtime: &RuntimeContext, value: &str) {
    let pressed = PTT.lock().unwrap().pressed_at.is_some();
    if !pressed {
        runtime.set_status(value);
    }
}

/// Clear all push-to-talk state (used by tests and the `/voice ptt off` path),
/// returning whether the pending recording should be sent.
fn ptt_reset() -> bool {
    let mut ptt = PTT.lock().unwrap();
    let send = !ptt.send_tokens.is_empty();
    ptt.pressed_at = None;
    ptt.pressed_token = None;
    ptt.token = next_ptt_token();
    ptt.stop = None;
    ptt.recording_owner = None;
    ptt.send_tokens.clear();
    send
}

/// Finish a recording owned by `token`, returning whether to send it.
///
/// Ownership-checked: only state that still belongs to *this* press is cleared.
/// When an overlapping newer press has already taken over, its `pressed_at` is
/// left intact — otherwise the user's eventual release would find no press
/// recorded and fall through to the editor as a typed space.
fn ptt_finish(token: u64) -> bool {
    let mut ptt = PTT.lock().unwrap();
    // Consume this recording's own send intent (never a later press's).
    let send = if let Some(idx) = ptt.send_tokens.iter().position(|t| *t == token) {
        ptt.send_tokens.remove(idx);
        true
    } else {
        false
    };
    if ptt.recording_owner == Some(token) {
        ptt.stop = None;
        ptt.recording_owner = None;
    }
    // Only clear the held-state if this press still owns it.
    if ptt.pressed_token == Some(token) {
        ptt.pressed_at = None;
        ptt.pressed_token = None;
    }
    send
}

/// Handle to a running "listening" animation.
pub struct ListeningAnimation {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ListeningAnimation {
    /// Stop the animation and join its thread before another status writer
    /// publishes a follow-up message. Joining also prevents stale animation
    /// frames from a previous turn racing with the next recording.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How a "listening" turn ends, so one meter can serve every recording path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenHint {
    /// Push-to-talk: the user decides by releasing the key.
    Release,
    /// Hands-free / one-shot `/voice`: trailing silence ends the turn.
    Silence,
}

impl ListenHint {
    #[allow(dead_code)] // kept for symmetry with the enum's docs
    fn trailer(self) -> &'static str {
        match self {
            ListenHint::Release => "release to send",
            ListenHint::Silence => "silence ends the turn",
        }
    }
}

/// Render the listening status line for a given input `level` (0.0..=1.0) and
/// elapsed hold time. Split out so the formatting is unit-testable.
///
/// The bar width tracks the live microphone level; when the mic is silent the
/// bar stays at its floor, which is the user's cue that they need to speak up.
/// Split into `WIDTH` cells rather than one solid run so small level changes are
/// visible — a bar that never moves is the signal that the mic is not hearing
/// anything.
fn listening_status(
    level: f32,
    elapsed_ms: u64,
    hint: ListenHint,
    start_window_ms: Option<u64>,
    warmup_ms: u64,
) -> String {
    const WIDTH: usize = 12;
    let filled = ((level.clamp(0.0, 1.0)) * WIDTH as f32).round() as usize;
    // Always show one cell so the line never looks broken/dead.
    let filled = filled.max(1).min(WIDTH);
    let bar: String = "█".repeat(filled) + &"░".repeat(WIDTH - filled);
    // Say exactly how long there is left to *start* talking. A hands-free turn
    // that silently expires reads as "it did not hear me"; a visible countdown
    // turns that into "I was too late", which is actionable.
    let trailer = match (hint, warmup_ms, start_window_ms) {
        (ListenHint::Silence, warmup, _) if warmup > 0 && elapsed_ms < warmup => {
            "warming microphone".to_string()
        }
        (ListenHint::Silence, warmup, _) if warmup > 0 => "speak naturally".to_string(),
        (ListenHint::Release, _, _) => "release to send".to_string(),
        (ListenHint::Silence, _, Some(window)) if elapsed_ms < window => format!(
            "speak to start ({:.1}s left)",
            (window - elapsed_ms) as f64 / 1000.0
        ),
        (ListenHint::Silence, _, _) => "silence ends the turn".to_string(),
    };
    format!(
        "voice: 🎤 listening {bar} {:.1}s — {trailer}",
        elapsed_ms as f64 / 1000.0
    )
}

/// Animate the `voice` status while the mic is open, sampling `meter` every
/// `LISTENING_FRAME_MS`. Returns `None` if the thread could not be spawned
/// (the recording still works; it just won't animate).
fn start_listening_animation(
    runtime: RuntimeContext,
    meter: recorder::LevelMeter,
    hint: ListenHint,
    start_window_ms: Option<u64>,
    warmup_ms: u64,
) -> Option<ListeningAnimation> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let started = std::time::Instant::now();
    let thread = std::thread::Builder::new()
        .name("rpi-voice-meter".to_string())
        .spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                let elapsed = started.elapsed().as_millis() as u64;
                runtime.set_status(&listening_status(
                    meter.get(),
                    elapsed,
                    hint,
                    start_window_ms,
                    warmup_ms,
                ));
                std::thread::sleep(std::time::Duration::from_millis(LISTENING_FRAME_MS));
            }
        })
        .ok()?;
    Some(ListeningAnimation {
        stop,
        thread: Some(thread),
    })
}

/// How often the listening meter publishes a new frame. The host's tick loop
/// polls extension status frequently; 200ms keeps the footer responsive without
/// creating a synchronous FFI action for every host render tick.
const LISTENING_FRAME_MS: u64 = 200;

// ---------------------------------------------------------------------------
// Playback: music-style equalizer + a handle to interrupt it
// ---------------------------------------------------------------------------

/// Number of bars in the playback equalizer, and how often it steps.
const EQ_BARS: usize = 7;
const PLAYING_FRAME_MS: u64 = 90;

/// Block glyphs, quietest → loudest. Using partial blocks (rather than a
/// filled/unfilled pair) is what gives the line its "level meter" read.
const EQ_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Render the `voice` line shown while speech is playing.
///
/// Bar heights follow the real audio level (`level`, 0.0..=1.0) but each bar
/// lags the previous one by a fixed phase and the phase advances every frame,
/// so the bars ripple left → right at a steady tempo: a wave whose *amplitude*
/// is the actual speech loudness. A silent passage would freeze the bars flat,
/// which reads as "broken", so a small floor keeps them breathing.
///
/// Pure (no clock, no atomics) so the animation itself is unit-testable.
fn playing_status(level: f32, frame: u64, elapsed_ms: u64) -> String {
    let level = level.clamp(0.0, 1.0).max(0.22);
    let mut bars = String::with_capacity(EQ_BARS);
    for i in 0..EQ_BARS {
        // Negative phase lag → the crest travels to the right over time.
        let phase = frame as f32 * 0.6 - i as f32 * 0.85;
        let wave = 0.5 + 0.5 * phase.sin();
        let height = (level * (0.4 + 0.6 * wave)).clamp(0.0, 1.0);
        let idx =
            ((height * (EQ_GLYPHS.len() - 1) as f32).round() as usize).min(EQ_GLYPHS.len() - 1);
        bars.push(EQ_GLYPHS[idx]);
    }
    format!("voice: ♪ {bars} playing {:.1}s", elapsed_ms as f64 / 1000.0)
}

/// Handle to the running playback animation; stopping it publishes the final
/// status synchronously so the footer never keeps a stale equalizer frame.
pub struct PlayingAnimation {
    stop: Arc<AtomicBool>,
}

impl PlayingAnimation {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Animate the `voice` status while speech is on the speakers. Returns `None`
/// when the thread could not be spawned (playback is unaffected — it just
/// won't animate).
fn start_playing_animation() -> Option<PlayingAnimation> {
    let runtime = RUNTIME_CTX.get().copied()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let animation = PlayingAnimation { stop };
    let started = std::time::Instant::now();
    std::thread::Builder::new()
        .name("rpi-voice-eq".to_string())
        .spawn(move || {
            let mut frame: u64 = 0;
            while !stop_thread.load(Ordering::Relaxed) {
                let level = f32::from_bits(PLAYBACK_LEVEL.load(Ordering::Relaxed));
                let elapsed = started.elapsed().as_millis() as u64;
                runtime.set_status(&playing_status(level, frame, elapsed));
                frame = frame.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_millis(PLAYING_FRAME_MS));
            }
            // Hand the footer back, but never stomp a recording that started
            // while this utterance was still finishing.
            set_idle_if_not_recording(&runtime);
        })
        .ok()
        .map(|_| animation)
}

/// Restore the resting status unless a recording owns the line right now.
fn set_idle_if_not_recording(runtime: &RuntimeContext) {
    let listening = PTT.lock().unwrap().recording_owner.is_some();
    if !RECORDING.load(Ordering::Relaxed) && !listening {
        runtime.set_status("voice: idle");
    }
}

// ---------------------------------------------------------------------------
// Hands-free conversation loop
// ---------------------------------------------------------------------------

/// Reopen the mic for the next turn, if hands-free mode wants one.
///
/// This is the *only* hook the loop needs: it runs when a reply has finished
/// playing, which closes the cycle
///
/// ```text
/// speak ─▶ transcribe ─▶ send ─▶ reply ─▶ speak (playback ends) ─▶ speak ─▶ …
/// ```
///
/// Ordering matters for more than tidiness — starting the recording from the
/// playback-completion point is what guarantees the microphone never overlaps
/// the speakers, so the assistant cannot transcribe its own voice.
fn maybe_auto_listen(runtime: RuntimeContext) {
    if !AUTO_TALK_ENABLED.load(Ordering::Relaxed) {
        debug_log("auto-listen skipped: continuous mode is off");
        return;
    }
    // A manual recording (PTT or one-shot `/voice`) owns the mic right now. Its
    // own reply will trigger the next hands-free turn.
    if RECORDING.load(Ordering::Relaxed) {
        debug_log("auto-listen skipped: another recording owns the mic");
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    *AUTO_TALK_STOP.lock().unwrap() = Some(stop.clone());
    let spawned = std::thread::Builder::new()
        .name("rpi-voice-auto".to_string())
        .spawn(move || auto_talk_turn(&runtime, stop))
        .is_ok();
    if !spawned {
        *AUTO_TALK_STOP.lock().unwrap() = None;
        runtime.set_status("voice: ⚠ could not start the hands-free listener");
    }
}

/// Record one hands-free turn and deliver it.
///
/// A turn that hears nothing is *not* the end of the conversation — the user may
/// just be thinking — so the mic reopens, up to
/// [`auto_talk_max_empty`] times in a row before the mode exits on its own.
fn auto_talk_turn(runtime: &RuntimeContext, stop: Arc<AtomicBool>) {
    // Claim the mic. If something else got there first, bow out quietly.
    if RECORDING.swap(true, Ordering::Relaxed) {
        debug_log("auto turn skipped: RECORDING was already set");
        return;
    }
    debug_log("auto turn: opening the mic");
    let loop_result = loop {
        if !AUTO_TALK_ENABLED.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            debug_log("auto turn aborted before/while recording");
            break;
        }
        let params = recorder::RecordParams::for_auto_turn();
        debug_log(&format!(
            "auto turn: recording (no_speech_ms={:?}, silence_ms={}, max_ms={})",
            params.no_speech_ms, params.silence_ms, params.max_ms
        ));
        match record_and_send_until(runtime, params, stop.clone(), ListenHint::Silence, true) {
            Ok(()) => {
                debug_log("auto turn: delivered a transcription");
                AUTO_TALK_EMPTY.store(0, Ordering::Relaxed);
                break;
            }
            Err(error) if is_empty_turn(&error) => {
                // A turn cut short because *we* were told to stand down (the user
                // started typing, or `/voice auto off`) is not an empty turn. It
                // must not cost a strike, and above all it must not report a
                // microphone diagnosis: a recording truncated to 0.1s has a peak
                // of 0.0000 for reasons that say nothing about the hardware.
                if !counts_as_empty_turn(
                    &error,
                    stop.load(Ordering::Relaxed),
                    AUTO_TALK_ENABLED.load(Ordering::Relaxed),
                ) {
                    debug_log("auto turn aborted on request — not counted as empty");
                    break;
                }
                let empties = AUTO_TALK_EMPTY.fetch_add(1, Ordering::Relaxed) + 1;
                let max = auto_talk_max_empty();
                let peak = f32::from_bits(LAST_PEAK_LEVEL.load(Ordering::Relaxed));
                let secs = LAST_DURATION_MS.load(Ordering::Relaxed) as f64 / 1000.0;
                debug_log(&format!(
                    "auto turn: empty ({empties}/{max}) — {error} (peak={peak:.4}, {secs:.1}s, speech={}ms)",
                    LAST_SPEECH_MS.load(Ordering::Relaxed)
                ));
                if empties >= max {
                    AUTO_TALK_ENABLED.store(false, Ordering::Relaxed);
                    runtime.set_status(&format!(
                        "voice: 💤 heard nothing {empties}× (peak {peak:.3}, {secs:.1}s) — continuous mode stopped; use `/voice auto` to resume"
                    ));
                    break;
                }
                // Naming the peak *and* the length turns "nothing heard" into an
                // actionable report. Three bands, because the fixes differ:
                //   ~0 + short  the turn was truncated by a stand-down
                //   ~0 + full   nothing arrived at all   → device/mute problem
                //   small       only room noise          → probably the wrong device
                //   healthy     audio but no words       → timing or STT
                if peak < DEAF_PEAK_LEVEL {
                    runtime.set_status(&format!(
                        "voice: 🎧 mic is silent (peak {peak:.4}, {secs:.1}s) — check the input device ({empties}/{max})"
                    ));
                } else if peak < ROOM_NOISE_PEAK_LEVEL {
                    // The most common real-world cause: the microphone is fine but
                    // its level is far too low, so the voice never rises above the
                    // room. Name both fixes rather than the symptom.
                    runtime.set_status(&format!(
                        "voice: 🎧 only room noise (peak {peak:.3}, {secs:.1}s, gain {:.0}x) — wrong mic, or set RPI_VOICE_INPUT_GAIN ({empties}/{max})",
                        f32::from_bits(LAST_GAIN.load(Ordering::Relaxed))
                    ));
                } else {
                    runtime.set_status(&format!(
                        "voice: 🎧 nothing heard (peak {peak:.3}, {secs:.1}s) — still listening ({empties}/{max})"
                    ));
                }
            }
            Err(error) => {
                debug_log(&format!("auto turn: failed — {error}"));
                AUTO_TALK_ENABLED.store(false, Ordering::Relaxed);
                runtime.set_status(&format!("voice: ⚠ {error}"));
                break;
            }
        }
    };
    let _ = loop_result;
    {
        let mut slot = AUTO_TALK_STOP.lock().unwrap();
        if slot.as_ref().is_some_and(|f| Arc::ptr_eq(f, &stop)) {
            *slot = None;
        }
    }
    RECORDING.store(false, Ordering::Relaxed);
}

/// Turn on speech for replies, returning whether it had been muted.
///
/// `/voice auto` and `/voice ptt` are both spoken *conversations*: the user
/// talks and expects to hear the answer, so a muted session looks broken —
/// you speak, a reply arrives, nothing is said. Both modes enable it here and
/// mention it, because silently unmuting is rude.
///
/// One helper is the point: `/voice ptt` originally forgot this call and was
/// silent, while `/voice auto` worked.
fn enable_auto_tts() -> bool {
    !AUTO_TTS_ENABLED.swap(true, Ordering::Relaxed)
}

/// Enable hands-free mode and listen immediately, so `/voice auto` starts the
/// conversation rather than waiting for a reply that may never come.
fn enable_auto_talk(runtime: RuntimeContext) -> String {
    // The loop is closed by *playback finishing*, so muted replies would leave
    // it stalled with the mic shut. Say so, since silently unmuting is rude.
    let was_muted = enable_auto_tts();
    preload_stt_model();
    AUTO_TALK_ENABLED.store(true, Ordering::Relaxed);
    AUTO_TALK_EMPTY.store(0, Ordering::Relaxed);
    stop_playback();
    // `maybe_auto_listen` spawns the listener and returns immediately, so the
    // first turn starts right away instead of waiting for a reply to finish.
    debug_log("continuous mode ON");
    maybe_auto_listen(runtime);
    let mut note = if was_muted {
        "🔊 Auto-TTS was off — turned it back on (the loop needs replies spoken)".to_string()
    } else {
        "🔁 Continuous mode ON — just talk; typing pauses it, `/voice auto off` exits".to_string()
    };
    if std::env::var("RPI_VOICE_DEBUG").is_ok() {
        note.push_str(&format!("\n  trace → {}", debug_log_path().display()));
    }
    note
}

/// Extract text content from a message JSON object (text parts only).
fn extract_message_text(message: &Value) -> String {
    if let Some(content) = message.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(parts) = content.as_array() {
            let mut text = String::new();
            for part in parts {
                // Only `{"type":"text","text":...}` parts carry speech; thinking
                // (`thinking`) and toolCall parts have no `text` field.
                if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                        text.push(' ');
                    }
                }
            }
            return text;
        }
    }

    message
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Strip markdown so TTS does not read `**`, backticks, links, code, etc.
fn sanitize_for_speech(text: &str) -> String {
    // 1. Drop fenced code blocks entirely.
    let mut body = String::with_capacity(text.len());
    let mut fence: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        if let Some(marker) = marker {
            match fence.as_deref() {
                Some(open) if open == marker => fence = None,
                None => fence = Some(marker.to_string()),
                _ => {}
            }
            continue;
        }
        if fence.is_none() {
            body.push_str(line);
            body.push('\n');
        }
    }

    // 2. Line-level markdown prefixes (headings, quotes, bullets, rules).
    let mut cleaned = String::with_capacity(body.len());
    for line in body.lines() {
        let trimmed = line.trim_start();
        let stripped =
            trimmed.trim_start_matches(|c: char| c == '#' || c == '>' || c.is_whitespace());
        let stripped = stripped
            .strip_prefix("- ")
            .or_else(|| stripped.strip_prefix("* "))
            .or_else(|| stripped.strip_prefix("+ "))
            .unwrap_or(stripped);
        if stripped
            .chars()
            .all(|c| c == '-' || c == '*' || c == '_' || c == ' ')
            && !stripped.is_empty()
        {
            continue; // horizontal rule
        }
        cleaned.push_str(stripped);
        cleaned.push('\n');
    }

    // 3. Inline cleanup: images, links (keep label), emphasis/backtick markers.
    let chars: Vec<char> = cleaned.chars().collect();
    let mut out = String::with_capacity(cleaned.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '`' => i += 1,
            '!' if chars.get(i + 1) == Some(&'[') => {
                i += 2;
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                i += 1; // consume ']'
                if chars.get(i) == Some(&'(') {
                    while i < chars.len() && chars[i] != ')' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '[' => {
                i += 1;
                while i < chars.len() && chars[i] != ']' {
                    out.push(chars[i]);
                    i += 1;
                }
                i += 1; // consume ']'
                if chars.get(i) == Some(&'(') {
                    while i < chars.len() && chars[i] != ')' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '*' => i += 1,
            _ => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the microphone currently owns the floor, so speaking would talk over
/// the user (or be transcribed back as the assistant's own voice).
///
/// This is the invariant the whole loop depends on: the mic and the speakers
/// must never be open at once. The hands-free loop enforces it in the other
/// direction (it reopens the mic only once playback has *finished*), but nothing
/// stopped the reverse — a reply queued before the user started talking would
/// begin playing into an open microphone, and the recogniser would receive the
/// assistant reading its own previous answer.
///
/// Checked immediately before playback rather than at enqueue time, because a
/// keystroke or a held key during synthesis is exactly the case that matters.
fn mic_owns_the_floor() -> bool {
    if RECORDING.load(Ordering::Relaxed) {
        return true;
    }
    // A push-to-talk press that has not reached its hold threshold yet has not
    // set `RECORDING`; the hold owns the key either way, so treat it as talking.
    PTT.lock().unwrap().pressed_at.is_some()
}

/// Synthesize text to speech and play it (blocking).
///
/// Streaming: the MP3 is decoded and played **as it arrives**, so the first word
/// is heard after the first packet rather than after the whole utterance. The
/// stop handle is published *before* the request is sent, so a barge-in now
/// aborts the synthesis itself — previously the flag only existed after
/// `synthesize` had already buffered everything, which made an interrupt during
/// the network fetch a no-op.
fn synthesize_and_play(text: &str) -> Result<(), String> {
    let voice = current_voice();
    let stop = Arc::new(AtomicBool::new(false));
    // Published up front so a keystroke can cut the utterance short from the
    // very first byte. A stale handle would let a later barge-in flag a dead
    // utterance, so every exit path clears it (below).
    *PLAYBACK_STOP.lock().unwrap() = Some(stop.clone());
    let animation = start_playing_animation();

    let stop_for_stream = stop.clone();
    // The producer thread takes ownership, so hand it an owned copy of the text
    // instead of borrowing the caller's `&str`.
    let text = text.to_string();
    let result = player::play_mp3_stream(
        move |sink: &mut dyn FnMut(&[u8]) -> Result<(), String>| {
            let (end, bytes) = edge_tts::synthesize_stream(
                &text,
                &voice,
                "+0%",
                "+0Hz",
                "+0%",
                &|| stop_for_stream.load(Ordering::Relaxed),
                |chunk| {
                    // A closed consumer means playback already stopped.
                    let _ = sink(chunk);
                },
            )?;
            debug_log(&format!(
                "tts stream {end:?}: {bytes} bytes delivered to the player"
            ));
            Ok(())
        },
        stop.clone(),
        &PLAYBACK_LEVEL,
    );

    if let Some(animation) = animation {
        animation.stop();
    }
    // Drop our handle only if it is still ours — an interrupt may have taken it
    // already, and a newer utterance must not be clobbered.
    {
        let mut slot = PLAYBACK_STOP.lock().unwrap();
        if slot.as_ref().is_some_and(|f| Arc::ptr_eq(f, &stop)) {
            *slot = None;
        }
    }
    PLAYBACK_LEVEL.store(0f32.to_bits(), Ordering::Relaxed);
    // `Ok(false)` is "the user cancelled", which is not a failure to report.
    result.map(|_played_to_end| ())
}

// ---------------------------------------------------------------------------
// Command handler: /voice → record → STT → editor draft (or direct send)
// ---------------------------------------------------------------------------

extern "C" fn voice_command(
    args_json: StbStringRef,
    out: *mut StbString,
    _user_data: *mut c_void,
) -> i32 {
    let args_str = unsafe { args_json.as_str() };
    let args: Value = serde_json::from_str(args_str).unwrap_or(Value::Null);
    let action = args
        .get("args")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let (verb, rest) = match action.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (action.as_str(), ""),
    };

    match verb {
        "off" => {
            AUTO_TTS_ENABLED.store(false, Ordering::Relaxed);
            AUTO_MODE_ENABLED.store(false, Ordering::Relaxed);
            // Turning Auto-TTS off must also stop hands-free mode. Otherwise a
            // running continuous loop can call `enable_auto_talk` on its next
            // turn and silently turn Auto-TTS back on.
            AUTO_TALK_ENABLED.store(false, Ordering::Relaxed);
            if let Some(stop) = AUTO_TALK_STOP.lock().unwrap().take() {
                stop.store(true, Ordering::Relaxed);
            }
            // Muting means silence *now*, not from the next reply on.
            stop_playback();
            set_output(
                out,
                json!({"kind": "message", "text": "🔇 Auto-TTS disabled; continuous mode stopped"}),
            );
            return 0;
        }
        "auto" | "talk" | "continuous" => {
            let enable = match rest {
                // Bare `/voice auto` turns it on and starts listening — that is
                // the whole point of the command, so it should not be a toggle
                // the user has to reason about.
                "" | "on" => true,
                "off" | "quiet" => false,
                other => {
                    set_output(
                        out,
                        json!({"kind": "message", "text": format!(
                            "Usage: /voice auto [on|off]  (got `{other}`)"
                        )}),
                    );
                    return 0;
                }
            };
            if !enable {
                AUTO_MODE_ENABLED.store(false, Ordering::Relaxed);
                AUTO_TALK_ENABLED.store(false, Ordering::Relaxed);
                // Abort the in-flight listen (and any speech) right away.
                let stop = AUTO_TALK_STOP.lock().unwrap().take();
                if let Some(stop) = stop {
                    stop.store(true, Ordering::Relaxed);
                }
                stop_playback();
                set_output(
                    out,
                    json!({"kind": "message", "text": "⏹ Continuous mode off"}),
                );
                return 0;
            }
            let Some(ctx) = RUNTIME_CTX.get().copied() else {
                set_output(
                    out,
                    json!({"kind": "message", "text": "❌ Runtime context not available"}),
                );
                return 1;
            };
            AUTO_MODE_ENABLED.store(true, Ordering::Relaxed);
            preload_stt_model();
            let note = enable_auto_talk(ctx);
            set_output(out, json!({"kind": "message", "text": note}));
            return 0;
        }
        "stop" | "shush" => {
            // A manual stop is an explicit session-level mute, not just a
            // one-frame player interruption. Do not let the next assistant
            // message or continuous-mode callback turn speech back on.
            AUTO_TTS_ENABLED.store(false, Ordering::Relaxed);
            let stopped = stop_playback();
            set_output(
                out,
                json!({"kind": "message", "text": if stopped {
                    "🤫 Playback stopped; Auto-TTS muted for this session (use `/voice on` to resume)"
                } else {
                    "🔈 No playback; Auto-TTS muted for this session (use `/voice on` to resume)"
                }}),
            );
            return 0;
        }
        "on" => {
            AUTO_TTS_ENABLED.store(true, Ordering::Relaxed);
            set_output(
                out,
                json!({"kind": "message", "text": "🔊 Auto-TTS enabled"}),
            );
            return 0;
        }
        "status" => {
            set_output(out, json!({"kind": "message", "text": status_text()}));
            return 0;
        }
        "ptt" => {
            let enable = match rest {
                "" | "on" => true,
                "off" => false,
                other => {
                    set_output(
                        out,
                        json!({"kind": "message", "text": format!(
                            "Usage: /voice ptt [on|off]  (got `{other}`)"
                        )}),
                    );
                    return 0;
                }
            };
            if !enable {
                PTT_ENABLED.store(false, Ordering::Relaxed);
                // Abort anything in flight: a release after this would otherwise
                // send a half utterance.
                let stop = PTT.lock().unwrap().stop.take();
                if let Some(stop) = stop {
                    stop.store(true, Ordering::Relaxed);
                }
                ptt_reset();
                set_output(
                    out,
                    json!({"kind": "message", "text": "🔇 Push-to-talk off"}),
                );
                return 0;
            }
            PTT_ENABLED.store(true, Ordering::Relaxed);
            // Push-to-talk is a spoken conversation, so replies must be read
            // aloud — without this the mode is silent.
            let was_muted = enable_auto_tts();
            preload_stt_model();
            set_output(
                out,
                json!({"kind": "message", "text": format!(
                    "🎤 Push-to-talk ON\n  key: hold `{}` for {}s, release → {}\n  (only while the input box is empty; `/voice ptt off` to exit){}",
                    ptt_key(),
                    ptt_hold_ms() as f64 / 1000.0,
                    match output_mode() {
                        OutputMode::Send => "send".to_string(),
                        OutputMode::Draft if draft_auto_send_ms() == 0 =>
                            "input box (Enter to send)".to_string(),
                        OutputMode::Draft => format!(
                            "input box, auto-send in {}s",
                            draft_auto_send_ms() as f64 / 1000.0
                        ),
                    },
                    if was_muted {
                        "
  🔊 Auto-TTS was off — turned it back on so replies are spoken"
                    } else {
                        ""
                    }
                )}),
            );
            return 0;
        }
        "output" | "mode" => {
            match rest {
                "" => set_output(
                    out,
                    json!({"kind": "message", "text": format!(
                        "Transcription output: {}\nUsage: /voice output [draft|send]",
                        output_mode_label()
                    )}),
                ),
                "draft" | "edit" => {
                    *OUTPUT_OVERRIDE.lock().unwrap() = Some(OutputMode::Draft);
                    set_output(
                        out,
                        json!({"kind": "message", "text": format!(
                            "✏️ Transcriptions go to the input box — {}",
                            if draft_auto_send_ms() == 0 {
                                "press Enter to send".to_string()
                            } else {
                                format!("auto-send in {}s, type to edit", draft_auto_send_ms() as f64 / 1000.0)
                            }
                        )}),
                    );
                }
                "send" | "direct" => {
                    *OUTPUT_OVERRIDE.lock().unwrap() = Some(OutputMode::Send);
                    set_output(
                        out,
                        json!({"kind": "message", "text": "➡ Transcriptions are sent immediately"}),
                    );
                }
                other => set_output(
                    out,
                    json!({"kind": "message", "text": format!(
                        "Usage: /voice output [draft|send]  (got `{other}`)"
                    )}),
                ),
            }
            return 0;
        }
        "gain" | "level" => {
            if rest.is_empty() {
                set_output(
                    out,
                    json!({"kind": "message", "text": format!(
                        "Input level: {}\n  auto  — normalise each recording for STT (default)\n  off   — leave the signal exactly as captured\n  <n>   — fixed gain applied at capture (e.g. 20)\nUsage: /voice gain [auto|off|<n>]",
                        recorder::gain_pref_label()
                    )}),
                );
                return 0;
            }
            // `off` stores unity gain — the same thing as an explicit `1`.
            let requested = if rest.eq_ignore_ascii_case("off") {
                "1"
            } else {
                rest
            };
            match recorder::set_input_gain_pref(requested) {
                Ok(label) => set_output(
                    out,
                    json!({"kind": "message", "text": format!(
                        "🔊 Input level → {label}\n  Saved to ~/.rpi/agent/voice.json (survives restarts)"
                    )}),
                ),
                Err(error) => set_output(
                    out,
                    json!({"kind": "message", "text": format!("⚠ {error}")}),
                ),
            }
            return 0;
        }
        "input" | "device" | "mic" => {
            if rest.is_empty() {
                // Listing what exists is the whole point: the machine has several
                // capture endpoints and the Windows default is often not the one
                // you are talking into.
                let active = recorder::active_input_device();
                let system_default = recorder::system_default_device();
                let pinned = recorder::configured_device_pref();
                let mut text = String::from("🎤 input devices\n");
                for name in recorder::list_input_devices() {
                    let mut marks = Vec::new();
                    if name == active {
                        marks.push("ACTIVE");
                    }
                    if Some(&name) == system_default.as_ref() {
                        marks.push("system default");
                    }
                    text.push_str(&format!(
                        "  {name}{}\n",
                        if marks.is_empty() {
                            String::new()
                        } else {
                            format!("   [{}]", marks.join(", "))
                        }
                    ));
                }
                match pinned {
                    Some(pinned) => text.push_str(&format!("  pinned to: {pinned}\n")),
                    None => text.push_str("  pinned to: (system default)\n"),
                }
                text.push_str("Usage: /voice input <substring of a name>");
                set_output(out, json!({"kind": "message", "text": text}));
                return 0;
            }
            match recorder::set_input_device_pref(rest) {
                Ok(name) => set_output(
                    out,
                    json!({"kind": "message", "text": format!(
                        "🎤 Input device → {name}\n  Saved to ~/.rpi/agent/voice.json (survives restarts)\n  A Bluetooth headset needs ~4s to wake on the first capture."
                    )}),
                ),
                Err(error) => set_output(
                    out,
                    json!({"kind": "message", "text": format!("⚠ {error}")}),
                ),
            }
            return 0;
        }
        "model" => {
            if rest == "download" {
                let Some(ctx) = RUNTIME_CTX.get().copied() else {
                    set_output(
                        out,
                        json!({"kind": "message", "text": "❌ Runtime context not available"}),
                    );
                    return 1;
                };
                set_output(
                    out,
                    json!({"kind": "message", "text": format!(
                        "⬇ Downloading STT model in the background…\n{}",
                        model_info()
                    )}),
                );
                std::thread::Builder::new()
                    .name("rpi-voice-model".to_string())
                    .spawn(move || match stt_prepare_model(&ctx) {
                        Ok(_dir) => ctx.set_status("voice: model ready"),
                        Err(e) => {
                            eprintln!("[rpi-voice] model download error: {e}");
                            ctx.set_status(&format!("voice: ⚠ {e}"));
                        }
                    })
                    .ok();
                return 0;
            }
            if rest == "load" {
                #[cfg(not(feature = "local-stt"))]
                {
                    set_output(
                        out,
                        json!({"kind": "message", "text":
                            "❌ local STT not compiled in (rebuild with --features local-stt)"}),
                    );
                    return 1;
                }
                #[cfg(feature = "local-stt")]
                {
                    if local_stt::is_ready() {
                        set_output(
                            out,
                            json!({"kind": "message", "text":
                                "🧠 STT model already loaded — transcriptions are instant"}),
                        );
                        return 0;
                    }
                    let Some(ctx) = RUNTIME_CTX.get().copied() else {
                        set_output(
                            out,
                            json!({"kind": "message", "text": "❌ Runtime context not available"}),
                        );
                        return 1;
                    };
                    set_output(
                        out,
                        json!({"kind": "message", "text":
                            "🧠 Loading the STT model now… (~5 s, one time)
  speak once it says ready — /voice status shows the state"}),
                    );
                    std::thread::Builder::new()
                        .name("rpi-voice-stt-load".to_string())
                        .spawn(move || {
                            local_stt::preload_in_background();
                            // Wait for the actual result so the status line can
                            // report success or the load error.
                            match local_stt::preload() {
                                Ok(()) => ctx.set_status("voice: 🧠 model ready"),
                                Err(e) => {
                                    eprintln!("[rpi-voice] STT load error: {e}");
                                    ctx.set_status(&format!("voice: ⚠ {e}"));
                                }
                            }
                        })
                        .ok();
                    return 0;
                }
            }
            set_output(out, json!({"kind": "message", "text": model_info()}));
            return 0;
        }
        "set" | "voice" => {
            if rest.is_empty() {
                set_output(
                    out,
                    json!({"kind": "message", "text": format!(
                        "Current voice: {}\nUsage: /voice set <voice-name>",
                        current_voice()
                    )}),
                );
                return 0;
            }
            *VOICE_OVERRIDE.lock().unwrap() = Some(rest.to_string());
            set_output(
                out,
                json!({"kind": "message", "text": format!("🔊 TTS voice set to {rest}")}),
            );
            return 0;
        }
        "help" => {
            set_output(out, json!({"kind": "message", "text": help_text()}));
            return 0;
        }
        "" => {}
        other => {
            set_output(
                out,
                json!({"kind": "message", "text": format!(
                    "Unknown option `{other}`.\n{}",
                    help_text()
                )}),
            );
            return 0;
        }
    }

    // Default: start voice input.
    if RECORDING.load(Ordering::Relaxed) {
        set_output(
            out,
            json!({"kind": "message", "text": "⏳ Already recording… please wait"}),
        );
        return 0;
    }

    let Some(ctx) = RUNTIME_CTX.get().copied() else {
        set_output(
            out,
            json!({"kind": "message", "text": "❌ Runtime context not available"}),
        );
        return 1;
    };

    // Starting a fresh dictation silences the assistant first (barge-in):
    // otherwise the mic would pick up its own voice through the speakers.
    stop_playback();
    RECORDING.store(true, Ordering::Relaxed);
    let params = recorder::RecordParams::from_env();
    set_output(
        out,
        json!({"kind": "message", "text": format!(
            "🎤 Recording… speak now (auto-stops after {:.1}s of silence, max {:.0}s)",
            params.silence_ms as f64 / 1000.0,
            params.max_ms as f64 / 1000.0
        )}),
    );

    std::thread::Builder::new()
        .name("rpi-voice-input".to_string())
        .spawn(move || {
            let result = record_and_send(&ctx, params);
            RECORDING.store(false, Ordering::Relaxed);
            match result {
                Ok(()) => ctx.set_status("voice: idle"),
                Err(e) => {
                    eprintln!("[rpi-voice] Voice input error: {e}");
                    ctx.set_status(&format!("voice: ⚠ {e}"));
                }
            }
        })
        .expect("spawn rpi-voice input thread");

    0
}

/// Record from microphone, transcribe, and deliver the text.
fn record_and_send(runtime: &RuntimeContext, params: recorder::RecordParams) -> Result<(), String> {
    // Recording auto-stops on silence; the stop flag lets it be cut short.
    let stop = Arc::new(AtomicBool::new(false));
    record_and_send_until(runtime, params, stop, ListenHint::Silence, false)
}

/// Like [`record_and_send`], but with a caller-owned stop flag — the hands-free
/// loop keeps a handle so `/voice auto off` (or the user taking the keyboard)
/// can end its listen immediately.
///
/// Every recording path goes through here so they all show the same live level
/// meter while the mic is open: push-to-talk, one-shot `/voice`, and hands-free
fn record_and_send_until(
    runtime: &RuntimeContext,
    params: recorder::RecordParams,
    stop: Arc<AtomicBool>,
    hint: ListenHint,
    auto_mode: bool,
) -> Result<(), String> {
    let meter = recorder::LevelMeter::new();
    // Tell the user how long they have to *start* talking; `None` for
    // push-to-talk, where release is the only deadline.
    let window = match hint {
        ListenHint::Silence => params.no_speech_ms,
        ListenHint::Release => None,
    };
    let animation =
        start_listening_animation(*runtime, meter.clone(), hint, window, params.warmup_ms);
    // Publish the first frame so the line is right even if the animation thread
    // could not be spawned.
    runtime.set_status(&listening_status(0.0, 0, hint, window, params.warmup_ms));

    let recorded = recorder::record_until_with_level(stop, params, Some(meter));
    // Stop the animation before publishing any follow-up status ("transcribing…"),
    // so the two writers cannot fight over the status line.
    if let Some(animation) = animation {
        animation.stop();
    }

    finish_recording(runtime, recorded?, auto_mode)
}

/// The error to report when a recording produced no usable speech.
///
/// Pure, so the wording and the clipping branch are unit-testable. A gain high
/// enough to clip is the most common self-inflicted cause of an empty
/// transcription, and it is invisible in the level numbers — the peak simply
/// reads full scale, which looks like a *healthy* microphone. Measured in the
/// wild: a webcam microphone whose speech already peaked at `0.55` of full scale
/// was given a fixed `x20`, so every sample clamped, the recogniser received a
/// square wave, and it returned nothing.
fn no_speech_error(recording: &recorder::Recording) -> String {
    if recording.clipped_ratio > recorder::CLIPPING_WARN_RATIO {
        return format!(
            "{} — gain x{:.0} clipped {:.0}% of the samples; run `/voice gain auto`",
            ERR_NO_SPEECH,
            recording.gain,
            recording.clipped_ratio * 100.0
        );
    }
    ERR_NO_SPEECH.to_string()
}

/// Convert an already-captured recording and deliver it (editor draft by
/// default, immediate send when configured).
///
/// Shared by the `/voice` one-shot path and push-to-talk (which ends the
/// recording on key release rather than on trailing silence).
fn finish_recording(
    runtime: &RuntimeContext,
    recording: recorder::Recording,
    auto_mode: bool,
) -> Result<(), String> {
    // Publish the capture diagnostics first: every exit below (too short, no
    // speech, a failed request) is otherwise indistinguishable to the user from
    // "the mic is not working".
    LAST_PEAK_LEVEL.store(recording.peak_level.to_bits(), Ordering::Relaxed);
    LAST_SPEECH_MS.store(recording.speech_ms as usize, Ordering::Relaxed);
    LAST_GAIN.store(recording.gain.to_bits(), Ordering::Relaxed);
    LAST_DURATION_MS.store(
        (recording.duration_secs() * 1000.0) as usize,
        Ordering::Relaxed,
    );
    debug_log(&format!(
        "recording: {:.2}s device='{}' peak={:.4} gain={:.1}x clipped={:.1}% speech={}ms samples={}",
        recording.duration_secs(),
        recording.device_name,
        recording.peak_level,
        recording.gain,
        recording.clipped_ratio * 100.0,
        recording.speech_ms,
        recording.samples.len()
    ));
    LAST_CLIPPED.store(recording.clipped_ratio.to_bits(), Ordering::Relaxed);
    if recording.duration_secs() < 0.4 {
        return Err(ERR_TOO_SHORT.to_string());
    }

    runtime.set_status("voice: 📝 transcribing…");
    let language = std::env::var("RPI_VOICE_STT_LANG")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let text = transcribe(runtime, &recording, language.as_deref())?;

    if text.is_empty() {
        return Err(no_speech_error(&recording));
    }

    match if auto_mode {
        OutputMode::Draft
    } else {
        output_mode()
    } {
        OutputMode::Send => {
            runtime.set_status(&format!("voice: ➡ {}", truncate(&text, 40)));
            runtime.action(RuntimeActionId::SendUserMessage, json!({ "text": text }))?;
        }
        OutputMode::Draft => {
            // `append` keeps an in-progress draft: a second utterance is added
            // after the first rather than replacing it.
            let ms = draft_auto_send_ms();
            runtime.set_status(&format!(
                "voice: ✏️ draft{}",
                if ms == 0 {
                    " — Enter to send".to_string()
                } else {
                    format!(" — auto-send in {}s (type to edit)", ms as f64 / 1000.0)
                }
            ));
            runtime.action(
                RuntimeActionId::SetEditorText,
                json!({ "text": text, "mode": "append", "autoSendMs": ms }),
            )?;
        }
    }
    Ok(())
}

/// Transcribe a recording using the preferred engine, falling back to the API
/// when the local model is unavailable and the preference is not forced.
fn transcribe(
    runtime: &RuntimeContext,
    recording: &recorder::Recording,
    language: Option<&str>,
) -> Result<String, String> {
    #[cfg(feature = "local-stt")]
    {
        let pref = stt_engine_pref();
        if pref != "api" {
            match stt_prepare_model(runtime) {
                Ok(_dir) => {
                    // Usually already loaded (voice-on preloads it), so this is
                    // a lock, not a 5.5 s load. Only the very first utterance in
                    // a session where the preload lost the race still waits.
                    if !local_stt::is_ready() {
                        runtime.set_status("voice: 🧠 loading model…");
                    }
                    let mut engine = local_stt::engine()?;
                    let engine = engine.as_mut().map_err(|e| e.clone())?;
                    runtime.set_status("voice: 🧠 transcribing…");
                    return engine.transcribe(&recording.to_f32_16k_mono());
                }
                Err(e) => {
                    if pref == "local" {
                        return Err(e);
                    }
                    eprintln!("[rpi-voice] local STT unavailable, falling back to API: {e}");
                }
            }
        }
    }

    runtime.set_status("voice: ☁ transcribing…");
    let client = whisper::WhisperProvider::new(None, None)?;
    client.transcribe_wav(&recording.to_wav_bytes()?, language)
}

/// Ensure the local model files exist, reporting progress to the TUI status.
#[cfg(feature = "local-stt")]
fn stt_prepare_model(runtime: &RuntimeContext) -> Result<std::path::PathBuf, String> {
    local_stt::ensure_model(|msg| runtime.set_status(&format!("voice: {msg}")))
}

#[cfg(not(feature = "local-stt"))]
#[allow(dead_code)]
fn stt_prepare_model(_runtime: &RuntimeContext) -> Result<std::path::PathBuf, String> {
    Err("local STT not compiled in (rebuild with --features local-stt)".to_string())
}

/// Human-readable STT engine/model report for `/voice model`.
fn model_info() -> String {
    let pref = stt_engine_pref();
    let mut lines = vec![
        "🧠 STT configuration".to_string(),
        format!("  engine: RPI_STT_ENGINE={pref}"),
    ];
    #[cfg(feature = "local-stt")]
    {
        let dir = local_stt::model_dir();
        let model = local_stt::model_path();
        let tokens = local_stt::tokens_path();
        let present = model.is_file() && tokens.is_file();
        let size = if present {
            let bytes: u64 = [&model, &tokens]
                .iter()
                .filter_map(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
                .sum();
            format!(" ({:.0} MB)", bytes as f64 / 1e6)
        } else {
            String::new()
        };
        #[cfg(feature = "local-stt")]
        let loaded = if local_stt::is_ready() {
            " ✓ loaded in memory"
        } else {
            " (not loaded yet)"
        };
        lines.push(format!(
            "  local model: SenseVoice{}{}\n    dir: {}\n    {}",
            size,
            loaded,
            dir.display(),
            if present {
                "✓ ready"
            } else {
                "✗ not downloaded (/voice model download)"
            }
        ));
    }
    #[cfg(not(feature = "local-stt"))]
    lines.push("  local model: not compiled in (build with --features local-stt)".to_string());
    lines.push(format!("  effective: {}", stt_summary()));
    lines.join("\n")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn status_text() -> String {
    let enabled = AUTO_TTS_ENABLED.load(Ordering::Relaxed);
    let playing = TTS_PLAYING.load(Ordering::Relaxed);
    let recording = RECORDING.load(Ordering::Relaxed);
    let ptt = PTT_ENABLED.load(Ordering::Relaxed);
    let mut lines = format!(
        "🎤 rpi-voice\n  Auto-TTS: {}\n  Voice: {}\n  Push-to-talk: {}\n  Continuous: {}\n  Output: {}\n  Input device: {}\n  Input level: {}\n  Last capture: {}\n  Playing: {}\n  Recording: {}\n  STT: {}",
        if enabled { "enabled" } else { "disabled" },
        current_voice(),
        if ptt {
            format!("on (hold `{}` {}s)", ptt_key(), ptt_hold_ms() as f64 / 1000.0)
        } else {
            "off".to_string()
        },
        if AUTO_TALK_ENABLED.load(Ordering::Relaxed) {
            "on (hands-free)".to_string()
        } else {
            "off".to_string()
        },
        output_mode_label(),
        {
            let active = recorder::active_input_device();
            match recorder::configured_device_pref() {
                Some(pinned) => format!("{active}   [pinned: {pinned}]"),
                None => format!("{active}   [system default]"),
            }
        },
        recorder::gain_pref_label(),
        // What the last recording actually measured — the fastest way to tell a
        // dead microphone from a quiet one from a mis-selected one.
        {
            let peak = f32::from_bits(LAST_PEAK_LEVEL.load(Ordering::Relaxed));
            let speech = LAST_SPEECH_MS.load(Ordering::Relaxed);
            if peak == 0.0 && speech == 0 {
                "(nothing recorded yet)".to_string()
            } else {
                format!(
                    "peak {peak:.4}, {:.1}s, gain {:.0}x, speech {speech}ms",
                    LAST_DURATION_MS.load(Ordering::Relaxed) as f64 / 1000.0,
                    f32::from_bits(LAST_GAIN.load(Ordering::Relaxed))
                )
            }
        },
        if playing { "yes" } else { "no" },
        if recording { "yes" } else { "no" },
        stt_summary()
    );
    lines.push_str(&format!("\n  Output device: {}", player::active_output_device()));
    if let Some(err) = LAST_TTS_ERROR.lock().unwrap().as_ref() {
        lines.push_str(&format!("\n  Last speech: ⚠ {err}"));
    }
    lines
}

/// One-line summary of which STT backend `/voice` will use.
fn stt_summary() -> String {
    #[cfg(feature = "local-stt")]
    {
        let pref = stt_engine_pref();
        if pref != "api" {
            let model = local_stt::model_path();
            let tokens = local_stt::tokens_path();
            if model.is_file() && tokens.is_file() {
                return "local SenseVoice ✓".to_string();
            }
            if pref == "local" {
                return "local SenseVoice (model missing)".to_string();
            }
            // auto, not downloaded yet
            return "local SenseVoice (will download)".to_string();
        }
    }

    let base = std::env::var("RPI_STT_API_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let has_key = std::env::var("RPI_STT_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let hosted = base.trim_end_matches('/') == "https://api.openai.com/v1";
    let key = if has_key {
        "key set"
    } else if hosted {
        "⚠ key missing"
    } else {
        "no key (local)"
    };
    format!("api {base} ({key})")
}

fn help_text() -> String {
    "🎤 /voice — usage\n  /voice                record mic → transcribe → input box\n  /voice auto [on|off]  continuous conversation: reply, then listen again — no\n                        buttons (typing pauses it)\n  /voice ptt [on|off]   push-to-talk: hold the key, release to deliver\n  /voice input [name]   list capture devices, or pin one (persisted)\n  /voice gain [auto|n]  input level: auto-normalise (default) or a fixed gain\n  /voice stop           stop speaking (typing/pressing also does this)\n  /voice output [draft|send]\n                        where a transcription goes (default: draft)\n  /voice on|off         toggle auto-TTS of replies (also preloads the STT model)\n  /voice status         show current state, incl. output device + last error\n  /voice set <name>     set the TTS voice for this session\n  /voice model          show STT engine + local model\n  /voice model load     load the STT model now (~5 s) instead of on first use\n  /voice model download pre-fetch the local model\n  /voice help           show this help\n\nSpeech (TTS) is off by default and does NOT follow `/voice` — turn it on with\n`/voice on` or `RPI_VOICE_AUTO_TTS=on`. If replies are silent, `/voice status`\nnow names the output device and the last playback error."
        .to_string()
}

/// Helper to set command output (host reclaims it via `free_string`).
fn set_output(out: *mut StbString, value: Value) {
    if !out.is_null() {
        unsafe {
            *out = StbString::from_string(value.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin registration
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe {
        register_entrypoint(api, |api| {
            let ctx = RuntimeContext {
                runtime_action: api.runtime_action,
                free_string: api.free_string,
                user_data: api.user_data,
            };
            let _ = RUNTIME_CTX.set(ctx);

            // Auto-TTS is opt-in. Set `RPI_VOICE_AUTO_TTS=on` to enable it
            // for the whole session, or use `/voice on` interactively.
            if let Ok(v) = std::env::var("RPI_VOICE_AUTO_TTS") {
                let on = matches!(
                    v.trim().to_lowercase().as_str(),
                    "on" | "1" | "true" | "yes"
                );
                AUTO_TTS_ENABLED.store(on, Ordering::Relaxed);
            }

            // Auto-TTS is opt-in for every assistant message.
            if let Some(register_event) = api.register_event_handler {
                let rc = register_event(EventTag::MessageEnd, on_message_end, api.user_data);
                if rc != 0 {
                    eprintln!("[rpi-voice] Failed to register MessageEnd handler: {rc}");
                }
                // Push-to-talk: key events routed by the host. Subscribing is
                // harmless while PTT is off — the handler declines (CONTINUE)
                // and the key reaches the editor normally.
                let rc = register_event(EventTag::Input, on_input_key, api.user_data);
                if rc != 0 {
                    eprintln!("[rpi-voice] Failed to register Input handler: {rc}");
                }
                // Barge-in: the user started typing, so stop the current
                // utterance. Harmless (and silent) when nothing is playing.
                let rc = register_event(EventTag::EditorChange, on_editor_change, api.user_data);
                if rc != 0 {
                    eprintln!("[rpi-voice] Failed to register EditorChange handler: {rc}");
                }
            }

            // /voice command (record + settings).
            if let Some(register_command) = api.register_command {
                let name = StbStringRef::from_str("voice");
                let description = StbStringRef::from_str(
                    "Voice mode: record & transcribe (default), or auto|ptt|input|gain|stop|on|off|status|set <voice>|model [load|download]|output|help",
                );
                let rc = register_command(name, description, voice_command);
                if rc != 0 {
                    eprintln!("[rpi-voice] Failed to register /voice command: {rc}");
                }
            }

            // Spoken-style prompt: while replies are being read aloud, ask the
            // model to write for the ear. Registered only when the host offers
            // the slot; a host without it simply never gets the style, which is
            // the behaviour it had before this existed.
            if spoken_style::register(api) {
                debug_log("spoken style: registered before_agent_start transformer");
            } else {
                debug_log(
                    "spoken style: host has no before_agent_start slot —                      spoken replies get no style section",
                );
            }

            // Claim the push-to-talk key so the host routes it to this plugin
            // (instead of the editor) once `/voice ptt` turns the mode on. The
            // host only honors the claim while the input box is empty.
            if let Some(register_shortcut) = api.register_shortcut {
                let key = StbStringRef::from_str(&ptt_key());
                let description =
                    StbStringRef::from_str("rpi-voice: hold to talk, release to send");
                let rc = register_shortcut(key, description);
                if rc != 0 {
                    eprintln!("[rpi-voice] Failed to register PTT shortcut: {rc}");
                }
            }

            0 // Success
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that touch the process-global playback slot.
    static PLAYBACK_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// The equalizer is a real level meter: the bars track the audio loudness
    /// we publish, the crest travels so it visibly animates, and silence still
    /// breathes (a frozen line would read as a bug).
    #[test]
    fn playing_status_renders_a_wave_that_follows_the_level() {
        let quiet = playing_status(0.0, 0, 500);
        let loud = playing_status(1.0, 0, 500);
        assert_ne!(quiet, loud, "loudness must move the bars");

        for status in [&quiet, &loud] {
            assert!(status.starts_with("voice: ♪ "), "{status}");
            assert!(status.ends_with("playing 0.5s"), "{status}");
            assert_eq!(
                status.chars().filter(|c| EQ_GLYPHS.contains(c)).count(),
                EQ_BARS,
                "{status}"
            );
        }

        // Consecutive frames differ: the wave animates.
        assert_ne!(playing_status(0.6, 0, 0), playing_status(0.6, 1, 0));

        // Silence must not freeze every bar at the lowest glyph.
        let floor = EQ_GLYPHS[0] as u32;
        let silence_bars: Vec<u32> = quiet
            .chars()
            .filter(|c| EQ_GLYPHS.contains(c))
            .map(|c| c as u32)
            .collect();
        assert!(
            silence_bars.iter().any(|h| *h > floor),
            "silence froze the meter: {quiet}"
        );
        // Every height is a real glyph, loudest includes the full block.
        assert!(loud
            .chars()
            .filter(|c| EQ_GLYPHS.contains(c))
            .all(|c| EQ_GLYPHS.contains(&c)));
        assert!(
            loud.contains(EQ_GLYPHS[EQ_GLYPHS.len() - 1]),
            "a full-scale level should reach the tallest bar: {loud}"
        );
    }

    /// A microphone that is merely *quiet* must not be reported as silent.
    ///
    /// Regression: `DEAF_PEAK_LEVEL` was `0.02`, which sits right on top of what
    /// a healthy USB microphone idles at (0.008–0.02). A perfectly working device
    /// was therefore reported as "mic is silent — check the input device",
    /// sending the user hunting for a hardware fault that did not exist. The
    /// real message was "only room noise — is this the mic you speak into?".
    #[test]
    fn a_quiet_microphone_is_not_reported_as_silent() {
        // Measured on a real, working USB mic idling in a quiet room.
        assert!(
            0.019 > DEAF_PEAK_LEVEL,
            "a live mic's idle peak must not read as digital silence"
        );
        // It *is* below the room-noise band, so the advice becomes "wrong
        // device" instead of the useless "still listening".
        assert!(0.019 < ROOM_NOISE_PEAK_LEVEL);
        // Genuine digital silence is still caught.
        assert!(0.0 < DEAF_PEAK_LEVEL);
        // The bands are ordered, so every level lands in exactly one of them.
        assert!(DEAF_PEAK_LEVEL < ROOM_NOISE_PEAK_LEVEL);
    }

    /// A gain high enough to clip must be **named** as the cause, not reported as
    /// plain "no speech": the level numbers look healthy (peak at full scale), so
    /// the user would otherwise go hunting for a broken microphone.
    #[test]
    fn a_clipping_gain_is_reported_as_the_cause() {
        let base = recorder::Recording {
            samples: Vec::new(),
            sample_rate: 16000,
            channels: 1,
            peak_level: 1.0,
            speech_ms: 900,
            device_name: "test".to_string(),
            gain: 20.0,
            clipped_ratio: 0.0,
        };

        // Clipped: blame the gain and name the fix.
        let clipped = recorder::Recording {
            clipped_ratio: 0.4,
            ..base.clone()
        };
        let error = no_speech_error(&clipped);
        assert!(error.contains(ERR_NO_SPEECH), "{error}");
        assert!(error.contains("clipped 40%"), "{error}");
        assert!(error.contains("/voice gain auto"), "{error}");
        // It must still count as an empty turn, so the loop behaves as before.
        assert!(is_empty_turn(&error), "{error}");

        // Unclipped: the plain message, so the branch is not always taken.
        assert_eq!(no_speech_error(&base), ERR_NO_SPEECH);
    }

    /// Only "the user wasn't there" counts as an empty turn. A real failure
    /// (mic busy, model missing, network) must end the loop instead of quietly
    /// burning strikes while the mic keeps reopening.
    #[test]
    fn empty_turn_detection_separates_silence_from_real_errors() {
        assert!(is_empty_turn(ERR_NO_SPEECH));
        assert!(is_empty_turn(ERR_TOO_SHORT));
        // Decorated sentinels must still classify as empty turns. Regression: an
        // exact match meant adding the clipping hint to the message turned a
        // retryable empty turn into a fatal error that stopped the session.
        assert!(is_empty_turn(&format!(
            "{ERR_NO_SPEECH} — gain x20 clipped 40%"
        )));
        assert!(is_empty_turn(&format!("{ERR_TOO_SHORT} (0.2s)")));
        assert!(!is_empty_turn("Build input stream error: device busy"));
        assert!(!is_empty_turn("STT request failed: 401 Unauthorized"));
        // A message merely *containing* the phrase must not qualify.
        assert!(!is_empty_turn(
            "request failed after no speech detected timeout"
        ));
    }

    /// A turn we ourselves cut short must never be blamed on the microphone.
    ///
    /// Regression: typing a command during a hands-free turn pauses the mode,
    /// which aborts the recording in flight. The truncated capture (a fraction of
    /// a second, peak `0.0000`) was then fed through the "heard nothing" path,
    /// burning a strike and reporting `mic is silent — check the input device` —
    /// a hardware fault that did not exist, on a microphone that was fine.
    #[test]
    fn an_aborted_turn_is_not_an_empty_turn() {
        // Genuinely empty, mode still running, nobody asked us to stop.
        assert!(counts_as_empty_turn(ERR_NO_SPEECH, false, true));
        assert!(counts_as_empty_turn(ERR_TOO_SHORT, false, true));

        // Cut short because the user took over: not their turn to lose.
        assert!(!counts_as_empty_turn(ERR_TOO_SHORT, true, true));
        assert!(!counts_as_empty_turn(ERR_NO_SPEECH, true, true));
        // Mode switched off underneath us.
        assert!(!counts_as_empty_turn(ERR_TOO_SHORT, false, false));
        // A real error never counts, whatever the flags say.
        assert!(!counts_as_empty_turn(
            "no input device (microphone) found",
            false,
            true
        ));
    }

    /// The hands-free loop must not misread its own injected transcription as
    /// the user taking the keyboard — that would cancel the conversation on
    /// every turn. A human keystroke, by contrast, does stand it down.
    #[test]
    fn only_user_editor_changes_pause_continuous_mode() {
        let _guard = PLAYBACK_TEST_LOCK.lock().unwrap();

        AUTO_TALK_ENABLED.store(true, Ordering::Relaxed);
        let rc = handle_editor_change(r#"{"chars":5,"empty":false,"source":"extension"}"#);
        assert_eq!(rc, rpi_plugin_sdk::EVENT_HANDLER_CONTINUE);
        assert!(
            AUTO_TALK_ENABLED.load(Ordering::Relaxed),
            "the loop cancelled itself on its own injected draft"
        );

        handle_editor_change(r#"{"chars":0,"empty":true}"#);
        assert!(
            AUTO_TALK_ENABLED.load(Ordering::Relaxed),
            "clearing the command editor must not pause continuous mode"
        );

        handle_editor_change(r#"{"chars":6,"empty":false,"source":"user"}"#);
        assert!(
            !AUTO_TALK_ENABLED.load(Ordering::Relaxed),
            "typing must pause continuous mode"
        );

        // Absent `source` is read as the user: the safe direction, since the
        // worst case is a stopped voice rather than a mangled draft.
        AUTO_TALK_ENABLED.store(true, Ordering::Relaxed);
        handle_editor_change(r#"{"chars":1,"empty":false}"#);
        assert!(!AUTO_TALK_ENABLED.load(Ordering::Relaxed));

        // A keystroke that pauses the mode claims no key and never vetoes.
        AUTO_TALK_ENABLED.store(false, Ordering::Relaxed);
        assert_eq!(
            handle_editor_change("not json at all"),
            rpi_plugin_sdk::EVENT_HANDLER_CONTINUE
        );
    }

    /// The walk-away guard: quit after N empty turns, and never accept a limit
    /// that would exit before the first listen even starts.
    #[test]
    fn empty_turn_limit_defaults_and_overrides() {
        let previous = std::env::var("RPI_VOICE_AUTO_EMPTY_MAX").ok();
        std::env::remove_var("RPI_VOICE_AUTO_EMPTY_MAX");
        assert_eq!(auto_talk_max_empty(), 3);
        std::env::set_var("RPI_VOICE_AUTO_EMPTY_MAX", "0");
        assert_eq!(auto_talk_max_empty(), 3, "0 would never listen at all");
        std::env::set_var("RPI_VOICE_AUTO_EMPTY_MAX", "5");
        assert_eq!(auto_talk_max_empty(), 5);
        std::env::set_var("RPI_VOICE_AUTO_EMPTY_MAX", "junk");
        assert_eq!(auto_talk_max_empty(), 3);
        match previous {
            Some(value) => std::env::set_var("RPI_VOICE_AUTO_EMPTY_MAX", value),
            None => std::env::remove_var("RPI_VOICE_AUTO_EMPTY_MAX"),
        }
    }

    /// A hands-free turn bounds how long it waits for the user to *start*
    /// talking; without it a silent room would hold the mic for the full cap.
    #[test]
    fn auto_turn_waits_for_speech_then_gives_up() {
        let previous = std::env::var("RPI_VOICE_NO_SPEECH_MS").ok();
        let previous_warmup = std::env::var("RPI_VOICE_WARMUP_MS").ok();
        std::env::remove_var("RPI_VOICE_NO_SPEECH_MS");
        std::env::remove_var("RPI_VOICE_WARMUP_MS");
        let params = recorder::RecordParams::for_auto_turn();
        assert_eq!(params.no_speech_ms, Some(10_000));
        assert_eq!(params.max_ms, 60_000);
        assert_eq!(params.warmup_ms, 800);
        // Silence auto-stop after real speech is unchanged.
        assert!(params.silence_ms > 0);

        std::env::set_var("RPI_VOICE_NO_SPEECH_MS", "2500");
        assert_eq!(
            recorder::RecordParams::for_auto_turn().no_speech_ms,
            Some(2_500)
        );

        // The one-shot `/voice` path keeps its old behaviour: no early give-up.
        std::env::remove_var("RPI_VOICE_NO_SPEECH_MS");
        std::env::set_var("RPI_VOICE_WARMUP_MS", "2500");
        assert_eq!(recorder::RecordParams::for_auto_turn().warmup_ms, 2_500);
        std::env::remove_var("RPI_VOICE_WARMUP_MS");
        assert_eq!(recorder::RecordParams::from_env().no_speech_ms, None);

        match previous {
            Some(value) => std::env::set_var("RPI_VOICE_NO_SPEECH_MS", value),
            None => std::env::remove_var("RPI_VOICE_NO_SPEECH_MS"),
        }
        match previous_warmup {
            Some(value) => std::env::set_var("RPI_VOICE_WARMUP_MS", value),
            None => std::env::remove_var("RPI_VOICE_WARMUP_MS"),
        }
    }

    /// Barge-in flags the utterance that is playing and consumes the handle, so
    /// a second interrupt reports "nothing was playing" instead of lying.
    #[test]
    fn barge_in_stops_the_current_utterance_exactly_once() {
        let _guard = PLAYBACK_TEST_LOCK.lock().unwrap();
        *PLAYBACK_STOP.lock().unwrap() = None;
        assert!(!stop_playback(), "idle: nothing to stop");

        let flag = Arc::new(AtomicBool::new(false));
        *PLAYBACK_STOP.lock().unwrap() = Some(flag.clone());
        PLAYBACK_LEVEL.store(0.5f32.to_bits(), Ordering::Relaxed);

        assert!(stop_playback(), "a playing utterance must report a stop");
        assert!(
            flag.load(Ordering::Relaxed),
            "the player's flag must be set"
        );
        assert_eq!(
            f32::from_bits(PLAYBACK_LEVEL.load(Ordering::Relaxed)),
            0.0,
            "the meter must drop to zero immediately"
        );
        // Handle consumed ⇒ never flag a later, unrelated utterance.
        assert!(!stop_playback());
        *PLAYBACK_STOP.lock().unwrap() = None;
    }

    /// A barge-in must discard replies that are still *queued*, not just the one
    /// already on the speakers.
    ///
    /// Regression: only the playing utterance was stoppable, so a reply that had
    /// queued up behind it began playing a moment later — the interrupt looked
    /// ignored, and in hands-free mode the microphone was already open, so the
    /// assistant transcribed its own voice.
    #[test]
    fn a_barge_in_supersedes_queued_utterances() {
        let _guard = PLAYBACK_TEST_LOCK.lock().unwrap();
        let before = PLAYBACK_GENERATION.load(Ordering::Relaxed);
        // Reported "nothing playing" is fine — the queue still has to go.
        let _ = stop_playback();
        let after = PLAYBACK_GENERATION.load(Ordering::Relaxed);
        assert_eq!(
            after,
            before + 1,
            "every barge-in must bump the generation, even when silent"
        );
    }

    /// An utterance queued before a barge-in must not be spoken afterwards,
    /// while one queued after it must be.
    #[test]
    fn only_jobs_from_the_current_generation_are_speakable() {
        let _guard = PLAYBACK_TEST_LOCK.lock().unwrap();
        let queued_at = PLAYBACK_GENERATION.load(Ordering::Relaxed);
        let stale = SpeechJob {
            text: "queued first".to_string(),
            generation: queued_at,
        };
        let _ = stop_playback(); // the user interrupts
        let fresh = SpeechJob {
            text: "said after the interrupt".to_string(),
            generation: PLAYBACK_GENERATION.load(Ordering::Relaxed),
        };

        let current = PLAYBACK_GENERATION.load(Ordering::Relaxed);
        assert_ne!(
            stale.generation, current,
            "the queued reply must be recognisable as superseded"
        );
        assert_eq!(
            fresh.generation, current,
            "a reply queued after the barge-in must still be spoken"
        );
    }

    /// Speaking while the microphone is open would feed the assistant's own
    /// voice into the recogniser.
    /// Speaking while the microphone is open would feed the assistant's own
    /// voice into the recogniser.
    #[test]
    fn the_microphone_owns_the_floor_while_recording() {
        // `with_ptt_state` is the shared lock for the process-global PTT state;
        // it resets on both sides, so this cannot leak into the other PTT tests
        // (which is exactly what happened when this test managed its own lock).
        with_ptt_state(|| {
            RECORDING.store(false, Ordering::Relaxed);
            assert!(!mic_owns_the_floor(), "idle: the floor is free");

            RECORDING.store(true, Ordering::Relaxed);
            assert!(
                mic_owns_the_floor(),
                "an open microphone must block playback"
            );
            RECORDING.store(false, Ordering::Relaxed);

            // A held key that has not reached the threshold yet still owns the
            // floor — `RECORDING` is not set until the hold elapses, but the key
            // is already claimed, so it must disable the editor too.
            PTT.lock().unwrap().pressed_at = Some(std::time::Instant::now());
            assert!(
                mic_owns_the_floor(),
                "a held push-to-talk key must block playback"
            );
        });
        assert!(!mic_owns_the_floor(), "state must be clean after the guard");
    }

    /// A transcription lands in the editor as an auto-sending draft by default;
    /// only an explicit `send` (env or `/voice output send`) bypasses it.
    #[test]
    fn output_mode_defaults_to_draft_and_send_is_opt_in() {
        let previous = OUTPUT_OVERRIDE.lock().unwrap().take();
        std::env::remove_var("RPI_VOICE_OUTPUT");
        assert_eq!(output_mode(), OutputMode::Draft);

        std::env::set_var("RPI_VOICE_OUTPUT", "send");
        assert_eq!(output_mode(), OutputMode::Send);
        // An unrecognised value falls back to the safe (editable) default.
        std::env::set_var("RPI_VOICE_OUTPUT", "banana");
        assert_eq!(output_mode(), OutputMode::Draft);

        // The session override wins over the environment.
        std::env::set_var("RPI_VOICE_OUTPUT", "send");
        *OUTPUT_OVERRIDE.lock().unwrap() = Some(OutputMode::Draft);
        assert_eq!(output_mode(), OutputMode::Draft);

        std::env::remove_var("RPI_VOICE_OUTPUT");
        *OUTPUT_OVERRIDE.lock().unwrap() = previous;
    }

    /// The auto-send window is 2s by default and can be disabled (`0`) so the
    /// draft waits for a manual Enter.
    #[test]
    fn draft_auto_send_defaults_to_two_seconds() {
        std::env::remove_var("RPI_VOICE_DRAFT_MS");
        assert_eq!(draft_auto_send_ms(), 2000);
        std::env::set_var("RPI_VOICE_DRAFT_MS", "0");
        assert_eq!(draft_auto_send_ms(), 0);
        std::env::set_var("RPI_VOICE_DRAFT_MS", "4500");
        assert_eq!(draft_auto_send_ms(), 4500);
        std::env::set_var("RPI_VOICE_DRAFT_MS", "not-a-number");
        assert_eq!(draft_auto_send_ms(), 2000);
        std::env::remove_var("RPI_VOICE_DRAFT_MS");
    }

    #[test]
    fn sanitize_strips_markdown_keeps_prose() {
        let input = "# Title\n\nHello **world** and `code`.\n\n```rust\nlet x = 1;\n```\n\n[link](https://x.y) end";
        let out = sanitize_for_speech(input);
        assert_eq!(out, "Title Hello world and code. link end");
    }

    #[test]
    fn extract_only_text_parts() {
        let msg = json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "secret"},
                {"type": "text", "text": "spoken"},
                {"type": "toolCall", "name": "bash"}
            ]
        });
        assert_eq!(extract_message_text(&msg).trim(), "spoken");
    }

    #[test]
    fn kinds_and_roles_both_detected() {
        assert_eq!(
            json!({"role": "assistant"})
                .get("role")
                .and_then(Value::as_str),
            Some("assistant")
        );
    }

    /// Live check of the Edge TTS endpoint. Runs only when `RPI_VOICE_TEST_TTS`
    /// is set (network + the undocumented Microsoft websocket).
    #[test]
    fn edge_tts_synthesizes_mp3() {
        if std::env::var("RPI_VOICE_TEST_TTS").is_err() {
            return;
        }
        // Streaming path: record when the first chunk lands, since that is the
        // number the whole change exists to improve.
        let t0 = std::time::Instant::now();
        let mut first_at = None;
        let mut chunks = 0usize;
        let mut total = 0usize;
        let (end, bytes) = crate::edge_tts::synthesize_stream(
            "你好，这是语音合成测试。",
            "zh-CN-XiaoxiaoNeural",
            "+0%",
            "+0Hz",
            "+0%",
            &|| false,
            |chunk| {
                if first_at.is_none() {
                    first_at = Some(t0.elapsed());
                }
                chunks += 1;
                total += chunk.len();
            },
        )
        .expect("edge tts synthesize_stream");
        let done = t0.elapsed();
        eprintln!(
            "tts stream: first={:?} done={:?} chunks={chunks} bytes={total} (reported {bytes}) end={end:?}",
            first_at.unwrap_or_default(),
            done
        );
        assert_eq!(end, crate::edge_tts::StreamEnd::Complete);
        assert_eq!(total, bytes);
        assert!(total > 2_000, "suspiciously small mp3: {total}");
        assert!(
            first_at.unwrap() < done,
            "streaming must deliver audio before the utterance completes"
        );
    }

    /// Guards the streaming decoder's codec: a hand-built 1-frame MPEG-1 Layer
    /// III stream must probe and decode rather than be rejected. (The player no
    /// longer uses rodio's `Decoder` — it decodes through symphonia directly so
    /// the first chunk can play before the utterance ends — but the codec must
    /// stay registered either way.)
    #[test]
    fn rodio_decodes_mp3() {
        // 0xFF 0xFB = MPEG-1 Layer III, 128 kbps, 44.1 kHz, no padding.
        let mut mp3 = Vec::new();
        for _ in 0..40 {
            mp3.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            mp3.extend_from_slice(&[0u8; 413]);
        }
        assert!(
            rodio::Decoder::new(std::io::Cursor::new(mp3)).is_ok(),
            "rodio was built without the `mp3` feature"
        );
    }

    // -- push-to-talk key routing -------------------------------------------

    const CLAIMED: i32 = rpi_plugin_sdk::EVENT_HANDLER_CLAIMED;
    const CONTINUE: i32 = rpi_plugin_sdk::EVENT_HANDLER_CONTINUE;

    fn key_json(key: &str, kind: &str) -> String {
        json!({"type": "key", "key": key, "kind": kind,
               "ctrl": false, "alt": false, "shift": false})
        .to_string()
    }

    /// PTT state is process-global; serialize the tests that touch it.
    fn with_ptt_state<R>(f: impl FnOnce() -> R) -> R {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ptt_reset();
        PTT_ENABLED.store(false, Ordering::Relaxed);
        let out = f();
        ptt_reset();
        PTT_ENABLED.store(false, Ordering::Relaxed);
        out
    }

    /// Drive `/voice` the way the host does: args in, output out.
    fn run_voice_command(args: &str) -> (i32, String) {
        let args_json = serde_json::json!({"args": args}).to_string();
        let mut out = StbString::empty();
        let rc = voice_command(StbStringRef::from_str(&args_json), &mut out, std::ptr::null_mut());
        (rc, unsafe { out.to_string_lossy() })
    }

    #[test]
    fn ptt_turns_speech_on_not_just_recording() {
        // The reported bug: `/voice ptt on` recorded and transcribed, but the
        // reply was never spoken, so the mode looked broken. PTT is a spoken
        // conversation, so enabling it must enable speech.
        with_ptt_state(|| {
            AUTO_TTS_ENABLED.store(false, Ordering::Relaxed);
            let (rc, text) = run_voice_command("ptt on");
            assert_eq!(rc, 0);
            assert!(
                AUTO_TTS_ENABLED.load(Ordering::Relaxed),
                "/voice ptt on must enable auto-TTS; output was: {text}"
            );
            // The user has to be told, since the switch is otherwise invisible.
            assert!(text.contains("Auto-TTS was off"), "output was: {text}");
        });
    }

    #[test]
    fn auto_tts_is_off_until_explicitly_enabled() {
        // The whole "/voice but no reading" confusion: recording and speech are
        // separate switches, and speech must stay off unless asked for.
        with_ptt_state(|| {
            AUTO_TTS_ENABLED.store(false, Ordering::Relaxed);
            assert!(!AUTO_TTS_ENABLED.load(Ordering::Relaxed));
        });
    }

    #[test]
    fn ptt_ignores_unrelated_keys_and_payloads() {
        with_ptt_state(|| {
            PTT_ENABLED.store(true, Ordering::Relaxed);
            // A different key is never ours.
            assert_eq!(handle_ptt_key(&key_json("a", "press"), "space"), CONTINUE);
            // Not a key payload at all.
            assert_eq!(
                handle_ptt_key(&json!({"type": "mouse"}).to_string(), "space"),
                CONTINUE
            );
            // Malformed JSON must not panic or claim.
            assert_eq!(handle_ptt_key("not json", "space"), CONTINUE);
            // An unknown kind is not ours.
            assert_eq!(
                handle_ptt_key(&key_json("space", "weird"), "space"),
                CONTINUE
            );
        });
    }

    #[test]
    fn ptt_declines_while_disabled_so_the_key_still_types() {
        with_ptt_state(|| {
            // Disabled: even the configured key passes through to the editor.
            assert_eq!(
                handle_ptt_key(&key_json("space", "press"), "space"),
                CONTINUE
            );
            assert_eq!(
                handle_ptt_key(&key_json("space", "release"), "space"),
                CONTINUE
            );
        });
    }

    #[test]
    fn ptt_claims_press_and_release_while_enabled() {
        with_ptt_state(|| {
            PTT_ENABLED.store(true, Ordering::Relaxed);
            assert_eq!(
                handle_ptt_key(&key_json("space", "press"), "space"),
                CLAIMED
            );
            // Repeats while held are swallowed too.
            assert_eq!(
                handle_ptt_key(&key_json("space", "repeat"), "space"),
                CLAIMED
            );
            assert_eq!(
                handle_ptt_key(&key_json("space", "release"), "space"),
                CLAIMED
            );
            // A release without a matching press is not ours.
            assert_eq!(
                handle_ptt_key(&key_json("space", "release"), "space"),
                CONTINUE
            );
        });
    }

    #[test]
    fn ptt_leaves_modifier_chords_to_the_editor() {
        with_ptt_state(|| {
            PTT_ENABLED.store(true, Ordering::Relaxed);
            let ctrl_space = json!({"type": "key", "key": "space", "kind": "press",
                                    "ctrl": true, "alt": false, "shift": false})
            .to_string();
            assert_eq!(handle_ptt_key(&ctrl_space, "space"), CONTINUE);
            let alt_space = json!({"type": "key", "key": "space", "kind": "press",
                                   "ctrl": false, "alt": true, "shift": false})
            .to_string();
            assert_eq!(handle_ptt_key(&alt_space, "space"), CONTINUE);
            // Shift+space is still a deliberate hold.
            let shift_space = json!({"type": "key", "key": "space", "kind": "press",
                                     "ctrl": false, "alt": false, "shift": true})
            .to_string();
            assert_eq!(handle_ptt_key(&shift_space, "space"), CLAIMED);
        });
    }

    #[test]
    fn ptt_release_marks_a_recording_for_sending() {
        with_ptt_state(|| {
            PTT_ENABLED.store(true, Ordering::Relaxed);
            handle_ptt_key(&key_json("space", "press"), "space");
            // Simulate the hold timer having opened the mic.
            let stop = Arc::new(AtomicBool::new(false));
            {
                let mut ptt = PTT.lock().unwrap();
                ptt.stop = Some(stop.clone());
                ptt.recording_owner = Some(ptt.token);
            }
            let owner = PTT.lock().unwrap().token;
            assert_eq!(
                handle_ptt_key(&key_json("space", "release"), "space"),
                CLAIMED
            );
            // Release stops the capture and flags it to be sent.
            assert!(stop.load(Ordering::Relaxed), "release must stop recording");
            assert!(
                ptt_finish(owner),
                "release must mark the recording for sending"
            );
            // State is clean for the next hold.
            assert!(PTT.lock().unwrap().pressed_at.is_none());
        });
    }

    /// A recording thread that finishes *after* a newer press took over must
    /// still send its own utterance, yet must not clear that newer press's
    /// state — otherwise the user's eventual release falls through to the
    /// editor as a typed space.
    #[test]
    fn stale_recording_does_not_clear_a_newer_press() {
        with_ptt_state(|| {
            PTT_ENABLED.store(true, Ordering::Relaxed);

            // First hold: the mic is open (recording owned by `first`).
            handle_ptt_key(&key_json("space", "press"), "space");
            let first = {
                let mut ptt = PTT.lock().unwrap();
                ptt.stop = Some(Arc::new(AtomicBool::new(false)));
                ptt.recording_owner = Some(ptt.token);
                ptt.token
            };
            // Released: the first utterance is marked to be sent.
            handle_ptt_key(&key_json("space", "release"), "space");

            // Second press begins before the first thread has torn down.
            handle_ptt_key(&key_json("space", "press"), "space");
            let second = PTT.lock().unwrap().token;
            assert_ne!(first, second);

            // The stale thread finishes: it still sends *its own* utterance…
            assert!(ptt_finish(first), "first utterance should still be sent");
            // …but must leave the newer press alone.
            assert!(
                PTT.lock().unwrap().pressed_at.is_some(),
                "stale finish wiped the newer press"
            );

            // The newer release is still handled as a real hold (the mic it
            // never opened sends nothing, but the key is claimed, not typed).
            assert_eq!(
                handle_ptt_key(&key_json("space", "release"), "space"),
                CLAIMED
            );
            assert!(PTT.lock().unwrap().pressed_at.is_none());
            // And the send intent did not leak into the second press.
            assert!(PTT.lock().unwrap().send_tokens.is_empty());
        });
    }

    #[test]
    fn hold_status_fills_toward_the_threshold() {
        // Just pressed: empty bar, full remaining time.
        let s = hold_status(0, 2000);
        assert!(s.contains("2.0s to talk"), "{s}");
        assert!(s.contains('▱'), "{s}");
        assert!(!s.contains('▰'), "should be empty at t=0: {s}");

        // Halfway: half full, 1.0s left.
        let s = hold_status(1000, 2000);
        assert!(s.contains("1.0s to talk"), "{s}");
        assert!(s.contains('▰') && s.contains('▱'), "{s}");

        // At/past the threshold: full bar, nothing left.
        let s = hold_status(2000, 2000);
        assert!(s.contains("0.0s to talk"), "{s}");
        assert!(!s.contains('▱'), "should be full: {s}");
        // Overshoot never overflows the bar.
        let s = hold_status(9999, 2000);
        assert_eq!(s.matches('▰').count(), 12, "{s}");
        assert!(!s.contains('▱'), "{s}");
    }

    #[test]
    fn listening_status_tracks_the_mic_level() {
        // Silence still shows a floor cell so the line never looks dead.
        let quiet = listening_status(0.0, 0, ListenHint::Release, None, 0);
        assert!(quiet.contains("0.0s"), "{quiet}");
        assert_eq!(quiet.matches('█').count(), 1, "{quiet}");
        assert!(quiet.contains("release to send"), "{quiet}");

        // Speaking widens the bar.
        let loud = listening_status(1.0, 1500, ListenHint::Release, None, 0);
        assert_eq!(loud.matches('█').count(), 12, "{loud}");
        assert!(loud.contains("1.5s"), "{loud}");

        // Out-of-range input is clamped, not panicking or overflowing.
        let over = listening_status(9.9, 0, ListenHint::Release, None, 0);
        assert_eq!(over.matches('█').count(), 12, "{over}");
        let under = listening_status(-1.0, 0, ListenHint::Release, None, 0);
        assert_eq!(under.matches('█').count(), 1, "{under}");
    }

    /// The same meter serves every recording path; only the trailing hint
    /// differs, because push-to-talk ends on release while hands-free ends on
    /// silence. Getting this wrong would tell a hands-free user to "release" a
    /// key they are not holding.
    #[test]
    fn listening_hint_matches_how_the_turn_ends() {
        let ptt = listening_status(0.5, 100, ListenHint::Release, None, 0);
        assert!(ptt.contains("release to send"), "{ptt}");
        assert!(!ptt.contains("silence"), "{ptt}");

        let hands_free = listening_status(0.5, 100, ListenHint::Silence, None, 0);
        assert!(hands_free.contains("silence ends the turn"), "{hands_free}");
        assert!(!hands_free.contains("release"), "{hands_free}");

        let warming = listening_status(0.5, 1_000, ListenHint::Silence, Some(15_000), 4_000);
        assert!(warming.contains("warming microphone"), "{warming}");
        let natural = listening_status(0.5, 5_000, ListenHint::Silence, Some(15_000), 4_000);
        assert!(natural.contains("speak naturally"), "{natural}");

        // Both render the identical level meter.
        let bars = |s: &str| s.chars().filter(|c| *c == '█' || *c == '░').count();
        assert_eq!(bars(&ptt), bars(&hands_free));
        assert_eq!(bars(&ptt), 12);
    }

    #[test]
    fn hold_defaults_and_overrides() {
        std::env::remove_var("RPI_VOICE_PTT_HOLD_MS");
        assert_eq!(ptt_hold_ms(), PTT_HOLD_DEFAULT_MS);
        std::env::set_var("RPI_VOICE_PTT_HOLD_MS", "3000");
        assert_eq!(ptt_hold_ms(), 3000);
        // Garbage / zero falls back to the default rather than "hold forever".
        std::env::set_var("RPI_VOICE_PTT_HOLD_MS", "0");
        assert_eq!(ptt_hold_ms(), PTT_HOLD_DEFAULT_MS);
        std::env::set_var("RPI_VOICE_PTT_HOLD_MS", "abc");
        assert_eq!(ptt_hold_ms(), PTT_HOLD_DEFAULT_MS);
        std::env::remove_var("RPI_VOICE_PTT_HOLD_MS");
    }
}
