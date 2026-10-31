//! Immutable, in-memory stdio authority and bounded catalog decoding.
use crate::{CoreRequestContext, StdioServer, Tool};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// An immutable stdio configuration with explicit scope and absolute working directory.
/// Clones share discovery admission, but never retain a runtime or connection.
/// Intentionally not `Debug`: command arguments and environment may contain secrets.
#[derive(Clone)]
pub struct ConfiguredStdio {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) config: StdioServer,
    pub(crate) generation: Arc<Mutex<Option<Arc<()>>>>,
}

impl ConfiguredStdio {
    pub fn new(
        context: &CoreRequestContext,
        server_name: &str,
        mut config: StdioServer,
    ) -> Result<Self, String> {
        fn field(value: &str) -> bool {
            !value.trim().is_empty() && !value.contains('\0')
        }
        if !field(&context.scope) || !field(server_name) {
            return Err("MCP stdio scope and server name must be nonblank and NUL-free".into());
        }
        let cwd = context.cwd.to_str().filter(|s| field(s));
        if !context.cwd.is_absolute() || cwd.is_none() {
            return Err("MCP stdio context cwd must be an absolute UTF-8 path without NUL".into());
        }
        if !field(&config.command)
            || config.args.iter().any(|s| s.contains('\0'))
            || config
                .env
                .iter()
                .any(|(k, v)| !field(k) || k.contains('=') || v.contains('\0'))
        {
            return Err("MCP stdio command and environment keys must be nonblank; all fields must be NUL-free and environment keys cannot contain '='".into());
        }
        if !config.enabled || !(1..=300).contains(&config.timeout) {
            return Err(
                "MCP stdio target must be enabled with timeout from 1 to 300 seconds".into(),
            );
        }
        match &config.cwd {
            Some(path) if !field(path) || !Path::new(path).is_absolute() => {
                return Err("MCP stdio configured cwd must be absolute and NUL-free".into());
            }
            None => config.cwd = Some(cwd.expect("validated cwd").into()),
            _ => {}
        }
        // Nested identity tag cannot collide with legacy [scope, string, name] keys.
        // Never place command, args, env, cwd, or serialized configuration in a key.
        let key =
            serde_json::to_string(&(context.scope.as_str(), ["configured-stdio"], server_name))
                .map_err(|_| "MCP stdio target identity could not be encoded")?;
        Ok(Self {
            key,
            name: server_name.into(),
            config,
            generation: Arc::new(Mutex::new(None)),
        })
    }
}

/// Bounds applied during discovery, before accumulating each tool. No implicit defaults.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryLimits {
    pub max_tools: usize,
    /// UTF-8 JSON bytes of the catalog array, including unknown tool fields.
    pub max_catalog_bytes: usize,
}

pub(crate) struct Catalog {
    pub tools: Vec<Tool>,
    names: HashSet<String>,
    cursors: HashSet<String>,
    bytes: usize,
    pages: usize,
    limits: DiscoveryLimits,
}
impl Catalog {
    pub fn new(limits: DiscoveryLimits) -> Result<Self, String> {
        if limits.max_catalog_bytes < 2 {
            return Err("MCP tool catalog byte limit must allow an empty array".into());
        }
        Ok(Self {
            tools: Vec::new(),
            names: HashSet::new(),
            cursors: HashSet::new(),
            bytes: 2,
            pages: 0,
            limits,
        })
    }

    pub fn page(&mut self, result: &Value) -> Result<Option<String>, String> {
        self.pages += 1;
        if self.pages > 100 {
            return Err("MCP tools/list exceeded 100 pages".into());
        }
        let items = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or("MCP tools/list result has no tools array")?;
        for item in items {
            if self.tools.len() >= self.limits.max_tools.min(16_384) {
                return Err("MCP tool catalog exceeded tool count limit".into());
            }
            let mut counter = ByteLimit {
                used: self.bytes + usize::from(!self.tools.is_empty()),
                max: self.limits.max_catalog_bytes,
            };
            serde_json::to_writer(&mut counter, item)
                .map_err(|_| "MCP tool catalog exceeded byte limit")?;
            let tool: Tool = serde_json::from_value(item.clone())
                .map_err(|_| "MCP tools/list returned an invalid tool")?;
            validate_tool_name(&tool.name)?;
            if !tool.input_schema.is_object()
                || tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
            {
                return Err("MCP tool inputSchema must have object type".into());
            }
            if !self.names.insert(tool.name.clone()) {
                return Err("MCP tools/list returned a duplicate tool name".into());
            }
            self.bytes = counter.used;
            self.tools.push(tool);
        }
        match result.get("nextCursor") {
            None => Ok(None),
            Some(Value::String(next)) => {
                if !self.cursors.insert(next.clone()) {
                    return Err("MCP tools/list returned a repeated pagination cursor".into());
                }
                if self.pages == 100 {
                    return Err("MCP tools/list exceeded 100 pages".into());
                }
                Ok(Some(next.clone()))
            }
            _ => Err("MCP tools/list nextCursor must be a string".into()),
        }
    }
}

