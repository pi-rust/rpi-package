use rpi_plugin_sdk::{CommandHandlerFn, PluginApi, RuntimeActionId, StbString, StbStringRef};
use serde_json::{json, Value};
use std::sync::{mpsc, Condvar, Mutex};
use std::time::Duration;

static COMMAND: Mutex<Option<CommandHandlerFn>> = Mutex::new(None);
static STATE: Mutex<Value> = Mutex::new(Value::Null);
static RUN: Mutex<(bool, bool, bool)> = Mutex::new((false, false, false));
static RUN_CHANGED: Condvar = Condvar::new();

extern "C" fn register_command(_: StbStringRef, _: StbStringRef, handler: CommandHandlerFn) -> i32 {
    *COMMAND.lock().unwrap() = Some(handler);
    0
}

rpi_plugin_sdk::string_handler! {
fn runtime_action(action, args, out, _user_data) {
    let args: Value = serde_json::from_str(args).unwrap();
    let result = match RuntimeActionId::try_from(action).unwrap() {
        RuntimeActionId::GetSessionBranch => {
            assert_eq!(args, json!({"customTypes": ["goal-state"]}));
            let state = STATE.lock().unwrap().clone();
            json!({"filtered": true, "entries": [{"type":"custom", "customType":"goal-state", "data":state}]})
        }
        RuntimeActionId::AppendEntry => {
            if args["customType"] == "goal-state" {
                *STATE.lock().unwrap() = args["data"].clone();
            } else {
                assert_eq!(args["message"]["data"]["customType"], "goal");
            }
            json!({"entryId":"test-entry"})
        }
        RuntimeActionId::SendUserMessage => {
            assert_eq!(STATE.lock().unwrap()["active"]["title"], "Keep the editor responsive");
            assert_eq!(args["text"], "Work on the active goal: Keep the editor responsive\nNotes: regression test");
            let mut run = RUN.lock().unwrap();
            run.0 = true;
            RUN_CHANGED.notify_all();
            // Model a run that stays active until the test explicitly finishes it.
            let (mut run, _) = RUN_CHANGED
                .wait_timeout_while(run, Duration::from_secs(5), |run| !run.1)
                .unwrap();
            run.2 = true;
            RUN_CHANGED.notify_all();
            json!({"status":"completed"})
        }
        other => panic!("unexpected runtime action: {other:?}"),
    };
    out.write(result.to_string());
    0
}
}

fn invoke(args: &str) -> Value {
    let command = COMMAND.lock().unwrap().unwrap();
    let input = json!({"command":"goal", "args":args}).to_string();
    let mut output = StbString::empty();
    let rc = command(
        StbStringRef::from_str(&input),
        &mut output,
        std::ptr::null_mut(),
    );
    let text = output.to_string_lossy();
    rpi_plugin_sdk::callbacks::free_string(output);
    assert_eq!(rc, 0);
    serde_json::from_str(&text).unwrap()
}

#[test]
fn start_command_returns_and_accepts_more_commands_while_work_is_running() {
    let api = PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        runtime_action,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        user_data: std::ptr::null_mut(),
        register_command: Some(register_command),
        register_tool: None,
        register_shortcut: None,
        register_flag: None,
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: None,
        register_resources_discover: None,
        dispatch_event: None,
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
    };
    // A missing tool registrar returns 1 after registering the slash command.
    assert_eq!(rpi_goal::rpi_plugin_register(&api), 1);

    for prefix in ["start ", ""] {
        *RUN.lock().unwrap() = (false, false, false);
        let (tx, rx) = mpsc::channel();
        let args = format!("{prefix}Keep the editor responsive --notes regression test");
        let command_thread = std::thread::spawn(move || tx.send(invoke(&args)).unwrap());
        let response = rx.recv_timeout(Duration::from_secs(2));
        if response.is_err() {
            RUN.lock().unwrap().1 = true;
            RUN_CHANGED.notify_all();
            command_thread.join().unwrap();
            panic!("/goal blocked the editor until the agent run completed");
        }
        let response = response.unwrap();
        assert_eq!(response["kind"], "message");
        assert!(response["text"].as_str().unwrap().contains("Goal started"));
        command_thread.join().unwrap();

        let (run, timeout) = RUN_CHANGED
            .wait_timeout_while(RUN.lock().unwrap(), Duration::from_secs(2), |run| !run.0)
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "the first work message was never sent"
        );
        assert!(
            !run.2,
            "the run must still be active when the command returns"
        );
        drop(run);
        assert!(invoke("status")["text"]
            .as_str()
            .unwrap()
            .contains("Keep the editor responsive"));
        assert!(invoke("help")["text"]
            .as_str()
            .unwrap()
            .contains("/goal start"));

        RUN.lock().unwrap().1 = true;
        RUN_CHANGED.notify_all();
        let (_run, timeout) = RUN_CHANGED
            .wait_timeout_while(RUN.lock().unwrap(), Duration::from_secs(2), |run| !run.2)
            .unwrap();
        assert!(!timeout.timed_out(), "the background run did not finish");
    }
}
