//! Small, API-key-free Microsoft Edge TTS client shared by Web and voice paths.

mod drm;
mod edge_tts;

pub use edge_tts::{synthesize_stream, StreamEnd};

/// Synthesize one utterance into an MP3 buffer.
pub fn synthesize(text: &str, voice: &str) -> Result<Vec<u8>, String> {
    let mut audio = Vec::new();
    edge_tts::synthesize_stream(text, voice, "+0%", "+0Hz", "default", &|| false, |chunk| {
        audio.extend_from_slice(chunk)
    })?;
    Ok(audio)
}
