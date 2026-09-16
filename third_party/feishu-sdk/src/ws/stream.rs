use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::http::Response as WsHttpResponse;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use super::stream_protocol::{
    ClientConfig as ServerClientConfig, EndpointResponse, Frame, FrameType, HEADER_BIZ_RT,
    HEADER_HANDSHAKE_AUTH_ERR_CODE, HEADER_HANDSHAKE_MSG, HEADER_HANDSHAKE_STATUS,
    HEADER_MESSAGE_ID, HEADER_SEQ, HEADER_SUM, HEADER_TRACE_ID, HEADER_TYPE, MessageType,
    QUERY_DEVICE_ID, QUERY_SERVICE_ID, STATUS_AUTH_FAILED, STATUS_EXCEED_CONN_LIMIT,
    STATUS_FORBIDDEN, STATUS_INTERNAL_ERROR, STATUS_OK, STATUS_SYSTEM_BUSY, STREAM_ENDPOINT_URI,
    StreamResponse,
};
use crate::card::CardActionHandler;
use crate::core::{Config, Error, LoggerRef};
use crate::event::{EventDispatcher, EventResp};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type WsWriter = SplitSink<WsStream, Message>;
type WsReader = SplitStream<WsStream>;
type SharedWriter = Arc<Mutex<WsWriter>>;

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub locale: String,
    pub auto_reconnect: bool,
    pub reconnect_count: i32,
    pub reconnect_interval: Duration,
    pub reconnect_nonce: u64,
    pub ping_interval: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            locale: "zh".to_string(),
            auto_reconnect: true,
            reconnect_count: -1,
            reconnect_interval: Duration::from_secs(120),
            reconnect_nonce: 30,
            ping_interval: Duration::from_secs(120),
        }
    }
}

impl StreamConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = locale.into();
        self
    }

    pub fn auto_reconnect(mut self, enabled: bool) -> Self {
        self.auto_reconnect = enabled;
        self
    }

    pub fn reconnect_count(mut self, count: i32) -> Self {
        self.reconnect_count = count;
        self
    }

    pub fn reconnect_interval(mut self, interval: Duration) -> Self {
        self.reconnect_interval = interval;
        self
    }

    pub fn reconnect_nonce(mut self, nonce_seconds: u64) -> Self {
        self.reconnect_nonce = nonce_seconds;
        self
    }

    pub fn ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = interval;
        self
    }
}

pub struct StreamClientBuilder {
    config: Config,
    stream_config: StreamConfig,
    event_dispatcher: Option<Arc<EventDispatcher>>,
    card_handler: Option<Arc<CardActionHandler>>,
}

impl StreamClientBuilder {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            stream_config: StreamConfig::default(),
            event_dispatcher: None,
            card_handler: None,
        }
    }

    pub fn stream_config(mut self, config: StreamConfig) -> Self {
        self.stream_config = config;
        self
    }

    pub fn event_dispatcher(mut self, dispatcher: EventDispatcher) -> Self {
        self.event_dispatcher = Some(Arc::new(dispatcher));
        self
    }

    pub fn event_dispatcher_ref(mut self, dispatcher: Arc<EventDispatcher>) -> Self {
        self.event_dispatcher = Some(dispatcher);
        self
    }

    pub fn card_handler(mut self, handler: CardActionHandler) -> Self {
        self.card_handler = Some(Arc::new(handler));
        self
    }

    pub fn card_handler_ref(mut self, handler: Arc<CardActionHandler>) -> Self {
        self.card_handler = Some(handler);
        self
    }

    pub fn build(self) -> Result<StreamClient, Error> {
        if self.config.app_id.is_empty() {
            return Err(Error::MissingConfig("app_id"));
        }
        if self.config.app_secret.is_empty() {
            return Err(Error::MissingConfig("app_secret"));
        }

        let mut http_builder = reqwest::Client::builder();
        if let Some(timeout) = self.config.request_timeout {
            http_builder = http_builder.timeout(timeout);
        }
        let endpoint_http = http_builder.build()?;

        Ok(StreamClient {
            inner: Arc::new(StreamClientInner {
                config: self.config.clone(),
                logger: self.config.logger.clone(),
                stream_config: Mutex::new(self.stream_config),
                event_dispatcher: self.event_dispatcher,
                card_handler: self.card_handler,
                endpoint_http,
                chunk_buffer: Mutex::new(HashMap::new()),
            }),
        })
    }
}

