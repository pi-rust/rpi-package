//! Unit tests: OAuth URL and token wire shapes against a loopback port, JWT account
//! extraction, request conversion and the SSE state machine. Nothing here reaches OpenAI.
use super::auth::{self, Endpoints, Pasted, PendingLogins, Port, Reply, Request};
use super::convert::build_request_body;
use super::sse::DecoderState;
use super::stream::{EventSink, Flow, State};
use super::{catalog, models};
use rpi_ai::types::ThinkingLevel;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Instant;

fn access_jwt(account: &str) -> String {
    auth::test_jwt(account)
}

// ---------------------------------------------------------------------------
// Loopback port
// ---------------------------------------------------------------------------

type Handler = Box<dyn Fn(&Request) -> (u16, &'static str, String) + Send + Sync>;

struct FakeNet {
    handler: Handler,
    seen: Mutex<Vec<Request>>,
}
impl FakeNet {
    fn new(
        handler: impl Fn(&Request) -> (u16, &'static str, String) + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}
struct FakeBody(String);
impl auth::Body for FakeBody {
    async fn text(self) -> Result<String, String> {
        Ok(self.0)
    }
}
impl Port for FakeNet {
    type Body = FakeBody;
    async fn send(&self, request: Request) -> Result<Reply<FakeBody>, String> {
        let (status, status_text, body) = (self.handler)(&request);
        self.seen.lock().unwrap().push(request);
        Ok(Reply {
            status,
            status_text: status_text.into(),
            body: FakeBody(body),
        })
    }
    fn now_ms(&self) -> f64 {
        1_700_000_000_000.0
    }
    fn user_agent(&self) -> String {
        "codex-test".into()
    }
}

fn token_json(access: &str, refresh: &str, expires_in: u64) -> String {
    json!({
        "access_token": access,
        "refresh_token": refresh,
        "expires_in": expires_in,
        "token_type": "Bearer",
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// OAuth: authorize, paste parsing, pending logins
// ---------------------------------------------------------------------------

#[test]
fn authorize_url_carries_public_client_pkce_and_pi_originator() {
    let verifier = [7u8; 32];
    let state = [9u8; 32];
    let authorization = auth::authorize_with_entropy(&verifier, &state);
    let url = url::Url::parse(&authorization.url).unwrap();
    assert_eq!(url.host_str(), Some("auth.openai.com"));
    assert_eq!(url.path(), "/oauth/authorize");
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], auth::CLIENT_ID);
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["redirect_uri"], auth::REDIRECT_URI);
    assert_eq!(query["scope"], "openid profile email offline_access");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["originator"], "pi");
    assert_eq!(query["codex_cli_simplified_flow"], "true");
    // The verifier never travels in the URL; its S256 digest does.
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use sha2::{Digest, Sha256};
    let verifier = URL_SAFE_NO_PAD.encode(verifier);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(query["code_challenge"], challenge);
    assert_eq!(authorization.login.verifier, verifier);
    assert_eq!(authorization.state, query["state"]);
}

#[test]
fn pasted_input_parses_callback_hash_params_and_bare_codes() {
    let callback = Pasted::parse("http://localhost:1455/auth/callback?code=abc&state=xyz");
    assert_eq!(callback.code, "abc");
    assert_eq!(callback.state.as_deref(), Some("xyz"));
    let hash = Pasted::parse("abc#xyz");
    assert_eq!(hash.code, "abc");
    assert_eq!(hash.state.as_deref(), Some("xyz"));
    let params = Pasted::parse("code=abc&state=xyz");
    assert_eq!(params.code, "abc");
    assert_eq!(params.state.as_deref(), Some("xyz"));
    let bare = Pasted::parse("  abc  ");
    assert_eq!(bare.code, "abc");
    assert_eq!(bare.state, None);
    assert!(Pasted::parse("   ").code.is_empty());
}

#[test]
fn pending_logins_redeem_once_and_check_state() {
    let now = Instant::now();
    let mut logins = PendingLogins::default();
    let login = || auth::PendingLogin {
        verifier: "verifier".into(),
    };
    logins.insert("state-1".into(), login(), now);
    // A bare code takes the most recent login.
    let redeemed = logins
        .redeem(&Pasted::parse("code"), now)
        .expect("bare code redeems the latest login");
    assert_eq!(redeemed.verifier, "verifier");
    // The entry is gone: a second attempt fails.
    logins.insert("state-2".into(), login(), now);
    let mismatch = logins
        .redeem(&Pasted::parse("http://x/?code=c&state=other"), now)
        .unwrap_err();
    assert_eq!(mismatch, auth::MISMATCH);
    let ok = logins
        .redeem(&Pasted::parse("http://x/?code=c&state=state-2"), now)
        .expect("matching state redeems");
    assert_eq!(ok.verifier, "verifier");
    // Nothing left.
    assert_eq!(
        logins.redeem(&Pasted::parse("c"), now).unwrap_err(),
        auth::NO_LOGIN
    );
}

// ---------------------------------------------------------------------------
// OAuth: exchange and refresh against a loopback token endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn account_id_comes_out_of_the_access_jwt() {
    assert_eq!(
        auth::account_id(&access_jwt("acct-1")).as_deref(),
        Some("acct-1")
    );
    assert_eq!(auth::account_id("not-a-jwt"), None);
    // A JWT without the claim path is not a Codex credential.
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let part = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
    let bare = format!("{}.{}.x", part(json!({})), part(json!({"sub": "user"})));
    assert_eq!(auth::account_id(&bare), None);
}

#[tokio::test]
async fn exchange_posts_pkce_form_and_requires_a_usable_token_set() {
    let net = FakeNet::new(|request| {
        assert!(request.url.starts_with("http://127.0.0.1:"));
        assert_eq!(request.method, "POST");
        assert!(request
            .body
            .as_ref()
            .unwrap()
            .contains("grant_type=authorization_code"));
        assert!(request
            .body
            .as_ref()
            .unwrap()
            .contains(&format!("client_id={}", auth::CLIENT_ID)));
        assert!(request
            .body
            .as_ref()
            .unwrap()
            .contains("code_verifier=verifier-1"));
        assert!(request
            .body
            .as_ref()
            .unwrap()
            .contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        (
            200,
            "OK",
            token_json(&access_jwt("acct-ex"), "refresh-1", 3600),
        )
    });
    let endpoints = Endpoints {
        token: "http://127.0.0.1:9/token".into(),
    };
    let login = auth::PendingLogin {
        verifier: "verifier-1".into(),
    };
    let result = auth::exchange(&net, &endpoints, "code-1", &login)
        .await
        .expect("exchange succeeds");
    assert_eq!(result["access"], access_jwt("acct-ex"));
    assert_eq!(result["refresh"], "refresh-1");
    // Expiry is absolute: the loopback clock plus expires_in.
    assert_eq!(result["expires"], 1_700_000_000_000.0 + 3_600_000.0);
}

#[tokio::test]
async fn exchange_and_refresh_reject_missing_fields_and_missing_account() {
    let net = FakeNet::new(|request| {
        if request
            .body
            .as_ref()
            .unwrap()
            .contains("grant_type=refresh_token")
        {
            // A refresh without a refresh_token rotation is still valid pi-ai shape.
            (
                200,
                "OK",
                token_json(&access_jwt("acct-r"), "refresh-2", 60),
            )
        } else {
            // No account id in the JWT: unusable for Codex.
            (200, "OK", token_json("plain-token", "refresh-x", 60))
        }
    });
    let endpoints = Endpoints {
        token: "http://127.0.0.1:9/token".into(),
    };
    let login = auth::PendingLogin {
        verifier: "v".into(),
    };
    let error = auth::exchange(&net, &endpoints, "c", &login)
        .await
        .unwrap_err();
    assert!(error.contains("account id"), "{error}");
    let refreshed = auth::refresh(&net, &endpoints, "refresh-1")
        .await
        .expect("refresh succeeds");
    assert_eq!(refreshed["refresh"], "refresh-2");
    assert_eq!(refreshed["expires"], 1_700_000_000_000.0 + 60_000.0);
}

#[tokio::test]
async fn token_endpoint_errors_report_status_only() {
    let net = FakeNet::new(|_| {
        (
            400,
            "Bad Request",
            r#"{"error":"invalid_grant","error_description":"the code echoes secrets"}"#.into(),
        )
    });
    let endpoints = Endpoints {
        token: "http://127.0.0.1:9/token".into(),
    };
    let login = auth::PendingLogin {
        verifier: "v".into(),
    };
    let error = auth::exchange(&net, &endpoints, "c", &login)
        .await
        .unwrap_err();
    assert!(error.contains("400 Bad Request"), "{error}");
    assert!(!error.contains("invalid_grant"), "body leaked: {error}");
}

#[tokio::test]
async fn complete_never_sends_a_rejected_input() {
    let net = FakeNet::new(|_| (200, "OK", token_json(&access_jwt("a"), "r", 60)));
    let endpoints = Endpoints {
        token: "http://127.0.0.1:9/token".into(),
    };
    let logins = Mutex::new(PendingLogins::default());
    let error = auth::complete(&net, &endpoints, &logins, "   ", Instant::now())
        .await
        .unwrap_err();
    assert_eq!(error, "OpenAI Codex OAuth input is empty");
    assert!(net.seen().is_empty(), "nothing reached the port");
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

#[test]
fn catalog_declares_the_default_models_in_pi_order() {
    let models = models();
    let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["gpt-6.1-sol", "gpt-6.1-terra", "gpt-5.6-luna"]);
    for model in &models {
        assert_eq!(model.provider, "openai-codex");
        assert_eq!(model.base_url, catalog::BASE_URL);
        assert!(model.reasoning);
        assert!(model.has_pricing());
        assert!(model.context_window >= 272_000);
    }
    // gpt-6.1-sol disables thinking at `off` (explicit null); terra maps it to "none".
    let sol = models
        .iter()
        .find(|model| model.id == "gpt-6.1-sol")
        .unwrap();
    assert!(sol
        .thinking_level_map
        .as_ref()
        .is_some_and(|map| map.get(&ThinkingLevel::Off) == Some(&None)));
    let terra = models
        .iter()
        .find(|model| model.id == "gpt-6.1-terra")
        .unwrap();
    assert!(terra
        .thinking_level_map
        .as_ref()
        .is_some_and(|map| map.get(&ThinkingLevel::Off) == Some(&Some("none".into()))));
}

#[test]
fn effort_mapping_follows_the_model_map() {
    let model = serde_json::to_value(models().remove(0)).unwrap();
    assert_eq!(
        catalog::mapped_effort(&model, ThinkingLevel::Off),
        Some(None)
    );
    assert_eq!(
        catalog::mapped_effort(&model, ThinkingLevel::Medium),
        Some(Some("medium".into()))
    );
    assert_eq!(
        catalog::mapped_effort(&model, ThinkingLevel::High),
        Some(Some("high".into()))
    );
    // Every level of sol's map resolves; `off` is the explicit-null entry.
    assert_eq!(
        catalog::mapped_effort(&model, ThinkingLevel::Minimal),
        Some(Some("low".into()))
    );
    assert_eq!(
        catalog::mapped_effort(&model, ThinkingLevel::Xhigh),
        Some(Some("xhigh".into()))
    );
    assert!(!catalog::off_maps_to_effort(&model));
}

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

fn context_value() -> Value {
    json!({
        "systemPrompt": null,
        "tools": [{
            "name": "lookup",
            "description": "Look things up",
            "parameters": {"type": "object", "properties": {"q": {"type": "string"}}},
        }],
        "messages": [
            {"role": "system", "content": "You are helpful.", "sections": {}, "timestamp": 1},
            {"role": "user", "content": "hello", "timestamp": 2},
            {"role": "assistant", "content": [
                {"type": "text", "text": "Hi!", "textSignature": "{\"v\":1,\"id\":\"msg_1\"}"},
                {"type": "toolCall", "id": "call_1|fc_item1", "name": "lookup", "arguments": {"q": "x"}},
            ], "provider": "openai-codex", "api": "openai-codex-responses", "model": "gpt-6.1-sol", "timestamp": 3},
            {"role": "toolResult", "toolCallId": "call_1|fc_item1", "toolName": "lookup", "content": "result", "isError": false, "timestamp": 4},
        ],
    })
}

#[test]
fn request_body_matches_pi_codex_shape() {
    let model = serde_json::to_value(models().remove(0)).unwrap();
    let body = build_request_body(
        &model,
        &context_value(),
        Some(ThinkingLevel::Medium),
        Some("session-1"),
        None,
    )
    .unwrap();
    assert_eq!(body["model"], "gpt-6.1-sol");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["instructions"], "You are helpful.");
    assert_eq!(body["text"], json!({"verbosity": "low"}));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(body["prompt_cache_key"], "session-1");
    assert_eq!(
        body["reasoning"],
        json!({"effort": "medium", "summary": "auto"})
    );
    // Tools arrive as function tools with strict null, parameters untouched.
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "lookup");
    assert_eq!(body["tools"][0]["strict"], Value::Null);
    assert_eq!(
        body["tools"][0]["parameters"],
        json!({"type": "object", "properties": {"q": {"type": "string"}}})
    );
    // The transcript converts to Responses input items.
    let input = body["input"].as_array().unwrap();
    assert_eq!(
        input[0],
        json!({"role": "user", "content": [{"type": "input_text", "text": "hello"}]})
    );
    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[1]["id"], "msg_1");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[2]["id"], "fc_item1");
    assert_eq!(input[2]["arguments"], "{\"q\":\"x\"}");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_1");
    assert_eq!(input[3]["output"], "result");
}

