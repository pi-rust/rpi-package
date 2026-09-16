use std::future::Future;
use std::pin::Pin;

use crate::core::{Error, LoggerRef};
use crate::event::{EventReq, EventResp};

pub type CardActionHandlerFn = Box<
    dyn Fn(
            super::models::CardAction,
        ) -> Pin<
            Box<dyn Future<Output = Result<Option<super::models::CardResponse>, Error>> + Send>,
        > + Send
        + Sync,
>;

pub struct CardResponse;

impl CardResponse {
    pub fn success() -> serde_json::Value {
        serde_json::json!({"success": true})
    }

    pub fn with_toast(toast: super::models::CardToast) -> serde_json::Value {
        serde_json::to_value(super::models::CardResponse::new().toast(toast))
            .unwrap_or_else(|_| Self::success())
    }
}

pub struct CardActionHandler {
    verification_token: Option<String>,
    event_encrypt_key: Option<String>,
    skip_signature_verification: bool,
    handler: Option<CardActionHandlerFn>,
    logger: LoggerRef,
}

impl CardActionHandler {
    pub fn new(logger: LoggerRef) -> Self {
        Self {
            verification_token: None,
            event_encrypt_key: None,
            skip_signature_verification: false,
            handler: None,
            logger,
        }
    }

    pub fn verification_token(mut self, token: impl Into<String>) -> Self {
        self.verification_token = Some(token.into());
        self
    }

    pub fn event_encrypt_key(mut self, key: impl Into<String>) -> Self {
        self.event_encrypt_key = Some(key.into());
        self
    }

    pub fn skip_signature_verification(mut self, skip: bool) -> Self {
        self.skip_signature_verification = skip;
        self
    }

    pub fn with_sdk_config(mut self, config: &crate::core::Config) -> Self {
        self.skip_signature_verification = config.skip_sign_verify;
        self
    }

    pub fn handler<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(super::models::CardAction) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<super::models::CardResponse>, Error>> + Send + 'static,
    {
        self.handler = Some(Box::new(move |action| Box::pin(handler(action))));
        self
    }

    pub async fn handle(&self, req: EventReq) -> Result<EventResp, Error> {
        self.logger.debug(&format!(
            "Card action request: uri={}, body_len={}",
            req.request_uri,
            req.body.len()
        ));

        let decrypted_body = self.decrypt_body_if_needed(&req).await?;

        let mut card_action: super::models::CardAction = serde_json::from_slice(&decrypted_body)
            .map_err(|e| Error::InvalidCardActionFormat(e.to_string()))?;

        card_action.event_req = Some(req.clone());

        if card_action.is_challenge() {
            self.logger.info("Received card challenge request");
            return self.handle_challenge(&card_action);
        }

        if !self.skip_signature_verification {
            self.verify_signature(&req, &decrypted_body)?;
        }

        let handler = self
            .handler
            .as_ref()
            .ok_or_else(|| Error::InvalidCardActionFormat("no handler registered".to_string()))?;

        let result = handler(card_action).await?;

        Ok(match result {
            Some(response) => {
                let body = serde_json::to_vec(&response)
                    .map_err(|e| Error::SerializationError(e.to_string()))?;
                EventResp::ok(body)
            }
            None => EventResp::ok(serde_json::to_vec(&CardResponse::success()).unwrap()),
        })
    }

    pub async fn handle_payload(&self, payload: &[u8]) -> Result<EventResp, Error> {
        self.logger.debug(&format!(
            "Card action stream payload: body_len={}",
            payload.len()
        ));

        let decrypted_body = self.decrypt_payload_if_needed(payload).await?;

        let card_action: super::models::CardAction = serde_json::from_slice(&decrypted_body)
            .map_err(|e| Error::InvalidCardActionFormat(e.to_string()))?;

        if card_action.is_challenge() {
            self.logger.info("Received card challenge request");
            return self.handle_challenge(&card_action);
        }

        let handler = self
            .handler
            .as_ref()
            .ok_or_else(|| Error::InvalidCardActionFormat("no handler registered".to_string()))?;

        let result = handler(card_action).await?;

        Ok(match result {
            Some(response) => {
                let body = serde_json::to_vec(&response)
                    .map_err(|e| Error::SerializationError(e.to_string()))?;
                EventResp::ok(body)
            }
            None => EventResp::ok(serde_json::to_vec(&CardResponse::success()).unwrap()),
        })
    }

