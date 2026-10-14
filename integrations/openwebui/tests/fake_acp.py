"""Offline ACP peer used by the Pipe transport tests; no models or tools run."""
import json
import os
from pathlib import Path
import sys
import uuid

sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8")

root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
(root / "gate-env.json").write_text(json.dumps({key:os.environ.get(key) for key in
    ("RPI_ACP_NO_PERMISSION_GATE", "RPI_PERMISSION_GATE")}), encoding="utf-8")
prompt_id = None
dialog_mode = False


def send(message):
    print(json.dumps({"jsonrpc": "2.0", **message}, ensure_ascii=False), flush=True)


def complete(session):
    send({"method":"session/update", "params":{"sessionId":session, "update":{
        "sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"完成🌊"}}}})
    send({"id":prompt_id, "result":{"stopReason":"end_turn"}})


for line in sys.stdin:
    message = json.loads(line)
    with (root / "messages.jsonl").open("a", encoding="utf-8") as log:
        log.write(json.dumps(message, ensure_ascii=False) + "\n")
    method = message.get("method")
    params = message.get("params", {})
    if method == "initialize":
        send({"id":message["id"], "result":{"protocolVersion":1, "agentCapabilities":{"_meta":{"pi-rust/questions":1}}}})
    elif method == "session/new":
        session = str(uuid.uuid4())
        (root / session).write_text("saved", encoding="utf-8")
        send({"id":message["id"], "result":{"sessionId":session}})
    elif method == "session/resume":
        session = params["sessionId"]
        if (root / session).is_file():
            send({"id":message["id"], "result":{}})
        else:
            send({"id":message["id"], "error":{"code":-32000,"message":"Missing session"}})
    elif method == "session/prompt":
        prompt_id = message["id"]
        session = params["sessionId"]
        text = params["prompt"][0]["text"]
        if "disconnect" in text:
            break
        elif "closed dialog" in text:
            send({"id":100, "method":"_rpi/request_question", "params":{"sessionId":session,"requestId":"q", "ui":{"kind":"input", "question":"Waiting?"}}})
            send({"method":"_rpi/dialog_closed", "params":{"sessionId":session,"requestId":"q"}})
            complete(session)
        elif "dialogs" in text:
            dialog_mode = True
            send({"method":"session/update", "params":{"sessionId":session, "update":{"sessionUpdate":"tool_call", "toolCallId":"ask", "title":"ask_user", "status":"in_progress"}}})
            send({"id":100, "method":"_rpi/request_question", "params":{"sessionId":session,"requestId":"q", "ui":{
                "kind":"selector", "question":"语言？", "options":[{"title":"Rust"},{"title":"Python"}],
                "allowMultiple":True, "allowComment":True}}})
        else:
            complete(session)
    elif method == "session/cancel":
        send({"id":prompt_id, "result":{"stopReason":"cancelled"}})
    elif message.get("id") == 100 and "result" in message:
        send({"method":"_rpi/dialog_closed", "params":{"sessionId":session,"requestId":"q"}})
        if os.environ.get("RPI_ACP_NO_PERMISSION_GATE") == "1":
            complete(session)
            continue
        send({"id":101, "method":"session/request_permission", "params":{"sessionId":session,
            "_meta":{"pi-rust/dialog":{"requestId":"p"}}, "toolCall":{"toolCallId":"bash", "title":"Run command?"},
            "options":[{"optionId":"allow", "name":"允许一次", "kind":"allow_once"},
                       {"optionId":"reject", "name":"拒绝", "kind":"reject_once"}]}})
    elif message.get("id") == 101 and "result" in message:
        send({"method":"session/update", "params":{"sessionId":session, "update":{"sessionUpdate":"tool_call_update", "toolCallId":"ask", "status":"completed", "rawOutput":"<script>danger</script>"}}})
        complete(session)
