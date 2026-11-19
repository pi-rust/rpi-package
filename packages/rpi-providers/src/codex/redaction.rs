//! Redact credentials and account identifiers from error text, never successful model text.
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
/// `credentials` are replaced entirely; account identifiers are masked so diagnosis stays
/// possible without exposing the account.
pub fn text(input: &str, credentials: &[String], accounts: &[String]) -> String {
    let mut output = input.to_owned();
    for account in accounts.iter().filter(|account| !account.is_empty()) {
        output = output.replace(account.as_str(), &mask_identifier(account));
    }
    for credential in credentials
        .iter()
        .filter(|credential| !credential.is_empty())
    {
        output = output.replace(credential.as_str(), "[credential]");
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
    let _ = |capture: &Captures<'_>| capture[0].to_owned();
    output
}