#[test]
fn history_repairs_foreign_and_interrupted_tool_turns() {
    let model = serde_json::to_value(models().remove(0)).unwrap();
    let mut context = context_value();
    context["messages"] = json!([
        {"role":"assistant", "provider":"other", "api":"other", "model":"other",
         "content":[{"type":"thinking", "thinking":"secret", "thinkingSignature":"{\"type\":\"reasoning\",\"id\":\"foreign\"}"},
                    {"type":"toolCall", "id":"call.foo|item", "name":"lookup", "arguments":{}}]},
        {"role":"toolResult", "toolCallId":"call.foo|item", "content":"ok"},
        {"role":"assistant", "content":[{"type":"toolCall", "id":"missing", "name":"lookup", "arguments":{}}]},
        {"role":"user", "content":"continue"},
        {"role":"assistant", "stopReason":"aborted", "content":[{"type":"toolCall", "id":"aborted", "name":"lookup", "arguments":{}}]},
        {"role":"toolResult", "toolCallId":"aborted", "content":"orphan"}
    ]);
    let body = build_request_body(&model, &context, None, None, None).unwrap();
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 5);
    assert_eq!(input[0]["id"], "fc_1c0dv6z7f3wp3");
    assert_eq!(input[0]["call_id"], "call_foo");
    assert_eq!(input[1]["call_id"], "call_foo");
    assert_eq!(input[1]["output"], "ok");
    assert_eq!(input[3]["call_id"], "missing");
    assert_eq!(input[3]["output"], "No result provided");
    assert_eq!(input[4]["role"], "user");
}

