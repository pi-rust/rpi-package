//! Microphone recording via cpal.
//!
//! Records audio from the default input device until either the caller's `stop`
//! flag is set, a period of trailing silence is detected (push-to-talk without a
//! key hook), or a hard cap is reached. Returns raw PCM samples at the device's
//! native sample rate.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// RMS above which a frame counts as speech (i16 scale). Most laptop/headset
/// mics put ambient noise well below this while speech sits above it.
/// Absolute floor for "speech", in i16 RMS units (of 32767).
///
/// Deliberately low: the adaptive floor below does the real work. A *high*
/// fixed floor is what made a perfectly good quiet microphone look dead — the
/// measured ambient on a normal USB mic is around RMS 30, so the old `260`
/// threshold meant an ordinary speaking voice never registered as speech at
/// all, and hands-free mode concluded nobody was there.
const VOICE_RMS_ABS_MIN: f64 = 80.0;

/// A frame counts as speech when it is this many times the measured ambient
/// floor (≈ +10 dB SNR).
const SPEECH_NOISE_RATIO: f64 = 3.0;

/// Ambient frames that train the noise floor. Kept short so the floor is usable
/// within the first fraction of a second of a turn.
const NOISE_EMA_WEIGHT: f64 = 0.1;

/// The RMS a frame must reach to count as speech, given the ambient estimate.
/// Pure so the rule is testable.
fn speech_threshold(noise_rms: f64) -> f64 {
    (noise_rms * SPEECH_NOISE_RATIO).max(VOICE_RMS_ABS_MIN)
}

/// Upper bound on a fixed [`GainPref::Fixed`]: past this the amplified noise
/// floor is worse than the quiet signal it was meant to rescue.
const MAX_INPUT_GAIN: f32 = 100.0;

/// Above this share of clipped samples the recording is damaged enough that a
/// recogniser will likely fail, and the cause is the gain — not the microphone.
pub const CLIPPING_WARN_RATIO: f32 = 0.02;

/// How the input is levelled before speech recognition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GainPref {
    /// Multiply every sample by a fixed factor, from the moment of capture.
    Fixed(f32),
    /// Leave capture alone and normalise each recording towards
    /// [`STT_TARGET_PEAK`] on the way to STT.
    Auto,
}

/// Peak a normalised recording aims for: loud enough for a recogniser, low
/// enough to keep headroom.
const STT_TARGET_PEAK: f32 = 0.7;

/// Ceiling on automatic normalisation. Beyond this the boosted noise floor and
/// clipping artefacts cost more accuracy than the extra loudness buys.
const MAX_AUTO_GAIN: f32 = 40.0;

