use super::*;

#[test]
fn validation_is_page_bounded_nondestructive_and_preserves_replay_order() {
    let mut spool = HistorySpool::create().unwrap();
    spool.push(&[json!({"entryId":"new"})]).unwrap();
    spool
        .push(&[json!({"entryId":"old-1"}), json!({"entryId":"old-2"})])
        .unwrap();
    let end = spool.end;
    let length = spool.file.as_ref().unwrap().metadata().unwrap().len();
    spool
        .file
        .as_mut()
        .unwrap()
        .seek(SeekFrom::Start(3))
        .unwrap();
    let mut seen = Vec::new();
    spool
        .validate(|m| {
            seen.push(m["entryId"].as_str().unwrap().to_owned());
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, ["old-1", "old-2", "new"]);
    assert_eq!(spool.end, end);
    assert_eq!(
        spool.file.as_ref().unwrap().metadata().unwrap().len(),
        length
    );
    assert_eq!(spool.file.as_mut().unwrap().stream_position().unwrap(), 3);
    assert!(spool
        .validate(|m| if m["entryId"] == "new" {
            Err("late bad data".into())
        } else {
            Ok(())
        })
        .is_err());
    assert_eq!(spool.end, end);
    assert_eq!(
        spool.file.as_ref().unwrap().metadata().unwrap().len(),
        length
    );
    assert_eq!(spool.file.as_mut().unwrap().stream_position().unwrap(), 3);
    assert_eq!(spool.pop().unwrap().unwrap()[1]["entryId"], "old-2");
    assert_eq!(spool.pop().unwrap().unwrap()[0]["entryId"], "new");
    assert!(spool.pop().unwrap().is_none());
}

#[test]
fn corrupt_spool_lengths_fail_without_truncation_or_underflow() {
    let mut spool = HistorySpool::create().unwrap();
    spool.push(&[json!({"entryId":"x"})]).unwrap();
    let size = spool.end;
    spool
        .file
        .as_mut()
        .unwrap()
        .seek(SeekFrom::Start(size - 8))
        .unwrap();
    spool
        .file
        .as_mut()
        .unwrap()
        .write_all(&u64::MAX.to_le_bytes())
        .unwrap();
    assert!(spool.validate(|_| Ok(())).is_err());
    assert_eq!(spool.end, size);
    assert_eq!(spool.file.as_ref().unwrap().metadata().unwrap().len(), size);
    spool.end = 1;
    assert!(spool.pop().is_err());
}

#[test]
fn fallback_identity_comes_from_entry_key_not_nested_provider_fields() {
    let entries = HashMap::from([(
        "native-entry".to_owned(),
        json!({"type":"message","parentId":null,
        "message":{"role":"assistant","content":"same", "entryId":"not-native", "responseId":"provider"}}),
    )]);
    let messages =
        super::super::branch_messages_from_entries(&entries, Some("native-entry")).unwrap();
    assert_eq!(messages[0]["entryId"], "native-entry");
    assert_eq!(messages[0]["responseId"], "provider");
}
