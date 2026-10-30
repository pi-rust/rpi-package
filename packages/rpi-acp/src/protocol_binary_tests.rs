use super::*;

fn media(kind: &str, data: &str, mime: &str) -> Value {
    json!({"type":kind,"data":data,"mimeType":mime})
}

#[test]
fn canonical_base64_preserves_bytes_and_regular_acp_metadata() {
    for (kind, mime) in [("image", "image/png"), ("audio", "audio/wav")] {
        for data in [
            "AA==", "AAA=", "AAEC", "Zg==", "Zm8=", "Zm9v", "+/8=", "data",
        ] {
            let mut value = media(kind, data, mime);
            value["annotations"] = json!({"audience":["user"]});
            value["_meta"] = json!({"future":true});
            let block: ContentBlock = serde_json::from_value(value).unwrap();
            match block {
                ContentBlock::Image {
                    data: actual,
                    mime_type,
                }
                | ContentBlock::Audio {
                    data: actual,
                    mime_type,
                } => {
                    assert_eq!(actual, data);
                    assert_eq!(mime_type, mime);
                }
                _ => panic!("expected media"),
            }
        }
    }
}

#[test]
fn malformed_media_is_rejected_by_deserialization_and_prompt_parsing() {
    for (kind, mime) in [("image", "image/png"), ("audio", "audio/wav")] {
        for data in [
            "",
            " ",
            "%%%not-base64%%%",
            "A",
            "AA",
            "AAA",
            "A===",
            "====",
            "=AAA",
            "AA=A",
            "AA==AAAA",
            "AAAA=",
            "AAAA====",
            "AA===",
            "AA==\n",
            " AA==",
            "AA\r\n==",
            "AA-_",
            "éAA",
            "AB==",
            "AAB=",
            "/x==",
            "//9=",
            "data:image/png;base64,AA==",
        ] {
            let value = media(kind, data, mime);
            assert!(
                serde_json::from_value::<ContentBlock>(value.clone()).is_err(),
                "{value}"
            );
            let error = prompt_params(&json!({"prompt":[{"type":"text","text":"before"},value]}))
                .unwrap_err();
            assert!(error.contains("prompt[1] is invalid"), "{error}");
        }
        for data in [Value::Null, json!(123), json!([]), json!({})] {
            let mut value = media(kind, "AAEC", mime);
            value["data"] = data;
            assert!(serde_json::from_value::<ContentBlock>(value).is_err());
        }
    }
}

#[test]
fn media_mime_is_concrete_lowercase_and_matches_modality() {
    for (kind, good, wrong) in [
        ("image", "image/png", "audio/wav"),
        ("audio", "audio/wav", "image/png"),
    ] {
        for mime in [
            "",
            " ",
            "text/plain",
            wrong,
            "image",
            "audio",
            "image/",
            "audio/",
            "image/*",
            "audio/*",
            "IMAGE/png",
            "audio/WAV",
            "image/png; charset=utf-8",
            "audio/wav\r\nX: y",
            "image/png/extra",
            "image/💥",
        ] {
            assert!(
                serde_json::from_value::<ContentBlock>(media(kind, "AAEC", mime)).is_err(),
                "{mime}"
            );
        }
        assert!(serde_json::from_value::<ContentBlock>(media(kind, "AAEC", good)).is_ok());
        let prefix = format!("{kind}/");
        let exact = format!("{prefix}{}", "a".repeat(256 - prefix.len()));
        assert!(serde_json::from_value::<ContentBlock>(media(kind, "AAEC", &exact)).is_ok());
        assert!(
            serde_json::from_value::<ContentBlock>(media(kind, "AAEC", &(exact + "a"))).is_err()
        );
    }
    for mime in ["image/jpeg", "image/svg+xml", "image/vnd.example+png"] {
        assert!(serde_json::from_value::<ContentBlock>(media("image", "AAEC", mime)).is_ok());
    }
    for mime in [
        "audio/x-wav",
        "audio/mpeg",
        "audio/ogg",
        "audio/vnd.example",
    ] {
        assert!(serde_json::from_value::<ContentBlock>(media("audio", "AAEC", mime)).is_ok());
    }
}

#[test]
fn media_base64_encoded_size_boundary_is_enforced() {
    let exact = "A".repeat(MAX_MEDIA_BASE64_BYTES);
    assert!(valid_media_base64(&exact));
    for (kind, mime) in [("image", "image/png"), ("audio", "audio/wav")] {
        assert!(serde_json::from_value::<ContentBlock>(media(kind, &exact, mime)).is_ok());
        let oversized = format!("{exact}AAAA");
        assert!(serde_json::from_value::<ContentBlock>(media(kind, &oversized, mime)).is_err());
    }
}