/// Resolve the gain setting: the environment variable wins (for a one-off
/// test), then the persisted setting, then [`GainPref::Auto`].
///
/// `auto` is the default because a quiet microphone is a *very* common real
/// world case and it silently costs transcription accuracy — the failure looks
/// like "speech recognition is bad", not "your microphone is quiet".
fn input_gain_pref() -> GainPref {
    let raw = std::env::var("RPI_VOICE_INPUT_GAIN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            load_config()
                .get("input_gain")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    match raw.as_deref() {
        Some(value) if value.eq_ignore_ascii_case("auto") => GainPref::Auto,
        Some(value) => match value.parse::<f32>() {
            Ok(gain) if gain.is_finite() && gain > 1.0 => GainPref::Fixed(gain.min(MAX_INPUT_GAIN)),
            // `1`, `0`, garbage → leave the signal alone.
            _ => GainPref::Fixed(1.0),
        },
        None => GainPref::Auto,
    }
}

/// Persist a gain setting (`auto`, or a factor).
pub fn set_input_gain_pref(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.eq_ignore_ascii_case("auto") {
        save_config("input_gain", "auto")?;
        return Ok("auto (normalise each recording for STT)".to_string());
    }
    let gain: f32 = value
        .parse()
        .map_err(|_| format!("'{value}' is not a number or 'auto'"))?;
    if !gain.is_finite() || gain < 1.0 {
        return Err(format!("gain must be >= 1 (got '{value}')"));
    }
    let gain = gain.min(MAX_INPUT_GAIN);
    save_config("input_gain", &format!("{gain}"))?;
    Ok(format!("x{gain:.0}"))
}

/// Human-readable current gain setting.
pub fn gain_pref_label() -> String {
    match input_gain_pref() {
        GainPref::Auto => "auto (normalise for STT)".to_string(),
        GainPref::Fixed(gain) if gain <= 1.0 => "off".to_string(),
        GainPref::Fixed(gain) => format!("x{gain:.0}"),
    }
}

/// Recording behaviour (all durations in milliseconds).
#[derive(Clone, Copy, Debug)]
pub struct RecordParams {
    /// Hard cap on recording length; always applied.
    pub max_ms: u64,
    /// Stop once this much trailing silence follows detected speech. `0` disables
    /// silence auto-stop (record until `max_ms` or the caller's stop flag).
    pub silence_ms: u64,
    /// Minimum amount of speech required before silence auto-stop can trigger.
    pub min_speech_ms: u64,
    /// Give up when *no* speech has been heard at all within this long.
    ///
    /// Without it, a silent microphone records until `max_ms` — fine when a
    /// human deliberately pressed a key, disastrous for a hands-free loop,
    /// which would hold the mic for the full cap on every empty turn. `None`
    /// keeps that original behaviour (one-shot `/voice`).
    pub no_speech_ms: Option<u64>,
    /// Initial microphone wake-up period; it does not consume no_speech_ms.
    pub warmup_ms: u64,
}

impl Default for RecordParams {
    fn default() -> Self {
        Self {
            max_ms: 20_000,
            silence_ms: 1_200,
            min_speech_ms: 500,
            no_speech_ms: None,
            warmup_ms: 0,
        }
    }
}

impl RecordParams {
    /// Read overrides from the environment, falling back to defaults.
    pub fn from_env() -> Self {
        let mut p = Self::default();
        if let Some(v) = env_millis("RPI_VOICE_RECORD_MS") {
            p.max_ms = v.max(500);
        }
        if let Some(v) = env_millis("RPI_VOICE_SILENCE_MS") {
            p.silence_ms = v;
        }
        if let Some(v) = env_millis("RPI_VOICE_MIN_SPEECH_MS") {
            p.min_speech_ms = v;
        }
        if let Some(v) = env_millis("RPI_VOICE_NO_SPEECH_MS") {
            p.no_speech_ms = Some(v);
        }
        if let Some(v) = env_millis("RPI_VOICE_WARMUP_MS") {
            p.warmup_ms = v;
        }
        p
    }

    /// Params for one hands-free turn: keep the stream open like PTT, detect
    /// speech when it arrives, then stop after trailing silence.
    ///
    /// The default has no speech-start timeout. This matches PTT's reliable
    /// hold-to-talk boundary while letting the user pause before speaking.
    pub fn for_auto_turn() -> Self {
        let mut p = Self::from_env();
        if std::env::var("RPI_VOICE_RECORD_MS").is_err() {
            // Match PTT's generous safety cap. A speech-start timeout would
            // make a Bluetooth wake-up or a thoughtful pause look like failure.
            p.max_ms = 60_000;
        }
        if std::env::var("RPI_VOICE_WARMUP_MS").is_err() {
            p.warmup_ms = 4_000;
        }
        p
    }
}

fn env_millis(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// Recorded audio samples.
#[derive(Clone)]
pub struct Recording {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Loudest frame observed, `0.0..=1.0` of full scale. A value at the noise
    /// floor means the microphone captured nothing usable — a muted device or
    /// the wrong input — which is a very different problem from the user simply
    /// not having spoken yet.
    pub peak_level: f32,
    /// Milliseconds the VAD classified as speech.
    pub speech_ms: u64,
    /// Which capture endpoint produced this, so a surprising result can be
    /// traced to a surprising device.
    pub device_name: String,
    /// Software input gain that was applied, so a reported level can be read
    /// back against the raw one.
    pub gain: f32,
    /// Fraction of samples (0..=1) the gain pushed past full scale and had to
    /// clamp. Clipping is not a subtle degradation: a heavily clipped signal is a
    /// square wave, and a speech recogniser fed one tends to return nothing at
    /// all — which looks like "the microphone is broken" rather than "the gain
    /// is too high". Published so the tool can say so out loud.
    pub clipped_ratio: f32,
}

impl Recording {
    /// Duration in seconds.
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 || self.channels == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / (self.sample_rate as f64 * self.channels as f64)
    }

    /// Encode as WAV bytes (16-bit PCM).
    /// Resamples to 16kHz mono if needed (for Whisper API).
    pub fn to_wav_bytes(&self) -> Result<Vec<u8>, String> {
        // Resample to 16kHz mono for Whisper, then apply the *same* levelling the
        // local engine gets — otherwise switching STT backend would silently
        // change how much of a quiet microphone survives.
        let gain = self.stt_normalisation_gain();
        let samples_16k: Vec<i16> = self
            .resample_to_16k_mono()
            .into_iter()
            .map(|sample| {
                if gain == 1.0 {
                    sample
                } else {
                    ((sample as f32 * gain).clamp(i16::MIN as f32, i16::MAX as f32)) as i16
                }
            })
            .collect();

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| format!("WAV writer error: {e}"))?;

        for sample in &samples_16k {
            writer
                .write_sample(*sample)
                .map_err(|e| format!("WAV write error: {e}"))?;
        }

        writer
            .finalize()
            .map_err(|e| format!("WAV finalize error: {e}"))?;

        Ok(cursor.into_inner())
    }

    /// 16kHz mono samples as f32 in [-1.0, 1.0] (what whisper.cpp wants).
    #[cfg(feature = "local-stt")]
    pub fn to_f32_16k_mono(&self) -> Vec<f32> {
        let mut samples: Vec<f32> = self
            .resample_to_16k_mono()
            .into_iter()
            .map(|s| s as f32 / 32768.0)
            .collect();
        // Normalise for STT. Speech recognisers are trained on reasonably loud
        // audio, and a microphone delivering 2% of full scale loses a large
        // amount of accuracy even though the words are all there. Applied only
        // to what STT sees — the meter and the VAD keep working on the raw
        // signal, so the diagnostics stay honest.
        let gain = self.stt_normalisation_gain();
        if gain != 1.0 {
            for sample in &mut samples {
                *sample = (*sample * gain).clamp(-1.0, 1.0);
            }
        }
        samples
    }

    /// Gain that lifts this capture towards a healthy level for STT.
    ///
    /// Uses the recording's own peak, so it adapts per utterance: a loud mic is
    /// left alone, a quiet one is lifted. Deliberately NOT used for the level
    /// meter or the voice-activity detector — those must keep reporting the true
    /// signal, or "the microphone is quiet" becomes invisible.
    fn stt_normalisation_gain(&self) -> f32 {
        match input_gain_pref() {
            GainPref::Fixed(gain) => gain,
            GainPref::Auto => {
                if self.peak_level <= 0.0 {
                    return 1.0;
                }
                (STT_TARGET_PEAK / self.peak_level).clamp(1.0, MAX_AUTO_GAIN)
            }
        }
    }

    /// Resample to 16kHz mono using linear interpolation.
    fn resample_to_16k_mono(&self) -> Vec<i16> {
        // First, downmix to mono if needed
        let mono: Vec<i16> = if self.channels == 1 {
            self.samples.clone()
        } else {
            let ch = self.channels as usize;
            self.samples
                .chunks(ch)
                .map(|chunk| {
                    let sum: i32 = chunk.iter().map(|&s| s as i32).sum();
                    (sum / ch as i32) as i16
                })
                .collect()
        };

        // Then resample to 16kHz
        let src_rate = self.sample_rate as f64;
        let target_rate = 16000.0;

        if (src_rate - target_rate).abs() < 1.0 {
            return mono;
        }

        let ratio = target_rate / src_rate;
        let out_len = (mono.len() as f64 * ratio) as usize;
        let mut out = Vec::with_capacity(out_len);

        for i in 0..out_len {
            let src_pos = i as f64 / ratio;
            let idx = src_pos as usize;
            let frac = src_pos - idx as f64;

            if idx + 1 < mono.len() {
                let interpolated =
                    (1.0 - frac) * mono[idx] as f64 + frac * mono[idx + 1] as f64;
                out.push(interpolated as i16);
            } else if idx < mono.len() {
                out.push(mono[idx]);
            }
        }

        out
    }
}

