//! Microsoft Edge TTS — free, no API key required.
//!
//! Implements the WebSocket protocol used by Microsoft Edge's read-aloud feature.
//! Returns MP3 audio bytes (audio-24khz-48kbitrate-mono-mp3).

use futures_util::{SinkExt, StreamExt};
use std::sync::OnceLock;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_tungstenite::{connect_async, tungstenite::Message};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const WSS_URL: &str = "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
const CHROMIUM_MAJOR: &str = "143";
const SEC_MS_GEC_VERSION: &str = "1-143.0.3650.75";
const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
const WIN_EPOCH_SECS: f64 = 11_644_473_600.0;

// ---------------------------------------------------------------------------
// Connection reuse
// ---------------------------------------------------------------------------

/// A process-wide TLS connector.
///
/// The handshake dominates time-to-first-audio (measured: ~0.7-1.6 s against
/// this endpoint, versus ~0.13 s to synthesize). The connector owns the TLS
/// session cache, so building it once lets every later utterance reuse a
/// session instead of negotiating from scratch. `native-tls` (schannel on
/// Windows) keeps that cache internally.
fn tls_connector() -> Option<tokio_tungstenite::Connector> {
    static CONNECTOR: OnceLock<Option<tokio_tungstenite::Connector>> = OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            // `native-tls` is a transitive dependency of the `native-tls`
            // feature; naming it here needs it as a direct dep, so it is
            // declared in Cargo.toml rather than reached through tokio-tungstenite.
            let connector = native_tls::TlsConnector::new().ok()?;
            Some(tokio_tungstenite::Connector::NativeTls(connector))
        })
        .clone()
}

/// Report once, at INFO-ish level through the voice debug log, whether the
/// connector cache is live — the difference between a ~1.3 s and a ~0.2 s
/// time-to-first-audio, so it is worth being able to see.
fn debug_connector_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::debug_log(&format!(
            "edge tts: cached TLS connector = {}",
            tls_connector().is_some()
        ));
    });
}

/// Whether a cached TLS connector is actually in play (diagnostics/tests).
#[allow(dead_code)] // kept for diagnostics; exercised by the connector probe
pub fn has_cached_tls_connector() -> bool {
    tls_connector().is_some()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Why a streaming synthesis stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// The server finished the utterance normally (`Path:turn.end`).
    Complete,
    /// The caller's `should_stop` returned true; playback was abandoned.
    Cancelled,
}

/// Synthesize and hand each MP3 chunk to `on_chunk` **as it arrives**.
///
/// This is the same request as [`synthesize`], except the audio never has to be
/// buffered whole: the caller can start decoding (and playing) the first chunk
/// while the rest is still in flight, and can abandon the utterance mid-stream
/// via `should_stop` — which is checked on every chunk and every poll, so a
/// barge-in during *synthesis* (not just during playback) takes effect.
///
/// `on_chunk` runs on the calling thread inside the runtime; it must not block
/// indefinitely, because it also gates reading the next chunk off the socket.
///
/// Returns how the stream ended, plus the total bytes handed to `on_chunk`. A
/// stream that produced no audio at all is an error, matching [`synthesize`].
pub fn synthesize_stream(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    should_stop: &dyn Fn() -> bool,
    mut on_chunk: impl FnMut(&[u8]),
) -> Result<(StreamEnd, usize), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| format!("Tokio runtime error: {e}"))?;
    rt.block_on(synthesize_stream_async(
        text,
        voice,
        rate,
        pitch,
        volume,
        should_stop,
        &mut on_chunk,
    ))
}

// ---------------------------------------------------------------------------
// Async implementation
// ---------------------------------------------------------------------------

