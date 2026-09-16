//! HTTP Server Extension
//!
//! This module provides HTTP server support for handling events and card actions.
//! It integrates with the `salvo` framework when the `server` feature is enabled.
//!
//! # Example
//!
//! ```rust,no_run
//! use feishu_sdk::core::noop_logger;
//! use feishu_sdk::event::{EventDispatcher, EventDispatcherConfig};
//! use feishu_sdk::server::FeishuServer;
//!
//! #[tokio::main]
//! async fn main() {
//!     let config = EventDispatcherConfig::new()
//!         .verification_token("your_token")
//!         .encrypt_key("your_key");
//!
//!     let dispatcher = EventDispatcher::new(config, noop_logger());
//!
//!     let server = FeishuServer::new(dispatcher).port(8080);
//!     server.run().await;
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use salvo::prelude::*;

use crate::card::CardActionHandler;
use crate::core::Error;
use crate::event::{EventDispatcher, EventReq, EventResp};

pub struct FeishuServer {
    event_dispatcher: Arc<EventDispatcher>,
    card_handler: Option<Arc<CardActionHandler>>,
    port: u16,
}

impl FeishuServer {
    pub fn new(event_dispatcher: EventDispatcher) -> Self {
        Self {
            event_dispatcher: Arc::new(event_dispatcher),
            card_handler: None,
            port: 8080,
        }
    }

    pub fn card_handler(mut self, handler: CardActionHandler) -> Self {
        self.card_handler = Some(Arc::new(handler));
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn build_router(&self) -> Router {
        let event_dispatcher = self.event_dispatcher.clone();
        let card_handler = self.card_handler.clone();

        let mut router = Router::new().push(
            Router::with_path("webhook/event")
                .hoop(Injector::new(event_dispatcher))
                .post(handle_event),
        );

        if let Some(handler) = card_handler {
            router = router.push(
                Router::with_path("webhook/card")
                    .hoop(CardInjector::new(handler))
                    .post(handle_card),
            );
        }

        router
    }

    pub async fn run(self) {
        let router = self.build_router();
        let addr = format!("0.0.0.0:{}", self.port);
        let listener = TcpListener::new(addr).bind().await;
        Server::new(listener).serve(router).await;
    }
}

struct Injector<T> {
    data: T,
}

impl<T> Injector<T> {
    fn new(data: T) -> Self {
        Self { data }
    }
}

#[async_trait]
impl<T: Clone + Send + Sync + 'static> Handler for Injector<T> {
    async fn handle(
        &self,
        _req: &mut Request,
        depot: &mut Depot,
        _res: &mut Response,
        _ctrl: &mut FlowCtrl,
    ) {
        depot.insert("data", self.data.clone());
    }
}

struct CardInjector {
    handler: Arc<CardActionHandler>,
}

impl CardInjector {
    fn new(handler: Arc<CardActionHandler>) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl Handler for CardInjector {
    async fn handle(
        &self,
        _req: &mut Request,
        depot: &mut Depot,
        _res: &mut Response,
        _ctrl: &mut FlowCtrl,
    ) {
        depot.insert("card_handler", self.handler.clone());
    }
}

#[handler]
async fn handle_event(req: &mut Request, res: &mut Response, depot: &mut Depot) {
    let event_dispatcher = match depot.get::<Arc<EventDispatcher>>("data") {
        Ok(d) => d.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain("event dispatcher not found"));
            return;
        }
    };

    match process_event_request(req, event_dispatcher).await {
        Ok(event_resp) => {
            event_resp_to_response(event_resp, res);
        }
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain(e.to_string()));
        }
    }
}

#[handler]
async fn handle_card(req: &mut Request, res: &mut Response, depot: &mut Depot) {
    let card_handler = match depot.get::<Arc<CardActionHandler>>("card_handler") {
        Ok(h) => h.clone(),
        Err(_) => {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Text::Plain("card handler not configured"));
            return;
        }
    };

    match process_card_request(req, card_handler).await {
        Ok(event_resp) => {
            event_resp_to_response(event_resp, res);
        }
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain(e.to_string()));
        }
    }
}

async fn process_event_request(
    req: &mut Request,
    event_dispatcher: Arc<EventDispatcher>,
) -> Result<EventResp, Error> {
    let event_req = request_to_event_req(req).await?;
    event_dispatcher.dispatch(event_req).await
}

async fn process_card_request(
    req: &mut Request,
    card_handler: Arc<CardActionHandler>,
) -> Result<EventResp, Error> {
    let event_req = request_to_event_req(req).await?;
    card_handler.handle(event_req).await
}

async fn request_to_event_req(req: &mut Request) -> Result<EventReq, Error> {
    let request_uri = req.uri().path().to_string();
    let header = header_map_to_hashmap(req.headers());
    let body = req.payload().await.map(|b| b.to_vec()).unwrap_or_default();

    Ok(EventReq {
        header,
        body,
        request_uri,
    })
}

fn header_map_to_hashmap(
    headers: &salvo::http::headers::HeaderMap,
) -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    for (key, value) in headers {
        if let Ok(v) = value.to_str() {
            map.entry(key.to_string())
                .or_insert_with(Vec::new)
                .push(v.to_string());
        }
    }
    map
}

fn event_resp_to_response(event_resp: EventResp, res: &mut Response) {
    res.status_code(
        StatusCode::from_u16(event_resp.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
    );

    for (key, values) in event_resp.headers {
        if let Ok(header_name) = key.parse::<salvo::http::headers::HeaderName>() {
            for value in values {
                if let Ok(header_value) = value.parse() {
                    res.headers_mut().append(header_name.clone(), header_value);
                }
            }
        }
    }

    res.render(Text::Plain(
        String::from_utf8_lossy(&event_resp.body).into_owned(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::noop_logger;
    use crate::event::EventDispatcherConfig;

    #[test]
    fn test_server_builder() {
        let config = EventDispatcherConfig::new();
        let dispatcher = EventDispatcher::new(config, noop_logger());
        let server = FeishuServer::new(dispatcher).port(3000);

        assert_eq!(server.port, 3000);
    }
}