/// Shared voice-activity tracker fed by the input callback.
#[derive(Default)]
struct VoiceActivity {
    /// Instant of the most recent speech frame, if any.
    last_voice: Option<Instant>,
    /// Total milliseconds classified as speech.
    speech_ms: u64,
    /// Loudest single frame seen, as a fraction of full scale. Published with
    /// the recording so "I heard nothing" can be told apart from "the
    /// microphone is not picking anything up at all".
    peak: f32,
    /// Slow average of *quiet* frames only — the ambient floor. Frames already
    /// classified as speech are excluded, so a long utterance cannot drag the
    /// floor up past the voice it is trying to detect.
    noise_rms: f64,
    /// Frames observed, for bootstrapping [`Self::noise_rms`].
    frames: u64,
}

/// Live input level (0.0..=1.0), published by the record loop so a caller can
/// drive a level meter. Cheap to poll: one relaxed atomic load.
///
/// Stored as `level * 10_000` in an integer because there is no `AtomicF32`.
#[derive(Clone, Default)]
pub struct LevelMeter(Arc<std::sync::atomic::AtomicU32>);

impl LevelMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current level, `0.0..=1.0`.
    pub fn get(&self) -> f32 {
        self.0.load(Ordering::Relaxed) as f32 / 10_000.0
    }

    /// Publish a level (clamped to `0.0..=1.0`).
    pub fn set(&self, level: f32) {
        let scaled = (level.clamp(0.0, 1.0) * 10_000.0) as u32;
        self.0.store(scaled, Ordering::Relaxed);
    }
}

/// Converts a callback buffer to i16 and feeds the samples/activity trackers.
type SampleFeed = Box<dyn FnMut(&[i16]) + Send>;

