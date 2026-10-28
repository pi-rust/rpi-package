//! Pinned Pi catalog and reachable model resolution; historical IDs remain readable.
use rpi_ai::{Api, InputModality, Model};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

pub const API: &str = "google-generative-ai";
/// Pi's registered base URL. Requests use `super::ENDPOINT`, as Pi's stream does.
pub const BASE_URL: &str = "https://cloudcode-pa.googleapis.com";
pub const RETIRED_MESSAGE: &str =
    "This Antigravity model is no longer offered. Choose another model.";

#[derive(Deserialize)]
struct Data {
    catalog: Vec<Value>,
    native: HashMap<String, NativeModel>,
    aliases: HashMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeModel {
    pub model: String,
    pub max_output_tokens: Option<u64>,
    pub thinking_level: Option<u8>,
    thinking_budget: Option<i64>,
    #[serde(default)]
    supports_thinking: bool,
    #[serde(default)]
    pub supports_adaptive_thinking: bool,
}
fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| serde_json::from_str(include_str!("catalog.json")).expect("pinned catalog"))
}
/// The six catalog entries exactly as Pi declares them.
pub(crate) fn entries() -> &'static [Value] {
    &data().catalog
}
/// Registration catalog: the six entries as host models, in Pi's order.
pub(crate) fn models(provider: &str) -> Vec<Model> {
    entries()
        .iter()
        .map(|entry| {
            let id = entry["id"].as_str().unwrap();
            let name = entry["name"].as_str().unwrap();
            let mut model = Model::new(id, name, Api::GoogleGenerativeAi, provider, BASE_URL);
            model.reasoning = entry["reasoning"].as_bool().unwrap_or(false);
            model.input = entry["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| match value.as_str() {
                    Some("image") => InputModality::Image,
                    _ => InputModality::Text,
                })
                .collect();
            model.context_window = entry["contextWindow"].as_u64().unwrap();
            model.max_tokens = entry["maxTokens"].as_u64().unwrap();
            model
        })
        .collect()
}
pub(crate) fn native(model: &str) -> Option<&'static NativeModel> {
    data().native.get(&model.to_lowercase())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedModel {
    pub actual_model: String,
    pub is_thinking_model: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_image_model: Option<bool>,
}
impl ResolvedModel {
    fn new(model: impl Into<String>, thinking: bool, tier: Option<&str>) -> Self {
        Self {
            actual_model: model.into(),
            is_thinking_model: thinking,
            thinking_budget: None,
            thinking_level: None,
            tier: tier.map(str::to_owned),
            is_image_model: None,
        }
    }
    fn budget(mut self, budget: Option<i64>) -> Self {
        self.thinking_budget = budget;
        self
    }
    fn level(mut self, level: &str) -> Self {
        self.thinking_level = Some(level.into());
        self
    }
}
fn prefix_i<'a>(value: &'a str, prefix: &str) -> &'a str {
    if value
        .get(..prefix.len())
        .is_some_and(|part| part.eq_ignore_ascii_case(prefix))
    {
        &value[prefix.len()..]
    } else {
        value
    }
}
fn suffix_i<'a>(value: &'a str, suffix: &str) -> &'a str {
    let start = value.len().saturating_sub(suffix.len());
    if value
        .get(start..)
        .is_some_and(|part| part.eq_ignore_ascii_case(suffix))
    {
        &value[..start]
    } else {
        value
    }
}
fn tier(value: &str) -> Option<&str> {
    value
        .rsplit_once('-')
        .map(|(_, tier)| tier)
        .filter(|tier| matches!(*tier, "minimal" | "low" | "medium" | "high"))
}
pub(crate) fn argon(lower: &str) -> bool {
    let value = lower.strip_prefix("antigravity-").unwrap_or(lower);
    matches!(
        value,
        "gemini-4-argon" | "gemini-4-argon-low" | "gemini-4-argon-medium" | "gemini-4-argon-high"
    )
}
fn claude55(lower: &str) -> bool {
    lower.starts_with("claude-opus-5-5") || lower.starts_with("claude-sonnet-5-5")
}
fn gemini3_family(lower: &str, family: &str) -> bool {
    let Some(mut suffix) = lower.strip_prefix("gemini-3") else {
        return false;
    };
    if let Some(decimal) = suffix.strip_prefix('.') {
        let count = decimal.bytes().take_while(u8::is_ascii_digit).count();
        if count == 0 {
            return false;
        }
        suffix = &decimal[count..];
    }
    suffix
        .strip_prefix('-')
        .is_some_and(|value| value.starts_with(family))
}

