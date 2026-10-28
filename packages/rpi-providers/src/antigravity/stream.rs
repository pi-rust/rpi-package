//! The Pi stream state machine; the driver supplies bounded, awaited event delivery.
use super::json::{finite_number, string, truthy};
use serde_json::{json, Value};
use std::future::Future;

pub trait EventSink {
    fn emit(&mut self, event: Value) -> impl Future<Output = Result<(), String>> + Send;
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Flow {
    Continue,
    TrailingUsage,
    Complete,
}

pub struct State {
    pub output: Value,
    model: Value,
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    pending_signature: Option<Value>,
    terminal_reason: Option<String>,
}
impl State {
    pub fn new(model: &Value, timestamp: i64) -> Self {
        Self {
            output: json!({"role":"assistant","content":[],"api":model["api"],"provider":model["provider"],"model":model["id"],
                "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
                    "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":timestamp}),
            model: model.clone(),
            text_index: None,
            thinking_index: None,
            pending_signature: None,
            terminal_reason: None,
        }
    }
    pub async fn start(&self, sink: &mut impl EventSink) -> Result<(), String> {
        sink.emit(json!({"type":"start","partial":self.output}))
            .await
    }
    async fn close_text(&mut self, sink: &mut impl EventSink) -> Result<(), String> {
        if let Some(index) = self.text_index.take() {
            sink.emit(json!({"type":"text_end","contentIndex":index,"content":self.output["content"][index]["text"],"partial":self.output})).await?;
        }
        Ok(())
    }
    async fn close_thinking(&mut self, sink: &mut impl EventSink) -> Result<(), String> {
        if let Some(index) = self.thinking_index.take() {
            sink.emit(json!({"type":"thinking_end","contentIndex":index,"content":self.output["content"][index]["thinking"],"partial":self.output})).await?;
        }
        Ok(())
    }
    pub async fn chunk(
        &mut self,
        chunk: &Value,
        next_id: &mut impl FnMut() -> String,
        sink: &mut impl EventSink,
    ) -> Result<Flow, String> {
        if chunk.is_null() {
            return Err("Cannot read properties of null (reading 'error')".into());
        }
        if let Some(error) = embedded_failure(chunk) {
            return Err(error);
        }
        update_usage(&self.model, &mut self.output, chunk.get("usageMetadata"));
        if self.terminal_reason.is_some() {
            return Ok(if truthy(&chunk["usageMetadata"]) {
                Flow::Complete
            } else {
                Flow::TrailingUsage
            });
        }
        let candidate = &chunk["candidates"][0];
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for part in parts {
                if part.is_null() {
                    return Err("Cannot read properties of null (reading 'thought')".into());
                }
                if !truthy(&part["thought"])
                    && !truthy(&part["functionCall"])
                    && !truthy(&part["text"])
                    && truthy(&part["thoughtSignature"])
                {
                    if let Some(index) = self.text_index {
                        self.output["content"][index]["textSignature"] =
                            part["thoughtSignature"].clone();
                    } else {
                        self.pending_signature = Some(part["thoughtSignature"].clone());
                    }
                    continue;
                }
                if truthy(&part["thought"])
                    && !truthy(&part["text"])
                    && truthy(&part["thoughtSignature"])
                {
                    self.pending_signature = Some(part["thoughtSignature"].clone());
                }
                if truthy(&part["functionCall"]) {
                    let call = &part["functionCall"];
                    let signature = part
                        .get("thoughtSignature")
                        .filter(|value| !value.is_null())
                        .cloned()
                        .or_else(|| self.pending_signature.clone());
                    self.pending_signature = None;
                    let tool = json!({"type":"toolCall",
                        "id":call.get("id").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| Value::String(format!("call_{}", next_id()))),
                        "name":call.get("name").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| json!("")),
                        "arguments":call.get("args").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| json!({}))});
                    let mut tool = tool;
                    if let Some(signature) = signature.filter(truthy) {
                        tool["thoughtSignature"] = signature;
                    }
                    self.close_text(sink).await?;
                    self.close_thinking(sink).await?;
                    let content = self.output["content"].as_array_mut().unwrap();
                    let index = content.len();
                    content.push(tool.clone());
                    sink.emit(
                        json!({"type":"toolcall_start","contentIndex":index,"partial":self.output}),
                    )
                    .await?;
                    sink.emit(json!({"type":"toolcall_end","contentIndex":index,"toolCall":tool,"partial":self.output})).await?;
                    self.output["stopReason"] = json!("toolUse");
                    continue;
                }
                if truthy(&part["thought"]) {
                    let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) else {
                        continue;
                    };
                    self.close_text(sink).await?;
                    let index = if let Some(index) = self.thinking_index {
                        index
                    } else {
                        let mut block = json!({"type":"thinking","thinking":""});
                        if truthy(&part["thoughtSignature"]) {
                            block["thinkingSignature"] = part["thoughtSignature"].clone();
                        }
                        let content = self.output["content"].as_array_mut().unwrap();
                        let index = content.len();
                        content.push(block);
                        self.thinking_index = Some(index);
                        sink.emit(json!({"type":"thinking_start","contentIndex":index,"partial":self.output})).await?;
                        index
                    };
                    let block = &mut self.output["content"][index];
                    let mut thinking = block["thinking"].as_str().unwrap().to_owned();
                    thinking.push_str(text);
                    block["thinking"] = thinking.into();
                    if truthy(&part["thoughtSignature"]) {
                        block["thinkingSignature"] = part["thoughtSignature"].clone();
                    }
                    sink.emit(json!({"type":"thinking_delta","contentIndex":index,"delta":text,"partial":self.output})).await?;
                    continue;
                }
                if let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) {
                    self.close_thinking(sink).await?;
                    let index = if let Some(index) = self.text_index {
                        index
                    } else {
                        let signature = part
                            .get("thoughtSignature")
                            .filter(|value| !value.is_null())
                            .cloned()
                            .or_else(|| self.pending_signature.clone());
                        self.pending_signature = None;
                        let mut block = json!({"type":"text","text":""});
                        if let Some(signature) = signature.filter(truthy) {
                            block["textSignature"] = signature;
                        }
                        let content = self.output["content"].as_array_mut().unwrap();
                        let index = content.len();
                        content.push(block);
                        self.text_index = Some(index);
                        sink.emit(
                            json!({"type":"text_start","contentIndex":index,"partial":self.output}),
                        )
                        .await?;
                        index
                    };
                    let block = &mut self.output["content"][index];
                    let mut current = block["text"].as_str().unwrap().to_owned();
                    current.push_str(text);
                    block["text"] = current.into();
                    if truthy(&part["thoughtSignature"]) {
                        block["textSignature"] = part["thoughtSignature"].clone();
                    }
                    sink.emit(json!({"type":"text_delta","contentIndex":index,"delta":text,"partial":self.output})).await?;
                }
            }
        }
        if truthy(&candidate["finishReason"]) {
            self.close_text(sink).await?;
            self.close_thinking(sink).await?;
            if self.output["stopReason"] != "toolUse" {
                self.output["stopReason"] = json!(if candidate["finishReason"] == "MAX_TOKENS" {
                    "length"
                } else {
                    "stop"
                });
            }
            self.terminal_reason = Some(string(&candidate["finishReason"]));
            if self.model["id"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains("gpt-oss")
                && !truthy(&chunk["usageMetadata"])
            {
                return Ok(Flow::TrailingUsage);
            }
            return Ok(Flow::Complete);
        }
        Ok(Flow::Continue)
    }
    pub async fn finish(
        &mut self,
        cancelled: bool,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        if cancelled {
            return Err("Request was aborted".into());
        }
        let Some(reason) = &self.terminal_reason else {
            return Err("Antigravity stream ended without a terminal candidate response".into());
        };
        if self.output["content"].as_array().unwrap().is_empty() {
            return Err(format!("Antigravity returned an empty response ({reason})"));
        }
        sink.emit(json!({"type":"done","reason":self.output["stopReason"],"message":self.output}))
            .await
    }
    pub async fn fail(
        &mut self,
        error: &str,
        cancelled: bool,
        credentials: &[String],
        projects: &[String],
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        self.output["stopReason"] = json!(if cancelled { "aborted" } else { "error" });
        self.output["errorMessage"] = super::redaction::text(error, credentials, projects).into();
        sink.emit(json!({"type":"error","reason":self.output["stopReason"],"error":self.output}))
            .await
    }
}