/// Record from the default microphone until `stop` is set, trailing silence is
/// detected, or `params.max_ms` elapses, publishing a live input level to
/// `meter` (for the listening meter) when one is supplied.
///
/// Returns the recorded samples at the device's native sample rate, plus the
/// capture diagnostics (`peak_level`, `speech_ms`) used to explain an empty
/// turn.
pub fn record_until_with_level(
    stop: Arc<AtomicBool>,
    params: RecordParams,
    meter: Option<LevelMeter>,
) -> Result<Recording, String> {
    let host = cpal::default_host();
    let (device, device_name) = select_input_device(&host)?;

    let config = device
        .default_input_config()
        .map_err(|e| format!("Input config error: {e}"))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels();

    let samples: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
    let activity: Arc<Mutex<VoiceActivity>> = Arc::new(Mutex::new(VoiceActivity::default()));
    // Counted inside the callback so a too-high gain can be reported instead of
    // silently destroying the audio.
    let clip_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let total_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop_flag = stop.clone();
    // Bluetooth input devices can emit a short wake-up burst when the stream
    // starts. Keep that audio for STT, but do not let it become the first
    // speech event that arms trailing-silence auto-stop.
    let capture_started = Instant::now();

    let err_fn = |err| eprintln!("Audio stream error: {err}");

    // Resolved once, outside the callback block: the feed closure needs it, and
    // the returned `Recording` reports it back to the caller. Only a *fixed*
    // gain is applied at capture time; `auto` normalises later, for STT only.
    let gain = match input_gain_pref() {
        GainPref::Fixed(gain) => gain,
        GainPref::Auto => 1.0,
    };

    let mut feed: SampleFeed = {
        let samples = samples.clone();
        let activity = activity.clone();
        let stop = stop_flag.clone();
        let meter = meter.clone();
        let channels = channels as usize;
        let sample_rate = sample_rate as f64;
        let clip_counter = clip_counter.clone();
        let total_counter = total_counter.clone();
        let warmup_ms = params.warmup_ms;
        Box::new(move |data: &[i16]| {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            // Apply the input gain once, up front, so every consumer below
            // (meter, VAD, stored samples → STT) sees the same boosted signal.
            // Only allocate when a gain is actually configured.
            let boosted;
            let data: &[i16] = if gain == 1.0 {
                data
            } else {
                let mut clipped = 0usize;
                let out = data
                    .iter()
                    .map(|sample| {
                        let scaled = *sample as f32 * gain;
                        if scaled > i16::MAX as f32 || scaled < i16::MIN as f32 {
                            clipped += 1;
                        }
                        scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16
                    })
                    .collect::<Vec<i16>>();
                if !out.is_empty() {
                    clip_counter.fetch_add(clipped, Ordering::Relaxed);
                    total_counter.fetch_add(out.len(), Ordering::Relaxed);
                }
                boosted = out;
                &boosted
            };
            samples.lock().unwrap().extend_from_slice(data);

            // Publish a live level for the caller's meter. Measured across
            // **every** channel: some USB microphones present the mic on one
            // channel only, and sampling a single channel would then read
            // silence forever on an otherwise healthy device.
            if let Some(meter) = &meter {
                let peak = data
                    .iter()
                    .map(|s| (s.unsigned_abs() as f32) / i16::MAX as f32)
                    .fold(0.0_f32, f32::max);
                meter.set(peak);
            }

            // Classify this callback buffer as speech vs. silence.
            let channels = channels.max(1);
            let frames = data.len() / channels;
            if frames == 0 {
                return;
            }
            // RMS over all interleaved samples. Averaging the channels (rather
            // than trusting channel 0) is what keeps a one-sided mono signal
            // detectable; it costs at most 3 dB, versus missing the voice
            // entirely.
            let sum_sq: f64 = data
                .iter()
                .map(|&s| {
                    let f = s as f64;
                    f * f
                })
                .sum();
            let rms = (sum_sq / data.len() as f64).sqrt();
            let frame_ms = (frames as f64 / sample_rate * 1000.0) as u64;
            {
                let mut a = activity.lock().unwrap();
                let level = (rms / i16::MAX as f64) as f32;
                if level > a.peak {
                    a.peak = level;
                }
                let in_warmup = (capture_started.elapsed().as_millis() as u64) < warmup_ms;
                if !in_warmup && rms >= speech_threshold(a.noise_rms) {
                    a.last_voice = Some(Instant::now());
                    a.speech_ms = a.speech_ms.saturating_add(frame_ms);
                } else {
                    // Warm-up frames are always quiet for VAD purposes, so a
                    // Bluetooth wake-up burst cannot arm the trailing timer.
                    // They still teach the ambient floor for the first real
                    // speech frame after warm-up.
                    a.noise_rms = if a.frames == 0 {
                        rms
                    } else {
                        a.noise_rms * (1.0 - NOISE_EMA_WEIGHT) + rms * NOISE_EMA_WEIGHT
                    };
                }
                a.frames = a.frames.saturating_add(1);
            }
        })
    };

    let stream = match config.sample_format() {
        cpal::SampleFormat::I16 => {
            let mut feed = feed;
            device.build_input_stream(
                &config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| feed(data),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::F32 => {
            device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let i16_data: Vec<i16> = data
                        .iter()
                        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                    feed(&i16_data);
                },
                err_fn,
                None,
            )
        }
        fmt => return Err(format!("Unsupported sample format: {fmt:?}")),
    };

    let stream = stream.map_err(|e| format!("Build input stream error: {e}"))?;
    stream.play().map_err(|e| format!("Play stream error: {e}"))?;

    let started = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let elapsed = started.elapsed();
        if elapsed.as_millis() as u64 >= params.max_ms {
            break;
        }

        if params.silence_ms > 0 {
            let elapsed_ms = elapsed.as_millis() as u64;
            let wait_elapsed_ms = elapsed_ms.saturating_sub(params.warmup_ms);
            // End once enough speech has been heard and it has been quiet since.
            let (speech_ms, last_voice) = {
                let a = activity.lock().unwrap();
                (a.speech_ms, a.last_voice)
            };
            // No *recent* voice at all ⇒ the user is not there. Phrased as
            // "quiet for N ms" rather than "never spoke", so neither an early
            // cough (which would otherwise pin this open for the whole turn) nor
            // a mic sitting just under the speech floor (which would otherwise
            // cut real speech off) can decide it the wrong way.
            if let Some(no_speech_ms) = params.no_speech_ms {
                let quiet_ms = last_voice
                    .map(|t| t.elapsed().as_millis() as u64)
                    .unwrap_or(wait_elapsed_ms);
                if quiet_ms >= no_speech_ms {
                    break;
                }
            }
            if speech_ms >= params.min_speech_ms {
                let silence = last_voice.map(|t| t.elapsed()).unwrap_or(elapsed);
                if silence.as_millis() as u64 >= params.silence_ms {
                    break;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(20));
    }

    // Small delay to capture trailing samples.
    std::thread::sleep(Duration::from_millis(50));
    drop(stream);

    let final_samples = samples.lock().unwrap().clone();
    let (peak_level, speech_ms) = {
        let a = activity.lock().unwrap();
        (a.peak, a.speech_ms)
    };
    let clipped = clip_counter.load(Ordering::Relaxed);
    let total = total_counter.load(Ordering::Relaxed);
    let clipped_ratio = if total == 0 {
        0.0
    } else {
        clipped as f32 / total as f32
    };
    Ok(Recording {
        samples: final_samples,
        sample_rate,
        channels,
        peak_level,
        speech_ms,
        device_name,
        gain,
        clipped_ratio,
    })
}

/// Per-user voice settings file, so a device choice survives a restart without
/// having to launch rpi through an environment variable.
///
/// `RPI_VOICE_CONFIG` overrides the location (also what the tests use, so they
/// never touch a real user's settings).
fn config_path() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("RPI_VOICE_CONFIG") {
        return Some(std::path::PathBuf::from(explicit));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(
        std::path::PathBuf::from(home)
            .join(".rpi")
            .join("agent")
            .join("voice.json"),
    )
}

/// Read the persisted settings (`{}` when absent, unreadable or corrupt — a
/// broken config must never stop voice from working).
fn load_config() -> serde_json::Value {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Merge one key into the persisted settings.
fn save_config(key: &str, value: &str) -> Result<std::path::PathBuf, String> {
    let path = config_path().ok_or("no HOME/USERPROFILE to store settings under")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut config = load_config();
    config[key] = serde_json::Value::String(value.to_string());
    std::fs::write(&path, config.to_string()).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The configured input-device selector: the environment variable wins (for a
/// one-off test), otherwise the persisted setting.
fn input_device_pref() -> Option<String> {
    std::env::var("RPI_VOICE_INPUT_DEVICE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            load_config()
                .get("input_device")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        })
}

/// Persist a capture-device selector. Validated against the devices that
/// actually exist, so a typo is reported now instead of silently falling back
/// to the wrong microphone on every future run.
pub fn set_input_device_pref(selector: &str) -> Result<String, String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err("empty selector".to_string());
    }
    let names = list_input_devices();
    let matched = names
        .iter()
        .find(|name| name.to_ascii_lowercase().contains(&selector.to_ascii_lowercase()));
    let Some(matched) = matched else {
        return Err(format!(
            "no input device matches '{selector}'. Known: {}",
            if names.is_empty() {
                "<none>".to_string()
            } else {
                names.join(", ")
            }
        ));
    };
    save_config("input_device", matched)?;
    Ok(matched.clone())
}

/// Every capture endpoint's name, in enumeration order.
pub fn list_input_devices() -> Vec<String> {
    match cpal::default_host().input_devices() {
        Ok(devices) => devices
            .filter_map(|device| device.name().ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Name of the system default capture endpoint, if any.
pub fn system_default_device() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .and_then(|device| device.name().ok())
}

/// The selector currently in force (env or persisted), for `/voice status`.
pub fn configured_device_pref() -> Option<String> {
    input_device_pref()
}

/// Choose which capture endpoint to record from.
///
/// A machine with a headset *and* a webcam has several capture endpoints, and
/// the Windows default is often not the one the user is actually talking into —
/// the result is a recording of the room while the user speaks into a device
/// nobody is listening to, which is indistinguishable from "the mic is broken"
/// until you look. `RPI_VOICE_INPUT_DEVICE` takes a case-insensitive substring
/// of the device name to disambiguate; otherwise the system default is used.
///
/// Returns the device and its name, so callers can report which one was used.
fn select_input_device(host: &cpal::Host) -> Result<(cpal::Device, String), String> {
    let wanted = input_device_pref().map(|value| value.to_ascii_lowercase());

    if let Some(wanted) = wanted {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                let name = device.name().unwrap_or_default();
                if name.to_ascii_lowercase().contains(&wanted) {
                    return Ok((device, name));
                }
            }
        }
        // Not fatal, but must not be silent: a selector that quietly does
        // nothing costs an hour of "why is it still the wrong microphone".
        let fallback = default_input_device(host)?;
        eprintln!(
            "[rpi-voice] input device selector '{wanted}' matched no device; \
             using '{}' instead",
            fallback.1
        );
        return Ok(fallback);
    }

    default_input_device(host)
}

/// The system default capture endpoint, with its name.
fn default_input_device(host: &cpal::Host) -> Result<(cpal::Device, String), String> {
    let device = host
        .default_input_device()
        .ok_or_else(|| "No input device (microphone) found".to_string())?;
    let name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
    Ok((device, name))
}

/// Human-readable description of the device that would be recorded from, for
/// `/voice status` — the first thing to check when voice input "hears nothing".
pub fn active_input_device() -> String {
    match select_input_device(&cpal::default_host()) {
        Ok((_, name)) => name,
        Err(error) => format!("<none: {error}>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that mutate process-global *environment variables*.
    ///
    /// `std::env::set_var` is process-wide while Rust runs tests on parallel
    /// threads: two tests pointing `RPI_VOICE_CONFIG` at different temp files
    /// read each other's settings and fail intermittently. Serialising them is
    /// the only correct fix — and a wrong-but-green test here would hide a real
    /// regression in settings precedence.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());


    /// Manual diagnostic for "voice input hears nothing".
    ///
    /// Run it and **speak while it records**:
    /// `cargo test -p rpi-voice --lib -- --ignored --nocapture mic_probe`
    ///
    /// It uses the exact same code path as the extension, so its verdict is
    /// authoritative: a peak of exactly `0.00000` means the device handed us
    /// digital silence (muted, wrong default device, or held exclusively by
    /// another app) — a device problem, not a voice-plugin problem.
    #[test]
    #[ignore = "needs a microphone and a human to speak"]
    fn mic_probe_reports_what_the_default_input_delivers() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let host = cpal::default_host();
        println!("--- input devices ---");
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                let name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
                let cfg = device.default_input_config().map(|c| {
                    format!("{}Hz {}ch {:?}", c.sample_rate().0, c.channels(), c.sample_format())
                });
                println!("  - {name}  [{}]", cfg.unwrap_or_else(|e| format!("no config: {e}")));
            }
        }
        match host.default_input_device() {
            Some(device) => println!(
                "DEFAULT INPUT: {}",
                device.name().unwrap_or_else(|_| "<unnamed>".to_string())
            ),
            None => {
                println!("DEFAULT INPUT: <none>");
                return;
            }
        }

        let params = RecordParams {
            max_ms: 4_000,
            silence_ms: 0,
            min_speech_ms: 0,
            no_speech_ms: None,
            warmup_ms: 0,
        };
        let stop = Arc::new(AtomicBool::new(false));
        println!("--- recording 4s: SPEAK NOW ---");
        let recording = record_until_with_level(stop, params, None).expect("recording");
        let rms = {
            let sum_sq: f64 = recording.samples.iter().map(|s| {
                let f = *s as f64;
                f * f
            }).sum();
            if recording.samples.is_empty() {
                0.0
            } else {
                (sum_sq / recording.samples.len() as f64).sqrt()
            }
        };
        println!(
            "samples={} rate={} ch={} dur={:.2}s",
            recording.samples.len(),
            recording.sample_rate,
            recording.channels,
            recording.duration_secs()
        );
        println!(
            "rms={:.2} (of {})",
            rms,
            i16::MAX
        );
        report_capture(&recording, "default");
        if recording.peak_level == 0.0 {
            println!("VERDICT: the device delivered pure digital silence (muted / wrong device).");
        } else if recording.speech_ms == 0 {
            println!(
                "VERDICT: audio arrived (peak {:.4}) but stayed under the speech threshold.",
                recording.peak_level
            );
        } else {
            println!("VERDICT: speech was detected — capture works.");
        }
    }

    /// Peak per channel, so a device whose microphone lands on only one channel
    /// (a classic USB mic quirk) is visible instead of looking deaf.
    fn per_channel_peak(recording: &Recording) -> Vec<f32> {
        let channels = (recording.channels as usize).max(1);
        (0..channels)
            .map(|channel| {
                recording
                    .samples
                    .iter()
                    .skip(channel)
                    .step_by(channels)
                    .map(|s| (s.unsigned_abs() as f32) / i16::MAX as f32)
                    .fold(0.0_f32, f32::max)
            })
            .collect()
    }

    /// Loudest frame in each second of the recording — a level-over-time profile.
    /// A single tall second means the microphone *did* catch something, which
    /// rules out "the device is dead" even when the average looks like silence.
    fn per_second_peak(recording: &Recording) -> Vec<f32> {
        let channels = (recording.channels as usize).max(1);
        let per_second = (recording.sample_rate as usize * channels).max(1);
        recording
            .samples
            .chunks(per_second)
            .map(|second| {
                second
                    .iter()
                    .map(|s| (s.unsigned_abs() as f32) / i16::MAX as f32)
                    .fold(0.0_f32, f32::max)
            })
            .collect()
    }

    /// Write the capture to a playable WAV. Nothing settles "did the microphone
    /// hear me?" faster than listening to the recording.
    fn write_probe_wav(recording: &Recording, tag: &str) -> Result<std::path::PathBuf, String> {
        let path = std::env::temp_dir().join(format!("rpi-voice-probe-{tag}.wav"));
        let spec = hound::WavSpec {
            channels: recording.channels,
            sample_rate: recording.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
        for sample in &recording.samples {
            writer.write_sample(*sample).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Everything worth knowing about one capture, in a form a human can act on.
    fn report_capture(recording: &Recording, tag: &str) {
        let channels = per_channel_peak(recording);
        let per_second = per_second_peak(recording);
        println!(
            "    used='{}'  {:.2}s  {}Hz {}ch",
            recording.device_name,
            recording.duration_secs(),
            recording.sample_rate,
            recording.channels
        );
        println!(
            "    peak={:.4}  speech={}ms  per-channel peak={:?}",
            recording.peak_level,
            recording.speech_ms,
            channels
                .iter()
                .map(|p| format!("{p:.4}"))
                .collect::<Vec<_>>()
        );
        println!(
            "    level per second={:?}",
            per_second
                .iter()
                .map(|p| format!("{p:.3}"))
                .collect::<Vec<_>>()
        );
        match write_probe_wav(recording, tag) {
            Ok(path) => println!("    LISTEN: {}", path.display()),
            Err(error) => println!("    (could not write wav: {error})"),
        }
    }

    /// Capture on a freshly spawned thread, exactly as the extension does.
    ///
    /// The extension *never* records on its "main" thread: push-to-talk and the
    /// hands-free loop both spawn a worker, and the real host is a TUI process
    /// rather than `cargo test`. If WASAPI behaves differently there (COM
    /// apartment, stream activation), the extension would record digital silence
    /// while the identical code on the test thread records perfectly — which is
    /// exactly the symptom being chased. Comparing the two in one process is the
    /// only way to tell them apart.
    #[test]
    #[ignore = "needs a microphone"]
    fn mic_probe_on_a_spawned_thread_matches_the_main_thread() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let params = || RecordParams {
            max_ms: 3_000,
            silence_ms: 0,
            min_speech_ms: 0,
            no_speech_ms: None,
            warmup_ms: 0,
        };

        println!("--- main thread ---");
        let on_main =
            record_until_with_level(Arc::new(AtomicBool::new(false)), params(), None).unwrap();
        println!(
            "    peak={:.4} samples={} device='{}'",
            on_main.peak_level,
            on_main.samples.len(),
            on_main.device_name
        );

        println!("--- spawned thread (what the extension does) ---");
        let handle = std::thread::Builder::new()
            .name("probe-worker".to_string())
            .spawn(move || {
                record_until_with_level(Arc::new(AtomicBool::new(false)), params(), None)
            })
            .unwrap();
        let on_worker = handle.join().unwrap().unwrap();
        println!(
            "    peak={:.4} samples={} device='{}'",
            on_worker.peak_level,
            on_worker.samples.len(),
            on_worker.device_name
        );

        if on_main.peak_level > 0.0 && on_worker.peak_level == 0.0 {
            println!("VERDICT: capture only works on the main thread — the extension's worker thread is the bug.");
        } else if on_worker.peak_level == 0.0 {
            println!("VERDICT: both silent — nothing is reaching the microphone right now.");
        } else {
            println!("VERDICT: both captured audio — the worker thread is fine.");
        }
    }

    /// Compare every capture endpoint, one window at a time, so it becomes
    /// obvious which device actually hears you.
    ///
    /// `cargo test -p rpi-voice --lib -- --ignored --nocapture mic_probe_all`
    ///
    /// **Speak during every window** — each one is announced by name. Because it
    /// drives the real `select_input_device`, this also validates
    /// `RPI_VOICE_INPUT_DEVICE`.
    #[test]
    #[ignore = "needs microphones and a human to speak"]
    fn mic_probe_compares_every_input_device() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let host = cpal::default_host();
        let default = host
            .default_input_device()
            .and_then(|device| device.name().ok());
        println!("system default = {default:?}");

        let names: Vec<String> = match host.input_devices() {
            Ok(devices) => devices.filter_map(|device| device.name().ok()).collect(),
            Err(error) => {
                println!("cannot enumerate input devices: {error}");
                return;
            }
        };
        if names.is_empty() {
            println!("no input devices");
            return;
        }

        let previous = std::env::var("RPI_VOICE_INPUT_DEVICE").ok();
        for (index, name) in names.iter().enumerate() {
            std::env::set_var("RPI_VOICE_INPUT_DEVICE", name);
            println!("\n--- {name} : SPEAK NOW (3s) ---");
            // A Bluetooth headset's microphone takes 3–4s to wake (A2DP → HFP),
            // so a short window measures a device that has not started streaming
            // yet and wrongly calls it dead. Override for a longer look.
            let ms = std::env::var("RPI_VOICE_PROBE_MS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(4_000);
            let params = RecordParams {
                max_ms: ms,
                silence_ms: 0,
                min_speech_ms: 0,
                no_speech_ms: None,
                warmup_ms: 0,
            };
            let stop = Arc::new(AtomicBool::new(false));
            match record_until_with_level(stop, params, None) {
                Ok(recording) => report_capture(&recording, &format!("{index}")),
                Err(error) => println!("    FAILED: {error}"),
            }
        }
        match previous {
            Some(value) => std::env::set_var("RPI_VOICE_INPUT_DEVICE", value),
            None => std::env::remove_var("RPI_VOICE_INPUT_DEVICE"),
        }
        println!("\nPoint the extension at the winner:");
        println!("  RPI_VOICE_INPUT_DEVICE=<substring of that name> rpi");
    }

    /// `RPI_VOICE_INPUT_GAIN` / the persisted setting must be forgiving: `1`,
    /// `0`, a typo, or a `-5` must all mean "leave the signal alone" rather than
    /// silently mangling the audio (a gain of `0` would turn every capture into
    /// pure silence and look like a dead microphone). The default is `auto`,
    /// because a quiet microphone is a common real-world case whose symptom
    /// looks like bad recognition rather than a level problem.
    #[test]
    fn gain_pref_is_forgiving_and_defaults_to_auto() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rpi-voice-gain-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let config = dir.join("voice.json");
        let _ = std::fs::remove_file(&config);

        let previous_config = std::env::var("RPI_VOICE_CONFIG").ok();
        let previous_gain = std::env::var("RPI_VOICE_INPUT_GAIN").ok();
        std::env::set_var("RPI_VOICE_CONFIG", &config);
        std::env::remove_var("RPI_VOICE_INPUT_GAIN");

        assert_eq!(input_gain_pref(), GainPref::Auto, "unset means auto");

        std::env::set_var("RPI_VOICE_INPUT_GAIN", "auto");
        assert_eq!(input_gain_pref(), GainPref::Auto);
        for unchanged in ["1", "1.0", "0", "-5", "junk", "NaN", "inf"] {
            std::env::set_var("RPI_VOICE_INPUT_GAIN", unchanged);
            assert_eq!(
                input_gain_pref(),
                GainPref::Fixed(1.0),
                "`{unchanged}` must leave the signal alone"
            );
        }
        std::env::set_var("RPI_VOICE_INPUT_GAIN", "20");
        assert_eq!(input_gain_pref(), GainPref::Fixed(20.0));
        std::env::set_var("RPI_VOICE_INPUT_GAIN", "100000");
        assert_eq!(input_gain_pref(), GainPref::Fixed(MAX_INPUT_GAIN));

        // Persisted settings round-trip, and a bad value is refused instead of
        // being written and silently ignored forever after.
        std::env::remove_var("RPI_VOICE_INPUT_GAIN");
        assert!(set_input_gain_pref("nonsense").is_err());
        assert!(set_input_gain_pref("0.5").is_err());
        assert_eq!(set_input_gain_pref("auto").unwrap(), "auto (normalise each recording for STT)");
        assert_eq!(input_gain_pref(), GainPref::Auto);
        set_input_gain_pref("12").unwrap();
        assert_eq!(input_gain_pref(), GainPref::Fixed(12.0));

        match previous_config {
            Some(value) => std::env::set_var("RPI_VOICE_CONFIG", value),
            None => std::env::remove_var("RPI_VOICE_CONFIG"),
        }
        match previous_gain {
            Some(value) => std::env::set_var("RPI_VOICE_INPUT_GAIN", value),
            None => std::env::remove_var("RPI_VOICE_INPUT_GAIN"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Automatic normalisation must lift a quiet recording and leave a healthy
    /// one alone — and never amplify digital silence into noise.
    #[test]
    fn auto_gain_lifts_a_quiet_capture_only() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var("RPI_VOICE_INPUT_GAIN").ok();
        std::env::set_var("RPI_VOICE_INPUT_GAIN", "auto");

        let quiet = Recording {
            samples: vec![0; 16],
            sample_rate: 16000,
            channels: 1,
            // Speech on a webcam microphone sits around here.
            peak_level: 0.09,
            speech_ms: 500,
            device_name: "test".to_string(),
            gain: 1.0,
            clipped_ratio: 0.0,
        };
        let gain = quiet.stt_normalisation_gain();
        assert!(gain > 5.0, "a 0.09 peak should be lifted, got {gain}");
        assert!(gain <= MAX_AUTO_GAIN);

        // A healthy capture is left essentially alone — at most a mild lift
        // towards the 0.7 target, with no risk of clipping.
        let healthy = Recording {
            peak_level: 0.6,
            ..quiet.clone()
        };
        let healthy_gain = healthy.stt_normalisation_gain();
        assert!(
            (1.0..1.5).contains(&healthy_gain),
            "a 0.6 peak should barely move, got {healthy_gain}"
        );

        // Digital silence must not be amplified into noise.
        let silent = Recording {
            peak_level: 0.0,
            ..quiet.clone()
        };
        assert_eq!(silent.stt_normalisation_gain(), 1.0);

        // An explicit factor overrides the automatic one.
        std::env::set_var("RPI_VOICE_INPUT_GAIN", "3");
        assert_eq!(quiet.stt_normalisation_gain(), 3.0);

        match previous {
            Some(value) => std::env::set_var("RPI_VOICE_INPUT_GAIN", value),
            None => std::env::remove_var("RPI_VOICE_INPUT_GAIN"),
        }
    }

    /// The device selector must prefer the environment (a one-off override) over
    /// the persisted setting, refuse a typo *before* persisting it, and survive a
    /// corrupt config file — a bad config must never leave voice unusable.
    #[test]
    fn input_device_pref_prefers_env_then_file_and_rejects_typos() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rpi-voice-cfg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let config = dir.join("voice.json");
        let _ = std::fs::remove_file(&config);

        let previous_config = std::env::var("RPI_VOICE_CONFIG").ok();
        let previous_pref = std::env::var("RPI_VOICE_INPUT_DEVICE").ok();
        std::env::set_var("RPI_VOICE_CONFIG", &config);
        std::env::remove_var("RPI_VOICE_INPUT_DEVICE");

        // Nothing configured yet.
        assert_eq!(input_device_pref(), None);

        // A typo is refused *and* the error lists what does exist, so the user is
        // not left guessing at device names.
        let error = set_input_device_pref("definitely-not-a-device").unwrap_err();
        assert!(error.contains("no input device matches"), "{error}");
        assert_eq!(
            input_device_pref(),
            None,
            "a rejected selector must not be persisted"
        );

        // A real device resolves to its full name and round-trips through disk.
        if let Some(existing) = list_input_devices().into_iter().next() {
            assert_eq!(set_input_device_pref(&existing).unwrap(), existing);
            assert_eq!(input_device_pref().as_deref(), Some(existing.as_str()));

            // The environment still wins, for a one-off test.
            std::env::set_var("RPI_VOICE_INPUT_DEVICE", "env-override");
            assert_eq!(input_device_pref().as_deref(), Some("env-override"));
            std::env::remove_var("RPI_VOICE_INPUT_DEVICE");
        }

        // A corrupt settings file is ignored rather than fatal.
        std::fs::write(&config, "{ this is not json").unwrap();
        assert_eq!(input_device_pref(), None);

        match previous_config {
            Some(value) => std::env::set_var("RPI_VOICE_CONFIG", value),
            None => std::env::remove_var("RPI_VOICE_CONFIG"),
        }
        match previous_pref {
            Some(value) => std::env::set_var("RPI_VOICE_INPUT_DEVICE", value),
            None => std::env::remove_var("RPI_VOICE_INPUT_DEVICE"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The threshold must adapt to the microphone, not assume a loud one.
    ///
    /// Regression: a fixed floor of 260 RMS made an ordinary USB microphone
    /// (measured ambient RMS 31) look completely dead — `speech_ms` never
    /// advanced, so hands-free mode reported "heard nothing" on every turn even
    /// though the microphone was working perfectly.
    #[test]
    fn speech_threshold_adapts_to_the_measured_ambient() {
        // A quiet mic (measured ambient RMS 31): the bar sits just above its own
        // floor — and, crucially, far below the old fixed 260.
        let quiet = speech_threshold(31.0);
        assert_eq!(quiet, 31.0 * SPEECH_NOISE_RATIO);
        assert!(
            quiet < 260.0,
            "a quiet microphone must not need 260 RMS to register speech: {quiet}"
        );

        // A loud room raises the bar proportionally, so fan noise isn't speech.
        let noisy = speech_threshold(300.0);
        assert_eq!(noisy, 300.0 * SPEECH_NOISE_RATIO);

        // A silent input still has an absolute floor, so a dead-silent stream
        // (all zeros) does not make digital silence count as speech.
        assert!(speech_threshold(0.0) > 0.0);
    }
}