pub fn is_retired(model: &str) -> bool {
    let lower = model.to_lowercase();
    let mut route = lower.strip_prefix("antigravity-").unwrap_or(&lower);
    if route.starts_with("gemini-claude-") {
        route = &route[7..];
    }
    route = route
        .strip_suffix("-preview-customtools")
        .or_else(|| route.strip_suffix("-preview"))
        .unwrap_or(route);
    let stem = tier(route)
        .map(|tier| &route[..route.len() - tier.len() - 1])
        .unwrap_or(route);
    let claude = stem.strip_suffix("-thinking").unwrap_or(stem);
    let retired_claude = matches!(claude, "claude-opus-4-6" | "claude-sonnet-4-6");
    let retired_pro = matches!(
        route,
        "gemini-3.1-pro" | "gemini-3.1-pro-low" | "gemini-3.1-pro-high"
    );
    let flash = route
        .strip_suffix("-low")
        .or_else(|| route.strip_suffix("-medium"))
        .or_else(|| route.strip_suffix("-high"))
        .or_else(|| route.strip_suffix("-tiered"))
        .unwrap_or(route);
    (retired_claude
        || retired_pro
        || matches!(flash, "gemini-3.6-flash" | "gemini-3.7-flash")
        || route == "gemini-pro-agent")
        && !data().native.contains_key(route)
}

/// Pi's reasoning override maps minimal to low and xhigh to high before resolution.
pub fn resolve_pi(model: &str, reasoning: bool, effort: Option<&str>) -> ResolvedModel {
    let lower = model.to_lowercase();
    let supported = reasoning
        && (argon(&lower)
            || lower.contains("gemini-3")
            || (lower.contains("claude")
                && (lower.contains("thinking")
                    || lower.contains("claude-opus-5-5")
                    || lower.contains("claude-sonnet-5-5"))));
    if supported {
        if let Some(effort) = effort.filter(|effort| !effort.is_empty()) {
            let effort = match effort {
                "minimal" => "low",
                "xhigh" => "high",
                other => other,
            };
            let base = ["-minimal", "-low", "-medium", "-high", "-xhigh"]
                .iter()
                .map(|suffix| suffix_i(model, suffix))
                .find(|base| base.len() != model.len())
                .unwrap_or(model);
            return resolve(&format!("{base}-{effort}"));
        }
    }
    resolve(model)
}

pub fn resolve(requested: &str) -> ResolvedModel {
    if !requested.to_lowercase().contains("gemini-3") {
        return resolve_tier(requested);
    }
    let transformed = prefix_i(
        suffix_i(suffix_i(requested, "-preview-customtools"), "-preview"),
        "antigravity-",
    );
    let lower = transformed.to_lowercase();
    let pro = gemini3_family(&lower, "pro");
    let flash = gemini3_family(&lower, "flash");
    let add_tier = (pro || flash)
        && !lower.starts_with("gemini-3.5-flash")
        && tier(&lower).is_none()
        && !lower.contains("image");
    let suffix = if add_tier {
        if pro {
            "-low"
        } else {
            "-medium"
        }
    } else {
        ""
    };
    resolve_tier(&format!("antigravity-{transformed}{suffix}"))
}

