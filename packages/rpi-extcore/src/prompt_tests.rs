//! Ordered native prompt structural/profile specifications.
use super::*;
use serde_json::json;

// The same structural fixture as native rpi-tools/src/image.rs::detects_png_static.
// Deliberately not a decodable pixel image: pixel validation belongs to native.
fn png_bytes() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&[0; 17]); // payload + CRC
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(b"IDAT");
    bytes.extend_from_slice(&[0; 4]);
    bytes
}

fn image_bytes(bytes: &[u8], mime: &str) -> PromptBlock {
    PromptBlock::Image {
        data: STANDARD.encode(bytes),
        mime_type: mime.into(),
    }
}

fn png(size: usize) -> PromptBlock {
    let mut bytes = png_bytes();
    assert!(size >= bytes.len());
    bytes.resize(size, 0);
    image_bytes(&bytes, "image/png")
}

fn text(value: &str) -> PromptBlock {
    PromptBlock::Text { text: value.into() }
}

fn command(blocks: Vec<PromptBlock>) -> Value {
    json!({"type":"prompt", "id":"test", "contentBlocks":blocks})
}

fn profile() -> Value {
    // Exact native prompt_capabilities() fixture advertised as ready.promptContent.
    json!({
        "contentBlocksVersion":1, "image":true,
        "imageMimeTypes":["image/png", "image/jpeg"],
        "maxImages":4, "maxImageBytes":262144, "maxTotalImageBytes":524288,
        "maxImageBase64Bytes":349528, "maxPromptBytes":786432,
        "maxTextBytes":32768, "maxContentBlocks":64, "maxImageSide":2000,
        "maxImagePixels":4000000, "imageDecodeAllocBudgetBytes":33554432,
        "frameBytes":1048576
    })
}

#[test]
fn ordered_serialization_is_exact_native_plain_blocks() {
    let image = png(png_bytes().len());
    let jpeg = image_bytes(&[0xff, 0xd8, 0xff, 0xe0, 0, 0], "image/jpeg");
    let blocks = vec![text("before"), image.clone(), text("after"), jpeg.clone()];
    let content = PromptContent::new(blocks.clone()).unwrap();
    assert!(content.has_images());
    assert_eq!(content.blocks(), blocks.as_slice());
    let wire = serde_json::to_value(&content).unwrap();
    assert_eq!(
        wire,
        json!([
            {"type":"text", "text":"before"},
            {"type":"image", "data":STANDARD.encode(png_bytes()), "mimeType":"image/png"},
            {"type":"text", "text":"after"},
            {"type":"image", "data":STANDARD.encode([0xff, 0xd8, 0xff, 0xe0, 0, 0]), "mimeType":"image/jpeg"}
        ])
    );
    assert_eq!(PromptContent::from_value(&wire).unwrap(), content);
    assert!(PromptContent::new(vec![image, jpeg]).unwrap().has_images());
    assert!(!PromptContent::new(vec![text("")]).unwrap().has_images());
}

#[test]
fn canonical_standard_base64_and_exact_mime_only() {
    for data in [
        "",
        "Zh==",
        "Zg",
        "Zg==\n",
        "_w==",
        "data:image/png;base64,AAAA",
        "secret-payload!",
    ] {
        let error = PromptContent::new(vec![PromptBlock::Image {
            data: data.into(),
            mime_type: "image/png".into(),
        }])
        .unwrap_err();
        assert!(error.len() < 160);
        assert!(!error.contains("secret-payload"));
    }
    for mime in [
        "image/jpeg",
        "image/jpg",
        "IMAGE/PNG",
        "image/png; charset=utf-8",
        "image/gif",
        "secret-mime",
    ] {
        let error = PromptContent::new(vec![image_bytes(&png_bytes(), mime)]).unwrap_err();
        assert!(!error.contains("secret-mime"));
    }
    assert!(PromptContent::new(vec![image_bytes(&[0xff, 0xd8, 0xff], "image/png")]).is_err());
}

