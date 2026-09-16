use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::handler::BoxedEventHandler;
use super::models::{Event, EventReq, EventResp};
use crate::core::{Error, LoggerRef};

pub const HEADER_REQUEST_TIMESTAMP: &str = "x-lark-request-timestamp";
pub const HEADER_REQUEST_NONCE: &str = "x-lark-request-nonce";
pub const HEADER_SIGNATURE: &str = "x-lark-signature";

pub const CONTENT_TYPE_JSON: &str = "application/json";

#[derive(Debug, Clone, Default)]
pub struct EventDispatcherConfig {
    pub verification_token: Option<String>,
    pub encrypt_key: Option<String>,
    pub skip_signature_verification: bool,
}

impl EventDispatcherConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_sdk_config(config: &crate::core::Config) -> Self {
        Self {
            verification_token: None,
            encrypt_key: None,
            skip_signature_verification: config.skip_sign_verify,
        }
    }

    pub fn verification_token(mut self, token: impl Into<String>) -> Self {
        self.verification_token = Some(token.into());
        self
    }

    pub fn encrypt_key(mut self, key: impl Into<String>) -> Self {
        self.encrypt_key = Some(key.into());
        self
    }

    pub fn skip_signature_verification(mut self, skip: bool) -> Self {
        self.skip_signature_verification = skip;
        self
    }
}

pub struct EventDispatcher {
    config: EventDispatcherConfig,
    handlers: Arc<RwLock<HashMap<String, BoxedEventHandler>>>,
    logger: LoggerRef,
}

impl EventDispatcher {
    pub fn new(config: EventDispatcherConfig, logger: LoggerRef) -> Self {
        Self {
            config,
            handlers: Arc::new(RwLock::new(HashMap::new())),
            logger,
        }
    }

    pub async fn register_handler(&self, handler: BoxedEventHandler) {
        let event_type = handler.event_type().to_string();
        let mut handlers = self.handlers.write().await;
        self.logger.info(&format!(
            "Registering handler for event type: {}",
            event_type
        ));
        handlers.insert(event_type, handler);
    }

    pub async fn dispatch(&self, req: EventReq) -> Result<EventResp, Error> {
        self.logger.debug(&format!(
            "Dispatching event request: uri={}, body_len={}",
            req.request_uri,
            req.body.len()
        ));

        let decrypted_body = self.decrypt_body_if_needed(&req).await?;

        let event: Event = serde_json::from_slice(&decrypted_body)
            .map_err(|e| Error::InvalidEventFormat(e.to_string()))?;

        if !self.config.skip_signature_verification {
            self.verify_signature(&req, &decrypted_body)?;
        }

        let routed = self.route_event(event).await?;
        Ok(routed.unwrap_or_else(|| EventResp::ok(b"{\"success\":true}".to_vec())))
    }

    pub async fn dispatch_payload(&self, payload: &[u8]) -> Result<Option<EventResp>, Error> {
        self.logger.debug(&format!(
            "Dispatching stream event payload: body_len={}",
            payload.len()
        ));

        let decrypted_body =
            crate::event::maybe_decrypt_event_body(payload, self.config.encrypt_key.as_deref())
                .map_err(|e| Error::EventDecryption(e.to_string()))?;
        let event: Event = serde_json::from_slice(&decrypted_body)
            .map_err(|e| Error::InvalidEventFormat(e.to_string()))?;

        self.route_event(event).await
    }

    async fn route_event(&self, event: Event) -> Result<Option<EventResp>, Error> {
        if event.is_challenge() {
            self.logger.info("Received challenge request");
            return self.handle_challenge(&event).map(Some);
        }

        let event_type = event
            .event_type()
            .ok_or_else(|| Error::InvalidEventFormat("missing event type".to_string()))?;

        self.logger
            .info(&format!("Dispatching event: {}", event_type));

        let handlers = self.handlers.read().await;
        if let Some(handler) = handlers.get(event_type) {
            let result: Option<EventResp> = handler.handle(event).await?;
            Ok(result)
        } else {
            self.logger.warn(&format!(
                "No handler registered for event type: {}",
                event_type
            ));
            Ok(None)
        }
    }

    async fn decrypt_body_if_needed(&self, req: &EventReq) -> Result<Vec<u8>, Error> {
        let body_str = String::from_utf8_lossy(&req.body);

        if body_str.contains("\"encrypt\"") {
            if let Some(ref key) = self.config.encrypt_key {
                self.logger.debug("Decrypting event body");
                crate::event::maybe_decrypt_event_body(&req.body, Some(key))
                    .map_err(|e| Error::EventDecryption(e.to_string()))
            } else {
                Err(Error::EventDecryption(
                    "encrypt_key not configured".to_string(),
                ))
            }
        } else {
            Ok(req.body.clone())
        }
    }

    fn handle_challenge(&self, event: &Event) -> Result<EventResp, Error> {
        if let Some(ref challenge) = event.challenge {
            if let Some(ref token) = self.config.verification_token
                && event.token.as_ref() != Some(token)
            {
                return Err(Error::EventSignatureVerification);
            }
            Ok(EventResp::challenge(challenge))
        } else {
            Err(Error::InvalidEventFormat(
                "challenge field missing".to_string(),
            ))
        }
    }

    fn verify_signature(&self, req: &EventReq, body: &[u8]) -> Result<(), Error> {
        let encrypt_key = match self.config.encrypt_key.as_ref() {
            Some(key) => key,
            None => return Ok(()),
        };

        let timestamp = req
            .get_header(HEADER_REQUEST_TIMESTAMP)
            .ok_or_else(|| Error::EventSignatureVerification)?;

        let nonce = req
            .get_header(HEADER_REQUEST_NONCE)
            .ok_or_else(|| Error::EventSignatureVerification)?;

        let signature = req
            .get_header(HEADER_SIGNATURE)
            .ok_or_else(|| Error::EventSignatureVerification)?;

        let body_str = String::from_utf8_lossy(body);
        let expected_signature =
            crate::event::event_signature_sha256(timestamp, nonce, encrypt_key, &body_str);

        if signature == expected_signature {
            self.logger.debug("Event signature verification passed");
            Ok(())
        } else {
            self.logger.error("Event signature verification failed");
            Err(Error::EventSignatureVerification)
        }
    }

    pub fn config(&self) -> &EventDispatcherConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventHandler, EventHandlerResult};

    struct PayloadHandler;

    impl EventHandler for PayloadHandler {
        fn event_type(&self) -> &str {
            "im.message.receive_v1"
        }

        fn handle(
            &self,
            _event: Event,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EventHandlerResult> + Send + '_>>
        {
            Box::pin(async { Ok(None) })
        }
    }

    #[tokio::test]
    async fn test_dispatcher_challenge() {
        let config = EventDispatcherConfig::new().verification_token("test_token");
        let logger = crate::core::noop_logger();
        let dispatcher = EventDispatcher::new(config, logger);

        let event = Event {
            type_: Some("url_verification".to_string()),
            challenge: Some("test_challenge".to_string()),
            token: Some("test_token".to_string()),
            ..Default::default()
        };

        let resp = dispatcher.handle_challenge(&event).unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn test_dispatch_payload_without_http_headers() {
        let logger = crate::core::noop_logger();
        let dispatcher = EventDispatcher::new(EventDispatcherConfig::new(), logger);
        dispatcher.register_handler(Box::new(PayloadHandler)).await;

        let result = dispatcher
            .dispatch_payload(
                br#"{"schema":"2.0","header":{"event_type":"im.message.receive_v1"}}"#,
            )
            .await
            .expect("dispatch payload");

        assert!(result.is_none());
    }
}
