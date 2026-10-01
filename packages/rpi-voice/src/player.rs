//! Audio playback via rodio.
//!
//! Plays MP3 audio data through the default output device.
//! Supports cancellation via a stop flag, and publishes a live loudness level
//! so the host UI can animate a music-style equalizer while speech plays.

use cpal::traits::{DeviceTrait, HostTrait};
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, Sink};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

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

// ---------------------------------------------------------------------------
// Streaming playback
// ---------------------------------------------------------------------------

/// Feed for the streaming MP3 decoder: a pull-based [`MediaSource`] backed by a
/// channel the network thread pushes chunks into.
///
/// symphonia wants a blocking `Read`; Edge TTS delivers over a websocket on
/// another thread. The channel bridges the two, and turns "the producer is
/// slow" into a real block (rather than an `UnexpectedEof` that a truncated
/// read would report). `None` from the channel means the stream ended — either
/// the utterance completed or the caller cancelled it.
struct ChunkSource {
    /// `Arc<Mutex<_>>`: `MediaSource` demands `Send + Sync`, and a bare
    /// `Receiver` is only `Send`. Only the decoding thread ever touches it, so
    /// the lock is uncontended.
    rx: Arc<std::sync::Mutex<Receiver<Vec<u8>>>>,
    current: Vec<u8>,
    pos: usize,
    finished: bool,
}

impl ChunkSource {
    fn new(rx: Receiver<Vec<u8>>) -> Self {
        Self {
            rx: Arc::new(std::sync::Mutex::new(rx)),
            current: Vec::new(),
            pos: 0,
            finished: false,
        }
    }
}

impl std::io::Read for ChunkSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.pos < self.current.len() {
                let n = (self.current.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if self.finished {
                return Ok(0);
            }
            let next = self
                .rx
                .lock()
                .map_err(|_| std::io::Error::other("chunk channel poisoned"))?
                .recv();
            match next {
                Ok(chunk) => {
                    self.current = chunk;
                    self.pos = 0;
                }
                // Producer hung up: a normal end-of-stream as far as the decoder
                // is concerned (symphonia tolerates a stream that stops at a
                // packet boundary, which MP3's framing guarantees).
                Err(_) => {
                    self.finished = true;
                    return Ok(0);
                }
            }
        }
    }
}

impl std::io::Seek for ChunkSource {
    /// Symphonia's `MediaSource` is a `Read + Seek` composite even though a
    /// stream may be unseekable. [`Self::is_seekable`] reports `false`, so this
    /// is never reached; it exists to satisfy the bound and errors loudly if a
    /// future caller seeks anyway.
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::other(
            "Edge TTS audio is a network stream and cannot be seeked",
        ))
    }
}

impl MediaSource for ChunkSource {
    /// The socket stream is not rewindable, so symphonia must not seek: it
    /// decodes forward from the first packet. Returning `false` here is what
    /// makes it read the stream start-to-finish instead of trying to seek to
    /// the end for a duration or a Xing header.
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Play MP3 chunks **as they arrive**.
///
/// Unlike [`play_mp3`], this does not need the whole clip: decoding starts on
/// the first chunk, so the caller hears the first word while the rest of the
/// utterance is still in flight. That is the difference between "wait for the
/// whole synthesis" and "wait for the first packet".
///
/// `stop` is honoured both while waiting for a chunk and once playback has
/// begun (the audio is dropped and the sink stopped). `level` receives the RMS
/// of each interleaved chunk, as in [`play_mp3`].
///
/// Returns `Ok(true)` when the whole stream was played, `Ok(false)` when `stop`
/// was set first.
/// Resolve the output device to play through.
///
/// `RPI_VOICE_OUTPUT_DEVICE` takes a case-insensitive substring of the device
/// name; without it the system default is used. This exists because the
/// Windows default output is frequently a **monitor** (HDMI/DisplayPort) or a
/// Bluetooth headset that is powered off — audio then plays into a device the
/// user is not listening to, which is indistinguishable from a broken player.
/// Matching is by substring so `RPI_VOICE_OUTPUT_DEVICE=Realtek` is enough.
fn output_device() -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    let want = std::env::var("RPI_VOICE_OUTPUT_DEVICE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(want) = want else {
        return host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string());
    };
    let needle = want.to_lowercase();
    let devices = host
        .output_devices()
        .map_err(|e| format!("cannot enumerate output devices: {e}"))?;
    let mut available = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        if name.to_lowercase().contains(&needle) {
            return Ok(device);
        }
        available.push(name);
    }
    Err(format!(
        "no output device matches `{want}`; available: {}",
        if available.is_empty() {
            "(none)".to_string()
        } else {
            available.join(", ")
        }
    ))
}

