use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{interval, sleep};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::core::{Error, LoggerRef};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug, Clone)]
pub struct WsConfig {
    pub url: String,
    pub heartbeat_interval: Duration,
    pub reconnect_delay: Duration,
    pub max_reconnect_attempts: u32,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            heartbeat_interval: Duration::from_secs(30),
            reconnect_delay: Duration::from_secs(5),
            max_reconnect_attempts: 10,
        }
    }
}

impl WsConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }

    pub fn heartbeat_interval(mut self, duration: Duration) -> Self {
        self.heartbeat_interval = duration;
        self
    }

    pub fn reconnect_delay(mut self, duration: Duration) -> Self {
        self.reconnect_delay = duration;
        self
    }

    pub fn max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.max_reconnect_attempts = attempts;
        self
    }
}

pub struct WebSocketClient {
    _config: WsConfig,
    _logger: LoggerRef,
    sender: mpsc::UnboundedSender<Vec<u8>>,
    is_connected: Arc<Mutex<bool>>,
}

impl WebSocketClient {
    pub async fn new(config: WsConfig, logger: LoggerRef) -> Result<Self, Error> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let is_connected = Arc::new(Mutex::new(false));

        let client = Self {
            _config: config.clone(),
            _logger: logger.clone(),
            sender,
            is_connected: is_connected.clone(),
        };

        client.spawn_connection_task(receiver, is_connected.clone(), config, logger);

        Ok(client)
    }

    fn spawn_connection_task(
        &self,
        mut receiver: mpsc::UnboundedReceiver<Vec<u8>>,
        is_connected: Arc<Mutex<bool>>,
        config: WsConfig,
        logger: LoggerRef,
    ) {
        tokio::spawn(async move {
            let mut reconnect_attempts = 0;

            loop {
                logger.info(&format!("Connecting to WebSocket: {}", config.url));

                match connect_async(&config.url).await {
                    Ok((ws_stream, _)) => {
                        logger.info("WebSocket connected");
                        reconnect_attempts = 0;
                        *is_connected.lock().await = true;

                        if let Err(e) = Self::handle_connection(
                            ws_stream,
                            &mut receiver,
                            &logger,
                            &config,
                            is_connected.clone(),
                        )
                        .await
                        {
                            logger.error(&format!("Connection error: {}", e));
                        }
                    }
                    Err(e) => {
                        logger.error(&format!("Failed to connect: {}", e));
                        reconnect_attempts += 1;

                        if reconnect_attempts >= config.max_reconnect_attempts {
                            logger.error("Max reconnect attempts reached");
                            break;
                        }

                        logger.info(&format!(
                            "Reconnecting in {:?}... (attempt {}/{})",
                            config.reconnect_delay,
                            reconnect_attempts,
                            config.max_reconnect_attempts
                        ));

                        sleep(config.reconnect_delay).await;
                    }
                }
            }
        });
    }

    async fn handle_connection(
        ws_stream: WsStream,
        receiver: &mut mpsc::UnboundedReceiver<Vec<u8>>,
        logger: &LoggerRef,
        config: &WsConfig,
        is_connected: Arc<Mutex<bool>>,
    ) -> Result<(), Error> {
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        let mut heartbeat = interval(config.heartbeat_interval);

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    let ping = Message::Ping(Bytes::new());
                    if let Err(e) = ws_sender.send(ping).await {
                        logger.error(&format!("Failed to send heartbeat: {}", e));
                        *is_connected.lock().await = false;
                        return Err(Error::WebSocketError(e.to_string()));
                    }
                    logger.debug("Heartbeat sent");
                }

                Some(data) = receiver.recv() => {
                    let msg = Message::Binary(Bytes::from(data));
                    if let Err(e) = ws_sender.send(msg).await {
                        logger.error(&format!("Failed to send message: {}", e));
                        *is_connected.lock().await = false;
                        return Err(Error::WebSocketError(e.to_string()));
                    }
                }

                msg = ws_receiver.next() => {
                    match msg {
                        Some(Ok(Message::Pong(_))) => {
                            logger.debug("Received pong");
                        }
                        Some(Ok(Message::Close(_))) => {
                            logger.info("WebSocket closed by server");
                            *is_connected.lock().await = false;
                            return Ok(());
                        }
                        Some(Ok(Message::Text(text))) => {
                            logger.debug(&format!("Received text message: {}", text));
                        }
                        Some(Ok(Message::Binary(data))) => {
                            logger.debug(&format!("Received binary message: {} bytes", data.len()));
                        }
                        Some(Err(e)) => {
                            logger.error(&format!("WebSocket error: {}", e));
                            *is_connected.lock().await = false;
                            return Err(Error::WebSocketError(e.to_string()));
                        }
                        None => {
                            logger.info("WebSocket stream ended");
                            *is_connected.lock().await = false;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    pub async fn send(&self, data: Vec<u8>) -> Result<(), Error> {
        self.sender
            .send(data)
            .map_err(|e| Error::WebSocketError(e.to_string()))
    }

    pub async fn is_connected(&self) -> bool {
        *self.is_connected.lock().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_config_builder() {
        let config = WsConfig::new("wss://example.com/ws")
            .heartbeat_interval(Duration::from_secs(10))
            .reconnect_delay(Duration::from_secs(3))
            .max_reconnect_attempts(5);

        assert_eq!(config.url, "wss://example.com/ws");
        assert_eq!(config.heartbeat_interval, Duration::from_secs(10));
        assert_eq!(config.reconnect_delay, Duration::from_secs(3));
        assert_eq!(config.max_reconnect_attempts, 5);
    }

    #[test]
    fn test_ws_config_default() {
        let config = WsConfig::default();
        assert_eq!(config.heartbeat_interval, Duration::from_secs(30));
        assert_eq!(config.reconnect_delay, Duration::from_secs(5));
        assert_eq!(config.max_reconnect_attempts, 10);
    }
}