    async fn decrypt_body_if_needed(&self, req: &EventReq) -> Result<Vec<u8>, Error> {
        self.decrypt_payload_if_needed(&req.body).await
    }

    async fn decrypt_payload_if_needed(&self, payload: &[u8]) -> Result<Vec<u8>, Error> {
        let body_str = String::from_utf8_lossy(payload);

        if body_str.contains("\"encrypt\"") {
            if let Some(ref key) = self.event_encrypt_key {
                self.logger.debug("Decrypting card action body");
                crate::event::maybe_decrypt_event_body(payload, Some(key))
                    .map_err(|e| Error::EventDecryption(e.to_string()))
            } else {
                Err(Error::EventDecryption(
                    "event_encrypt_key not configured".to_string(),
                ))
            }
        } else {
            Ok(payload.to_vec())
        }
    }

    fn handle_challenge(
        &self,
        card_action: &super::models::CardAction,
    ) -> Result<EventResp, Error> {
        if let Some(ref challenge) = card_action.challenge {
            if let Some(ref token) = self.verification_token
                && card_action.token.as_ref() != Some(token)
            {
                return Err(Error::CardSignatureVerification);
            }
            Ok(EventResp::challenge(challenge))
        } else {
            Err(Error::InvalidCardActionFormat(
                "challenge field missing".to_string(),
            ))
        }
    }

    fn verify_signature(&self, req: &EventReq, body: &[u8]) -> Result<(), Error> {
        let token = match self.verification_token.as_ref() {
            Some(token) => token,
            None => return Ok(()),
        };

        let timestamp = req
            .get_header("x-lark-request-timestamp")
            .ok_or_else(|| Error::CardSignatureVerification)?;

        let nonce = req
            .get_header("x-lark-request-nonce")
            .ok_or_else(|| Error::CardSignatureVerification)?;

        let signature = req
            .get_header("x-lark-signature")
            .ok_or_else(|| Error::CardSignatureVerification)?;

        let body_str = String::from_utf8_lossy(body);
        let expected_signature =
            crate::event::card_signature_sha1(timestamp, nonce, token, &body_str);

        if signature == expected_signature {
            self.logger.debug("Card signature verification passed");
            Ok(())
        } else {
            self.logger.error("Card signature verification failed");
            Err(Error::CardSignatureVerification)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::noop_logger;

    #[tokio::test]
    async fn test_card_handler_challenge() {
        let handler = CardActionHandler::new(noop_logger()).verification_token("test_token");

        let card_action = crate::card::models::CardAction {
            type_: Some("url_verification".to_string()),
            challenge: Some("test_challenge".to_string()),
            token: Some("test_token".to_string()),
            ..Default::default()
        };

        let resp = handler.handle_challenge(&card_action).unwrap();
        assert_eq!(resp.status_code, 200);
    }

    #[tokio::test]
    async fn test_card_handler_payload_entrypoint() {
        let handler = CardActionHandler::new(noop_logger()).handler(|_action| async {
            Ok(Some(
                crate::card::models::CardResponse::new()
                    .toast(crate::card::models::CardToast::success("ok")),
            ))
        });

        let resp = handler
            .handle_payload(
                br#"{"open_id":"ou_1","token":"verify_token","action":{"tag":"button"}}"#,
            )
            .await
            .expect("handle payload");

        assert_eq!(resp.status_code, 200);
        assert!(String::from_utf8_lossy(&resp.body).contains("\"toast\""));
    }
}