async fn synthesize_stream_async(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    should_stop: &dyn Fn() -> bool,
    on_chunk: &mut dyn FnMut(&[u8]),
) -> Result<(StreamEnd, usize), String> {
    let drm_token = generate_sec_ms_gec();
    let muid = generate_muid();
    let conn_id = uuid::Uuid::new_v4().simple();

    let ws_url = format!(
        "{WSS_URL}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}\
         &Sec-MS-GEC={drm_token}\
         &Sec-MS-GEC-Version={SEC_MS_GEC_VERSION}\
         &ConnectionId={conn_id}"
    );

    // Build from the URI so tungstenite generates the handshake headers
    // (Sec-WebSocket-Key/Version, Host, Connection/Upgrade). Hand-building a
    // `http::Request` skips that and the server rejects the upgrade.
    let mut request = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("Request build error: {e}"))?;
    {
        let headers = request.headers_mut();
        headers.insert("Pragma", HeaderValue::from_static("no-cache"));
        headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
        headers.insert(
            "Origin",
            HeaderValue::from_static("chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold"),
        );
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(&format!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/{CHROMIUM_MAJOR}.0.0.0 Safari/537.36 \
                 Edg/{CHROMIUM_MAJOR}.0.0.0"
            ))
            .map_err(|e| format!("User-Agent error: {e}"))?,
        );
        headers.insert(
            "Cookie",
            HeaderValue::from_str(&format!("muid={muid};"))
                .map_err(|e| format!("Cookie error: {e}"))?,
        );
    }

    // `connect_async_tls_with_config` with a cached connector: the TLS session
    // cache is what makes a repeat utterance cheap.
    let (mut ws, _) = if let Some(connector) = tls_connector() {
        tokio_tungstenite::connect_async_tls_with_config(request, None, true, Some(connector))
            .await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?
    } else {
        connect_async(request)
            .await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?
    };

    // Send speech.config — text frames are `headers + "\r\n\r\n" + body`
    // (the `--` delimiter belongs to binary audio frames only).
    let ts = date_to_string();
    let config_msg = format!(
        "X-Timestamp:{ts}\r\n\
         Content-Type:application/json; charset=utf-8\r\n\
         Path:speech.config\r\n\
         \r\n\
         {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\
         \"sentenceBoundaryEnabled\":\"true\",\n         \"wordBoundaryEnabled\":\"true\"\n         }},\"outputFormat\":\"{OUTPUT_FORMAT}\"}}}}}}}}"
    );
    ws.send(Message::Text(config_msg))
        .await
        .map_err(|e| format!("Send config error: {e}"))?;

    // Send SSML
    let ssml = build_ssml(text, voice, rate, pitch, volume);
    let req_id = uuid::Uuid::new_v4().simple();
    let ssml_msg = format!(
        "X-RequestId:{req_id}\r\n\
         Content-Type:application/ssml+xml\r\n\
         X-Timestamp:{ts}Z\r\n\
         Path:ssml\r\n\
         \r\n\
         {ssml}"
    );
    ws.send(Message::Text(ssml_msg))
        .await
        .map_err(|e| format!("Send SSML error: {e}"))?;

    debug_connector_once();

    // Stream the audio out. Each binary frame carries a big-endian u16 header
    // length, then that header, then the payload; only `Path:audio` frames carry
    // MP3. The stop check is deliberately inside the loop *and* used to race the
    // socket read below, so a barge-in aborts the network fetch itself instead of
    // waiting for the utterance to finish arriving.
    let mut total = 0usize;
    let mut ended = StreamEnd::Complete;
    loop {
        if should_stop() {
            ended = StreamEnd::Cancelled;
            break;
        }

        let next = tokio::time::timeout(std::time::Duration::from_millis(100), ws.next()).await;
        let msg = match next {
            // Poll interval elapsed with nothing to read: re-check the stop flag.
            Err(_elapsed) => continue,
            Ok(None) => break,
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(e))) => return Err(format!("WebSocket error: {e}")),
        };

        match msg {
            Message::Text(text) => {
                if text.contains("Path:turn.end") {
                    break;
                }
            }
            Message::Binary(data) => {
                if data.len() < 2 {
                    continue;
                }
                let header_len = u16::from_be_bytes([data[0], data[1]]) as usize;
                if data.len() < 2 + header_len {
                    continue;
                }

                let header_bytes = &data[2..2 + header_len];
                let header_str = String::from_utf8_lossy(header_bytes);

                if header_str.contains("Path:audio") {
                    let audio_start = 2 + header_len;
                    if audio_start < data.len() {
                        let chunk = &data[audio_start..];
                        on_chunk(chunk);
                        total += chunk.len();
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    if total == 0 {
        return Err("No audio data received from Edge TTS".into());
    }

    Ok((ended, total))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_sec_ms_gec() -> String {
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    let mut ticks = unix_secs + WIN_EPOCH_SECS;
    ticks -= ticks % 300.0;
    let ticks_100ns = (ticks * 10_000_000.0) as u64;

    let hash_input = format!("{ticks_100ns}{TRUSTED_CLIENT_TOKEN}");
    let hash = Sha256::digest(hash_input.as_bytes());
    hash.iter().map(|b| format!("{b:02X}")).collect()
}

fn generate_muid() -> String {
    uuid::Uuid::new_v4().simple().to_string().to_uppercase()
}

fn date_to_string() -> String {
    // JavaScript-compatible UTC date string, e.g.
    // `Sat Sep 27 2026 01:45:00 GMT+0000 (Coordinated Universal Time)`.
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let weekday = (days + 4).rem_euclid(7) as usize;

    format!(
        "{} {} {:02} {} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
        DAYS[weekday],
        MONTHS[(m - 1) as usize],
        d,
        y,
        hh,
        mm,
        ss
    )
}

fn build_ssml(text: &str, voice: &str, rate: &str, pitch: &str, volume: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;");

    format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>\
         <voice name='{voice}'>\
         <prosody pitch='{pitch}' rate='{rate}' volume='{volume}'>\
         {escaped}\
         </prosody>\
         </voice>\
         </speak>"
    )
}
