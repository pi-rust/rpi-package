use std::time::Duration;

use feishu_sdk::core::{new_logger, Config, LogLevel, FEISHU_BASE_URL};
use feishu_sdk::event::{
    Event, EventDispatcher, EventDispatcherConfig, EventHandler, EventHandlerResult,
};
use feishu_sdk::ws::{StreamClient, StreamConfig};
use feishu_sdk::Client;

struct MessageHandler;

impl EventHandler for MessageHandler {
    fn event_type(&self) -> &str {
        "im.message.receive_v1"
    }

    fn handle(
        &self,
        event: Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EventHandlerResult> + Send + '_>> {
        println!(">>> HANDLER CALLED! event_type={:?}", event.event_type());
        println!(">>> event payload: {:?}", event.event);
        Box::pin(async { Ok(None) })
    }
}

#[tokio::main]
async fn main() {
    let app_id = std::env::var("FEISHU_APP_ID").expect("FEISHU_APP_ID");
    let app_secret = std::env::var("FEISHU_APP_SECRET").expect("FEISHU_APP_SECRET");
    let run_seconds: u64 = std::env::var("RUN_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90);

    let config = Config::builder(&app_id, &app_secret)
        .base_url(FEISHU_BASE_URL)
        .log_level(LogLevel::Debug)
        .build();

    let client = Client::new(config).expect("client");
    let dispatcher =
        EventDispatcher::new(EventDispatcherConfig::new(), new_logger(LogLevel::Debug));
    dispatcher.register_handler(Box::new(MessageHandler)).await;

    let stream_config = StreamConfig::new()
        .auto_reconnect(true)
        .reconnect_interval(Duration::from_secs(5))
        .reconnect_count(-1)
        .ping_interval(Duration::from_secs(10));

    let stream: StreamClient = client
        .stream()
        .stream_config(stream_config)
        .event_dispatcher(dispatcher)
        .build()
        .expect("stream build");

    println!(">>> starting stream (run {}s)...", run_seconds);
    let task = tokio::spawn(async move {
        let result = stream.start().await;
        println!(">>> stream.start() returned: {:?}", result);
    });

    tokio::time::sleep(Duration::from_secs(run_seconds)).await;
    println!(">>> timeout {}s reached, aborting", run_seconds);
    task.abort();
    let _ = task.await;
    println!(">>> done");
}
