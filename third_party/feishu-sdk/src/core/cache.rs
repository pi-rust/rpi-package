use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

#[async_trait]
pub trait Cache: Send + Sync + Debug {
    async fn get(&self, key: &str) -> Option<String>;
    async fn set(&self, key: String, value: String, ttl: Duration);
    async fn remove(&self, key: &str);
}

pub type CacheRef = Arc<dyn Cache>;

#[cfg(test)]
mod mock_cache {
    use std::collections::HashMap;

    use tokio::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    pub struct MockCache {
        data: Mutex<HashMap<String, String>>,
    }

    #[async_trait]
    impl Cache for MockCache {
        async fn get(&self, key: &str) -> Option<String> {
            let data = self.data.lock().await;
            data.get(key).cloned()
        }

        async fn set(&self, key: String, value: String, _ttl: Duration) {
            let mut data = self.data.lock().await;
            data.insert(key, value);
        }

        async fn remove(&self, key: &str) {
            let mut data = self.data.lock().await;
            data.remove(key);
        }
    }

    impl MockCache {
        pub fn new() -> Self {
            Self::default()
        }
    }
}

#[cfg(test)]
pub use mock_cache::MockCache;

pub fn default_cache() -> CacheRef {
    Arc::new(InMemoryCache::new())
}

#[derive(Debug, Default)]
pub struct InMemoryCache {
    inner: Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>,
}

impl InMemoryCache {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Cache for InMemoryCache {
    async fn get(&self, key: &str) -> Option<String> {
        use std::time::Instant;

        let inner = self.inner.lock().await;
        let (value, expires_at) = inner.get(key)?;

        if Instant::now() >= *expires_at {
            return None;
        }

        Some(value.clone())
    }

    async fn set(&self, key: String, value: String, ttl: Duration) {
        use std::time::Instant;

        let mut inner = self.inner.lock().await;
        inner.insert(key, (value, Instant::now() + ttl));
    }

    async fn remove(&self, key: &str) {
        let mut inner = self.inner.lock().await;
        inner.remove(key);
    }
}

use tokio::sync::Mutex;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_cache_set_get() {
        let cache = InMemoryCache::new();

        cache
            .set(
                "key1".to_string(),
                "value1".to_string(),
                Duration::from_secs(60),
            )
            .await;

        let value = cache.get("key1").await;
        assert_eq!(value, Some("value1".to_string()));
    }

    #[tokio::test]
    async fn test_in_memory_cache_remove() {
        let cache = InMemoryCache::new();

        cache
            .set(
                "key1".to_string(),
                "value1".to_string(),
                Duration::from_secs(60),
            )
            .await;
        cache.remove("key1").await;

        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_in_memory_cache_expiry() {
        let cache = InMemoryCache::new();

        cache
            .set(
                "key1".to_string(),
                "value1".to_string(),
                Duration::from_millis(10),
            )
            .await;

        tokio::time::sleep(Duration::from_millis(20)).await;

        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_mock_cache() {
        let cache = MockCache::new();

        cache
            .set(
                "key1".to_string(),
                "value1".to_string(),
                Duration::from_secs(60),
            )
            .await;

        let value = cache.get("key1").await;
        assert_eq!(value, Some("value1".to_string()));

        cache.remove("key1").await;

        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }
}
