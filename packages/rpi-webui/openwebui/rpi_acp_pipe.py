"""
title: RPI ACP
author: pi-rust
version: 0.1.0
description: Connect Open WebUI to rpi over ACP, including questions and permissions.
"""
import asyncio
from contextlib import contextmanager
import hashlib
import html
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time
import uuid
from typing import Literal

from pydantic import BaseModel, Field


QUESTION_CAPABILITY = "pi-rust/questions"


def text_content(message):
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list) and all(item.get("type") == "text" for item in content):
        return "\n".join(item.get("text", "") for item in content)
    raise ValueError("RPI ACP currently accepts text messages only.")


def fingerprint(messages):
    return hashlib.sha256(json.dumps(
        [(m.get("role"), text_content(m)) for m in messages], ensure_ascii=False,
        separators=(",", ":"),
    ).encode()).hexdigest()


def select_values(raw, options, multiple=False, freeform=False):
    """Option numbers are unambiguous even when labels contain commas or digits."""
    if not isinstance(raw, str) or not raw.strip():
        raise ValueError("Please enter an answer.")
    raw = raw.strip()
    if raw.startswith("text:"):
        values = [raw[5:].strip()]
    elif raw.startswith("["):
        values = json.loads(raw)
        if not isinstance(values, list):
            raise ValueError("Expected a JSON array of labels.")
    elif raw in options:
        values = [raw]
    else:
        parts = [value.strip() for value in raw.replace("，", ",").split(",")]
        if all(value.isdecimal() and 1 <= int(value) <= len(options) for value in parts):
            values = [options[int(value) - 1] for value in parts]
        else:
            values = [raw]
    if not values or len(values) > 64 or (not multiple and len(values) != 1):
        raise ValueError("Select one option." if not multiple else "Select at least one option.")
    if any(not isinstance(v, str) or not v.strip() or len(v.encode()) > 65536 for v in values):
        raise ValueError("Invalid answer.")
    if len(set(values)) != len(values):
        raise ValueError("Do not select the same option twice.")
    if not freeform and any(value not in options for value in values):
        raise ValueError("Choose one of the listed options.")
    return values


class StateStore:
    """Persistent chat mapping plus leases shared by Open WebUI worker processes."""
    def __init__(self, directory):
        directory = Path(directory).expanduser()
        directory.mkdir(parents=True, exist_ok=True)
        self.path = directory / "sessions.sqlite3"
        with self.connect() as db:
            db.execute("CREATE TABLE IF NOT EXISTS sessions (key TEXT PRIMARY KEY, session TEXT, history TEXT, owner TEXT, expires REAL)")

    @contextmanager
    def connect(self):
        db = sqlite3.connect(self.path, timeout=5)
        try:
            with db:
                yield db
        finally:
            db.close()

    def acquire(self, key, owner, timeout):
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT session, history, owner, expires FROM sessions WHERE key=?", (key,)).fetchone()
            if row and row[2] and row[3] > time.time():
                raise RuntimeError("A reply is already running for this chat.")
            db.execute("INSERT INTO sessions VALUES (?,NULL,NULL,?,?) ON CONFLICT(key) DO UPDATE SET owner=excluded.owner, expires=excluded.expires", (key, owner, time.time() + timeout + 60))
            return (row[0], row[1]) if row else (None, None)

    def release(self, key, owner, session=None, history=None):
        with self.connect() as db:
            # An interrupted turn is not reusable: its backend history may be partial.
            db.execute("UPDATE sessions SET session=?, history=?, owner=NULL, expires=0 WHERE key=? AND owner=?", (session, history, key, owner))


