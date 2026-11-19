# Goal mode architecture

`rpi-goal` keeps goal mode inside the extension. The native host needs no
goal-specific projector, event, or context rule.

## State

One `goal-state` custom entry per session branch holds the goal (v2 adds the goal
id, iteration, safety counters, token usage, elapsed active time, and waiting
record). Reads are bounded: `GetSessionBranch { customTypes: ["goal-state"] }`
returns only `goal-state` entries and must confirm `filtered: true`.

Writes are queued and flushed at a safe boundary (turn end, command return, or the
continuation worker's own boundary). Appending from inside a tool batch would place
an entry between an `assistant(tool_calls)` message and its `toolResult`, which
strict OpenAI-compatible providers reject.

Read-modify-write of goal state is serialized by one in-process lock, and the
queued write is taken and committed under the same lock. A take that released the
lock before writing could commit an older snapshot after a newer mutation.

## Context notice

A goal change queues a serialized `AgentMessage::Custom` notice:

```json
{
  "kind": "custom",
  "role": "custom",
  "content": [{"type": "text", "text": "🎯 Active goal: ..."}],
  "data": {
    "customType": "goal",
    "display": true,
    "details": {"goalId": "...", "status": "active"}
  },
  "timestamp": 0
}
```

The host forwards custom messages through the ordinary session-context path. The
notice is not the work prompt: each continuation is a user message that carries the
objective, the current goal id, and the goal-mode rules.

Compaction keeps transcript entries but replaces the model-visible context with a
summary, so `SessionCompact` re-appends the current notice.

## Continuation

```text
AgentSettled ──wake──▶ worker ──▶ evaluate ──▶ SendMessage{triggerTurn:true}
                          ▲                          │  (blocks for the run)
                          └────────── settled ───────┘
```

- `AgentSettled` is a *notification*. The worker re-reads state and decides; a
  repeated or coalesced signal cannot dispatch twice.
- `SendMessage` is the reservation primitive. An idle lane starts a run and the call
  returns when it ends; a busy lane queues the message into the live run. rpi has no
  `isIdle()` equivalent and does not need one.
- While a run is live (`AgentStart` without `AgentEnd`/`AgentSettled`), the worker
  waits for the settled boundary instead of injecting a continuation.
- Every dispatch re-reads state afterwards. A pause, replacement, completion, or
  limit that happened during the run wins over the stale intent; a delayed
  continuation for a replaced goal id is dropped.
- Budgets are checked before dispatching and at `TurnEnd`, so a token or time
  budget exhaustion never buys one more turn.
- A committed stop (`/goal pause`, budget, safety, usage limit) requests the host's
  `Abort` action (raw id 27) when a run is live, so the stop takes effect
  immediately instead of after the current turn. The action is absent from the
  published SDK, so the call is numeric and a rejecting host degrades to the
  turn-boundary behavior with one warning.

## Accounting

- `TurnEnd` counts one turn, attributes it to automatic or user work, and feeds the
  no-progress heuristic for tool-free automatic output.
- `AfterProviderResponse` carries the terminal `AssistantMessage`, so token usage
  and interruption classification (`stop_reason`, `error_message`) come from the
  provider's own report rather than from transcript scraping.
- `ToolCall` marks the run as having attempted work, which resets the no-progress
  counter.

## Guarantees

- `rpi-cli` has no goal-specific projector, dispatcher, or renderer.
- One automatic run per settled boundary; no nested run, no double dispatch.
- The terminal tools reject a stale goal id before any state mutation.
- Notices are retried at a later boundary if an append fails.
- Subagent processes skip accounting, injection, and dispatch.
