//! Outbound HTTP clients configured from persisted application proxy settings.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use parking_lot::RwLock;

use crate::{store::ProxySettingsSecret, store::Store, Result};

#[derive(Clone, Default)]
pub struct HttpClientManager {
    cached: Arc<RwLock<Option<(ProxySettingsSecret, Duration, reqwest::Client)>>>,
    build_count: Arc<AtomicUsize>,
}

impl HttpClientManager {
    pub fn new() -> Self {
        Self {
            cached: Arc::new(RwLock::new(None)),
            build_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn client(&self, store: &Store, timeout: Duration) -> Result<reqwest::Client> {
        let settings = store.proxy_settings_secret().await?;
        {
            let guard = self.cached.read();
            if let Some((cached_settings, cached_timeout, client)) = &*guard {
                if cached_settings == &settings && *cached_timeout == timeout {
                    return Ok(client.clone());
                }
            }
        }

        let mut builder = reqwest::Client::builder().timeout(timeout);
        if settings.mode.is_custom() {
            let mut proxy = reqwest::Proxy::all(&settings.address)?;
            if settings.auth_enabled {
                proxy = proxy.basic_auth(&settings.username, &settings.password);
            }
            builder = builder.no_proxy().proxy(proxy);
        }
        let client = builder.build()?;
        self.build_count.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.cached.write();
        *guard = Some((settings, timeout, client.clone()));
        Ok(client)
    }

    pub fn invalidate(&self) {
        let mut guard = self.cached.write();
        *guard = None;
    }

    #[cfg(test)]
    fn build_count(&self) -> usize {
        self.build_count.load(Ordering::Relaxed)
    }
}

pub async fn client_builder(store: &Store) -> Result<reqwest::ClientBuilder> {
    let settings = store.proxy_settings_secret().await?;
    let mut builder = reqwest::Client::builder();
    if settings.mode.is_custom() {
        let mut proxy = reqwest::Proxy::all(&settings.address)?;
        if settings.auth_enabled {
            proxy = proxy.basic_auth(&settings.username, &settings.password);
        }
        builder = builder.no_proxy().proxy(proxy);
    }
    Ok(builder)
}

pub async fn client(store: &Store) -> Result<reqwest::Client> {
    Ok(client_builder(store).await?.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_client_manager_reuses_client_instance_when_settings_unchanged() {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let manager = HttpClientManager::new();
        let _client1 = manager
            .client(&store, Duration::from_secs(30))
            .await
            .unwrap();
        let _client2 = manager
            .client(&store, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(manager.build_count(), 1);

        let _client3 = manager
            .client(&store, Duration::from_secs(31))
            .await
            .unwrap();
        assert_eq!(manager.build_count(), 2);
    }
}
