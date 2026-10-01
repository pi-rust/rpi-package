//! Embedded offline STT via sherpa-onnx — **SenseVoice** model.
//!
//! No API key, no server, no network at transcription time, and (with the
//! `static` feature) no runtime DLL to ship next to the extension. SenseVoice
//! (`zh-en-ja-ko-yue`) is a strong Chinese/English recognizer with automatic
//! language detection and inverse text normalization (punctuation/numbers).
//!
//! Model files are resolved under `<models-dir>/sense-voice/`:
//! - `model.int8.onnx` (int8-quantized, ~230 MB)
//! - `tokens.txt`
//!
//! `<models-dir>` is `$RPI_CODING_AGENT_DIR/models`, else `~/.rpi/agent/models`.
//! A missing file is downloaded from Hugging Face, falling back to
//! `hf-mirror.com` when huggingface.co is unreachable.

use std::path::PathBuf;

use sherpa_rs::sense_voice::{SenseVoiceConfig, SenseVoiceRecognizer};

const SUBDIR: &str = "sense-voice";
const MODEL_FILE: &str = "model.int8.onnx";
const TOKENS_FILE: &str = "tokens.txt";

const HF_BASE: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main";
const HF_MIRROR_BASE: &str =
    "https://hf-mirror.com/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main";

/// Directory that holds downloaded models.
pub fn models_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RPI_CODING_AGENT_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir).join("models");
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".rpi")
        .join("agent")
        .join("models")
}

/// Directory holding the SenseVoice files.
pub fn model_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RPI_STT_MODEL_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    models_dir().join(SUBDIR)
}

pub fn model_path() -> PathBuf {
    model_dir().join(MODEL_FILE)
}

pub fn tokens_path() -> PathBuf {
    model_dir().join(TOKENS_FILE)
}

/// Download any missing model files. `progress` receives short status strings.
/// Returns the directory once both files are present.
pub fn ensure_model<F: Fn(&str)>(progress: F) -> Result<PathBuf, String> {
    let dir = model_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create model dir {}: {e}", dir.display()))?;

    for file in [MODEL_FILE, TOKENS_FILE] {
        let dest = dir.join(file);
        if dest.is_file() {
            continue;
        }
        // A `tokens.txt` next to a user-supplied model dir may be missing; download.
        let mut last_err = String::new();
        let mut ok = false;
        for base in [HF_BASE, HF_MIRROR_BASE] {
            let url = format!("{base}/{file}");
            progress(&format!("downloading {file}…"));
            match download(&url, &dest, &progress) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => {
                    last_err = format!("{url}: {e}");
                    let _ = std::fs::remove_file(&dest);
                }
            }
        }
        if !ok {
            return Err(format!(
                "model download failed ({last_err}). Set RPI_STT_MODEL_DIR to a directory \
                 containing {MODEL_FILE} and {TOKENS_FILE}"
            ));
        }
    }
    Ok(dir)
}

fn download<F: Fn(&str)>(url: &str, dest: &std::path::Path, progress: &F) -> Result<(), String> {
    use std::io::{Read, Write};

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| e.to_string())?;

    let mut resp = client.get(url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let total = resp.content_length();

    // Write to a temp file, then rename into place (crash-safe).
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 1 << 20];
    let mut done: u64 = 0;
    let mut last_report = std::time::Instant::now();
    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last_report.elapsed().as_secs() >= 2 {
            last_report = std::time::Instant::now();
            match total {
                Some(t) => progress(&format!(
                    "downloading… {:.0}% ({:.0}/{:.0} MB)",
                    done as f64 * 100.0 / t as f64,
                    done as f64 / 1e6,
                    t as f64 / 1e6
                )),
                None => progress(&format!("downloading… {:.0} MB", done as f64 / 1e6)),
            }
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

/// A process-wide recognizer, loaded once and reused.
///
/// Loading is by far the expensive part — measured on the int8 model: **5.5 s
/// to load, 0.03 s to transcribe**. Doing it at transcription time means every
/// first utterance eats that 5.5 s *after* the user stops talking, which is
/// exactly the wrong moment. So the load starts in the background as soon as
/// voice is switched on, and transcription waits here for it.
static ENGINE: std::sync::OnceLock<std::sync::Mutex<Result<SenseVoice, String>>> =
    std::sync::OnceLock::new();

/// Load the recognizer now, in this thread, and cache it.
///
/// Idempotent and cheap after the first call. Returns the load error (as a
/// string) if the model is missing, so callers can fall back to the API.
pub fn preload() -> Result<(), String> {
    let slot = ENGINE.get_or_init(|| std::sync::Mutex::new(SenseVoice::load()));
    match &*slot.lock().unwrap() {
        Ok(_) => Ok(()),
        Err(e) => Err(e.clone()),
    }
}

/// Get the shared recognizer, loading it if `preload` has not run yet.
pub fn engine() -> Result<std::sync::MutexGuard<'static, Result<SenseVoice, String>>, String> {
    preload()?;
    Ok(ENGINE.get().expect("just initialised").lock().unwrap())
}

