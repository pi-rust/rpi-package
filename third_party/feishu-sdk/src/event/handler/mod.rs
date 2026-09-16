use std::future::Future;
use std::pin::Pin;

use super::models::{Event, EventResp};

pub type EventHandlerResult = Result<Option<EventResp>, crate::core::Error>;

pub trait EventHandler: Send + Sync {
    fn event_type(&self) -> &str;

    fn handle(&self, event: Event)
    -> Pin<Box<dyn Future<Output = EventHandlerResult> + Send + '_>>;
}

pub type BoxedEventHandler = Box<dyn EventHandler>;

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHandler;

    impl EventHandler for TestHandler {
        fn event_type(&self) -> &str {
            "test.event"
        }

        fn handle(
            &self,
            _event: Event,
        ) -> Pin<Box<dyn Future<Output = EventHandlerResult> + Send + '_>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[test]
    fn test_event_handler_trait() {
        let handler = TestHandler;
        assert_eq!(handler.event_type(), "test.event");
    }
}
