use rpi_agent::{AgentTool, TextContentOrImage};
use rpi_extensions::{load_session_mixed, NullDiagnostics, PluginToolAdapter};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "set RPI_TODO_DLL to the installed todo DLL"]
async fn installed_todo_updates_return_complete_task_states() {
    let dll = PathBuf::from(std::env::var("RPI_TODO_DLL").unwrap());
    let session = load_session_mixed(&[], &[dll], Arc::new(NullDiagnostics), None);
    let snapshot = session.snapshot().unwrap();
    let tool = snapshot
        .tools()
        .iter()
        .find(|tool| tool.tool.name == "todo")
        .unwrap();
    let adapter = PluginToolAdapter::new(tool.tool.clone(), tool.handle(), session.keepalive());
    let run = |params| {
        let adapter = &adapter;
        async move {
            adapter
                .execute(
                    "todo-smoke",
                    params,
                    CancellationToken::new(),
                    Arc::new(|_| {}),
                )
                .await
        }
    };
    run(json!({"action":"clear"})).await.unwrap();
    run(json!({"action":"add","text":"Run tests"}))
        .await
        .unwrap();
    let added = run(json!({"action":"add","text":"Build release"}))
        .await
        .unwrap();
    let TextContentOrImage::Text(text) = &added.content[0] else {
        panic!("text checklist expected");
    };
    assert!(text.text.contains("Run tests") && text.text.contains("Build release"));
    let active = run(json!({"action":"update","id":1,"status":"in_progress"}))
        .await
        .unwrap();
    assert_eq!(active.details["todos"][0]["status"], "in_progress");
    let completed = run(json!({"action":"update","id":1,"status":"completed"}))
        .await
        .unwrap();
    assert_eq!(completed.details["todos"][0]["done"], true);
    assert!(run(json!({"action":"update","id":2,"status":"invalid"}))
        .await
        .is_err());
    let listed = run(json!({"action":"list","includeDone":true}))
        .await
        .unwrap();
    assert_eq!(listed.details["todos"][1]["status"], "pending");
}