#[test]
fn off_reasoning_uses_the_map_and_explicit_null_omits_reasoning() {
    let models = models();
    let sol = serde_json::to_value(models.iter().find(|m| m.id == "gpt-6.1-sol").unwrap()).unwrap();
    let terra =
        serde_json::to_value(models.iter().find(|m| m.id == "gpt-6.1-terra").unwrap()).unwrap();
    // sol: off is an explicit null -> reasoning omitted entirely.
    let body = build_request_body(&sol, &context_value(), None, None, None).unwrap();
    assert!(body.get("reasoning").is_none(), "{body}");
    // terra: off maps to "none".
    let body = build_request_body(&terra, &context_value(), None, None, None).unwrap();
    assert_eq!(
        body["reasoning"],
        json!({"effort": "none", "summary": "auto"})
    );
    // luna: no off entry, reasoning on -> "none" fallback for Off.
    let luna =
        serde_json::to_value(models.iter().find(|m| m.id == "gpt-5.6-luna").unwrap()).unwrap();
    let body = build_request_body(&luna, &context_value(), None, None, None).unwrap();
    assert_eq!(
        body["reasoning"],
        json!({"effort": "none", "summary": "auto"})
    );
    let body = build_request_body(
        &luna,
        &context_value(),
        Some(ThinkingLevel::Xhigh),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        body["reasoning"],
        json!({"effort": "xhigh", "summary": "auto"})
    );
}