pub(crate) fn validate_tool_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.len() > 200 {
        return Err("MCP tool name must be nonblank and at most 200 UTF-8 bytes".into());
    }
    Ok(())
}

struct ByteLimit {
    used: usize,
    max: usize,
}
impl Write for ByteLimit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .used
            .checked_add(bytes.len())
            .filter(|n| *n <= self.max)
            .ok_or_else(|| io::Error::other("catalog byte limit"))?;
        self.used = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn context() -> CoreRequestContext {
        CoreRequestContext {
            scope: "typed-test".into(),
            cwd: std::env::current_dir().unwrap(),
        }
    }
    pub(crate) fn config() -> StdioServer {
        StdioServer {
            command: "never-spawn-test-command".into(),
            args: vec!["SECRET-ARG".into()],
            env: [("TOKEN".into(), "SECRET-ENV".into())].into(),
            cwd: None,
            enabled: true,
            timeout: 5,
            exposure: crate::Exposure::Direct,
        }
    }
    fn tool(name: &str) -> Value {
        json!({"name":name,"description":"中文","inputSchema":{"type":"object"}})
    }
    fn catalog(max_tools: usize, max_catalog_bytes: usize) -> Catalog {
        Catalog::new(DiscoveryLimits {
            max_tools,
            max_catalog_bytes,
        })
        .unwrap()
    }

    #[test]
    fn target_identity_is_namespaced_nonsecret_and_immutable() {
        let ctx = context();
        let mut original = config();
        let target = ConfiguredStdio::new(&ctx, "server.with spaces/中文", original.clone())
            .ok()
            .unwrap();
        original.command = "changed".into();
        assert_eq!(target.config.command, "never-spawn-test-command");
        assert_eq!(target.config.cwd.as_deref(), ctx.cwd.to_str());
        let key: Value = serde_json::from_str(&target.key).unwrap();
        assert_eq!(
            key,
            json!([ctx.scope, ["configured-stdio"], "server.with spaces/中文"])
        );
        for secret in ["SECRET-ARG", "SECRET-ENV", "never-spawn-test-command"] {
            assert!(!target.key.contains(secret));
        }
        let mut changed = config();
        changed.env.clear();
        assert_eq!(
            target.key,
            ConfiguredStdio::new(&ctx, &target.name, changed)
                .ok()
                .unwrap()
                .key
        );
        assert_ne!(
            target.key,
            serde_json::to_string(&(ctx.scope, "configured-stdio", &target.name)).unwrap()
        );
        let clone = target.clone();
        assert!(Arc::ptr_eq(&clone.generation, &target.generation));
        let token = Arc::new(());
        *target.generation.lock().unwrap() = Some(token.clone());
        assert!(Arc::ptr_eq(
            clone.generation.lock().unwrap().as_ref().unwrap(),
            &token
        ));
        let fresh = ConfiguredStdio::new(&context(), &target.name, config())
            .ok()
            .unwrap();
        assert!(fresh.generation.lock().unwrap().is_none());
    }

    #[test]
    fn constructor_rejects_invalid_authority_without_echoing_values() {
        for bad in ["", "  ", "SECRET\0VALUE"] {
            let mut ctx = context();
            ctx.scope = bad.into();
            assert!(ConfiguredStdio::new(&ctx, "server", config()).is_err());
            assert!(ConfiguredStdio::new(&context(), bad, config()).is_err());
            for part in 0..5 {
                let mut cfg = config();
                match part {
                    0 => cfg.command = bad.into(),
                    1 if !bad.contains('\0') => continue,
                    1 => cfg.args.push(bad.into()),
                    2 => {
                        cfg.env.insert(bad.into(), "value".into());
                    }
                    3 if !bad.contains('\0') => continue,
                    3 => {
                        cfg.env.insert("key".into(), bad.into());
                    }
                    _ => cfg.cwd = Some(bad.into()),
                }
                let error = ConfiguredStdio::new(&context(), "server", cfg)
                    .err()
                    .unwrap();
                assert!(!error.contains("SECRET"));
            }
        }
        let mut ctx = context();
        ctx.cwd = "relative".into();
        assert!(ConfiguredStdio::new(&ctx, "server", config()).is_err());
        for timeout in [0, 301, u64::MAX] {
            let mut cfg = config();
            cfg.timeout = timeout;
            assert!(ConfiguredStdio::new(&context(), "server", cfg).is_err());
        }
        for timeout in [1, 300] {
            let mut cfg = config();
            cfg.timeout = timeout;
            assert!(ConfiguredStdio::new(&context(), "server", cfg).is_ok());
        }
        let mut cfg = config();
        cfg.cwd = Some("relative".into());
        assert!(ConfiguredStdio::new(&context(), "server", cfg).is_err());
        let mut cfg = config();
        cfg.enabled = false;
        assert!(ConfiguredStdio::new(&context(), "server", cfg).is_err());
        let mut cfg = config();
        cfg.env.insert("bad=key".into(), "value".into());
        assert!(ConfiguredStdio::new(&context(), "server", cfg).is_err());
    }

    #[test]
    fn empty_and_whitespace_arguments_and_environment_values_are_preserved() {
        let mut cfg = config();
        cfg.args = vec![String::new(), " \t".into()];
        cfg.env.insert("EMPTY".into(), String::new());
        cfg.env.insert("SPACES".into(), " \t".into());
        let target = ConfiguredStdio::new(&context(), "server", cfg.clone())
            .ok()
            .unwrap();
        assert_eq!(target.config.args, cfg.args);
        assert_eq!(target.config.env, cfg.env);
    }

    #[test]
    fn tool_names_allow_unicode_punctuation_and_use_byte_limits() {
        for name in ["not a native/name:中文", &"é".repeat(100)] {
            let mut decoder = catalog(10, 4096);
            decoder.page(&json!({"tools":[tool(name)]})).unwrap();
            assert_eq!(decoder.tools[0].name, name);
        }
        for name in ["", " \t\n", &"é".repeat(101)] {
            assert!(catalog(10, 4096)
                .page(&json!({"tools":[tool(name)]}))
                .is_err());
        }
    }

    #[test]
    fn catalog_enforces_exact_serialized_bytes_across_pages() {
        let one = tool("one");
        let two = tool("two");
        let exact = serde_json::to_vec(&json!([one, two])).unwrap().len();
        let mut decoder = catalog(2, exact);
        assert_eq!(
            decoder
                .page(&json!({"tools":[one],"nextCursor":"next"}))
                .unwrap(),
            Some("next".into())
        );
        assert_eq!(decoder.page(&json!({"tools":[two]})).unwrap(), None);
        assert_eq!(decoder.bytes, exact);
        let mut decoder = catalog(2, exact - 1);
        decoder.page(&json!({"tools":[one]})).unwrap();
        assert!(decoder
            .page(&json!({"tools":[two]}))
            .unwrap_err()
            .contains("byte limit"));
        assert_eq!(decoder.tools.len(), 1);
        // Fields not projected into Tool still count against the wire catalog budget.
        let mut extra = one.clone();
        extra["unknown"] = json!("x".repeat(500));
        assert!(catalog(2, exact).page(&json!({"tools":[extra]})).is_err());
    }

    #[test]
    fn catalog_enforces_count_duplicate_schema_and_shape_bounds() {
        let mut decoder = catalog(1, 4096);
        decoder.page(&json!({"tools":[tool("one")]})).unwrap();
        assert!(decoder
            .page(&json!({"tools":[tool("two")]}))
            .unwrap_err()
            .contains("count limit"));
        assert_eq!(decoder.tools.len(), 1);
        let mut decoder = catalog(10, 4096);
        decoder.page(&json!({"tools":[tool("one")]})).unwrap();
        assert!(decoder
            .page(&json!({"tools":[tool("one")]}))
            .unwrap_err()
            .contains("duplicate"));
        for schema in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"type":"string"}),
            json!({"type":["object"]}),
        ] {
            assert!(catalog(1, 4096)
                .page(&json!({"tools":[{"name":"x","inputSchema":schema}]}))
                .is_err());
        }
        for page in [
            json!({}),
            json!({"tools":{}}),
            json!({"tools":[{}]}),
            json!({"tools":[{"name":123}]}),
        ] {
            assert!(catalog(1, 4096).page(&page).is_err());
        }
        assert!(catalog(0, 2).page(&json!({"tools":[]})).is_ok());
        assert!(Catalog::new(DiscoveryLimits {
            max_tools: 1,
            max_catalog_bytes: 1
        })
        .is_err());
        assert!(catalog(0, 4096)
            .page(&json!({"tools":[tool("one")]}))
            .is_err());
    }

    #[test]
    fn pagination_rejects_repeated_invalid_and_excess_cursors() {
        let mut decoder = catalog(1, 4096);
        let page = json!({"tools":[],"nextCursor":"same"});
        decoder.page(&page).unwrap();
        assert!(decoder.page(&page).unwrap_err().contains("repeated"));
        for cursor in [Value::Null, json!(false), json!(1), json!({})] {
            assert!(catalog(1, 4096)
                .page(&json!({"tools":[],"nextCursor":cursor}))
                .is_err());
        }
        let mut decoder = catalog(1, 4096);
        for index in 0..99 {
            decoder
                .page(&json!({"tools":[],"nextCursor":index.to_string()}))
                .unwrap();
        }
        assert!(decoder.page(&json!({"tools":[]})).is_ok());
        let mut decoder = catalog(1, 4096);
        for index in 0..99 {
            decoder
                .page(&json!({"tools":[],"nextCursor":index.to_string()}))
                .unwrap();
        }
        assert!(decoder
            .page(&json!({"tools":[],"nextCursor":"last"}))
            .unwrap_err()
            .contains("100 pages"));
    }
}
