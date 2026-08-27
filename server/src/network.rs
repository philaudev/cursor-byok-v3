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
    // Use the platform TLS stack for compatibility with provider gateways that
    // only offer legacy TLS 1.2 cipher suites unsupported by rustls.
    let mut builder = reqwest::Client::builder().use_native_tls();
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

pub async fn blocking_client_builder(store: &Store) -> Result<reqwest::blocking::ClientBuilder> {
    let settings = store.proxy_settings_secret().await?;
    let mut builder = reqwest::blocking::Client::builder().use_native_tls();
    if settings.mode.is_custom() {
        let mut proxy = reqwest::Proxy::all(&settings.address)?;
        if settings.auth_enabled {
            proxy = proxy.basic_auth(&settings.username, &settings.password);
        }
        builder = builder.no_proxy().proxy(proxy);
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
    };

    use crate::store::{ProxyMode, ProxySettingsInput};

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

    #[tokio::test]
    async fn custom_proxy_applies_to_async_and_blocking_clients() {
        let directory = tempfile::tempdir().unwrap();
        let database_url = format!("sqlite://{}", directory.path().join("test.db").display());
        let store = Store::connect(&database_url).await.unwrap();
        let (proxy_address, requests, proxy) = proxy_server(2);
        store
            .set_proxy_settings(ProxySettingsInput {
                mode: ProxyMode::Custom,
                address: proxy_address,
                auth_enabled: true,
                username: "proxy-user".into(),
                password: Some("proxy-password".into()),
            })
            .await
            .unwrap();

        client(&store)
            .await
            .unwrap()
            .get("http://provider.invalid/async")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let blocking = blocking_client_builder(&store).await.unwrap();
        tokio::task::spawn_blocking(move || {
            blocking
                .build()
                .unwrap()
                .get("http://provider.invalid/blocking")
                .send()
                .unwrap()
                .error_for_status()
                .unwrap();
        })
        .await
        .unwrap();

        let requests = [requests.recv().unwrap(), requests.recv().unwrap()];
        assert!(requests
            .iter()
            .any(|request| request.starts_with("GET http://provider.invalid/async ")));
        assert!(requests
            .iter()
            .any(|request| request.starts_with("GET http://provider.invalid/blocking ")));
        assert!(requests.iter().all(|request| request
            .to_ascii_lowercase()
            .contains("\r\nproxy-authorization: basic ")));
        proxy.join().unwrap();
    }

    fn proxy_server(
        expected_requests: usize,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(expected_requests) {
                let mut stream = stream.unwrap();
                let mut request = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    request.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                sender.send(request).unwrap();
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .unwrap();
            }
        });
        (address, receiver, server)
    }
}