class AcpClient:
    def __init__(self, command, args, cwd, event_call=None, event_emitter=None, dialog_timeout=300,
                 bypass_tool_permissions=False):
        self.command, self.args, self.cwd = command, args, cwd
        self.event_call, self.event_emitter = event_call, event_emitter
        self.dialog_timeout = dialog_timeout
        self.bypass_tool_permissions = bypass_tool_permissions
        self.proc = None
        self.pending = {}
        self.events = asyncio.Queue(maxsize=1024)
        self.dialogs = {}
        self.closed_dialogs = set()
        self.next_id = 1
        self.write_lock = asyncio.Lock()
        self.dialog_lock = asyncio.Lock()
        self.reader = self.stderr = None

    async def start(self):
        env = os.environ.copy()
        env.update(RPI_WEBUI_CHILD="1", RPI_WEBUI_AUTOSTART="0", RPI_ACP_NO_AUTO_REGISTER_ZED="1")
        # Configure the child gate, rather than silently answering any dialog.
        # Explicit mode also overrides inherited deployment flags in either direction.
        env.update(RPI_ACP_NO_PERMISSION_GATE="1" if self.bypass_tool_permissions else "0",
                   RPI_PERMISSION_GATE="0" if self.bypass_tool_permissions else "1")
        kwargs = {"creationflags": subprocess.CREATE_NO_WINDOW} if os.name == "nt" else {}
        self.proc = await asyncio.create_subprocess_exec(
            self.command, *self.args, cwd=self.cwd, env=env,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, limit=2 * 1024 * 1024, **kwargs,
        )
        self.reader = asyncio.create_task(self.read())
        self.stderr = asyncio.create_task(self.drain_stderr())
        result = await self.request("initialize", {
            "protocolVersion": 1,
            "clientInfo": {"name": "rpi-openwebui", "version": "0.1.0"},
            "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False},
                                   "terminal": False, "_meta": {QUESTION_CAPABILITY: 1}},
        })
        if result.get("protocolVersion") != 1:
            raise RuntimeError("The bridge did not negotiate ACP v1.")
        if result.get("agentCapabilities", {}).get("_meta", {}).get(QUESTION_CAPABILITY) != 1:
            raise RuntimeError("Update rpi-acp-bridge: question extension v1 is required.")
        return result

    async def send(self, message):
        if not self.proc or self.proc.returncode is not None:
            raise RuntimeError("ACP bridge is closed.")
        async with self.write_lock:
            self.proc.stdin.write((json.dumps(message, ensure_ascii=False) + "\n").encode())
            await self.proc.stdin.drain()

    async def request(self, method, params, timeout=30):
        request_id = self.next_id
        self.next_id += 1
        future = asyncio.get_running_loop().create_future()
        self.pending[request_id] = future
        try:
            await self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
            return await asyncio.wait_for(future, timeout)
        finally:
            self.pending.pop(request_id, None)

    async def notify(self, method, params):
        await self.send({"jsonrpc": "2.0", "method": method, "params": params})

    async def drain_stderr(self):
        while await self.proc.stderr.read(4096):
            pass

    async def read(self):
        error = RuntimeError("ACP bridge disconnected before completing the request.")
        try:
            while line := await self.proc.stdout.readline():
                message = json.loads(line)
                if "method" not in message and "id" in message:
                    future = self.pending.get(message["id"])
                    if future and not future.done():
                        if "error" in message:
                            future.set_exception(RuntimeError(str(message["error"].get("message", "ACP request failed"))))
                        else:
                            future.set_result(message.get("result", {}))
                elif message.get("method") in ("session/request_permission", "_rpi/request_question") and "id" in message:
                    params = message.get("params", {})
                    dialog_id = params.get("requestId") or params.get("_meta", {}).get("pi-rust/dialog", {}).get("requestId") or str(message["id"])
                    if dialog_id in self.dialogs:
                        await self.send({"jsonrpc":"2.0", "id":message["id"], "error":{"code":-32602,"message":"Duplicate dialog"}})
                    else:
                        task = asyncio.create_task(self.handle_dialog(message, dialog_id))
                        self.dialogs[dialog_id] = task
                elif message.get("method") == "_rpi/dialog_closed":
                    dialog_id = message.get("params", {}).get("requestId")
                    self.closed_dialogs.add(dialog_id)
                    if task := self.dialogs.get(dialog_id):
                        task.cancel()
                elif "id" in message and "method" in message:
                    await self.send({"jsonrpc":"2.0", "id":message["id"], "error":{"code":-32601,"message":"Method not found"}})
                elif message.get("method") == "session/update":
                    self.events.put_nowait(message["params"])
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            error = RuntimeError(f"ACP transport failed: {exc}")
        finally:
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(error)
            for task in self.dialogs.values():
                task.cancel()

    async def call(self, data, timeout):
        if self.event_call is None:
            return None
        result = await asyncio.wait_for(self.event_call(data), timeout)
        # Open WebUI's native Cancel button acknowledges input events with False.
        return None if result is False or (isinstance(result, dict) and result.get("error")) else result

    async def choose(self, ui, timeout):
        options = ui.get("options") or []
        labels = [option if isinstance(option, str) else option["title"] for option in options]
        kind = ui.get("kind", "selector")
        question = ui.get("question") or ui.get("message") or "Your answer"
        chinese = any("\u4e00" <= char <= "\u9fff" for char in question)
        native_select = bool(labels) and not ui.get("allowMultiple") and not ui.get("allowFreeform")
        lines = [question]
        if ui.get("context"):
            lines.append(ui["context"])
        for index, option in enumerate(options, 1):
            description = option.get("description") if isinstance(option, dict) else None
            # Explicit brackets stay visible when the modal's Markdown CSS hides list markers.
            prefix = "" if native_select else f"[{index}] "
            lines.append(prefix + labels[index - 1] + (f" — {description}" if description else ""))
        if labels:
            if native_select:
                lines.append("请在下拉框中选择一项。" if chinese else "Choose one option from the dropdown.")
            elif chinese:
                lines.append("输入选项编号，多个选项用逗号分隔，例如 1,2。" if ui.get("allowMultiple") else "输入一个选项编号。")
            else:
                lines.append("Enter option numbers separated by commas." if ui.get("allowMultiple") else "Enter one option number.")
            if ui.get("allowFreeform"):
                lines.append("自定义回答请输入 text:你的回答；也支持 JSON 标签数组。" if chinese else "For a custom answer enter text:your answer. A JSON array of labels is also accepted.")
        deadline = asyncio.get_running_loop().time() + timeout
        failure = ""
        for _ in range(3):
            remaining = deadline - asyncio.get_running_loop().time()
            if remaining <= 0:
                return {"cancelled": True}
            data = {
                "title": ui.get("header") or ("RPI 提问" if chinese else "RPI question"), "message": "\n".join(lines) + failure,
                "value": ui.get("suggest") or "", "placeholder": "请输入回答" if chinese else "Your answer",
            }
            if native_select:
                data.update(type="select", options=[{"label":label, "value":label} for label in labels],
                            placeholder="请选择一项" if chinese else "Select an option")
            raw = await self.call({"type": "input", "data": data}, remaining)
            if raw is None:
                return {"cancelled": True}
            try:
                values = select_values(raw, labels, ui.get("allowMultiple", False), ui.get("allowFreeform", kind == "input"))
            except (ValueError, TypeError) as exc:
                failure = "\n回答无效，请选择有效选项，不要重复选择。" if chinese else f"\n{exc}"
                continue
            answer = {"values": values}
            if kind == "input":
                answer["text"] = values[0]
            elif ui.get("allowComment"):
                remaining = deadline - asyncio.get_running_loop().time()
                if remaining <= 0:
                    return {"cancelled": True}
                comment = await self.call({"type": "input", "data": {
                    "title": "RPI 备注" if chinese else "RPI comment",
                    "message": ("可填写补充说明：" if chinese else "Optional comment for: ") + ", ".join(values), "value": "",
                }}, remaining)
                if not isinstance(comment, str) or len(comment.encode()) > 65536:
                    return {"cancelled": True}
                answer["text"] = comment
            return {"answer": answer}
        return {"cancelled": True}

    async def handle_dialog(self, message, dialog_id):
        permission = message["method"] == "session/request_permission"
        result = {"outcome": {"outcome": "cancelled"}} if permission else {"cancelled": True}
        try:
            async with self.dialog_lock:
                if dialog_id in self.closed_dialogs:
                    return
                params = message["params"]
                if permission:
                    options = params.get("options", [])
                    ui = {"kind": "selector", "question": params.get("toolCall", {}).get("title", "Permission required"),
                          "options": [{"title": str(o["name"])} for o in options],
                          "allowMultiple": False, "allowFreeform": False,
                          "header": "RPI 权限确认" if any("\u4e00" <= c <= "\u9fff" for c in str(params.get("toolCall", {}).get("title", ""))) else "RPI permission"}
                else:
                    ui = params["ui"]
                timeout = min(self.dialog_timeout, max(1, ui.get("timeout") or self.dialog_timeout))
                answer = await asyncio.wait_for(self.choose(ui, timeout), timeout)
                if permission and "answer" in answer:
                    label = answer["answer"]["values"][0]
                    option = next(o for o in options if o["name"] == label)
                    result = {"outcome": {"outcome": "selected", "optionId": option["optionId"]}}
                elif not permission:
                    result = answer
        except asyncio.CancelledError:
            return
        except Exception:
            # Timeout, closed browser and invalid dialog all cancel; never approve implicitly.
            pass
        finally:
            self.dialogs.pop(dialog_id, None)
        if dialog_id not in self.closed_dialogs:
            try:
                await self.send({"jsonrpc": "2.0", "id": message["id"], "result": result})
            except (RuntimeError, BrokenPipeError, ConnectionResetError):
                pass

    async def close(self):
        if self.proc:
            if self.proc.stdin:
                self.proc.stdin.close()
            if self.proc.returncode is None:
                try:
                    await asyncio.wait_for(self.proc.wait(), 3)
                except asyncio.TimeoutError:
                    self.proc.terminate()
                    try:
                        await asyncio.wait_for(self.proc.wait(), 3)
                    except asyncio.TimeoutError:
                        self.proc.kill()
                        await self.proc.wait()
        tasks = [task for task in (self.reader, self.stderr, *self.dialogs.values()) if task]
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)