#[test]
fn strict_shapes_reject_all_non_plain_fields_without_echoing() {
    for value in [Value::Null, json!({}), json!("bad"), json!([])] {
        assert!(PromptContent::from_value(&value).is_err());
    }
    for block in [
        json!(null),
        json!(1),
        json!([]),
        json!("text"),
        json!({}),
        json!({"type":"secret-payload"}),
        json!({"type":"thinking", "thinking":"secret-payload"}),
        json!({"type":"toolCall", "id":"x", "name":"x", "arguments":{}}),
        json!({"type":"text"}),
        json!({"type":"text", "text":null}),
        json!({"type":"text", "text":2}),
        json!({"type":"text", "text":"ok", "textSignature":"secret-payload"}),
        json!({"type":"text", "text":"ok", "arbitrary":null}),
        json!({"type":"image", "data":"secret-payload"}),
        json!({"type":"image", "data":null, "mimeType":"image/png"}),
        json!({"type":"image", "data":"AAAA", "mimeType":null}),
        json!({"type":"image", "data":"AAAA", "mimeType":"image/png", "uri":"https://inert.invalid/secret-payload"}),
        json!({"type":"image", "path":"secret-payload", "mimeType":"image/png"}),
    ] {
        let error = PromptContent::from_value(&json!([block])).unwrap_err();
        assert!(!error.contains("secret-payload"));
        assert!(error.len() < 160);
    }
}

#[test]
fn native_signature_policy_is_structural_not_pixel_decoding() {
    let bytes = png_bytes();
    assert!(PromptContent::new(vec![image_bytes(&bytes, "image/png")]).is_ok());
    // Native decode rejects these truncated bodies; core intentionally does not
    // claim to do that work. The IHDR/signature policy still has to match native.
    assert!(PromptContent::new(vec![image_bytes(&bytes[..16], "image/png")]).is_ok());
    assert!(PromptContent::new(vec![image_bytes(&bytes[..8], "image/png")]).is_err());
    assert!(PromptContent::new(vec![image_bytes(
        &[0xff, 0xd8, 0xff, 0xe0, 0, 0],
        "image/jpeg"
    )])
    .is_ok());
    assert!(PromptContent::new(vec![image_bytes(
        &[0xff, 0xd8, 0xff, 0xf7, 0, 0],
        "image/jpeg"
    )])
    .is_err());
    let mut apng = bytes.clone();
    apng[37..41].copy_from_slice(b"acTL");
    assert!(PromptContent::new(vec![image_bytes(&apng, "image/png")]).is_err());
    let mut wrong_ihdr = bytes.clone();
    wrong_ihdr[11] = 12;
    assert!(PromptContent::new(vec![image_bytes(&wrong_ihdr, "image/png")]).is_err());
    for bytes in [
        &b"GIF89a"[..],
        &b"RIFF????WEBP"[..],
        &b"BM"[..],
        &b"hello world"[..],
    ] {
        assert!(PromptContent::new(vec![image_bytes(bytes, "image/png")]).is_err());
    }
}

#[test]
fn block_and_image_count_edges_are_exact() {
    assert!(PromptContent::new(vec![]).is_err());
    assert!(PromptContent::new(vec![text(""); MAX_CONTENT_BLOCKS]).is_ok());
    assert!(PromptContent::new(vec![text(""); MAX_CONTENT_BLOCKS + 1]).is_err());
    let image = png(png_bytes().len());
    assert!(PromptContent::new(vec![image.clone(); MAX_IMAGES]).is_ok());
    assert!(PromptContent::new(vec![image; MAX_IMAGES + 1]).is_err());
}

#[test]
fn aggregate_text_uses_utf8_bytes_and_checks_overflow() {
    let exact = vec![text(&"é".repeat(MAX_TEXT_BYTES / 2 - 1)), text("é")];
    assert!(PromptContent::new(exact.clone()).is_ok());
    let mut over = exact;
    over.push(text("x"));
    assert!(PromptContent::new(over).is_err());
    assert!(PromptContent::new(vec![text(&"\0".repeat(MAX_TEXT_BYTES))]).is_ok());
    assert!(PromptContent::new(vec![text(&"x".repeat(MAX_TEXT_BYTES + 1))]).is_err());
    let mut total = usize::MAX;
    assert!(add_bytes(&mut total, 1, usize::MAX, "overflow").is_err());
}

#[test]
fn exact_decoded_image_edges_not_encoded_length_estimates() {
    let exact = png(MAX_IMAGE_BYTES);
    let over = png(MAX_IMAGE_BYTES + 1);
    let encoded_len = |block: &PromptBlock| match block {
        PromptBlock::Image { data, .. } => data.len(),
        _ => unreachable!(),
    };
    assert_eq!(encoded_len(&exact), MAX_IMAGE_BASE64_BYTES);
    // Both have the same encoded length: decoded-byte validation is essential.
    assert_eq!(encoded_len(&exact), encoded_len(&over));
    assert!(PromptContent::new(vec![exact.clone()]).is_ok());
    assert!(PromptContent::new(vec![over]).is_err());
    assert!(PromptContent::new(vec![exact.clone(), exact.clone()]).is_ok());
    assert!(PromptContent::new(vec![exact.clone(), png(MAX_IMAGE_BYTES - 100), png(101)]).is_err());
    assert!(PromptContent::new(vec![
        exact,
        png(MAX_IMAGE_BYTES / 2),
        png(MAX_IMAGE_BYTES / 2)
    ])
    .is_ok());
    assert!(PromptContent::new(vec![PromptBlock::Image {
        data: "A".repeat(MAX_IMAGE_BASE64_BYTES + 1),
        mime_type: "image/png".into(),
    }])
    .is_err());
}