/// The output device speech will use, for `/voice status`.
pub fn active_output_device() -> String {
    match output_device() {
        Ok(d) => {
            let name = d.name().unwrap_or_else(|_| "(unnamed)".to_string());
            let source = if std::env::var("RPI_VOICE_OUTPUT_DEVICE").is_ok() {
                "RPI_VOICE_OUTPUT_DEVICE"
            } else {
                "system default"
            };
            format!("{name}   [{source}]")
        }
        Err(e) => format!("⚠ {e}"),
    }
}

pub fn play_mp3_stream(
    on_chunk: impl FnOnce(&mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String>
        + Send
        + 'static,
    stop: Arc<AtomicBool>,
    level: &AtomicU32,
) -> Result<bool, String> {
    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = std::sync::mpsc::channel();
    let producer_stop = stop.clone();

    // The producer pulls MP3 bytes (from the network) into the channel; the
    // decoder on this thread consumes them as they land. Dropping `tx` when the
    // closure returns is what signals end-of-stream — for completion and for
    // cancellation alike.
    let produced = std::thread::spawn(move || -> Result<(), String> {
        let result = on_chunk(&mut |chunk: &[u8]| {
            if producer_stop.load(Ordering::Relaxed) {
                // A cancelled utterance is not an error: report it as a clean
                // early stop so the caller can distinguish it from a failure.
                return Err(CANCELLED.to_string());
            }
            tx.send(chunk.to_vec())
                .map_err(|_| "playback consumer went away".to_string())
        });
        drop(tx);
        result
    });

    let outcome = decode_and_play(rx, &stop, level);

    let produce_result = produced
        .join()
        .map_err(|_| "TTS producer thread panicked".to_string())?;
    // A stop requested mid-synthesis surfaces as CANCELLED; that is expected.
    match produce_result {
        Err(e) if e == CANCELLED => {}
        other => other?,
    }
    outcome
}

/// Sentinel for "the caller asked us to stop", so a cancelled stream is not
/// reported as a failure.
pub(crate) const CANCELLED: &str = "__rpi_voice_cancelled";

/// Decode the chunk stream with symphonia and play it through the default device.
fn decode_and_play(
    rx: Receiver<Vec<u8>>,
    stop: &AtomicBool,
    level: &AtomicU32,
) -> Result<bool, String> {
    let device = output_device().map_err(|e| format!("Audio output error: {e}"))?;
    let (_stream, stream_handle) = OutputStream::try_from_device(&device)
        .map_err(|e| format!("Audio output error on `{}`: {e}", device.name().unwrap_or_default()))?;
    let sink = Sink::try_new(&stream_handle).map_err(|e| format!("Sink error: {e}"))?;

    let source = ChunkSource::new(rx);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("MP3 probe error: {e}"))?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| "MP3 stream has no track".to_string())?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("MP3 decoder error: {e}"))?;

    let mut spec: Option<SignalSpec> = None;
    let mut buffer: Option<SampleBuffer<f32>> = None;
    let mut cancelled = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean end of stream (`IoError` at a packet boundary is how a
            // closed channel surfaces in symphonia).
            Err(SymphoniaError::IoError(_)) => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("MP3 read error: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A truncated tail is expected when the utterance was cancelled.
            Err(SymphoniaError::IoError(_)) => break,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("MP3 decode error: {e}")),
        };

        let decoded_spec = *decoded.spec();
        if spec != Some(decoded_spec) {
            spec = Some(decoded_spec);
            // Capacity in frames; `copy_interleaved_ref` grows within it.
            buffer = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, decoded_spec));
        }
        let Some(buf) = buffer.as_mut() else {
            continue;
        };
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        if samples.is_empty() {
            continue;
        }
        let channels = decoded_spec.channels.count();
        if channels == 0 {
            continue;
        }
        level.store(level_bits(samples), Ordering::Relaxed);
        let frames = samples.len() / channels;
        sink.append(SamplesBuffer::new(
            channels as u16,
            decoded_spec.rate,
            samples.to_vec(),
        ));
        // Pace the feed so `level` tracks what is actually being heard instead
        // of racing the whole utterance into the device buffer.
        let ms = (frames as u64 * 1000) / decoded_spec.rate.max(1) as u64;
        let mut slept = 0u64;
        while slept < ms {
            if stop.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let step = (ms - slept).min(20);
            std::thread::sleep(Duration::from_millis(step));
            slept += step;
        }
        if cancelled {
            break;
        }
    }

    if cancelled || stop.load(Ordering::Relaxed) {
        sink.stop();
        level.store(0, Ordering::Relaxed);
        return Ok(false);
    }

    while !sink.empty() {
        if stop.load(Ordering::Relaxed) {
            sink.stop();
            level.store(0, Ordering::Relaxed);
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    level.store(0, Ordering::Relaxed);
    Ok(true)
}