#[test]
fn embedded_blob_presence_is_rejected_even_with_text_or_null() {
    for blob in [json!("AAEC"), json!(""), Value::Null, json!(123), json!({})] {
        for text in [None, Some("do not silently drop bytes")] {
            let mut resource = json!({"uri":"https://example.invalid/blob","blob":blob});
            if let Some(text) = text {
                resource["text"] = json!(text);
            }
            assert!(serde_json::from_value::<EmbeddedResource>(resource.clone()).is_err());
            let value = json!({"type":"resource","resource":resource});
            assert!(serde_json::from_value::<ContentBlock>(value.clone()).is_err());
            assert!(prompt_params(&json!({"prompt":[value]})).is_err());
        }
    }
}

#[test]
fn text_resources_and_links_retain_existing_contract() {
    for text in [None, Some("hello")] {
        let mut resource =
            json!({"uri":"https://example.invalid/text","mimeType":"text/plain","future":42});
        if let Some(text) = text {
            resource["text"] = json!(text);
        }
        let resource: EmbeddedResource = serde_json::from_value(resource).unwrap();
        assert_eq!(resource.uri, "https://example.invalid/text");
        assert_eq!(resource.text.as_deref(), text);
    }
    let blocks = prompt_params(&json!({"prompt":[
        {"type":"resource_link","uri":"https://example.invalid/link","name":"alias","future":42},
        {"type":"text","text":"follow-up","future":true}
    ]}))
    .unwrap();
    assert_eq!(blocks.len(), 2);
}

#[test]
fn all_possible_final_sextets_have_canonical_padding_bits() {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for (index, &byte) in alphabet.iter().enumerate() {
        let c = char::from(byte);
        assert_eq!(valid_media_base64(&format!("A{c}==")), index % 16 == 0);
        assert_eq!(valid_media_base64(&format!("AA{c}=")), index % 4 == 0);
        assert!(valid_media_base64(&format!("AAA{c}")));
    }
}

#[tokio::test]
async fn v1_and_v2_dispatch_reject_bad_content_before_session_lookup_and_stay_usable() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::time::{timeout, Duration};

    for version in [1, 2] {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let (_stop, stop_rx) = watch::channel(false);
        let (output, peer) = tokio::io::duplex(8192);
        let mut lines = BufReader::new(peer).lines();
        let server = tokio::spawn(run_with_io(stop_rx, rx, None, output));
        tx.send(
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                "protocolVersion":version,"info":{"name":"binary-validation-test","version":"1"},
                "capabilities":{},"clientCapabilities":{}
            }})
            .to_string(),
        )
        .await
        .unwrap();
        let initialized: Value = serde_json::from_str(
            &timeout(Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(initialized["result"]["protocolVersion"], version);

        for (index, block) in [
            media("image", "%%%not-base64%%%", "image/png"),
            media("audio", "%%%not-base64%%%", "audio/wav"),
            media("image", "AB==", "image/png"),
            media("audio", "AAB=", "audio/wav"),
            media("image", "AAEC", "audio/wav"),
            media("audio", "AAEC", "image/png"),
            json!({"type":"resource","resource":{"uri":"https://example.invalid/blob","blob":"AAEC"}}),
        ].into_iter().enumerate() {
            let id = index + 1;
            tx.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{
                "sessionId":"never-created","prompt":[block]
            }}).to_string()).await.unwrap();
            let response: Value = serde_json::from_str(
                &timeout(Duration::from_secs(5), lines.next_line()).await.unwrap().unwrap().unwrap()
            ).unwrap();
            assert_eq!(response["id"], id);
            assert_eq!(response["error"]["code"], -32602);
            assert!(response["error"]["message"].as_str().unwrap().contains("prompt[0] is invalid"), "{response}");
        }
        // A valid follow-up reaches the next dispatch stage, rather than a
        // poisoned parser. No native session/provider is created by this test.
        tx.send(
            json!({"jsonrpc":"2.0","id":100,"method":"session/prompt","params":{
                "sessionId":"never-created","prompt":[{"type":"text","text":"follow-up"}]
            }})
            .to_string(),
        )
        .await
        .unwrap();
        let response: Value = serde_json::from_str(
            &timeout(Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["id"], 100);
        assert_eq!(response["error"]["message"], "unknown sessionId");
        drop(tx);
        assert_eq!(
            timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}