pub fn embedded_failure(chunk: &Value) -> Option<String> {
    if let Some(error) = chunk.get("error") {
        if !error.is_object() && !error.is_array() {
            return Some(format!(
                "Antigravity stream error{}",
                error
                    .as_str()
                    .map(|text| format!(": {text}"))
                    .unwrap_or_default()
            ));
        }
        let mut labels = Vec::new();
        if let Some(status) = error["status"].as_str() {
            labels.push(status.to_owned());
        }
        if error["code"].is_number() || error["code"].is_string() {
            labels.push(format!("code {}", string(&error["code"])));
        }
        if let Some(reason) = error["details"]
            .as_array()
            .and_then(|details| details.iter().find_map(|detail| detail["reason"].as_str()))
        {
            if !reason.is_empty() && !labels.iter().any(|label| label == reason) {
                labels.push(reason.into());
            }
        }
        let label = if labels.is_empty() {
            String::new()
        } else {
            format!(" ({})", labels.join(", "))
        };
        let message = error["message"]
            .as_str()
            .filter(|message| !message.is_empty())
            .map(|message| format!(": {message}"))
            .unwrap_or_default();
        return Some(format!("Antigravity stream error{label}{message}"));
    }
    let feedback = &chunk["promptFeedback"];
    let reason = feedback["blockReason"]
        .as_str()
        .filter(|value| !value.is_empty());
    let message = feedback["blockReasonMessage"]
        .as_str()
        .filter(|value| !value.is_empty());
    if reason.is_none() && message.is_none() {
        return None;
    }
    Some(format!(
        "Antigravity prompt blocked{}{}",
        reason
            .map(|value| format!(" ({value})"))
            .unwrap_or_default(),
        message
            .map(|value| format!(": {value}"))
            .unwrap_or_default()
    ))
}
pub fn update_usage(model: &Value, output: &mut Value, metadata: Option<&Value>) {
    let Some(metadata) = metadata.filter(|value| truthy(value)) else {
        return;
    };
    let usage = &mut output["usage"];
    let cache = metadata["cachedContentTokenCount"]
        .as_f64()
        .unwrap_or_else(|| usage["cacheRead"].as_f64().unwrap());
    let prompt = metadata["promptTokenCount"]
        .as_f64()
        .unwrap_or_else(|| usage["input"].as_f64().unwrap() + usage["cacheRead"].as_f64().unwrap());
    usage["input"] = finite_number((prompt - cache).max(0.0));
    if metadata.get("candidatesTokenCount").is_some()
        || metadata.get("thoughtsTokenCount").is_some()
    {
        usage["output"] = finite_number(
            metadata["candidatesTokenCount"].as_f64().unwrap_or(0.0)
                + metadata["thoughtsTokenCount"].as_f64().unwrap_or(0.0),
        );
    }
    usage["cacheRead"] = finite_number(cache);
    usage["totalTokens"] = finite_number(
        ["input", "output", "cacheRead", "cacheWrite"]
            .iter()
            .map(|key| usage[key].as_f64().unwrap())
            .sum(),
    );
    let input_tokens =
        usage["input"].as_f64().unwrap() + cache + usage["cacheWrite"].as_f64().unwrap();
    let mut rates = &model["cost"];
    let mut threshold = -1.0;
    if let Some(tiers) = model["cost"]["tiers"].as_array() {
        for tier in tiers {
            if let Some(boundary) = tier["inputTokensAbove"].as_f64() {
                if input_tokens > boundary && boundary > threshold {
                    rates = tier;
                    threshold = boundary;
                }
            }
        }
    }
    let mut total = 0.0;
    for key in ["input", "output", "cacheRead"] {
        let cost = rates[key].as_f64().unwrap_or(0.0) / 1_000_000.0 * usage[key].as_f64().unwrap();
        usage["cost"][key] = finite_number(cost);
        total += cost;
    }
    let long_write = usage["cacheWrite1h"].as_f64().unwrap_or(0.0);
    let short_write = usage["cacheWrite"].as_f64().unwrap() - long_write;
    let write_cost = (rates["cacheWrite"].as_f64().unwrap_or(0.0) * short_write
        + rates["input"].as_f64().unwrap_or(0.0) * 2.0 * long_write)
        / 1_000_000.0;
    usage["cost"]["cacheWrite"] = finite_number(write_cost);
    total += write_cost;
    usage["cost"]["total"] = finite_number(total);
}