fn resolve_tier(requested: &str) -> ResolvedModel {
    let model = prefix_i(requested, "antigravity-");
    let is_antigravity = model.len() != requested.len();
    let lower = model.to_lowercase();
    let supports_tiers = argon(&lower)
        || lower.contains("gemini-3")
        || lower.contains("gemini-2.5")
        || (lower.contains("claude") && (lower.contains("thinking") || claude55(&lower)));
    let tier = if supports_tiers { tier(model) } else { None };
    let base = tier
        .map(|tier| &model[..model.len() - tier.len() - 1])
        .unwrap_or(model);
    let image = lower.contains("image");
    if argon(&lower) {
        let selected = tier
            .filter(|tier| matches!(*tier, "low" | "medium" | "high"))
            .unwrap_or("medium");
        return ResolvedModel::new(
            format!("gemini-4-argon-{selected}"),
            true,
            Some(tier.unwrap_or("medium")),
        )
        .level(tier.unwrap_or("medium"));
    }
    let grouped35 = is_antigravity
        && matches!(
            lower.as_str(),
            "gemini-3.5-flash-low" | "gemini-3.5-flash-medium" | "gemini-3.5-flash-high"
        );
    if let Some(native) = data().native.get(&lower).filter(|_| !grouped35) {
        let mut result = ResolvedModel::new(model, native.supports_thinking, tier).budget(
            if native.supports_adaptive_thinking {
                Some(0)
            } else {
                native.thinking_budget
            },
        );
        result.is_image_model = image.then_some(true);
        return result;
    }
    if matches!(lower.as_str(), "claude-opus-5-5" | "claude-sonnet-5-5") {
        return ResolvedModel::new(format!("{model}-low"), true, Some("low")).budget(Some(0));
    }
    let lower_base = base.to_lowercase();
    if lower_base.starts_with("gemini-3.1-pro") {
        let high = tier == Some("high");
        return ResolvedModel::new(
            if high {
                "gemini-3.1-pro-high"
            } else {
                "gemini-3.1-pro-low"
            },
            true,
            tier,
        )
        .budget(Some(if high { 10001 } else { 1001 }));
    }
    for version in ["3.5", "3.6", "3.7", "3.8"] {
        if lower_base.starts_with(&format!("gemini-{version}-flash")) {
            let selected = tier.unwrap_or("medium");
            let route = if version == "3.5" {
                match selected {
                    "low" => "gemini-3.5-flash-extra-low",
                    "medium" => "gemini-3.5-flash-low",
                    _ => "gemini-3-flash-agent",
                }
                .into()
            } else {
                format!(
                    "gemini-{version}-flash-{}",
                    if matches!(selected, "low" | "medium" | "high") {
                        selected
                    } else {
                        "medium"
                    }
                )
            };
            let budget = match tier {
                Some("low") => 1000,
                Some("high") => -1,
                _ => 4000,
            };
            return ResolvedModel::new(route, true, Some(selected)).budget(Some(budget));
        }
    }
    if matches!(lower_base.as_str(), "gpt-oss-120b" | "gpt-oss-120b-medium") {
        return ResolvedModel::new("gpt-oss-120b-medium", true, None).budget(Some(8192));
    }
    let skip_alias = is_antigravity && lower.starts_with("gemini-3");
    let resolved = if skip_alias {
        if (gemini3_family(&lower, "pro") || gemini3_family(&lower, "flash"))
            && tier.is_none()
            && !image
        {
            format!(
                "{model}-{}",
                if gemini3_family(&lower, "pro") {
                    "low"
                } else {
                    "medium"
                }
            )
        } else {
            model.into()
        }
    } else {
        data()
            .aliases
            .get(model)
            .or_else(|| data().aliases.get(base))
            .cloned()
            .unwrap_or_else(|| base.into())
    };
    let resolved_lower = resolved.to_lowercase();
    if image {
        let mut result = ResolvedModel::new(resolved, false, None);
        result.is_image_model = Some(true);
        return result;
    }
    let gemini3 = resolved_lower.contains("gemini-3");
    let claude_thinking = (resolved_lower.contains("claude")
        && resolved_lower.contains("thinking"))
        || (lower.contains("claude") && lower.contains("thinking"))
        || lower == "gemini-claude-sonnet-4-6";
    if gemini3 {
        return ResolvedModel::new(resolved, true, tier).level(tier.unwrap_or("low"));
    }
    if claude_thinking {
        return ResolvedModel::new(resolved, true, tier).budget(Some(1024));
    }
    let thinking = resolved_lower.contains("thinking")
        || resolved_lower.contains("gemini-3")
        || resolved_lower.contains("gemini-2.5");
    let budget = tier.and_then(|tier| {
        let budgets = if resolved.contains("claude") || resolved.contains("gemini-2.5-pro") {
            [8192, 16384, 32768]
        } else if resolved.contains("gemini-2.5-flash") {
            [6144, 12288, 24576]
        } else {
            [4096, 8192, 16384]
        };
        match tier {
            "low" => Some(budgets[0]),
            "medium" => Some(budgets[1]),
            "high" => Some(budgets[2]),
            _ => None,
        }
    });
    ResolvedModel::new(resolved, thinking, tier).budget(budget)
}
