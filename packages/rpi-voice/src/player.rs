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
use std::time::{Duration, Instant};

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

    #[test]
    fn the_meter_releases_levels_over_time_not_all_at_once() {
        // The meter must show each value for as long as its audio sounds.
        // Regression: the player used to get this by sleeping between packets,
        // which also throttled the decode feed and made playback stutter.
        // Pacing lives here now, so this is what keeps it honest.
        let level = AtomicU32::new(0);
        let stop = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel::<(u32, u64)>();
        // Three 100 ms packets.
        for bits in [1u32, 2, 3] {
            tx.send((bits, 100_000)).unwrap();
        }
        drop(tx);

        let start = Instant::now();
        run_level_meter(rx, &level, &stop);
        let elapsed = start.elapsed();

        // 3 x 100 ms of audio cannot be shown in less than ~300 ms.
        assert!(
            elapsed >= Duration::from_millis(280),
            "meter released 300 ms of audio in {elapsed:?} — it is not paced"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "meter took {elapsed:?} for 300 ms of audio — it is over-sleeping"
        );
        assert_eq!(level.load(Ordering::Relaxed), 0, "meter must clear the level");
    }

    #[test]
    fn the_meter_stops_promptly_when_asked() {
        let level = AtomicU32::new(0);
        let stop = AtomicBool::new(true); // already stopping
        let (tx, rx) = std::sync::mpsc::channel::<(u32, u64)>();
        tx.send((1u32, 60_000_000)).unwrap(); // 60 s of audio
        drop(tx);

        let start = Instant::now();
        run_level_meter(rx, &level, &stop);
        // Must not wait out the 60 s slice.
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "meter ignored stop for {:?}",
            start.elapsed()
        );
        assert_eq!(level.load(Ordering::Relaxed), 0);
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
/// of each decoded packet, published in step with playback rather than decode.
///
/// Returns `Ok(true)` when the whole stream was played, `Ok(false)` when `stop`
/// was set first.
///
/// Decoded audio is handed to the sink **as fast as it decodes**, and the
/// level meter runs on its own thread paced by elapsed time. An earlier version
/// instead slept one packet's worth of audio after each packet, meaning to keep
/// the meter in step with what was audible; because that sleep came *on top of*
/// the decode and allocation time it ran systematically slower than real time,
/// draining the device buffer between packets. Measured over a 2.4 s utterance
/// the feed took 2.48 s — an audible stutter on every reply. rodio's mixer
/// buffers far ahead on its own, so feeding it eagerly is both simpler and
/// correct.
///
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

    // The meter runs on its own thread: decoding queues levels, and the meter
    // releases each one only once its audio has had time to play. Decoupling
    // them is what lets the feed below run flat out — see the module note on
    // `play_mp3_stream` for why that matters.
    let (level_tx, level_rx) = std::sync::mpsc::channel::<(u32, u64)>();
    let meter = spawn_level_meter(level_rx, level, stop);

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
        let packet = match format_next_packet(&mut format) {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => return Err(e),
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

        // Feed the sink immediately. rodio buffers far ahead of the device, so
        // there is nothing to gain by pacing this — and pacing it (as an
        // earlier version did) runs slower than real time and stutters.
        let frames = samples.len() / channels;
        let rate = decoded_spec.rate.max(1);
        sink.append(SamplesBuffer::new(
            channels as u16,
            decoded_spec.rate,
            samples.to_vec(),
        ));
        let packet_us = (frames as u64 * 1_000_000) / rate as u64;
        let _ = level_tx.send((level_bits(samples), packet_us));
    }

    // Let the meter release anything still queued.
    drop(level_tx);
    if let Some(meter) = meter {
        let _ = meter.join();
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

/// Pull the next packet, treating a closed channel as a clean end of stream.
fn format_next_packet(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
) -> Result<Option<symphonia::core::formats::Packet>, String> {
    match format.next_packet() {
        Ok(p) => Ok(Some(p)),
        // `IoError` at a packet boundary is how a closed channel surfaces.
        Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::ResetRequired) => Ok(None),
        Err(e) => Err(format!("MP3 read error: {e}")),
    }
}

/// Start the meter thread, returning its handle when it could be spawned.
fn spawn_level_meter(
    rx: Receiver<(u32, u64)>,
    level: &AtomicU32,
    stop: &AtomicBool,
) -> Option<std::thread::JoinHandle<()>> {
    // SAFETY: both references outlive the utterance — they belong to the
    // caller's stack frame, which does not return until this thread is joined
    // (see the `drop(level_tx)`/`join` pair above).
    let level: &'static AtomicU32 = unsafe { std::mem::transmute(level) };
    let stop: &'static AtomicBool = unsafe { std::mem::transmute(stop) };
    std::thread::Builder::new()
        .name("rpi-voice-meter".to_string())
        .spawn(move || run_level_meter(rx, level, stop))
        .ok()
}

/// Publish queued RMS values at the rate they are actually heard.
///
/// `rx` yields `(level_bits, packet_duration_us)` in decode order. Decoding
/// finishes long before playback does, so this shows each value for the
/// duration its audio occupies — tracking "what is audible now" rather than
/// "what has been decoded". Exits when `rx` closes or `stop` is set, and
/// always leaves the level at zero.
fn run_level_meter(
    rx: Receiver<(u32, u64)>,
    level: &AtomicU32,
    stop: &AtomicBool,
) {
    let started = Instant::now();
    let mut due = Duration::ZERO;
    while let Ok((bits, packet_us)) = rx.recv() {
        // Show the slice that is sounding now, once it is actually due.
        if wait_until(started + due, stop) {
            break;
        }
        level.store(bits, Ordering::Relaxed);
        due += Duration::from_micros(packet_us);
    }
    // Hold the final value until its own audio has finished, so the last packet
    // is not wiped early, then clear.
    let _ = wait_until(started + due, stop);
    level.store(0, Ordering::Relaxed);
}

/// Sleep until `deadline`, returning `true` if `stop` was set first.
fn wait_until(deadline: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(20)));
    }
}