#[test]
fn responses_endpoint_accepts_all_supported_base_shapes() {
    for suffix in [
        "",
        "/",
        "/codex",
        "/codex/",
        "/codex/responses",
        "/codex/responses/",
    ] {
        let base = format!("https://chatgpt.com/backend-api{suffix}");
        assert_eq!(
            super::transport::responses_url(&base),
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }
    assert_eq!(
        super::transport::responses_url("http://127.0.0.1:1234/custom/codex/"),
        "http://127.0.0.1:1234/custom/codex/responses"
    );
}

#[test]
fn foreign_item_hash_matches_pi_utf16_hash() {
    let model = serde_json::to_value(models().remove(0)).unwrap();
    let context = json!({"messages":[{"role":"assistant","provider":"foreign","api":"other",
        "content":[{"type":"toolCall","id":"call.foo|foreign/item:😀","name":"lookup","arguments":{}}]},
        {"role":"toolResult","toolCallId":"call.foo|foreign/item:😀","content":"ok"}]});
    let body = build_request_body(&model, &context, None, None, None).unwrap();
    assert_eq!(body["input"][0]["id"], "fc_ojm3ab1nz04rk");
    assert_eq!(body["input"][0]["call_id"], body["input"][1]["call_id"]);
}

#[test]
fn no_system_message_uses_default_instructions() {
    let model = serde_json::to_value(models().remove(0)).unwrap();
    let mut context = context_value();
    context["messages"] = json!([{"role": "user", "content": "hi", "timestamp": 0}]);
    context["tools"] = Value::Null;
    let body = build_request_body(&model, &context, None, None, None).unwrap();
    assert_eq!(body["instructions"], super::convert::DEFAULT_INSTRUCTIONS);
    assert!(body.get("tools").is_none());
}

// ---------------------------------------------------------------------------
// SSE decoding and the stream state machine
// ---------------------------------------------------------------------------

#[test]
fn sse_decoder_yields_json_events_and_skips_done() {
    let mut decoder = DecoderState::default();
    decoder.push(b"data: {\"type\":\"response.created\",\"res");
    decoder.push(b"ponse\":{}}\r\n\r\ndata: [DONE]\r\n\r\n");
    let first = decoder.next_event(false).expect("first event");
    assert_eq!(first["type"], "response.created");
    assert!(decoder.next_event(false).is_none());
    decoder.push(b"data: {\"type\":\"response.completed\"}");
    assert!(decoder.next_event(false).is_none(), "no frame boundary yet");
    let last = decoder.next_event(true).expect("trailing frame at EOF");
    assert_eq!(last["type"], "response.completed");
}

#[derive(Default)]
struct VecSink(Vec<Value>);
impl EventSink for VecSink {
    async fn emit(&mut self, event: Value) -> Result<(), String> {
        self.0.push(event);
        Ok(())
    }
}

fn stream_model() -> Value {
    serde_json::to_value(models().remove(0)).unwrap()
}

#[tokio::test]
async fn stream_state_accumulates_text_thinking_and_tool_calls() {
    let mut state = State::new(&stream_model(), 42);
    let mut sink = VecSink::default();
    state.start(&mut sink).await.unwrap();

    let events = [
        json!({"type": "response.created", "response": {"id": "resp_1"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1"}}),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "ponder"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": [{"text": "ponder"}], "encrypted_content": "enc"}}),
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "id": "msg_1"}}),
        json!({"type": "response.output_text.delta", "output_index": 1, "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "output_index": 1, "delta": "lo"}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "message", "id": "msg_1", "content": [{"type": "output_text", "text": "Hello"}]}}),
        json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "lookup", "arguments": "{\"q\":"}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "\"x\"}"}),
        json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "lookup", "arguments": "{\"q\":\"x\"}"}}),
        json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": {"input_tokens": 100, "output_tokens": 10, "total_tokens": 110, "input_tokens_details": {"cached_tokens": 40, "cache_write_tokens": 10}, "output_tokens_details": {"reasoning_tokens": 3}}}}),
    ];
    let mut flow = Flow::Continue;
    for event in &events {
        flow = state.event(event, &mut sink).await.unwrap();
    }
    assert_eq!(flow, Flow::Complete);
    state.finish(false, &mut sink).await.unwrap();

    let kinds: Vec<_> = sink
        .0
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
    let output = &state.output;
    assert_eq!(output["responseId"], "resp_1");
    assert_eq!(output["stopReason"], "toolUse");
    let content = output["content"].as_array().unwrap();
    assert_eq!(content[0]["thinking"], "ponder");
    assert_eq!(
        content[0]["thinkingSignature"],
        serde_json::to_string(&json!({"type": "reasoning", "id": "rs_1", "summary": [{"text": "ponder"}], "encrypted_content": "enc"})).unwrap()
    );
    assert_eq!(content[1]["text"], "Hello");
    assert_eq!(content[1]["textSignature"], "{\"v\":1,\"id\":\"msg_1\"}");
    assert_eq!(content[2]["id"], "call_9|fc_1");
    assert_eq!(content[2]["name"], "lookup");
    assert_eq!(content[2]["arguments"], json!({"q": "x"}));
    // Usage: cached and cache-write tokens come out of input; cost uses tier rates.
    assert_eq!(output["usage"]["input"], 50);
    assert_eq!(output["usage"]["cacheRead"], 40);
    assert_eq!(output["usage"]["cacheWrite"], 10);
    assert_eq!(output["usage"]["reasoning"], 3);
    assert_eq!(output["usage"]["totalTokens"], 110);
    assert!((output["usage"]["cost"]["input"].as_f64().unwrap() - 0.0001).abs() < 1e-12);
    assert!((output["usage"]["cost"]["cacheRead"].as_f64().unwrap() - 0.000004).abs() < 1e-15);
}

