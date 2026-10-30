//! Independent semantic cases plus mapper-produced official-schema fixtures.
#[allow(dead_code)]
#[path = "../src/mcp.rs"]
mod mcp;
#[allow(dead_code)]
#[path = "../src/system_prompt.rs"]
mod system_prompt;
#[allow(dead_code)]
#[path = "../src/v2.rs"]
mod v2;

use serde_json::{json, Value};

fn diff(old: Value, new: &str) -> Value {
    v2::map_session_update(json!({
        "sessionUpdate":"tool_call", "toolCallId":"edit-1",
        "content":[{"type":"diff", "path":"C:/work/quoted\"\t雪.txt",
            "oldText":old, "newText":new, "_meta":{"source":"native"}}]
    }))
}

#[test]
fn fragments_do_not_claim_to_be_whole_file_patches() {
    // The actual file contains surrounding lines not supplied to the mapper.
    // Applying a full-file patch made from these fragments would lose them.
    let file = "prefix\nold\nsuffix\n";
    assert_eq!(file.replacen("old", "new", 1), "prefix\nnew\nsuffix\n");
    for (old, new) in [
        ("old", "new"),
        ("", "insert"),
        ("old", ""),
        ("no final newline", "replacement\n"),
        ("crlf\r\n", "lf\n"),
        ("same", "same"),
    ] {
        let update = diff(json!(old), new);
        let content = &update["content"][0];
        assert_eq!(content["changes"][0]["operation"], "modify");
        assert_eq!(content["changes"][0]["path"], "C:/work/quoted\"\t雪.txt");
        assert_eq!(content["_meta"]["source"], "native");
        assert!(
            content.get("patch").is_none(),
            "fragment must not claim a git patch"
        );
    }
}

#[test]
fn writes_cannot_distinguish_creation_from_overwrite() {
    // session.rs emits this exact oldText:null for BOTH cases. Reporting add
    // would be false for overwrites; reporting modify would be false for new files.
    for new in ["", "hello", "hello\n"] {
        let mapped = diff(Value::Null, new);
        assert_eq!(mapped["content"][0]["changes"][0]["operation"], "_write");
        assert!(mapped["content"][0].get("patch").is_none());
    }
}

#[test]
fn structured_new_deleted_and_patch_content_survive_idempotently() {
    for operation in ["add", "delete", "modify", "_write"] {
        let update = json!({"sessionUpdate":"tool_call_update", "toolCallId":"1",
            "content":[{"type":"diff", "changes":[{"operation":operation,"path":"/work/file"}]}]});
        assert_eq!(v2::map_session_update(update.clone()), update);
    }
    // Already-produced truthful patches must not be replaced by synthetic ones.
    let update = json!({"sessionUpdate":"tool_call_update", "toolCallId":"1", "content":[{
        "type":"diff", "changes":[{"operation":"add","path":"/work/file"}],
        "patch":{"format":"git_patch","text":"diff --git /work/file /work/file\nnew file mode 100644\n--- /dev/null\n+++ /work/file\n@@ -0,0 +1 @@\n+hello\n\\ No newline at end of file\n"}}]});
    assert_eq!(v2::map_session_update(update.clone()), update);
}

#[test]
fn replay_cursor_is_explicit_and_never_silently_downgraded() {
    for params in [json!({}), json!({"replayFrom":null})] {
        assert_eq!(v2::replay_from(&params).unwrap(), None);
    }
    let start = json!({"replayFrom":{"type":"start","_meta":{"future":true}}});
    assert_eq!(
        v2::replay_from(&start).unwrap(),
        Some(v2::ReplayFrom::Start)
    );
    assert!(v2::valid_session_setup(&start).is_ok());
    for cursor in [
        json!({}),
        json!([]),
        json!("start"),
        json!(false),
        json!(0),
        json!({"type":null}),
        json!({"type":2}),
        json!({"type":"message","messageId":"x"}),
        json!({"type":"_future"}),
    ] {
        assert!(v2::valid_session_setup(&json!({"replayFrom":cursor})).is_err());
    }
    for servers in [
        json!({}),
        json!(null),
        json!([{"type":"stdio","command":"tools"}]),
    ] {
        assert!(v2::valid_session_setup(&json!({"mcpServers":servers})).is_err());
    }
}

#[test]
fn v2_auth_requires_explicit_object_opt_in_not_legacy_flags() {
    for caps in [
        json!({}),
        json!({"auth":{"terminal":null}}),
        json!({"terminal":true,"_meta":{"terminal-auth":true}}),
    ] {
        let view = v2::legacy_capability_view(&json!({"capabilities":caps}));
        assert_eq!(view["clientCapabilities"]["auth"]["terminal"], false);
        assert_eq!(view["clientCapabilities"]["terminal"], false);
    }
    let params = json!({"protocolVersion":2,"info":{"name":"test","version":"1"},
        "capabilities":{"auth":{"terminal":{}}}});
    assert!(v2::valid_initialize_params(&params).is_ok());
    assert_eq!(
        v2::legacy_capability_view(&params)["clientCapabilities"]["auth"]["terminal"],
        true
    );
    for field in ["terminal", "form", "url"] {
        let group = if field == "terminal" {
            "auth"
        } else {
            "elicitation"
        };
        let mut invalid = params.clone();
        invalid["capabilities"] = json!({group:{field:true}});
        assert!(v2::valid_initialize_params(&invalid).is_err());
    }
    for version in [
        json!(null),
        json!(-1),
        json!(1),
        json!(65536),
        json!(2.5),
        json!("2"),
    ] {
        let mut invalid = params.clone();
        invalid["protocolVersion"] = version;
        assert!(v2::valid_initialize_params(&invalid).is_err());
    }
}

