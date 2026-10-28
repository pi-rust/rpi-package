//! Redact selected credentials and diagnostic token shapes, never successful model text.
use regex::{Captures, Regex};
use std::sync::OnceLock;

const WHITESPACE: &str =
    r"\x09-\x0d \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

fn mask_identifier(value: &str) -> String {
    let units: Vec<_> = value.encode_utf16().collect();
    if units.is_empty() {
        String::new()
    } else if units.len() <= 8 {
        "****".into()
    } else {
        format!(
            "{}****{}",
            String::from_utf16_lossy(&units[..4]),
            String::from_utf16_lossy(&units[units.len() - 4..])
        )
    }
}
fn expression<'a>(storage: &'a OnceLock<Regex>, pattern: &str) -> &'a Regex {
    storage.get_or_init(|| Regex::new(pattern).expect("pinned redaction expression"))
}
pub fn text(input: &str, credentials: &[String], projects: &[String]) -> String {
    let mut projects: Vec<_> = projects
        .iter()
        .filter(|project| !project.is_empty())
        .collect();
    projects.sort_by_key(|project| std::cmp::Reverse(project.encode_utf16().count()));
    projects.dedup();
    let mut output = if projects.is_empty() {
        input.into()
    } else {
        let pattern = projects
            .iter()
            .map(|project| regex::escape(project))
            .collect::<Vec<_>>()
            .join("|");
        Regex::new(&pattern)
            .expect("escaped project identifiers")
            .replace_all(input, |capture: &Captures<'_>| mask_identifier(&capture[0]))
            .into_owned()
    };
    static PROJECT: OnceLock<Regex> = OnceLock::new();
    output = expression(
        &PROJECT,
        &format!(r"((?i-u:\bproject)(?:[{WHITESPACE}_-]*(?i-u:id))?[{WHITESPACE}]*[:=][{WHITESPACE}]*)(?i-u:([a-z0-9_*][a-z0-9_*-]*))"),
    )
    .replace_all(&output, |capture: &Captures<'_>| {
        let value = &capture[2];
        if value.eq_ignore_ascii_case("unknown")
            || value.eq_ignore_ascii_case("na")
            || value.contains("****")
        {
            capture[0].to_owned()
        } else {
            format!("{}{}", &capture[1], mask_identifier(value))
        }
    })
    .into_owned();
    for credential in credentials
        .iter()
        .filter(|credential| !credential.is_empty())
    {
        output = output.replace(credential, "[credential]");
    }
    static JWT: OnceLock<Regex> = OnceLock::new();
    output = expression(
        &JWT,
        r"(?-u)\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b",
    )
    .replace_all(&output, "[credential]")
    .into_owned();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    // ECMAScript whitespace includes BOM; regex's ASCII \s does not.
    output = expression(
        &BEARER,
        &format!(r"(?i-u:\bBearer)[{WHITESPACE}]+[A-Za-z0-9._~+/-]+=*"),
    )
    .replace_all(&output, "[credential]")
    .into_owned();
    output
}
