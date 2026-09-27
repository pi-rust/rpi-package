//! Audio playback via rodio.
//!
//! Plays MP3 audio data through the default output device.
//! Supports cancellation via a stop flag, and publishes a live loudness level
//! so the host UI can animate a music-style equalizer while speech plays.

use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStream, Sink, Source};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How much audio is handed to the sink at once. ~60ms is short enough that
/// [`PLAYBACK_LEVEL`](crate::PLAYBACK_LEVEL) tracks what is actually being
/// heard (the animation looks dead if this is a whole utterance) and long
/// enough to keep the per-chunk overhead irrelevant.
const CHUNK_MS: u64 = 60;

/// Play MP3 audio data through the default output device.
///
/// Returns when playback completes, or early (silence) once `stop` is set —
/// that is the barge-in path: a caller flags the playback it wants cut off.
///
/// `level` receives the RMS of each chunk just before it is queued, as `f32`
/// bits, and is left at `0` once playback ends. It exists so the equalizer
/// animation follows the real audio envelope instead of a canned pattern.
pub fn play_mp3(mp3_data: &[u8], stop: Arc<AtomicBool>, level: &AtomicU32) -> Result<(), String> {
    let (_stream, stream_handle) =
        OutputStream::try_default().map_err(|e| format!("Audio output error: {e}"))?;

    let cursor = Cursor::new(mp3_data.to_vec());
    let source = Decoder::new(cursor).map_err(|e| format!("MP3 decode error: {e}"))?;

    // Decode up front so the samples can be sliced into level-instrumented
    // chunks; a short TTS clip is only a few hundred KB of f32. The decoder
    // yields i16, so convert to the normalized f32 rodio plays.
    let channels = source.channels();
    let sample_rate = source.sample_rate();
    let samples: Vec<f32> = source.convert_samples::<f32>().collect();

    let sink = Sink::try_new(&stream_handle).map_err(|e| format!("Sink error: {e}"))?;

    // One chunk ≈ CHUNK_MS of interleaved audio, rounded to a whole frame so a
    // stereo pair is never split across chunks.
    let frames = (sample_rate as u64 * CHUNK_MS / 1000).max(1) as usize;
    let chunk_len = (frames * channels as usize).max(1);

    for chunk in samples.chunks(chunk_len) {
        if stop.load(Ordering::Relaxed) {
            sink.stop();
            level.store(0, Ordering::Relaxed);
            return Ok(());
        }
        level.store(level_bits(chunk), Ordering::Relaxed);
        sink.append(SamplesBuffer::new(
            channels,
            sample_rate,
            chunk.to_vec(),
        ));
        // Feed at real time, so the level we publish corresponds to the audio
        // being consumed rather than racing ahead of the device.
        std::thread::sleep(Duration::from_millis(CHUNK_MS));
    }

    while !sink.empty() && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }

    if stop.load(Ordering::Relaxed) {
        sink.stop();
    } else {
        sink.sleep_until_end();
    }
    level.store(0, Ordering::Relaxed);

    Ok(())
}

/// RMS of one chunk as `f32` bits, clamped to `0.0..=1.0`. Tiny values (room
/// tone, codec noise) are floored to silence so a quiet passage reads as
/// "quiet" rather than as a low buzz.
fn level_bits(chunk: &[f32]) -> u32 {
    if chunk.is_empty() {
        return 0f32.to_bits();
    }
    let sum: f32 = chunk.iter().map(|s| s * s).sum();
    let rms = (sum / chunk.len() as f32).sqrt();
    let normalized = if rms < 0.01 { 0.0 } else { rms.min(1.0) };
    normalized.to_bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_is_zero_for_silence_and_rises_with_amplitude() {
        assert_eq!(f32::from_bits(level_bits(&[0.0; 64])), 0.0);
        // Below the noise floor is treated as silence.
        assert_eq!(f32::from_bits(level_bits(&[0.005; 64])), 0.0);

        let quiet = f32::from_bits(level_bits(&[0.1; 64]));
        let loud = f32::from_bits(level_bits(&[0.8; 64]));
        assert!(quiet > 0.0 && quiet < loud, "{quiet} !< {loud}");
        // Full-scale square wave is 1.0, and never exceeds it.
        assert!((f32::from_bits(level_bits(&[1.0; 64])) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn level_of_an_empty_chunk_is_silence() {
        assert_eq!(f32::from_bits(level_bits(&[])), 0.0);
    }
}