#[test]
fn final_frame_size_counts_id_extensions_escaping_type_and_lf() {
    let content = PromptContent::new(vec![text("\0\n\"\\é")]).unwrap();
    let mut input = json!({
        "type":"prompt", "id":{"nested":["\0\n\"\\é",123]},
        "contentBlocks":content, "extensionPadding":""
    });
    let base = serde_json::to_vec(&input).unwrap().len() + 1;
    input["extensionPadding"] = json!("x".repeat(MAX_PROMPT_BYTES - base));
    assert_eq!(
        serde_json::to_vec(&input).unwrap().len() + 1,
        MAX_PROMPT_BYTES
    );
    validate_command(&input).unwrap();
    PromptProfile::from_ready(&profile())
        .unwrap()
        .validate_command(&input)
        .unwrap();
    input["extensionPadding"] = json!(format!("{}x", input["extensionPadding"].as_str().unwrap()));
    assert_eq!(
        serde_json::to_vec(&input).unwrap().len() + 1,
        MAX_PROMPT_BYTES + 1
    );
    assert!(validate_command(&input).is_err());
    // An enormous id alone must be counted, not just id.as_str() or the blocks.
    input["extensionPadding"] = json!("");
    input["id"] = json!({"secret-payload":"x".repeat(MAX_PROMPT_BYTES)});
    let error = validate_command(&input).unwrap_err();
    assert!(!error.contains("secret-payload"));
}

#[test]
fn command_alias_presence_and_reserved_images_do_not_silently_fall_back() {
    for key in ["message", "content", "prompt", "images"] {
        for value in [Value::Null, json!("legacy"), json!([]), json!(2)] {
            let mut input = command(vec![text("new")]);
            input[key] = value;
            assert!(validate_command(&input).is_err());
        }
    }
    for input in [
        Value::Null,
        json!([]),
        json!({"type":"prompt", "message":"legacy"}),
        json!({"type":"compact", "contentBlocks":[{"type":"text", "text":"x"}]}),
        json!({"type":"prompt", "contentBlocks":null}),
        json!({"type":"prompt", "contentBlocks":[]}),
    ] {
        assert!(validate_command(&input).is_err());
    }
    assert!(validate_command(&command(vec![text("")])).is_ok());
    assert!(validate_command(&command(vec![png(MAX_IMAGE_BYTES), png(MAX_IMAGE_BYTES)])).is_ok());
}

#[test]
fn strict_profile_requires_version_image_mimes_and_every_native_limit() {
    let value = profile();
    assert!(PromptProfile::from_ready(&value).is_ok());
    for field in value.as_object().unwrap().keys() {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            PromptProfile::from_ready(&missing).is_err(),
            "missing {field}"
        );
        for invalid in [
            Value::Null,
            json!(false),
            json!(0),
            json!(-1),
            json!(1.5),
            json!("1"),
        ] {
            let mut bad = value.clone();
            bad[field] = invalid;
            assert!(PromptProfile::from_ready(&bad).is_err(), "invalid {field}");
        }
    }
    for (field, invalid) in [
        ("contentBlocksVersion", json!(2)),
        ("maxImages", json!(5)),
        ("maxImageBytes", json!(MAX_IMAGE_BYTES - 1)),
        ("frameBytes", json!(MAX_PROMPT_BYTES - 1)),
        ("imageMimeTypes", json!(["image/png"])),
        ("imageMimeTypes", json!(["image/png", "image/png"])),
        ("imageMimeTypes", json!(["image/png", "image/gif"])),
    ] {
        let mut bad = value.clone();
        bad[field] = invalid;
        assert!(PromptProfile::from_ready(&bad).is_err());
    }
    let mut reverse = value.clone();
    reverse["imageMimeTypes"] = json!(["image/jpeg", "image/png"]);
    assert!(PromptProfile::from_ready(&reverse).is_ok());
    // Actual current ready envelope: no native profile, no inferred support.
    assert!(PromptProfile::from_ready(&json!({
        "type":"ready", "protocol":"rpi", "limits":{"frameBytes":1048576,"maxConcurrentPrompts":1},
        "semantics":{"promptResponse":"accepted", "settlementEvent":"agent_settled", "agentEndIsTerminal":false}
    })).is_err());
}
