//! Invocation-local native receipts. Tokens never enter guest-visible objects.
use super::*;

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
    pub(super) fn release_reference(&mut self, token: usize) -> Result<(), String> {
        self.check()?;
        if token == 0 {
            return Ok(());
        } // WeakMap-authenticated, already materialized small result.
        let Some(reference) = self.references.remove(&token) else {
            return self.fail("released or invalid native receipt");
        };
        let reply = (self.action)(json!({"op":"release_result","resultId":reference.id}));
        match reply {
            Ok(reply)
                if reply.to_string().len() <= MAX_JSON
                    && reply["resultId"].as_str() == Some(&reference.id)
                    && reply["status"] == "released" => {}
            _ => return self.fail("host release_result failed (not retried)"),
        }
        if self.selected_reference == Some(token) {
            return self.fail("selected native receipt was released");
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
                let _ = (self.action)(json!({"op":"release_result","resultId":reference.id}));
            }
        }
        selected
    }
}
