//! The Responses stream state machine, following pi-ai's `processResponsesStream`.
use super::json::{finite_number, string, truthy};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;

pub trait EventSink {
    fn emit(&mut self, event: Value) -> impl Future<Output = Result<(), String>> + Send;
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Flow {
    Continue,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SlotKind {
    Thinking,
    Text,
    ToolCall,
}

pub struct State {
    pub output: Value,
    model: Value,
    slots: BTreeMap<u64, (SlotKind, usize)>,
    /// Raw arguments text per tool-call slot; parsed JSON lands on the block.
    arguments: BTreeMap<usize, String>,
    unfinished: Vec<String>,
    saw_terminal: bool,
}

impl State {
    pub fn new(model: &Value, timestamp: i64) -> Self {
        Self {
            output: json!({"role":"assistant","content":[],"api":model["api"],"provider":model["provider"],"model":model["id"],
                "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":0,
                    "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":timestamp}),
            model: model.clone(),
            slots: BTreeMap::new(),
            arguments: BTreeMap::new(),
            unfinished: Vec::new(),
            saw_terminal: false,
        }
    }
    pub async fn start(&self, sink: &mut impl EventSink) -> Result<(), String> {
        sink.emit(json!({"type":"start","partial":self.output}))
            .await
    }

    fn push_content(&mut self, block: Value) -> usize {
        let content = self.output["content"].as_array_mut().unwrap();
        content.push(block);
        content.len() - 1
    }
    fn block(&mut self, index: usize) -> &mut Value {
        &mut self.output["content"].as_array_mut().unwrap()[index]
    }

    async fn create_slot(
        &mut self,
        output_index: u64,
        item: &Value,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        let kind = match item["type"].as_str() {
            Some("reasoning") => SlotKind::Thinking,
            Some("message") => {
                // A final_answer-phase message means the reply ends with this item.
                if item["phase"] == "final_answer" {
                    self.output["stopReason"] = json!("stop");
                }
                SlotKind::Text
            }
            Some("function_call") => {
                let call_id = match item["id"].as_str().filter(|id| !id.is_empty()) {
                    Some(id) => format!("{}|{id}", item["call_id"].as_str().unwrap_or_default()),
                    None => item["call_id"].as_str().unwrap_or_default().to_owned(),
                };
                let raw = item["arguments"].as_str().unwrap_or_default().to_owned();
                let block = json!({
                    "type": "toolCall",
                    "id": call_id,
                    "name": item["name"].as_str().unwrap_or_default(),
                    "arguments": parse_arguments(&raw),
                });
                let index = self.push_content(block);
                self.arguments.insert(index, raw);
                self.unfinished.push(call_id);
                self.slots.insert(output_index, (SlotKind::ToolCall, index));
                sink.emit(
                    json!({"type":"toolcall_start","contentIndex":index,"partial":self.output}),
                )
                .await?;
                return Ok(());
            }
            _ => return Ok(()),
        };
        let index = match kind {
            SlotKind::Thinking => self.push_content(json!({"type": "thinking", "thinking": ""})),
            SlotKind::Text => self.push_content(json!({"type": "text", "text": ""})),
            SlotKind::ToolCall => unreachable!(),
        };
        self.slots.insert(output_index, (kind, index));
        let event = match kind {
            SlotKind::Thinking => "thinking_start",
            SlotKind::Text => "text_start",
            SlotKind::ToolCall => unreachable!(),
        };
        sink.emit(json!({"type": event, "contentIndex": index, "partial": self.output}))
            .await
    }

    fn slot_index(&self, output_index: u64, kind: SlotKind) -> Option<usize> {
        self.slots
            .get(&output_index)
            .filter(|(slot_kind, _)| *slot_kind == kind)
            .map(|(_, index)| *index)
    }

    async fn delta(
        &mut self,
        output_index: u64,
        kind: SlotKind,
        field: &str,
        delta: &str,
        event: &str,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        let Some(index) = self.slot_index(output_index, kind) else {
            return Ok(());
        };
        let block = self.block(index);
        let mut text = block[field].as_str().unwrap_or_default().to_owned();
        text.push_str(delta);
        block[field] = text.into();
        sink.emit(
            json!({"type": event, "contentIndex": index, "delta": delta, "partial": self.output}),
        )
        .await
    }

    /// One SSE event. The driver feeds only complete `data:` JSON values.
    pub async fn event(
        &mut self,
        event: &Value,
        sink: &mut impl EventSink,
    ) -> Result<Flow, String> {
        if event.is_null() {
            return Err("Cannot read properties of null (reading 'type')".into());
        }
        match event["type"].as_str().unwrap_or_default() {
            "response.created" => {
                if let Some(id) = event["response"]["id"].as_str() {
                    self.output["responseId"] = id.into();
                }
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item") {
                    self.create_slot(
                        event["output_index"].as_u64().unwrap_or_default(),
                        item,
                        sink,
                    )
                    .await?;
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let delta = event["delta"].as_str().unwrap_or_default();
                self.delta(
                    event["output_index"].as_u64().unwrap_or_default(),
                    SlotKind::Thinking,
                    "thinking",
                    delta,
                    "thinking_delta",
                    sink,
                )
                .await?;
            }
            "response.reasoning_summary_part.done" => {
                self.delta(
                    event["output_index"].as_u64().unwrap_or_default(),
                    SlotKind::Thinking,
                    "thinking",
                    "\n\n",
                    "thinking_delta",
                    sink,
                )
                .await?;
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = event["delta"].as_str().unwrap_or_default();
                self.delta(
                    event["output_index"].as_u64().unwrap_or_default(),
                    SlotKind::Text,
                    "text",
                    delta,
                    "text_delta",
                    sink,
                )
                .await?;
            }
            "response.function_call_arguments.delta" => {
                let output_index = event["output_index"].as_u64().unwrap_or_default();
                let Some(index) = self.slot_index(output_index, SlotKind::ToolCall) else {
                    return Ok(Flow::Continue);
                };
                let delta = event["delta"].as_str().unwrap_or_default().to_owned();
                let raw = self.arguments.entry(index).or_default();
                raw.push_str(&delta);
                self.block(index)["arguments"] = parse_arguments(raw);
                sink.emit(json!({
                    "type": "toolcall_delta", "contentIndex": index, "delta": delta, "partial": self.output,
                }))
                .await?;
            }
            "response.function_call_arguments.done" => {
                let output_index = event["output_index"].as_u64().unwrap_or_default();
                let Some(index) = self.slot_index(output_index, SlotKind::ToolCall) else {
                    return Ok(Flow::Continue);
                };
                if let Some(arguments) = event["arguments"].as_str() {
                    self.arguments.insert(index, arguments.to_owned());
                    self.block(index)["arguments"] = parse_arguments(arguments);
                }
            }
            "response.output_item.done" => {
                self.item_done(event, sink).await?;
            }
            "response.completed" | "response.incomplete" => {
                self.finalize_response(&event["response"])?;
                self.saw_terminal = true;
                return Ok(Flow::Complete);
            }
            "response.failed" => {
                self.saw_terminal = true;
                let status = event["response"]["status"].as_str().unwrap_or_default();
                let error = &event["response"]["error"];
                let details = &event["response"]["incomplete_details"];
                let message = if !error.is_null() {
                    format!(
                        "{}: {}",
                        error["code"].as_str().unwrap_or("unknown"),
                        error["message"].as_str().unwrap_or("no message")
                    )
                } else if let Some(reason) = details["reason"].as_str() {
                    format!("incomplete: {reason}")
                } else {
                    "Unknown error (no error details in response)".into()
                };
                return Err(format!(
                    "OpenAI Codex response failed ({status}): {message}"
                ));
            }
            "error" => {
                return Err(format!(
                    "Error Code {}: {}",
                    string(&event["code"]),
                    event["message"].as_str().unwrap_or("Unknown error")
                ));
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    async fn item_done(&mut self, event: &Value, sink: &mut impl EventSink) -> Result<(), String> {
        let output_index = event["output_index"].as_u64().unwrap_or_default();
        let Some(item) = event.get("item") else {
            return Ok(());
        };
        if item["type"] == "message" && item["phase"] == "final_answer" {
            self.output["stopReason"] = json!("stop");
        }
        // An item can arrive done without a preceding added event.
        if !self.slots.contains_key(&output_index) {
            self.create_slot(output_index, item, sink).await?;
        }
        let Some((kind, index)) = self.slots.get(&output_index).copied() else {
            return Ok(());
        };
        match (kind, item["type"].as_str()) {
            (SlotKind::Thinking, Some("reasoning")) => {
                let summary = item["summary"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                let content = item["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                let thinking = if !summary.is_empty() {
                    summary
                } else if !content.is_empty() {
                    content
                } else {
                    self.block(index)["thinking"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                };
                self.block(index)["thinking"] = thinking.clone().into();
                // The whole item is the replay signature for the next turn.
                self.block(index)["thinkingSignature"] =
                    serde_json::to_string(item).unwrap_or_default().into();
                sink.emit(json!({
                    "type": "thinking_end", "contentIndex": index, "content": thinking, "partial": self.output,
                }))
                .await?;
                self.slots.remove(&output_index);
            }
            (SlotKind::Text, Some("message")) => {
                let text = item["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| match part["type"].as_str() {
                                Some("output_text") => part["text"].as_str(),
                                Some("refusal") => part["refusal"].as_str(),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                self.block(index)["text"] = text.clone().into();
                let mut signature = json!({"v": 1, "id": item["id"].as_str().unwrap_or_default()});
                if let Some(phase) = item["phase"].as_str() {
                    signature["phase"] = phase.into();
                }
                self.block(index)["textSignature"] = signature.to_string().into();
                sink.emit(json!({
                    "type": "text_end", "contentIndex": index, "content": text, "partial": self.output,
                }))
                .await?;
                self.slots.remove(&output_index);
            }
            (SlotKind::ToolCall, Some("function_call")) => {
                if let Some(arguments) = item["arguments"].as_str() {
                    self.arguments.insert(index, arguments.to_owned());
                    self.block(index)["arguments"] = parse_arguments(arguments);
                }
                if let Some(namespace) = item.get("namespace").filter(|value| !value.is_null()) {
                    self.block(index)["namespace"] = namespace.clone();
                }
                let call = self.block(index).clone();
                if let Some(id) = call["id"].as_str() {
                    self.unfinished.retain(|known| known != id);
                }
                sink.emit(json!({
                    "type": "toolcall_end", "contentIndex": index, "toolCall": call, "partial": self.output,
                }))
                .await?;
                self.slots.remove(&output_index);
            }
            _ => {}
        }
        Ok(())
    }

    fn finalize_response(&mut self, response: &Value) -> Result<(), String> {
        if let Some(id) = response["id"].as_str() {
            self.output["responseId"] = id.into();
        }
        if let Some(usage) = response.get("usage").filter(|value| truthy(value)) {
            let input = usage["input_tokens"].as_f64().unwrap_or(0.0);
            let cached = usage["input_tokens_details"]["cached_tokens"]
                .as_f64()
                .unwrap_or(0.0);
            let cache_write = usage["input_tokens_details"]["cache_write_tokens"]
                .as_f64()
                .unwrap_or(0.0);
            let output = usage["output_tokens"].as_f64().unwrap_or(0.0);
            let reasoning = usage["output_tokens_details"]["reasoning_tokens"]
                .as_f64()
                .unwrap_or(0.0);
            let total = usage["total_tokens"].as_f64().unwrap_or(0.0);
            self.output["usage"] = json!({
                "input": finite_number((input - cached - cache_write).max(0.0)),
                "output": finite_number(output),
                "cacheRead": finite_number(cached),
                "cacheWrite": finite_number(cache_write),
                "reasoning": finite_number(reasoning),
                "totalTokens": finite_number(total),
            });
            let cost = self.cost(input + cache_write);
            self.output["usage"]["cost"] = cost;
        }
        let status = response["status"].as_str().unwrap_or_default();
        let incomplete_reason = response["incomplete_details"]["reason"].as_str();
        self.output["rawStopReason"] = match incomplete_reason {
            Some(reason) => format!("{status}.{reason}").into(),
            None => status.into(),
        };
        match status {
            "completed" => self.output["stopReason"] = json!("stop"),
            "incomplete" => {
                if incomplete_reason == Some("max_output_tokens") {
                    self.output["stopReason"] = json!("length");
                } else {
                    self.output["stopReason"] = json!("error");
                    self.output["errorMessage"] = match incomplete_reason {
                        Some(reason) => format!("Response incomplete: {reason}").into(),
                        None => "Response incomplete without a provider reason".into(),
                    };
                }
            }
            "failed" | "cancelled" => {
                self.output["stopReason"] = json!("error");
            }
            // in_progress / queued: the stream ended early; Pi maps these to stop.
            _ => self.output["stopReason"] = json!("stop"),
        }
        let has_tool_call = self.output["content"]
            .as_array()
            .is_some_and(|content| content.iter().any(|block| block["type"] == "toolCall"));
        if has_tool_call && self.output["stopReason"] == "stop" {
            self.output["stopReason"] = json!("toolUse");
        }
        Ok(())
    }

    fn cost(&self, input_tokens: f64) -> Value {
        let mut rates = &self.model["cost"];
        let mut threshold = -1.0;
        if let Some(tiers) = self.model["cost"]["tiers"].as_array() {
            for tier in tiers {
                if let Some(boundary) = tier["inputTokensAbove"].as_f64() {
                    if input_tokens > boundary && boundary > threshold {
                        rates = tier;
                        threshold = boundary;
                    }
                }
            }
        }
        let usage = &self.output["usage"];
        let mut total = 0.0;
        let mut cost = json!({});
        for key in ["input", "output", "cacheRead"] {
            let value = rates[key].as_f64().unwrap_or(0.0) / 1_000_000.0
                * usage[key].as_f64().unwrap_or(0.0);
            cost[key] = finite_number(value);
            total += value;
        }
        let write = rates["cacheWrite"].as_f64().unwrap_or(0.0) / 1_000_000.0
            * usage["cacheWrite"].as_f64().unwrap_or(0.0);
        cost["cacheWrite"] = finite_number(write);
        total += write;
        cost["total"] = finite_number(total);
        cost
    }

    pub async fn finish(
        &mut self,
        cancelled: bool,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        if cancelled {
            return Err("Request was aborted".into());
        }
        if !self.saw_terminal {
            return Err("OpenAI Codex stream ended before a terminal response event".into());
        }
        if self.output["content"].as_array().unwrap().is_empty() {
            return Err("OpenAI Codex returned an empty response".into());
        }
        if self.output["stopReason"] == "toolUse" && !self.unfinished.is_empty() {
            return Err(format!(
                "OpenAI Codex stream completed with an unfinished tool call ({})",
                self.unfinished.join(", ")
            ));
        }
        sink.emit(json!({"type":"done","reason":self.output["stopReason"],"message":self.output}))
            .await
    }

    pub async fn fail(
        &mut self,
        error: &str,
        cancelled: bool,
        credentials: &[String],
        accounts: &[String],
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        self.output["stopReason"] = json!(if cancelled { "aborted" } else { "error" });
        self.output["errorMessage"] = super::redaction::text(error, credentials, accounts).into();
        sink.emit(json!({"type":"error","reason":self.output["stopReason"],"error":self.output}))
            .await
    }
}

/// Parses streamed arguments text into a JSON value; partial JSON stays as raw text so a
/// cut-off stream never fabricates structure.
fn parse_arguments(raw: &str) -> Value {
    match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => Value::String(raw.to_owned()),
    }
}
