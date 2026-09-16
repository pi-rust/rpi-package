use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::Error;

#[async_trait]
pub trait AppTicketManager: Send + Sync {
    async fn get(&self, app_id: &str) -> Option<String>;
    async fn set(&self, app_id: &str, ticket: String);
    async fn remove(&self, app_id: &str);
}

pub type AppTicketManagerRef = Arc<dyn AppTicketManager>;

pub struct InMemoryAppTicketManager {
    tickets: RwLock<HashMap<String, (String, std::time::Instant)>>,
    ttl: Duration,
}

impl InMemoryAppTicketManager {
    pub fn new() -> Self {
        Self {
            tickets: RwLock::new(HashMap::new()),
            ttl: Duration::from_secs(3600),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }
}

impl Default for InMemoryAppTicketManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AppTicketManager for InMemoryAppTicketManager {
    async fn get(&self, app_id: &str) -> Option<String> {
        let tickets = self.tickets.read().await;
        tickets.get(app_id).and_then(|(ticket, expires_at)| {
            if std::time::Instant::now() < *expires_at {
                Some(ticket.clone())
            } else {
                None
            }
        })
    }

    async fn set(&self, app_id: &str, ticket: String) {
        let expires_at = std::time::Instant::now() + self.ttl;
        let mut tickets = self.tickets.write().await;
        tickets.insert(app_id.to_string(), (ticket, expires_at));
    }

    async fn remove(&self, app_id: &str) {
        let mut tickets = self.tickets.write().await;
        tickets.remove(app_id);
    }
}

pub struct MockAppTicketManager {
    tickets: HashMap<String, String>,
}

impl MockAppTicketManager {
    pub fn new() -> Self {
        Self {
            tickets: HashMap::new(),
        }
    }

    pub fn with_ticket(mut self, app_id: &str, ticket: &str) -> Self {
        self.tickets.insert(app_id.to_string(), ticket.to_string());
        self
    }
}

impl Default for MockAppTicketManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AppTicketManager for MockAppTicketManager {
    async fn get(&self, app_id: &str) -> Option<String> {
        self.tickets.get(app_id).cloned()
    }

    async fn set(&self, app_id: &str, ticket: String) {
        let _ = (app_id, ticket);
    }

    async fn remove(&self, app_id: &str) {
        let _ = app_id;
    }
}

pub fn noop_app_ticket_manager() -> AppTicketManagerRef {
    Arc::new(NoopAppTicketManager)
}

pub struct NoopAppTicketManager;

#[async_trait]
impl AppTicketManager for NoopAppTicketManager {
    async fn get(&self, _app_id: &str) -> Option<String> {
        None
    }

    async fn set(&self, _app_id: &str, _ticket: String) {}

    async fn remove(&self, _app_id: &str) {}
}

pub async fn resend_app_ticket_with_retry<F, Fut>(
    max_attempts: u32,
    retry_delay: Duration,
    mut resend: F,
) -> Result<(), Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        match resend().await {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempts >= max_attempts {
                    return Err(err);
                }
                tokio::time::sleep(retry_delay).await;
            }
        }
    }
}

pub async fn ensure_app_ticket_with_retry<F, Fut>(
    manager: &dyn AppTicketManager,
    app_id: &str,
    max_attempts: u32,
    retry_delay: Duration,
    resend: F,
) -> Result<String, Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    if let Some(ticket) = manager.get(app_id).await {
        return Ok(ticket);
    }

    resend_app_ticket_with_retry(max_attempts, retry_delay, resend).await?;

    manager
        .get(app_id)
        .await
        .ok_or_else(|| Error::MissingAppTicket)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[tokio::test]
    async fn test_in_memory_app_ticket_manager() {
        let manager = InMemoryAppTicketManager::new();

        assert!(manager.get("app_123").await.is_none());

        manager.set("app_123", "ticket_abc".to_string()).await;
        assert_eq!(manager.get("app_123").await, Some("ticket_abc".to_string()));

        manager.remove("app_123").await;
        assert!(manager.get("app_123").await.is_none());
    }

    #[tokio::test]
    async fn test_in_memory_app_ticket_manager_expiry() {
        let manager = InMemoryAppTicketManager::new().with_ttl(Duration::from_millis(50));

        manager.set("app_123", "ticket_abc".to_string()).await;
        assert_eq!(manager.get("app_123").await, Some("ticket_abc".to_string()));

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(manager.get("app_123").await.is_none());
    }

    #[tokio::test]
    async fn test_mock_app_ticket_manager() {
        let manager = MockAppTicketManager::new().with_ticket("app_123", "ticket_abc");

        assert_eq!(manager.get("app_123").await, Some("ticket_abc".to_string()));
        assert!(manager.get("app_456").await.is_none());
    }

    #[tokio::test]
    async fn test_noop_app_ticket_manager() {
        let manager = noop_app_ticket_manager();

        assert!(manager.get("app_123").await.is_none());
        manager.set("app_123", "ticket_abc".to_string()).await;
        assert!(manager.get("app_123").await.is_none());
    }

    #[tokio::test]
    async fn test_resend_app_ticket_with_retry() {
        let attempts = AtomicU32::new(0);
        let result = resend_app_ticket_with_retry(3, Duration::from_millis(1), || async {
            let current = attempts.fetch_add(1, Ordering::SeqCst);
            if current < 1 {
                Err(Error::MissingAppTicket)
            } else {
                Ok(())
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_ensure_app_ticket_with_retry() {
        let manager = Arc::new(InMemoryAppTicketManager::new());
        let attempts = Arc::new(AtomicU32::new(0));
        let manager_clone = manager.clone();
        let attempts_clone = attempts.clone();

        let ticket = ensure_app_ticket_with_retry(
            manager.as_ref(),
            "app_123",
            3,
            Duration::from_millis(1),
            move || {
                let manager_clone = manager_clone.clone();
                let attempts_clone = attempts_clone.clone();
                async move {
                    attempts_clone.fetch_add(1, Ordering::SeqCst);
                    manager_clone
                        .set("app_123", "ticket_after_resend".to_string())
                        .await;
                    Ok(())
                }
            },
        )
        .await
        .expect("ticket should be available");

        assert_eq!(ticket, "ticket_after_resend");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
