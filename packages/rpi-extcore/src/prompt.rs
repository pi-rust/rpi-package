//! Ordered native prompt input, independent of adapter types.
//!
//! Structural validation only: native owns pixel decoding, orientation, model
//! support and admission. No paths, URLs, fetching, normalization or image
//! omission are supported. These types do not change the legacy string path.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};

pub const MAX_CONTENT_BLOCKS: usize = 64;
pub const MAX_IMAGES: usize = 4;
pub const MAX_IMAGE_BYTES: usize = 256 * 1024;
pub const MAX_IMAGE_BASE64_BYTES: usize = 349_528;
pub const MAX_TOTAL_IMAGE_BYTES: usize = 512 * 1024;
pub const MAX_TEXT_BYTES: usize = 32 * 1024;
pub const MAX_PROMPT_BYTES: usize = 768 * 1024;

/// Only the native plain user-input variants, in their original order.
/// Constructing a block alone does not validate aggregate prompt budgets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PromptBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

/// Validated ordered blocks. The private vector prevents mutation after checking.
/// Serialization is the native `contentBlocks` array, not a command envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PromptContent {
    blocks: Vec<PromptBlock>,
}

impl PromptContent {
    pub fn new(blocks: Vec<PromptBlock>) -> Result<Self, String> {
        validate_blocks(
            blocks.len(),
            blocks.iter().map(|block| {
                Ok(match block {
                    PromptBlock::Text { text } => BlockRef::Text(text),
                    PromptBlock::Image { data, mime_type } => BlockRef::Image(data, mime_type),
                })
            }),
        )?;
        Ok(Self { blocks })
    }

    /// Parse only a content-block array; unknown variants and extra fields fail.
    /// Validate borrowed data before allocating any owned payload copies.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let blocks = value.as_array().ok_or("prompt content must be an array")?;
        validate_blocks(blocks.len(), blocks.iter().map(parse_block))?;
        let blocks = blocks
            .iter()
            .map(|block| match parse_block(block) {
                Ok(BlockRef::Text(text)) => PromptBlock::Text {
                    text: text.to_owned(),
                },
                Ok(BlockRef::Image(data, mime_type)) => PromptBlock::Image {
                    data: data.to_owned(),
                    mime_type: mime_type.to_owned(),
                },
                Err(_) => unreachable!("validated immutable block"),
            })
            .collect();
        Ok(Self { blocks })
    }

    pub fn blocks(&self) -> &[PromptBlock] {
        &self.blocks
    }

    pub fn has_images(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, PromptBlock::Image { .. }))
    }
}

enum BlockRef<'a> {
    Text(&'a str),
    Image(&'a str, &'a str),
}

fn parse_block(block: &Value) -> Result<BlockRef<'_>, String> {
    let fields = block.as_object().ok_or("prompt block must be an object")?;
    match fields.get("type").and_then(Value::as_str) {
        Some("text") if fields.len() == 2 => Ok(BlockRef::Text(
            fields
                .get("text")
                .and_then(Value::as_str)
                .ok_or("invalid text block")?,
        )),
        Some("image") if fields.len() == 3 => Ok(BlockRef::Image(
            fields
                .get("data")
                .and_then(Value::as_str)
                .ok_or("invalid image data")?,
            fields
                .get("mimeType")
                .and_then(Value::as_str)
                .ok_or("invalid image MIME")?,
        )),
        _ => Err("prompt blocks permit only plain text or inline image fields".into()),
    }
}

fn add_bytes(
    total: &mut usize,
    count: usize,
    limit: usize,
    error: &'static str,
) -> Result<(), String> {
    *total = total
        .checked_add(count)
        .filter(|n| *n <= limit)
        .ok_or(error)?;
    Ok(())
}

fn validate_blocks<'a>(
    count: usize,
    blocks: impl Iterator<Item = Result<BlockRef<'a>, String>>,
) -> Result<(), String> {
    if count == 0 || count > MAX_CONTENT_BLOCKS {
        return Err("prompt requires 1..64 content blocks".into());
    }
    let mut text_bytes = 0;
    let mut image_bytes = 0;
    let mut image_count = 0;
    for block in blocks {
        match block? {
            BlockRef::Text(text) => add_bytes(
                &mut text_bytes,
                text.len(),
                MAX_TEXT_BYTES,
                "aggregate prompt text exceeds 32768 UTF-8 bytes",
            )?,
            BlockRef::Image(data, mime_type) => {
                add_bytes(&mut image_count, 1, MAX_IMAGES, "prompt exceeds 4 images")?;
                let size = validate_image(data, mime_type)?;
                add_bytes(
                    &mut image_bytes,
                    size,
                    MAX_TOTAL_IMAGE_BYTES,
                    "aggregate prompt images exceed 524288 decoded bytes",
                )?;
            }
        }
    }
    Ok(())
}

fn validate_image(data: &str, mime_type: &str) -> Result<usize, String> {
    if !matches!(mime_type, "image/png" | "image/jpeg") {
        return Err("image MIME must be image/png or image/jpeg".into());
    }
    if data.is_empty() || data.len() > MAX_IMAGE_BASE64_BYTES {
        return Err("image requires 1..262144 decoded bytes".into());
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| "image requires canonical standard base64")?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err("image requires 1..262144 decoded bytes".into());
    }
    if STANDARD.encode(&bytes) != data {
        return Err("image requires canonical standard base64".into());
    }
    let signature_matches = match mime_type {
        "image/png" => plain_png_signature(&bytes),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff") && bytes.get(3) != Some(&0xf7),
        _ => false,
    };
    if !signature_matches {
        return Err("image signature does not match declared MIME".into());
    }
    // A matching signature is not proof of a valid image. Native must decode.
    Ok(bytes.len())
}

