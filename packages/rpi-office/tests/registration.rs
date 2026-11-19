//! Plugin registration + `/office` command flow against a fake host.
//!
//! One test function on purpose: the plugin keeps process-wide state, so a
//! second concurrent test in this binary would race it.

mod support;

use rpi_plugin_sdk::EventTag;
use serde_json::json;

#[test]
fn registration_and_office_command_flow() {
    support::register_plugin();

    // ── tools ───────────────────────────────────────────────────────────
    let tools = support::tools();
    assert_eq!(tools.len(), 33, "32 catalog ops + the discovery tool");
    let names: Vec<&str> = tools.iter().map(|(name, _, _, _)| name.as_str()).collect();
    assert!(names.contains(&"office_excel_read_range"));
    assert!(names.contains(&"office_word_insert_text"));
    assert!(names.contains(&"office_powerpoint_add_slide"));
    assert!(names.contains(&"office_list_documents"));
    for (name, description, parameters, _) in &tools {
        assert!(!description.trim().is_empty(), "{name} has no description");
        assert_eq!(parameters["type"], json!("object"), "{name} parameters");
    }
    let (_, parameters, _) = support::tool("office_excel_read_range").expect("read_range");
    assert!(parameters["properties"]["document"].is_object());
    assert!(parameters["properties"]["range"].is_object());
    let required = parameters["required"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!required.iter().any(|value| value == "document"));

    // ── tool prompt metadata (promptSnippet / promptGuidelines) ─────────
    let prompts = support::prompts();
    assert_eq!(
        prompts.len(),
        tools.len(),
        "every registered tool should get prompt metadata"
    );
    for (name, _, _, _) in &tools {
        let (_, metadata) = prompts
            .iter()
            .find(|(tool, _)| tool == name)
            .unwrap_or_else(|| panic!("{name} has no prompt metadata"));
        assert!(
            metadata["promptSnippet"].as_str().unwrap_or_default().len() > 3,
            "{name} has no promptSnippet: {metadata}"
        );
    }
    // At least one Excel op carries model-facing guidelines from the catalog.
    let (_, read_range_meta) = prompts
        .iter()
        .find(|(tool, _)| tool == "office_excel_get_overview")
        .expect("overview metadata");
    assert!(read_range_meta["promptSnippet"].is_string());

    // ── plugin lifecycle hooks (reload/shutdown safety) ────────────────
    assert!(
        support::lifecycle_hooks().is_some(),
        "register_plugin_lifecycle must be called so a reload can join the bridge"
    );

    // ── commands, flags, events, prompt transformer ─────────────────────
    let command_names: Vec<String> = support::commands()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(command_names, vec!["office", "office-tools"]);
    assert_eq!(
        support::flags(),
        vec!["office-bridge-port", "office-bridge-autostart"]
    );

    // ── the bridge starts off ───────────────────────────────────────────
    let status = support::run_command("office", "");
    assert_eq!(status["kind"], json!("message"));
    let text = status["text"].as_str().unwrap();
    assert!(text.contains("Office bridge is off (port 38617)"), "{text}");
    assert!(text.contains("/office connect"));

    // SessionStart with the bridge off removes every office tool from the
    // active set, so an offline session does not advertise Office support.
    support::run_empty_event(EventTag::SessionStart);
    let active = support::active_tools();
    assert!(
        !active.iter().any(|name| name.starts_with("office_")),
        "office tools must be inactive while the bridge is off: {active:?}"
    );
    assert!(support::status().is_none(), "status line cleared when off");

    // No panes attached → the prompt transformer stays out of the way.
    assert!(support::run_before_agent_start("base prompt").is_none());

    // ── connect ────────────────────────────────────────────────────────
    let port = support::free_port();
    let connected = support::run_command("office", &format!("connect {port}"));
    let text = connected["text"].as_str().unwrap();
    assert!(
        text.contains(&format!("listening on ws://127.0.0.1:{port}")),
        "{text}"
    );
    let active = support::active_tools();
    assert!(
        active.contains(&"office_list_documents".to_string()),
        "the discovery tool activates with the bridge: {active:?}"
    );
    assert!(
        !active.iter().any(|name| name == "office_excel_read_range"),
        "no catalog op is active without an attached pane"
    );
    assert!(support::status()
        .as_deref()
        .unwrap_or_default()
        .contains("no app attached"));

    // The real server is listening and speaks the protocol.
    let body = support::health(port);
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["service"], json!("pi-office-bridge"));
    assert_eq!(body["panes"].as_array().unwrap().len(), 0);
    assert_eq!(body["catalogVersion"], json!(3));

    // Connecting again on the same port is idempotent; another port needs a
    // restart.
    assert!(
        support::run_command("office", &format!("connect {port}"))["text"]
            .as_str()
            .unwrap()
            .contains("listening")
    );
    assert!(
        support::run_command("office", &format!("connect {}", port + 1))["text"]
            .as_str()
            .unwrap()
            .contains("Unable to run /office: Office bridge is already listening")
    );

    // ── status + tools while on ────────────────────────────────────────
    let status = support::run_command("office", "status");
    let text = status["text"].as_str().unwrap();
    assert!(text.contains(&format!("listening on :{port}")), "{text}");
    assert!(text.contains("no app attached"));

    let listing = support::run_command("office-tools", "");
    let text = listing["text"].as_str().unwrap();
    assert!(text.contains("[on]  office_list_documents"), "{text}");
    assert!(text.contains("[off] office_excel_read_range"), "{text}");
    assert!(text.contains("32 catalogue"), "{text}");

    // ── close + restart ────────────────────────────────────────────────
    let closed = support::run_command("office", "close");
    assert!(closed["text"].as_str().unwrap().contains("bridge closed"));
    assert!(!support::active_tools()
        .iter()
        .any(|name| name.starts_with("office_")));

    let reopened = support::run_command("office", &format!("restart {port}"));
    assert!(
        reopened["text"]
            .as_str()
            .unwrap()
            .contains("restarted on ws://127.0.0.1:"),
        "{}",
        reopened["text"]
    );

    // ── shutdown ───────────────────────────────────────────────────────
    support::run_empty_event(EventTag::SessionShutdown);
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the listener is released on session shutdown"
    );
    assert!(support::status().is_none());
}
