//! Owns reusable outbound HTTP clients configured from persisted proxy settings.

use std::{sync::Arc, time::Duration};

use tokio::sync::RwLock;

use crate::{store::Store, Result};

#[derive(Clone)]
pub struct NetworkClients {
    store: Store,
    cache: Arc<RwLock<ClientCache>>,
}

#[derive(Default)]
struct ClientCache {
    default: Option<reqwest::Client>,
    cursor: Option<reqwest::Client>,
    provider: Option<(Duration, reqwest::Client)>,
}

impl NetworkClients {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            cache: Arc::new(RwLock::new(ClientCache::default())),
        }
    }

    pub async fn default_client(&self) -> Result<reqwest::Client> {
        if let Some(client) = self.cache.read().await.default.clone() {
            return Ok(client);
        }
        let mut cache = self.cache.write().await;
        if let Some(client) = cache.default.clone() {
            return Ok(client);
        }
        let client = client_builder(&self.store).await?.build()?;
        cache.default = Some(client.clone());
        Ok(client)
    }

    pub async fn cursor_client(&self) -> Result<reqwest::Client> {
        if let Some(client) = self.cache.read().await.cursor.clone() {
            return Ok(client);
        }
        let mut cache = self.cache.write().await;
        if let Some(client) = cache.cursor.clone() {
            return Ok(client);
        }
        let client = client_builder(&self.store)
            .await?
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        cache.cursor = Some(client.clone());
        Ok(client)
    }

    pub async fn provider_client(&self, timeout: Duration) -> Result<reqwest::Client> {
        if let Some((_, client)) = self
            .cache
            .read()
            .await
            .provider
            .as_ref()
            .filter(|(cached_timeout, _)| *cached_timeout == timeout)
        {
            return Ok(client.clone());
        }
        let mut cache = self.cache.write().await;
        if let Some((_, client)) = cache
            .provider
            .as_ref()
            .filter(|(cached_timeout, _)| *cached_timeout == timeout)
        {
            return Ok(client.clone());
        }
        let client = client_builder(&self.store)
            .await?
            .timeout(timeout)
            .build()?;
        cache.provider = Some((timeout, client.clone()));
        Ok(client)
    }

    pub async fn invalidate(&self) {
        *self.cache.write().await = ClientCache::default();
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
