//! Pi's line-wise Gemini SSE decoding, including its per-read CRLF normalization.
use super::json::trim;
use encoding_rs::{Decoder, UTF_8};
use serde_json::Value;

pub struct DecoderState {
    decoder: Decoder,
    buffer: String,
    frame: Option<(String, usize)>,
}
impl Default for DecoderState {
    fn default() -> Self {
        Self {
            decoder: UTF_8.new_decoder(),
            buffer: String::new(),
            frame: None,
        }
    }
}
impl DecoderState {
    pub fn push(&mut self, bytes: &[u8]) {
        let mut text = String::with_capacity(
            self.decoder
                .max_utf8_buffer_length(bytes.len())
                .expect("UTF-8 buffer capacity"),
        );
        let (status, read, _) = self.decoder.decode_to_string(bytes, &mut text, false);
        debug_assert_eq!(status, encoding_rs::CoderResult::InputEmpty);
        debug_assert_eq!(read, bytes.len());
        self.buffer.push_str(&text.replace("\r\n", "\n"));
    }
    pub fn next_chunk(&mut self, eof: bool) -> Option<Value> {
        loop {
            if let Some((frame, offset)) = &mut self.frame {
                while *offset < frame.len() {
                    let line_end = frame[*offset..]
                        .find('\n')
                        .map_or(frame.len(), |index| *offset + index);
                    let line = &frame[*offset..line_end];
                    *offset = line_end + 1;
                    let Some(data) = line.strip_prefix("data:").map(trim) else {
                        continue;
                    };
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    if let Ok(mut value) = serde_json::from_str::<Value>(data) {
                        if value
                            .get("response")
                            .is_some_and(|value| value.is_object() || value.is_array())
                        {
                            value = value["response"].take();
                        }
                        return Some(value);
                    }
                }
                self.frame = None;
            }
            if let Some(boundary) = self.buffer.find("\n\n") {
                let rest = self.buffer.split_off(boundary + 2);
                self.buffer.truncate(boundary);
                self.frame = Some((std::mem::replace(&mut self.buffer, rest), 0));
            } else if eof && !trim(&self.buffer).is_empty() {
                self.frame = Some((std::mem::take(&mut self.buffer), 0));
            } else {
                return None;
            }
        }
    }
}