def tool_block(update):
    title = html.escape(str(update.get("title") or update.get("kind") or "Tool"))
    output = update.get("rawOutput", update.get("content", ""))
    rendered = html.escape(output if isinstance(output, str) else json.dumps(output, ensure_ascii=False))
    args = html.escape(json.dumps(update.get("rawInput", {}), ensure_ascii=False), quote=True)
    # Open WebUI's details tokenizer requires a newline after the opening tag.
    return f'\n<details type="tool_calls" done="true" id="{html.escape(str(update.get("toolCallId", "")), quote=True)}" name="{title}" arguments="{args}">\n<summary>{title}</summary>\n{rendered}\n</details>\n'


class Pipe:
    class Valves(BaseModel):
        MODEL_NAME: str = "RPI (ACP)"
        ACP_COMMAND: str = Field(default="rpi-acp-bridge", description="ACP executable on the Open WebUI server")
        ACP_ARGS: str = Field(default="[]", description="JSON array of arguments; use [\"--acp\"] with rpi")
        WORKSPACE: str = Field(default="", description="Absolute working directory on the server")
        STATE_DIR: str = Field(default="~/.rpi/openwebui", description="Persistent session mapping directory")
        TURN_TIMEOUT: int = Field(default=600, ge=1, le=86400)
        DIALOG_TIMEOUT: int = Field(default=300, ge=1, le=86400)
        PERMISSION_MODE: Literal["ask", "bypass"] = Field(
            default="ask", description="ask: confirm tool execution; bypass: run tools without approval. ask_user questions are still shown.")

    def __init__(self):
        self.valves = self.Valves()

    def pipes(self):
        return [{"id": "rpi", "name": self.valves.MODEL_NAME}]

    async def pipe(self, body: dict, __user__: dict = None, __chat_id__: str = None,
                   __event_emitter__=None, __event_call__=None, __task__: str = None):
        # Title/tag/follow-up generation must never invoke a coding agent and its tools.
        if __task__:
            return
        messages = body.get("messages", [])
        if not messages or messages[-1].get("role") != "user":
            raise ValueError("The last message must be a user message.")
        if not __user__ or not __user__.get("id") or not __chat_id__:
            raise ValueError("An authenticated Open WebUI chat is required.")
        if not self.valves.WORKSPACE:
            raise ValueError("Set WORKSPACE to the absolute project directory in the Pipe Valves.")
        cwd = Path(self.valves.WORKSPACE).expanduser()
        if not cwd.is_absolute() or not cwd.is_dir():
            raise ValueError("WORKSPACE must be an existing absolute directory on the Open WebUI server.")
        args = json.loads(self.valves.ACP_ARGS)
        if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
            raise ValueError("ACP_ARGS must be a JSON array of strings.")
        command = self.valves.ACP_COMMAND
        key = hashlib.sha256(json.dumps([__user__["id"], __chat_id__, str(cwd), command, args]).encode()).hexdigest()
        store = await asyncio.to_thread(StateStore, self.valves.STATE_DIR)
        owner = uuid.uuid4().hex
        saved, history = await asyncio.to_thread(store.acquire, key, owner, self.valves.TURN_TIMEOUT)
        client = AcpClient(command, args, str(cwd), __event_call__, __event_emitter__, self.valves.DIALOG_TIMEOUT,
                           bypass_tool_permissions=self.valves.PERMISSION_MODE == "bypass")
        completed_session = completed_history = None
        prompt_task = next_event = None
        chunks = []
        try:
            async with asyncio.timeout(self.valves.TURN_TIMEOUT):
                await client.start()
                resumed = saved and history == fingerprint(messages[:-1])
                if resumed:
                    # Do not fall back to an empty session when a saved session is missing.
                    await client.request("session/resume", {"sessionId": saved, "cwd": str(cwd), "mcpServers": []})
                    session_id = saved
                    content = text_content(messages[-1])
                else:
                    session_id = (await client.request("session/new", {"cwd": str(cwd), "mcpServers": []}))["sessionId"]
                    content = text_content(messages[-1]) if len(messages) == 1 else "Conversation context:\n" + "\n\n".join(
                        f"{m.get('role', 'user')}:\n{text_content(m)}" for m in messages
                    )
                prompt_task = asyncio.create_task(client.request("session/prompt", {
                    "sessionId": session_id, "prompt": [{"type": "text", "text": content}],
                }, self.valves.TURN_TIMEOUT))
                next_event = asyncio.create_task(client.events.get())
                tools = {}
                thinking = False
                while True:
                    done, _ = await asyncio.wait((prompt_task, next_event), return_when=asyncio.FIRST_COMPLETED)
                    if next_event in done:
                        params = next_event.result()
                        update = params.get("update", {}) if params.get("sessionId") == session_id else {}
                        kind = update.get("sessionUpdate")
                        chunk = ""
                        if kind == "agent_message_chunk" and update.get("content", {}).get("type") == "text":
                            chunk = update["content"]["text"]
                        elif kind == "agent_thought_chunk" and update.get("content", {}).get("type") == "text":
                            chunk = ("" if thinking else "<think>") + html.escape(update["content"]["text"])
                            thinking = True
                        elif kind == "plan" and __event_emitter__:
                            entries = update.get("entries", [])
                            description = "; ".join(str(entry.get("content", "")) for entry in entries)
                            await __event_emitter__({"type":"status", "data":{
                                "description":description, "done":bool(entries) and all(e.get("status") == "completed" for e in entries),
                            }})
                        elif kind in ("tool_call", "tool_call_update"):
                            tool = tools.setdefault(update.get("toolCallId"), {})
                            tool.update(update)
                            if __event_emitter__:
                                await __event_emitter__({"type":"status", "data":{
                                    "description": str(tool.get("title") or "Executing tool"),
                                    "done": tool.get("status") in ("completed", "failed"),
                                }})
                            if tool.get("status") in ("completed", "failed"):
                                chunk = tool_block(tool)
                                tools.pop(update.get("toolCallId"), None)
                        if chunk:
                            if thinking and kind != "agent_thought_chunk":
                                chunk = "</think>\n" + chunk
                                thinking = False
                            chunks.append(chunk)
                            yield chunk
                        next_event = asyncio.create_task(client.events.get())
                    # A prompt response ends the turn even if no further notification arrives.
                    if prompt_task.done() and client.events.empty() and not next_event.done():
                        # Queue.get may have consumed the final update but not resumed its task yet.
                        await asyncio.sleep(0)
                        if next_event.done() or not client.events.empty():
                            continue
                        result = prompt_task.result()
                        if result.get("stopReason") == "cancelled":
                            raise RuntimeError("RPI reply was cancelled.")
                        if thinking:
                            chunks.append("</think>\n")
                            yield "</think>\n"
                        completed_session = session_id
                        completed_history = fingerprint(messages + [{"role":"assistant", "content":"".join(chunks)}])
                        break
        finally:
            if prompt_task and not prompt_task.done():
                try:
                    await asyncio.wait_for(client.notify("session/cancel", {"sessionId":session_id}), 1)
                except Exception:
                    pass
            for task in (prompt_task, next_event):
                if task:
                    task.cancel()
            if prompt_task or next_event:
                await asyncio.gather(*(task for task in (prompt_task, next_event) if task), return_exceptions=True)
            await client.close()
            await asyncio.to_thread(store.release, key, owner, completed_session, completed_history)
            if __event_emitter__:
                chinese = any("\u4e00" <= char <= "\u9fff" for char in text_content(messages[-1]))
                description = ("完成" if completed_session else "已停止") if chinese else ("Complete" if completed_session else "Stopped")
                await __event_emitter__({"type":"status", "data":{"description":description, "done":True}})
