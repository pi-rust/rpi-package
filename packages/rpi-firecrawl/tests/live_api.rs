//! Ignored live-API integration tests. Run with:
//!   cargo test -p rpi-firecrawl --test live_api -- --ignored --nocapture

use rpi_firecrawl::{scrape, search};
use serde_json::{json, Value};

#[test]
#[ignore = "requires network access to api.firecrawl.dev"]
fn live_search_returns_hits() {
    let out = search(&json!({"query": "firecrawl", "limit": 3})).expect("search should succeed");
    let v: Value = serde_json::from_str(&out).expect("output should be json");
    assert_eq!(v["engine"], "firecrawl");
    assert!(v["count"].as_u64().unwrap() >= 1);
    let first = v["results"][0].clone();
    assert!(!first["url"].as_str().unwrap().is_empty());
    assert!(!first["title"].as_str().unwrap().is_empty());
}

#[test]
#[ignore = "requires network access to api.firecrawl.dev"]
fn live_scrape_returns_markdown() {
    let out = scrape(&json!({"url": "https://example.com"})).expect("scrape should succeed");
    let v: Value = serde_json::from_str(&out).expect("output should be json");
    assert_eq!(v["statusCode"], 200);
    assert!(v["text"].as_str().unwrap().contains("Example Domain"));
}
