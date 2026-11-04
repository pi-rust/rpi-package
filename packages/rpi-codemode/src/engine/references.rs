//! Invocation-local native receipts. Tokens never enter guest-visible objects.
use super::*;

pub(super) enum OutputPart {
    Inline(Vec<Value>),
    Reference { token: usize, indices: Value },
}

pub(super) struct Reference {
    id: String,
    pub(super) bytes: usize,
    blocks: usize,
    pub(super) is_error: bool,
    pub(super) terminate: bool,
}
impl Bridge {
    pub(super) fn retain(&mut self, token: usize, id: &str, reply: &Value) -> Result<(), String> {
        let validated = (|| {
            if reply.get("result").is_some() {
                return Err("inline result after reference start");
            }
            let r = &reply["resultReference"];
            let bytes = r["byteLength"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= MEMORY as u64)
                .ok_or("invalid reference byteLength (maximum 32 MiB)")?
                as usize;
            let blocks = r["contentBlocks"]
                .as_u64()
                .filter(|n| *n <= bytes as u64)
                .ok_or("invalid reference contentBlocks")? as usize;
            let is_error = reply["isError"]
                .as_bool()
                .ok_or("invalid reference isError")?;
            if r["version"].as_u64() != Some(1)
                || r["id"].as_str() != Some(id)
                || r["isError"].as_bool() != Some(is_error)
            {
                return Err("invalid result reference identity/version/isError");
            }
            let terminate = r["terminate"]
                .as_bool()
                .ok_or("invalid reference terminate")?;
            Ok(Reference {
                id: id.into(),
                bytes,
                blocks,
                is_error,
                terminate,
            })
        })();
        match validated {
            Ok(reference) if self.references.len() < 16 => {
                self.references.insert(token, reference);
                Ok(())
            }
            Ok(_) => self.fail("retained reference limit exceeded"),
            Err(error) => self.fail(error),
        }
    }
    pub(super) fn read_reference(
        &mut self,
        token: usize,
        materialize: bool,
    ) -> Result<String, String> {
        let result: Result<String, String> = (|| {
            self.check()?;
            let reference = self
                .references
                .get(&token)
                .ok_or("released or invalid native receipt")?;
            if !materialize {
                return Ok(String::new());
            }
            let mut text = String::new();
            let mut offset = 0;
            while offset < reference.bytes {
                self.check()?;
                let reply = (self.action)(
                    json!({"op":"read_result","resultId":reference.id,"offset":offset,"limit":65536}),
                )?;
                self.check()?;
                if reply.to_string().len() > MAX_JSON {
                    return Err("host result frame exceeds 1 MiB".into());
                }
                let data = reply["data"].as_str().ok_or("invalid result chunk data")?;
                let end = offset
                    .checked_add(data.len())
                    .ok_or("result cursor overflow")?;
                if reply["resultId"].as_str() != Some(&reference.id)
                    || reply["offset"].as_u64() != Some(offset as u64)
                    || reply["byteLength"].as_u64() != Some(reference.bytes as u64)
                    || data.is_empty()
                    || data.len() > 65536
                    || end > reference.bytes
                    || reply.get("result").is_some()
                    || reply.get("resultReference").is_some()
                    || if end == reference.bytes {
                        reply.get("nextOffset") != Some(&Value::Null)
                    } else {
                        reply["nextOffset"].as_u64() != Some(end as u64)
                    }
                {
                    return Err("invalid result chunk identity/length/cursor/EOF".into());
                }
                text.push_str(data);
                offset = end;
            }
            self.check()?;
            let value: Value =
                serde_json::from_str(&text).map_err(|_| "invalid native result JSON")?;
            if !value.is_object()
                || value["content"].as_array().map(Vec::len) != Some(reference.blocks)
                || value
                    .get("terminate")
                    .is_some_and(|v| v.as_bool() != Some(reference.terminate))
                || (reference.terminate && value["terminate"] != true)
                || value
                    .get("isError")
                    .is_some_and(|v| v.as_bool() != Some(reference.is_error))
            {
                return Err("native result does not match reference manifest".into());
            }
            Ok(text)
        })();
        match result {
            Ok(text) => Ok(text),
            Err(e) => self.fail(&e),
        }
    }
    /// Account canonical block bytes, plus the final content ARRAY framing.
    pub(super) fn output_budget(&mut self, blocks: usize, bytes: usize) -> Result<(), String> {
        let count = self.output_blocks.saturating_add(blocks);
        let encoded = self.output_bytes.saturating_add(bytes);
        if self.output_parts.len() >= 128
            || count > content::MAX_BLOCKS
            || encoded
                .saturating_add(count.saturating_sub(1))
                .saturating_add(2)
                > MEMORY - OUTPUT_DIAGNOSTIC_RESERVE
        {
            return self.fail(
                "native output budget exceeded (128 parts / 128 blocks / 32 MiB content array including diagnostic reserve)",
            );
        }
        self.output_blocks = count;
        self.output_bytes = encoded;
        Ok(())
    }
    pub(super) fn emit_reference(&mut self, token: usize, encoded: String) -> Result<(), String> {
        let validated: Result<(Value, usize, usize), String> = (|| {
            self.check()?;
            if self.selected_result.is_some() || self.selected_reference.is_some() {
                return Err("tools.emit cannot be combined with tools.result".into());
            }
            let reference = self
                .references
                .get(&token)
                .ok_or("released or invalid native receipt")?;
            if encoded.len() > MAX_JSON {
                return Err("emitted indices exceed 1 MiB".into());
            }
            let indices: Value =
                serde_json::from_str(&encoded).map_err(|_| "invalid emitted indices")?;
            let count = match &indices {
                Value::Null => reference.blocks,
                Value::Array(values) if values.len() <= 128 => {
                    for index in values {
                        if !index.as_u64().is_some_and(|n| n < reference.blocks as u64) {
                            return Err("indices must contain in-range nonnegative integers".into());
                        }
                    }
                    values.len()
                }
                _ => return Err("indices must be null or an array of at most 128 integers".into()),
            };
            if count > 128 {
                return Err("native content exceeds 128 blocks".into());
            }
            let reply = (self.action)(
                json!({"op":"measure_result_content","resultId":reference.id,"indices":indices}),
            )?;
            self.check()?;
            if reply.to_string().len() > MAX_JSON
                || reply["resultId"].as_str() != Some(&reference.id)
                || reply["contentBlocks"].as_u64() != Some(count as u64)
                || !reply.as_object().is_some_and(|m| m.len() == 3)
            {
                return Err("invalid content measurement identity/count/frame".into());
            }
            let bytes = reply["encodedBytes"]
                .as_u64()
                .filter(|n| *n <= MEMORY as u64)
                .ok_or("invalid content measurement encodedBytes")?
                as usize;
            if (count == 0 && bytes != 0) || (count > 0 && bytes < count.saturating_mul(2)) {
                return Err("invalid content measurement encodedBytes/count".into());
            }
            Ok((indices, count, bytes))
        })();
        match validated {
            Ok((indices, count, bytes)) => {
                self.output_budget(count, bytes)?;
                self.output_parts
                    .push(OutputPart::Reference { token, indices });
                self.has_emission = true;
                Ok(())
            }
            Err(e) => self.fail(&e),
        }
    }
    /// Clean only unselected retained storage before proving emitted receipts
    /// still belong to the live invocation. At most 16 Rust-owned references are
    /// retained; duplicate emissions never multiply cleanup or liveness queries.
    pub(super) fn prepare_emitted_handoff(&mut self) -> bool {
        let tokens: HashSet<_> = self
            .output_parts
            .iter()
            .filter_map(|part| match part {
                OutputPart::Reference { token, .. } => Some(*token),
                OutputPart::Inline(_) => None,
            })
            .collect();
        if tokens.is_empty() || self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        let unselected: Vec<_> = self
            .references
            .keys()
            .filter(|token| !tokens.contains(token))
            .copied()
            .collect();
        let mut cleanup_ok = true;
        for token in unselected {
            let reference = self.references.remove(&token).expect("retained reference");
            let reply = (self.action)(json!({"op":"release_result","resultId":reference.id}));
            if !reply.is_ok_and(|reply| {
                reply.to_string().len() <= MAX_JSON
                    && reply["resultId"].as_str() == Some(&reference.id)
                    && reply["status"] == "released"
            }) {
                cleanup_ok = false;
            }
        }
        if !cleanup_ok || self.reference_cleanup_failed {
            // A known retained-reference cleanup failure remains fail-closed,
            // even if a different receipt could subsequently pass a probe.
            self.reference_cleanup_failed = true;
            self.host_live.store(false, Ordering::Release);
            return false;
        }
        if !self.host_live.load(Ordering::Acquire) {
            // A failed operation can be an ordinary native budget error, not
            // owner expiry. Do NOT retry it or clear the sticky guest failure.
            // Empty selection is a distinct read-only ownership query: no tool
            // starts, payload/chunk reads, storage mutation or new authority.
            for token in tokens {
                if self.cancelled.load(Ordering::Acquire) {
                    return false;
                }
                let Some(reference) = self.references.get(&token) else {
                    return false;
                };
                let reply = (self.action)(json!({"op":"measure_result_content",
                    "resultId":reference.id,"indices":[]}));
                if !reply.is_ok_and(|reply| {
                    reply.to_string().len() <= MAX_JSON
                        && reply.as_object().is_some_and(|m| m.len() == 3)
                        && reply["resultId"].as_str() == Some(&reference.id)
                        && reply["contentBlocks"].as_u64() == Some(0)
                        && reply["encodedBytes"].as_u64() == Some(0)
                }) {
                    return false;
                }
            }
            if self.cancelled.load(Ordering::Acquire) {
                return false;
            }
            // Only ALL successful probes restore attachment eligibility. Native
            // final decoding rechecks the actual owning invocation once again.
            self.host_live.store(true, Ordering::Release);
        }
        !self.cancelled.load(Ordering::Acquire)
    }
    /// Resolve only Rust-owned tokens, then remove handed-off storage from local
    /// cleanup. Native finish owns it until Done decoding (also on framing error).
    pub(super) fn handoff_content_parts(&mut self, keep: bool) -> Option<Value> {
        if !keep
            || !self
                .output_parts
                .iter()
                .any(|p| matches!(p, OutputPart::Reference { .. }))
        {
            return None;
        }
        let mut retained = HashSet::new();
        let parts = self.output_parts.iter().map(|part| match part {
            OutputPart::Inline(blocks) => json!({"content":blocks}),
            OutputPart::Reference { token, indices } => {
                retained.insert(*token);
                json!({"reference":{"version":1,"id":self.references[token].id},"indices":indices})
            }
        }).collect::<Vec<_>>();
        for token in retained {
            self.references.remove(&token);
        }
        Some(Value::Array(parts))
    }
    pub(super) fn release_reference(&mut self, token: usize) -> Result<(), String> {
        self.check()?;
        if token == 0 {
            return Ok(());
        } // WeakMap-authenticated, already materialized small result.
        if self.selected_reference == Some(token)
            || self.output_parts.iter().any(
                |p| matches!(p, OutputPart::Reference { token: emitted, .. } if *emitted == token),
            )
        {
            return self.fail("selected native receipt cannot be released");
        }
        let Some(reference) = self.references.remove(&token) else {
            return self.fail("released or invalid native receipt");
        };
        let reply = (self.action)(json!({"op":"release_result","resultId":reference.id}));
        match reply {
            Ok(reply)
                if reply.to_string().len() <= MAX_JSON
                    && reply["resultId"].as_str() == Some(&reference.id)
                    && reply["status"] == "released" => {}
            _ => {
                self.host_live.store(false, Ordering::Release);
                self.reference_cleanup_failed = true;
                return self.fail("host release_result failed (not retried)");
            }
        }
        Ok(())
    }
    pub(super) fn select_reference(&mut self, token: usize) -> Result<(), String> {
        self.check()?;
        if !self.references.contains_key(&token) {
            return self.fail("released or invalid native receipt");
        }
        if self.selected_reference.is_some() || self.selected_result.is_some() || self.has_emission
        {
            return self.fail(
                "tools.result requires exactly one selection and cannot combine with tools.emit",
            );
        }
        self.selected_reference = Some(token);
        Ok(())
    }
    pub(super) fn cleanup_references(&mut self, handoff: Option<usize>) -> Option<Value> {
        let mut selected = None;
        for (token, reference) in self.references.drain() {
            if Some(token) == handoff {
                selected = Some(json!({"version":1,"id":reference.id}));
            } else {
                if (self.action)(json!({"op":"release_result","resultId":reference.id})).is_err() {
                    self.host_live.store(false, Ordering::Release);
                }
            }
        }
        selected
    }
}