// Match native rpi-tools::image signature policy, including APNG/JPEG-XT
// exclusions. This bounded chunk walk is structural, not CRC/pixel validation.
fn plain_png_signature(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR") {
        return false;
    }
    let mut offset = 8usize;
    while let Some(header) = bytes.get(offset..).and_then(|tail| tail.get(..8)) {
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        match &header[4..8] {
            b"acTL" => return false,
            b"IDAT" => return true,
            _ => {}
        }
        let Some(next) = offset.checked_add(12).and_then(|n| n.checked_add(length)) else {
            return false;
        };
        offset = next;
    }
    true
}

/// Check the FINAL ordered prompt command, after request-id correlation and all
/// envelope fields have been added. Counts normalized serde JSON bytes plus LF,
/// including arbitrary JSON ids and escaping; no second serialized buffer.
/// Constructors cannot guarantee this envelope-dependent limit. If the writer
/// changes the command after this check, it must validate again.
///
/// This is not the legacy string-prompt validator: legacy callers stay on their
/// existing path. Reserved `images` and legacy aliases fail by field presence.
pub fn validate_command(command: &Value) -> Result<(), String> {
    validate_command_with_limit(command, MAX_PROMPT_BYTES)
}

fn validate_command_with_limit(command: &Value, limit: usize) -> Result<(), String> {
    let fields = command
        .as_object()
        .ok_or("prompt command must be an object")?;
    if fields.get("type").and_then(Value::as_str) != Some("prompt") {
        return Err("ordered prompt command requires type prompt".into());
    }
    if ["images", "message", "content", "prompt"]
        .iter()
        .any(|key| fields.contains_key(*key))
    {
        return Err("ordered prompt cannot contain images or legacy text fields".into());
    }
    let blocks = fields
        .get("contentBlocks")
        .and_then(Value::as_array)
        .ok_or("ordered prompt requires contentBlocks")?;
    // Charge the entire envelope before spending time decoding base64.
    struct Budget {
        remaining: usize,
    }
    impl Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.remaining = self.remaining.checked_sub(bytes.len()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "prompt byte budget exceeded")
            })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let remaining = limit.checked_sub(1).ok_or("invalid prompt byte budget")?;
    serde_json::to_writer(Budget { remaining }, command)
        .map_err(|_| "serialized prompt command plus LF exceeds byte budget")?;
    validate_blocks(blocks.len(), blocks.iter().map(parse_block))
}

/// Fixed v1 client profile. Accept only complete, exact v1 structural bounds;
/// smaller/different remote profiles fail closed rather than silently stripping
/// images or pretending local construction used negotiated bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptProfile {
    command_bytes: usize,
}

impl PromptProfile {
    /// Parse the native capability object from `ready.promptContent`.
    /// Missing/false/incomplete data is an error, never image-to-text fallback.
    ///
    /// Allocation/pixel metadata describes native decoder policy only; accepting
    /// it is not pixel validation here or a hard total 32 MiB heap guarantee.
    pub fn from_ready(profile: &Value) -> Result<Self, String> {
        let fields = profile.as_object().ok_or("missing native prompt profile")?;
        if fields.get("contentBlocksVersion").and_then(Value::as_u64) != Some(1)
            || fields.get("image").and_then(Value::as_bool) != Some(true)
        {
            return Err("native ordered image prompt profile is unsupported".into());
        }
        let mimes = fields
            .get("imageMimeTypes")
            .and_then(Value::as_array)
            .ok_or("incomplete native prompt profile")?;
        if mimes.len() != 2
            || !mimes.iter().any(|m| m.as_str() == Some("image/png"))
            || !mimes.iter().any(|m| m.as_str() == Some("image/jpeg"))
        {
            return Err("unsupported native prompt MIME profile".into());
        }
        for (name, expected) in [
            ("maxImages", MAX_IMAGES as u64),
            ("maxImageBytes", MAX_IMAGE_BYTES as u64),
            ("maxTotalImageBytes", MAX_TOTAL_IMAGE_BYTES as u64),
            ("maxImageBase64Bytes", MAX_IMAGE_BASE64_BYTES as u64),
            ("maxPromptBytes", MAX_PROMPT_BYTES as u64),
            ("maxTextBytes", MAX_TEXT_BYTES as u64),
            ("maxContentBlocks", MAX_CONTENT_BLOCKS as u64),
            ("maxImageSide", 2000),
            ("maxImagePixels", 4_000_000),
            ("imageDecodeAllocBudgetBytes", 32 * 1024 * 1024),
        ] {
            if fields.get(name).and_then(Value::as_u64) != Some(expected) {
                return Err("incomplete or incompatible native prompt limits".into());
            }
        }
        if fields
            .get("frameBytes")
            .and_then(Value::as_u64)
            .filter(|n| *n >= MAX_PROMPT_BYTES as u64)
            .is_none()
        {
            return Err("incompatible native prompt frame limit".into());
        }
        Ok(Self {
            command_bytes: MAX_PROMPT_BYTES,
        })
    }

    pub fn validate_command(&self, command: &Value) -> Result<(), String> {
        validate_command_with_limit(command, self.command_bytes)
    }
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