#[tokio::test]
async fn stream_state_maps_incomplete_and_failed_responses() {
    let mut state = State::new(&stream_model(), 0);
    let mut sink = VecSink::default();
    state.start(&mut sink).await.unwrap();
    state
        .event(
            &json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "m"}}),
            &mut sink,
        )
        .await
        .unwrap();
    state
        .event(
            &json!({"type": "response.output_text.delta", "output_index": 0, "delta": "x"}),
            &mut sink,
        )
        .await
        .unwrap();
    state
        .event(
            &json!({"type": "response.completed", "response": {"id": "r", "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}}}),
            &mut sink,
        )
        .await
        .unwrap();
    state.finish(false, &mut sink).await.unwrap();
    assert_eq!(state.output["stopReason"], "length");
    assert_eq!(
        state.output["rawStopReason"],
        "incomplete.max_output_tokens"
    );

    let mut state = State::new(&stream_model(), 0);
    let mut sink = VecSink::default();
    state.start(&mut sink).await.unwrap();
    let error = state
        .event(
            &json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "server_error", "message": "boom"}}}),
            &mut sink,
        )
        .await
        .unwrap_err();
    assert!(error.contains("server_error: boom"), "{error}");
}

#[tokio::test]
async fn stream_state_requires_a_terminal_event_and_finished_tool_calls() {
    let mut state = State::new(&stream_model(), 0);
    let mut sink = VecSink::default();
    state.start(&mut sink).await.unwrap();
    // EOF before any terminal event: one error, no done.
    let error = state.finish(false, &mut sink).await.unwrap_err();
    assert!(error.contains("terminal response event"), "{error}");

    // An unfinished tool call at a toolUse stop is refused.
    let mut state = State::new(&stream_model(), 0);
    let mut sink = VecSink::default();
    state.start(&mut sink).await.unwrap();
    state
        .event(
            &json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "lookup", "arguments": ""}}),
            &mut sink,
        )
        .await
        .unwrap();
    state
        .event(
            &json!({"type": "response.completed", "response": {"id": "r", "status": "completed", "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}}),
            &mut sink,
        )
        .await
        .unwrap();
    let error = state.finish(false, &mut sink).await.unwrap_err();
    assert!(error.contains("unfinished tool call"), "{error}");
}

#[tokio::test]
async fn redaction_masks_accounts_and_tokens_never_model_text() {
    let redacted = super::redaction::text(
        "Bearer the-access-token failed for account account-1234567890abcdef",
        &["the-access-token".into()],
        &["account-1234567890abcdef".into()],
    );
    assert!(!redacted.contains("the-access-token"), "{redacted}");
    assert!(!redacted.contains("account-1234567890abcdef"), "{redacted}");
    assert!(redacted.contains("****"), "{redacted}");
    // Successful model text passes through untouched.
    let model_text = super::redaction::text("the-access-token is a great word", &[], &[]);
    assert_eq!(model_text, "the-access-token is a great word");
}