#[derive(Clone)]
pub struct StreamClient {
    inner: Arc<StreamClientInner>,
}

struct StreamClientInner {
    config: Config,
    logger: LoggerRef,
    stream_config: Mutex<StreamConfig>,
    event_dispatcher: Option<Arc<EventDispatcher>>,
    card_handler: Option<Arc<CardActionHandler>>,
    endpoint_http: reqwest::Client,
    chunk_buffer: Mutex<HashMap<String, ChunkAssembly>>,
}

struct ChunkAssembly {
    deadline: Instant,
    parts: Vec<Option<Vec<u8>>>,
}

struct StreamConnection {
    url: String,
    device_id: String,
    service_id: i32,
    reader: WsReader,
    writer: WsWriter,
}

#[derive(Debug, Clone, Serialize)]
struct EndpointRequest<'a> {
    #[serde(rename = "AppID")]
    app_id: &'a str,
    #[serde(rename = "AppSecret")]
    app_secret: &'a str,
}

impl StreamClient {
    pub fn new(config: Config, event_dispatcher: EventDispatcher) -> Result<Self, Error> {
        Self::builder(config)
            .event_dispatcher(event_dispatcher)
            .build()
    }

    pub fn builder(config: Config) -> StreamClientBuilder {
        StreamClientBuilder::new(config)
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<Result<(), Error>> {
        tokio::spawn(async move { self.start().await })
    }

    pub async fn start(&self) -> Result<(), Error> {
        let mut reconnect_attempts = 0u32;

        loop {
            match self.run_connection().await {
                Ok(()) => {
                    self.inner.logger.warn("Stream connection closed");
                    if !self.auto_reconnect().await {
                        return Ok(());
                    }
                }
                Err(err) => {
                    if !self.auto_reconnect().await || is_fatal_stream_error(&err) {
                        return Err(err);
                    }
                    self.inner
                        .logger
                        .warn(&format!("Stream connection dropped, reconnecting: {}", err));
                }
            }

            let reconnect_limit = self.reconnect_count().await;
            if reconnect_limit >= 0 && reconnect_attempts >= reconnect_limit as u32 {
                return Err(Error::WebSocketError(format!(
                    "unable to reconnect after {} retries",
                    reconnect_limit
                )));
            }

            let delay = if reconnect_attempts == 0 {
                self.initial_reconnect_delay().await
            } else {
                self.reconnect_interval().await
            };

            if !delay.is_zero() {
                self.inner.logger.info(&format!(
                    "Reconnecting stream in {:?} (attempt {})",
                    delay,
                    reconnect_attempts + 1
                ));
                sleep(delay).await;
            }

            reconnect_attempts += 1;
        }
    }

    async fn run_connection(&self) -> Result<(), Error> {
        let connection = self.connect().await?;
        self.inner.logger.info(&format!(
            "Stream connected: {} [device_id={}]",
            connection.url, connection.device_id
        ));

        let writer = Arc::new(Mutex::new(connection.writer));
        let ping_client = self.clone();
        let ping_writer = writer.clone();
        let ping_task = tokio::spawn(async move {
            ping_client
                .ping_loop(ping_writer, connection.service_id)
                .await;
        });

        let read_result = self.read_loop(connection.reader, writer.clone()).await;
        ping_task.abort();
        let _ = ping_task.await;
        read_result
    }

    async fn connect(&self) -> Result<StreamConnection, Error> {
        let endpoint = self.fetch_endpoint().await?;
        let url = Url::parse(&endpoint.url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let device_id = url
            .query_pairs()
            .find_map(|(key, value)| (key == QUERY_DEVICE_ID).then(|| value.into_owned()))
            .unwrap_or_default();
        let service_id = url
            .query_pairs()
            .find_map(|(key, value)| (key == QUERY_SERVICE_ID).then(|| value.into_owned()))
            .ok_or_else(|| Error::InvalidUrl("missing service_id in stream url".to_string()))?
            .parse::<i32>()
            .map_err(|e| Error::InvalidUrl(format!("invalid service_id: {e}")))?;

        let (stream, response) = connect_async(endpoint.url.as_str())
            .await
            .map_err(map_connect_error)?;
        if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
            return Err(parse_handshake_response(&response));
        }

        let (writer, reader) = stream.split();
        Ok(StreamConnection {
            url: endpoint.url,
            device_id,
            service_id,
            reader,
            writer,
        })
    }

    async fn fetch_endpoint(&self) -> Result<super::stream_protocol::Endpoint, Error> {
        let request = EndpointRequest {
            app_id: &self.inner.config.app_id,
            app_secret: &self.inner.config.app_secret,
        };
        let locale = self.inner.stream_config.lock().await.locale.clone();

        let response = self
            .inner
            .endpoint_http
            .post(format!(
                "{}{}",
                self.inner.config.base_url.trim_end_matches('/'),
                STREAM_ENDPOINT_URI
            ))
            .header("locale", locale)
            .json(&request)
            .send()
            .await?;

        if response.status() != reqwest::StatusCode::OK {
            return Err(Error::StreamServerError {
                code: i32::from(response.status().as_u16()),
                msg: "system busy".to_string(),
            });
        }

        let endpoint_response: EndpointResponse = response.json().await?;
        match endpoint_response.code {
            STATUS_OK => {}
            STATUS_SYSTEM_BUSY | STATUS_INTERNAL_ERROR => {
                return Err(Error::StreamServerError {
                    code: endpoint_response.code,
                    msg: endpoint_response.msg,
                });
            }
            _ => {
                return Err(Error::StreamClientError {
                    code: endpoint_response.code,
                    msg: endpoint_response.msg,
                });
            }
        }

        let endpoint = endpoint_response.data.ok_or(Error::StreamServerError {
            code: STATUS_INTERNAL_ERROR,
            msg: "endpoint is null".to_string(),
        })?;
        if endpoint.url.is_empty() {
            return Err(Error::StreamServerError {
                code: STATUS_INTERNAL_ERROR,
                msg: "endpoint is null".to_string(),
            });
        }

        if let Some(ref client_config) = endpoint.client_config {
            self.apply_server_client_config(client_config).await;
        }

        Ok(endpoint)
    }

    async fn ping_loop(&self, writer: SharedWriter, service_id: i32) {
        loop {
            let interval = normalized_interval(self.ping_interval().await);
            // Send an initial ping immediately after connecting so the
            // protocol session is established with the server right away.
            // The official SDKs send the first ping as soon as the connection
            // opens; waiting for the full ping interval before the first ping
            // leaves the session inactive and the server pushes no events.
            let frame = Frame::ping(service_id);
            if let Err(err) = self.send_frame(&writer, frame).await {
                self.inner
                    .logger
                    .warn(&format!("Stream ping failed: {}", err));
                return;
            }
            self.inner.logger.debug("Stream ping sent");
            sleep(interval).await;
        }
    }

    async fn read_loop(&self, mut reader: WsReader, writer: SharedWriter) -> Result<(), Error> {
        while let Some(message) = reader.next().await {
            match message {
                Ok(Message::Binary(bytes)) => {
                    self.handle_binary_frame(&writer, bytes.as_ref()).await?;
                }
                Ok(Message::Text(text)) => {
                    self.inner
                        .logger
                        .warn(&format!("Ignored text stream frame: {}", text));
                }
                Ok(Message::Ping(payload)) => {
                    self.send_ws_message(&writer, Message::Pong(payload))
                        .await?;
                }
                Ok(Message::Pong(_)) => {
                    self.inner.logger.debug("Received websocket pong");
                }
                Ok(Message::Close(frame)) => {
                    if let Some(frame) = frame {
                        self.inner.logger.warn(&format!(
                            "Stream closed by server: code={}, reason={}",
                            frame.code, frame.reason
                        ));
                    }
                    return Ok(());
                }
                Ok(Message::Frame(_)) => {}
                Err(err) => {
                    return Err(Error::WebSocketError(err.to_string()));
                }
            }
        }

        Ok(())
    }

    async fn handle_binary_frame(&self, writer: &SharedWriter, data: &[u8]) -> Result<(), Error> {
        let frame = match Frame::decode_binary(data) {
            Ok(frame) => frame,
            Err(err) => {
                self.inner
                    .logger
                    .error(&format!("Failed to decode stream frame: {}", err));
                return Ok(());
            }
        };

        match FrameType::parse(frame.method) {
            Some(FrameType::Control) => self.handle_control_frame(frame).await,
            Some(FrameType::Data) => self.handle_data_frame(writer, frame).await,
            None => {
                self.inner
                    .logger
                    .warn(&format!("Unknown stream frame type: {}", frame.method));
                Ok(())
            }
        }
    }

    async fn handle_control_frame(&self, frame: Frame) -> Result<(), Error> {
        match frame.header(HEADER_TYPE).and_then(MessageType::parse) {
            Some(MessageType::Pong) => {
                self.inner.logger.debug("Received stream pong");
                if frame.payload.is_empty() {
                    return Ok(());
                }

                match serde_json::from_slice::<ServerClientConfig>(&frame.payload) {
                    Ok(config) => self.apply_server_client_config(&config).await,
                    Err(err) => self.inner.logger.warn(&format!(
                        "Failed to parse stream client config from pong: {}",
                        err
                    )),
                }
                Ok(())
            }
            Some(other) => {
                self.inner.logger.debug(&format!(
                    "Ignored stream control frame type={}",
                    other.as_str()
                ));
                Ok(())
            }
            None => Ok(()),
        }
    }

    async fn handle_data_frame(
        &self,
        writer: &SharedWriter,
        mut frame: Frame,
    ) -> Result<(), Error> {
        let message_type = match frame.header(HEADER_TYPE).and_then(MessageType::parse) {
            Some(message_type) => message_type,
            None => return Ok(()),
        };
        let message_id = frame
            .header(HEADER_MESSAGE_ID)
            .unwrap_or_default()
            .to_string();
        let trace_id = frame
            .header(HEADER_TRACE_ID)
            .unwrap_or_default()
            .to_string();
        let sum = frame.header_i32(HEADER_SUM).unwrap_or(1).max(1) as usize;
        let seq = frame.header_i32(HEADER_SEQ).unwrap_or(0);

        let payload = if sum > 1 {
            match self
                .combine_chunks(&message_id, sum, seq, frame.payload.clone())
                .await
            {
                Some(payload) => payload,
                None => return Ok(()),
            }
        } else {
            frame.payload.clone()
        };

        self.inner.logger.debug(&format!(
            "Received stream message: type={}, message_id={}, trace_id={}, payload_len={}",
            message_type.as_str(),
            message_id,
            trace_id,
            payload.len()
        ));

        let started = Instant::now();
        let mut stream_response = StreamResponse::ok();
        let dispatch_result = match message_type {
            MessageType::Event => self.dispatch_event_payload(&payload).await,
            MessageType::Card => self.dispatch_card_payload(&payload).await,
            MessageType::Ping | MessageType::Pong => return Ok(()),
        };

        match dispatch_result {
            Ok(Some(resp)) => {
                stream_response = StreamResponse::from(&resp);
            }
            Ok(None) => {}
            Err(err) => {
                self.inner.logger.error(&format!(
                    "Failed to handle stream message: type={}, message_id={}, trace_id={}, err={}",
                    message_type.as_str(),
                    message_id,
                    trace_id,
                    err
                ));
                stream_response = StreamResponse::from_status(500);
            }
        }

        frame.set_header(HEADER_BIZ_RT, started.elapsed().as_millis().to_string());
        frame.payload = serde_json::to_vec(&stream_response)
            .map_err(|e| Error::SerializationError(e.to_string()))?;
        self.send_frame(writer, frame).await
    }

    async fn dispatch_event_payload(&self, payload: &[u8]) -> Result<Option<EventResp>, Error> {
        match &self.inner.event_dispatcher {
            Some(dispatcher) => dispatcher.dispatch_payload(payload).await,
            None => {
                self.inner
                    .logger
                    .warn("Received stream event with no event dispatcher");
                Ok(None)
            }
        }
    }

    async fn dispatch_card_payload(&self, payload: &[u8]) -> Result<Option<EventResp>, Error> {
        match &self.inner.card_handler {
            Some(handler) => handler.handle_payload(payload).await.map(Some),
            None => {
                self.inner
                    .logger
                    .warn("Received stream card callback with no card handler");
                Ok(None)
            }
        }
    }

    async fn combine_chunks(
        &self,
        message_id: &str,
        sum: usize,
        seq: i32,
        payload: Vec<u8>,
    ) -> Option<Vec<u8>> {
        if message_id.is_empty() || seq < 0 {
            self.inner
                .logger
                .warn("Discarded invalid chunked stream message");
            return None;
        }

        let now = Instant::now();
        let mut chunk_buffer = self.inner.chunk_buffer.lock().await;
        chunk_buffer.retain(|_, chunk| chunk.deadline > now);

        let chunk = chunk_buffer
            .entry(message_id.to_string())
            .or_insert_with(|| ChunkAssembly {
                deadline: now + Duration::from_secs(5),
                parts: vec![None; sum],
            });

        if seq as usize >= chunk.parts.len() {
            chunk_buffer.remove(message_id);
            self.inner.logger.warn(&format!(
                "Discarded invalid stream chunk: message_id={}, seq={}, sum={}",
                message_id, seq, sum
            ));
            return None;
        }

        chunk.parts[seq as usize] = Some(payload);
        if chunk.parts.iter().any(Option::is_none) {
            chunk.deadline = now + Duration::from_secs(5);
            return None;
        }

        let mut combined = Vec::with_capacity(
            chunk
                .parts
                .iter()
                .filter_map(|part| part.as_ref().map(Vec::len))
                .sum(),
        );
        for part in chunk.parts.iter().flatten() {
            combined.extend_from_slice(part);
        }
        chunk_buffer.remove(message_id);
        Some(combined)
    }

    async fn send_frame(&self, writer: &SharedWriter, frame: Frame) -> Result<(), Error> {
        self.send_ws_message(writer, Message::Binary(frame.encode_binary().into()))
            .await
    }

    async fn send_ws_message(&self, writer: &SharedWriter, message: Message) -> Result<(), Error> {
        let mut writer = writer.lock().await;
        writer
            .send(message)
            .await
            .map_err(|e| Error::WebSocketError(e.to_string()))
    }

    async fn apply_server_client_config(&self, config: &ServerClientConfig) {
        let mut stream_config = self.inner.stream_config.lock().await;
        stream_config.reconnect_count = config.reconnect_count;
        stream_config.reconnect_interval =
            Duration::from_secs(config.reconnect_interval.max(0) as u64);
        stream_config.reconnect_nonce = config.reconnect_nonce.max(0) as u64;
        stream_config.ping_interval = Duration::from_secs(config.ping_interval.max(0) as u64);
    }

    async fn auto_reconnect(&self) -> bool {
        self.inner.stream_config.lock().await.auto_reconnect
    }

    async fn reconnect_count(&self) -> i32 {
        self.inner.stream_config.lock().await.reconnect_count
    }

    async fn reconnect_interval(&self) -> Duration {
        self.inner.stream_config.lock().await.reconnect_interval
    }

    async fn ping_interval(&self) -> Duration {
        self.inner.stream_config.lock().await.ping_interval
    }

    async fn initial_reconnect_delay(&self) -> Duration {
        let stream_config = self.inner.stream_config.lock().await;
        if stream_config.reconnect_nonce == 0 {
            return stream_config.reconnect_interval;
        }

        Duration::from_millis(pseudo_random_millis(
            stream_config.reconnect_nonce.saturating_mul(1000),
        ))
    }
}

fn is_fatal_stream_error(err: &Error) -> bool {
    matches!(err, Error::StreamClientError { .. })
}

fn normalized_interval(duration: Duration) -> Duration {
    if duration.is_zero() {
        Duration::from_secs(1)
    } else {
        duration
    }
}

fn pseudo_random_millis(max_millis: u64) -> u64 {
    if max_millis == 0 {
        return 0;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    now % max_millis
}

fn map_connect_error(err: tungstenite::Error) -> Error {
    match err {
        tungstenite::Error::Http(response) => parse_handshake_response(&response),
        other => Error::WebSocketError(other.to_string()),
    }
}

fn parse_handshake_response<T>(response: &WsHttpResponse<T>) -> Error {
    let status = response
        .headers()
        .get(HEADER_HANDSHAKE_STATUS)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(i32::from(response.status().as_u16()));
    let msg = response
        .headers()
        .get(HEADER_HANDSHAKE_MSG)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("websocket handshake failed")
        .to_string();

    match status {
        STATUS_AUTH_FAILED => {
            let auth_code = response
                .headers()
                .get(HEADER_HANDSHAKE_AUTH_ERR_CODE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or_default();
            if auth_code == STATUS_EXCEED_CONN_LIMIT {
                Error::StreamClientError { code: status, msg }
            } else {
                Error::StreamServerError { code: status, msg }
            }
        }
        STATUS_FORBIDDEN => Error::StreamClientError { code: status, msg },
        _ => Error::StreamServerError { code: status, msg },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio_tungstenite::tungstenite::http::{HeaderValue, Response};

    use super::*;
    use crate::core::noop_logger;
    use crate::event::EventDispatcherConfig;

    #[tokio::test]
    async fn combine_chunks_waits_until_all_parts_arrive() {
        let client = StreamClient::builder(Config::builder("app", "secret").build())
            .build()
            .expect("stream client");

        assert!(
            client
                .combine_chunks("msg_1", 2, 0, b"hello ".to_vec())
                .await
                .is_none()
        );
        let combined = client
            .combine_chunks("msg_1", 2, 1, b"world".to_vec())
            .await
            .expect("combined payload");
        assert_eq!(combined, b"hello world");
    }

    #[tokio::test]
    async fn applies_server_runtime_config() {
        let client = StreamClient::builder(Config::builder("app", "secret").build())
            .build()
            .expect("stream client");

        client
            .apply_server_client_config(&ServerClientConfig {
                reconnect_count: 5,
                reconnect_interval: 10,
                reconnect_nonce: 2,
                ping_interval: 30,
            })
            .await;

        assert_eq!(client.reconnect_count().await, 5);
        assert_eq!(client.reconnect_interval().await, Duration::from_secs(10));
        assert_eq!(client.ping_interval().await, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn dispatches_stream_events_via_payload_entrypoint() {
        struct TestHandler;

        impl crate::event::EventHandler for TestHandler {
            fn event_type(&self) -> &str {
                "im.message.receive_v1"
            }

            fn handle(
                &self,
                _event: crate::event::Event,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = crate::event::EventHandlerResult> + Send + '_>,
            > {
                Box::pin(async { Ok(None) })
            }
        }

        let dispatcher = EventDispatcher::new(EventDispatcherConfig::new(), noop_logger());
        dispatcher.register_handler(Box::new(TestHandler)).await;

        let client = StreamClient::builder(Config::builder("app", "secret").build())
            .event_dispatcher(dispatcher)
            .build()
            .expect("stream client");

        let payload = br#"{"schema":"2.0","header":{"event_type":"im.message.receive_v1"}}"#;
        let resp = client
            .dispatch_event_payload(payload)
            .await
            .expect("dispatch");
        assert!(resp.is_none());
    }

    #[test]
    fn parses_handshake_errors_using_server_headers() {
        let response = Response::builder()
            .status(403)
            .header(HEADER_HANDSHAKE_STATUS, HeaderValue::from_static("403"))
            .header(HEADER_HANDSHAKE_MSG, HeaderValue::from_static("forbidden"))
            .body(())
            .expect("response");

        let err = parse_handshake_response(&response);
        assert!(matches!(err, Error::StreamClientError { code: 403, .. }));
    }

    #[test]
    fn normalized_zero_interval_avoids_busy_loop() {
        assert_eq!(normalized_interval(Duration::ZERO), Duration::from_secs(1));
        assert_eq!(
            normalized_interval(Duration::from_secs(2)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn stream_response_keeps_first_header_value() {
        let mut headers = HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            vec!["application/json".to_string(), "ignored".to_string()],
        );
        let resp = EventResp {
            status_code: 200,
            headers,
            body: b"{}".to_vec(),
        };
        let stream_resp = StreamResponse::from(&resp);
        assert_eq!(
            stream_resp.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn stream_builder_keeps_custom_runtime_defaults() {
        let client = StreamClient::builder(Config::builder("app", "secret").build())
            .stream_config(
                StreamConfig::new()
                    .auto_reconnect(false)
                    .reconnect_count(1)
                    .reconnect_interval(Duration::from_secs(3))
                    .ping_interval(Duration::from_secs(9)),
            )
            .build()
            .expect("stream client");

        let runtime = client.inner.stream_config.blocking_lock().clone();
        assert!(!runtime.auto_reconnect);
        assert_eq!(runtime.reconnect_count, 1);
        assert_eq!(runtime.reconnect_interval, Duration::from_secs(3));
        assert_eq!(runtime.ping_interval, Duration::from_secs(9));
    }
}