fn commands() -> Value {
    v2::map_session_update(json!({"sessionUpdate":"available_commands_update",
        "availableCommands":[{"name":"review","description":"Review changes",
            "input":{"hint":"path"},"_meta":{"native":true}}]}))
}

fn permission() -> Value {
    v2::map_request("session/request_permission", json!({"sessionId":"s",
        "toolCall":{"toolCallId":"tool-1","title":"Allow this actual tool?"},
        "options":[{"optionId":"0","name":"Allow once","kind":"allow_once"}],
        "_meta":{"pi-rust/dialog":{"requestId":"native-1"}}}))
}

#[test]
fn commands_permissions_and_resources_use_known_v2_variants() {
    let commands = commands();
    assert_eq!(commands["availableCommands"][0]["type"], "command");
    assert_eq!(commands["availableCommands"][0]["input"]["type"], "text");
    assert_eq!(commands["availableCommands"][0]["input"]["hint"], "path");
    assert_eq!(commands["availableCommands"][0]["_meta"]["native"], true);
    assert_eq!(v2::map_session_update(commands.clone()), commands);
    let permission = permission();
    assert_eq!(permission["title"], "Allow this actual tool?");
    assert_eq!(permission["subject"]["type"], "tool_call");
    assert_eq!(permission["subject"]["toolCall"]["toolCallId"], "tool-1");
    assert_eq!(permission["_meta"]["pi-rust/dialog"]["requestId"], "native-1");
    assert!(permission.get("toolCall").is_none());
    assert_eq!(v2::map_request("session/request_permission", permission.clone()), permission);
    let uri = "https://example.test/no-fetch";
    assert_eq!(v2::prompt_resource(uri, None), json!({"type":"resource_link","uri":uri,"name":uri}));
    for text in ["", "resource body"] {
        assert_eq!(v2::prompt_resource(uri, Some(text)), json!({"type":"resource","resource":{"uri":uri,"text":text}}));
    }
}

#[test]
fn v2_schema_fixtures() {
    let Some(path) = std::env::var_os("RPI_ACP_V2_FIXTURES") else {
        return;
    };
    let initialize = v2::initialize_result(&json!({
        "agentInfo":{"name":"rpi","version":"1"},
        "agentCapabilities":{"auth":{"logout":true}},
        "authMethods":[{"id":"credentials","name":"Terminal","type":"terminal",
            "args":["--setup"],"env":{"A":"one","B":"two"}}]
    }));
    assert!(initialize["capabilities"].get("auth").is_none());
    assert_eq!(initialize["authMethods"][0]["methodId"], "credentials");
    assert_eq!(
        initialize["authMethods"][0]["env"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let plan = v2::map_session_update(json!({"sessionUpdate":"plan","entries":[{
        "content":"Verify","priority":"medium","status":"completed"}]}));
    assert_eq!(plan["sessionUpdate"], "plan_update");
    assert_eq!(plan["plan"]["entries"][0]["content"], "Verify");
    let fixtures = json!([
        ["InitializeResponse",initialize],
        ["InitializeRequest",{"protocolVersion":2,"info":{"name":"client","version":"1"},"capabilities":{"auth":{"terminal":{}}}}],
        ["LoginAuthRequest",{"methodId":"agent-method"}],
        ["LoginAuthResponse",v2::login_result()],
        ["LogoutAuthRequest",{}], ["LogoutAuthResponse",v2::logout_result()],
        ["SessionUpdate",diff(json!("old"),"new")],
        ["SessionUpdate",diff(Value::Null,"new")],
        ["SessionUpdate",plan],
        // State is a string discriminator in the pinned official v2 schema,
        // matching protocol/session output, not an object with a `type` field.
        ["SessionUpdate",v2::map_session_update(json!({"sessionUpdate":"state_update","state":"idle"}))],
        ["SessionUpdate",v2::map_session_update(json!({"sessionUpdate":"state_update","state":"running"}))],
        ["SessionUpdate",v2::map_session_update(json!({"sessionUpdate":"state_update","state":"idle","stopReason":"cancelled"}))],
        ["PromptResponse",v2::prompt_result("user-1")],
        ["SessionUpdate",v2::map_session_update(json!({"sessionUpdate":"agent_message_chunk","messageId":"assistant-1","content":{"type":"text","text":"hi"}}))],
        ["ReplayFrom",{"type":"start"}],
        ["SessionUpdate",commands()],
        ["RequestPermissionRequest",permission()],
        ["SessionUpdate",{"sessionUpdate":"user_message","messageId":"user-resource",
            "content":[v2::prompt_resource("https://example.test/no-fetch", None),
                v2::prompt_resource("file:///work/resource.txt", Some("preserved text"))]}]
    ]);
    std::fs::write(path, serde_json::to_vec_pretty(&fixtures).unwrap()).unwrap();
}
