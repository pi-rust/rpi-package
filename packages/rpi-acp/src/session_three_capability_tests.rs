#[test]
fn native_identity_is_a_content_chunk_sibling_for_text_and_thinking() {
    for kind in ["text", "thinking"] {
        let update = text_update(json!({"kind":kind,"text":"part","messageId":"native-entry"}));
        assert_eq!(update["messageId"], "native-entry");
        assert!(update["content"].get("messageId").is_none());
    }
    assert!(text_update(json!({"kind":"text","text":"old peer"}))
        .get("messageId")
        .is_none());
}

#[test]
fn ordered_inline_images_keep_resources_in_place_and_uri_inert() {
    use rpi_extcore::prompt::PromptBlock;
    let image: ContentBlock = serde_json::from_value(json!({"type":"image","data":"/9j/AA==","mimeType":"image/jpeg","uri":"https://never-fetch.invalid/a"})).unwrap();
    let blocks = vec![
        ContentBlock::Text {
            text: "before".into(),
        },
        image,
        ContentBlock::Resource {
            resource: crate::protocol::EmbeddedResource {
                uri: "file:///a".into(),
                text: Some("buffer".into()),
            },
        },
        ContentBlock::Text {
            text: "after".into(),
        },
    ];
    let content = prompt_content(&blocks).unwrap().unwrap();
    assert_eq!(
        content.blocks(),
        &[
            PromptBlock::Text {
                text: "before".into()
            },
            PromptBlock::Image {
                data: "/9j/AA==".into(),
                mime_type: "image/jpeg".into()
            },
            PromptBlock::Text {
                text: "<file uri=\"file:///a\">\nbuffer\n</file>".into()
            },
            PromptBlock::Text {
                text: "after".into()
            }
        ]
    );
    assert!(prompt_content(&[ContentBlock::Text {
        text: "x".repeat(40000)
    }])
    .unwrap()
    .is_none());
    for data in ["", "bad!", "aGVsbG8="] {
        assert!(prompt_content(&[ContentBlock::Image {
            data: data.into(),
            mime_type: "image/png".into(),
            uri: None
        }])
        .is_err());
    }
}

#[tokio::test]
async fn legacy_native_image_rejects_before_any_prompt_command() {
    let (session, commands) = test_session();
    let (output, _peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let result = session
        .prompt(
            &writer,
            "s",
            &[ContentBlock::Image {
                data: "/9j/AA==".into(),
                mime_type: "image/jpeg".into(),
                uri: None,
            }],
        )
        .await;
    assert!(result.is_err());
    let mut lines = BufReader::new(commands).lines();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line())
            .await
            .is_err()
    );
}