/// Start loading the recognizer on a background thread, unless it already is
/// (or has been) loaded. Never blocks; errors surface at [`engine`] time.
///
/// Call this when voice is switched on so that the ~5.5 s load overlaps with
/// the user talking instead of following it.
pub fn preload_in_background() {
    if ENGINE.get().is_some() {
        return;
    }
    std::thread::Builder::new()
        .name("rpi-voice-stt-load".to_string())
        .spawn(|| {
            if let Err(e) = preload() {
                eprintln!("[rpi-voice] STT preload failed: {e}");
            }
        })
        .ok();
}

/// Whether a recognizer is loaded and ready to transcribe right now.
pub fn is_ready() -> bool {
    matches!(ENGINE.get(), Some(slot) if slot.lock().unwrap().is_ok())
}

/// Loaded SenseVoice recognizer.
pub struct SenseVoice {
    inner: SenseVoiceRecognizer,
}

impl SenseVoice {
    /// Load the recognizer from the resolved model files.
    pub fn load() -> Result<Self, String> {
        let model = model_path();
        let tokens = tokens_path();
        if !model.is_file() || !tokens.is_file() {
            return Err(format!(
                "model files missing in {} (run /voice model download)",
                model_dir().display()
            ));
        }

        let language = std::env::var("RPI_VOICE_STT_LANG")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "auto".to_string());
        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(2)
            .clamp(1, 8);

        let config = SenseVoiceConfig {
            model: model.to_string_lossy().into_owned(),
            tokens: tokens.to_string_lossy().into_owned(),
            language,
            use_itn: true,
            provider: None,
            num_threads: Some(num_threads),
            debug: false,
        };

        let inner =
            SenseVoiceRecognizer::new(config).map_err(|e| format!("load SenseVoice: {e}"))?;
        Ok(Self { inner })
    }

    /// Transcribe 16kHz mono f32 samples.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String, String> {
        let result = self.inner.transcribe(16_000, samples);
        Ok(strip_tags(result.text.trim()))
    }
}

/// Remove any residual `<|lang|>`/`<|emotion|>`/`<|event|>` tags.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' if !in_tag => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_tags_removes_special_tokens() {
        assert_eq!(strip_tags("<|zh|><|NEUTRAL|>你好 world"), "你好 world");
    }

    /// End-to-end check against a real 16 kHz mono WAV. Runs only when
    /// `RPI_VOICE_TEST_WAV` is set and the model is present.
    #[test]
    fn transcribes_sample_wav() {
        let Ok(path) = std::env::var("RPI_VOICE_TEST_WAV") else {
            return;
        };
        let mut reader = hound::WavReader::open(&path).expect("open wav");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1, "test wav must be mono");
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.expect("sample") as f32 / 32768.0)
            .collect();
        assert_eq!(spec.sample_rate, 16_000, "test wav must be 16 kHz");

        let mut engine = SenseVoice::load().expect("load SenseVoice (model downloaded?)");
        let text = engine.transcribe(&samples).expect("transcribe");
        eprintln!("transcript: {text}");
        assert!(!text.trim().is_empty(), "empty transcript");
    }
}
