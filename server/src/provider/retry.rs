use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

use super::CallRecorder;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryPolicy {
    pub retries: u32,
    pub delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            retries: 5,
            delay: Duration::from_secs(5),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Attempt {
    Response(reqwest::Response),
    Cancelled,
}

pub(crate) async fn send_with_retry<F>(
    label: &str,
    build: F,
    policy: RetryPolicy,
    cancellation: &CancellationToken,
    recorder: Option<&CallRecorder>,
    request_headers: serde_json::Value,
    request_body: &serde_json::Value,
) -> Result<Attempt>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    for attempt in 0..=policy.retries {
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
            response = build().send() => response,
        }?;
        if let Some(recorder) = recorder {
            recorder
                .response_headers(response.status().as_u16())
                .await?;
        }
        if response.status().is_success() {
            return Ok(Attempt::Response(response));
        }
        let status = response.status();
        let bytes = response.bytes().await?;
        let error = Error::Provider(format!(
            "{label} {status}: {}",
            String::from_utf8_lossy(&bytes)
        ));
        if attempt == policy.retries {
            if let Some(recorder) = recorder {
                recorder.failed(&error).await?;
            }
            return Err(error);
        }
        tracing::warn!(
            provider = label,
            status = status.as_u16(),
            attempt = attempt + 1,
            retries = policy.retries,
            delay_ms = policy.delay.as_millis(),
            "provider returned a non-success status, retrying"
        );
        if let Some(recorder) = recorder {
            recorder
                .retry(&error, request_headers.clone(), request_body)
                .await?;
        }
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
            _ = tokio::time::sleep(policy.delay) => {}
        }
    }
    unreachable!("the retry loop returns on the final attempt")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };

    use axum::{extract::State, http::StatusCode, routing::post, Router};

    fn fast(retries: u32) -> RetryPolicy {
        RetryPolicy {
            retries,
            delay: Duration::from_millis(20),
        }
    }

    async fn status_server(statuses: Vec<u16>) -> (String, Arc<AtomicU32>) {
        async fn endpoint(
            State((statuses, calls)): State<(Arc<Vec<u16>>, Arc<AtomicU32>)>,
        ) -> (StatusCode, String) {
            let index = calls.fetch_add(1, Ordering::SeqCst) as usize;
            let status = statuses
                .get(index)
                .copied()
                .unwrap_or_else(|| *statuses.last().unwrap());
            (
                StatusCode::from_u16(status).unwrap(),
                format!("body for attempt {index}"),
            )
        }

        let calls = Arc::new(AtomicU32::new(0));
        let app = Router::new()
            .route("/responses", post(endpoint))
            .with_state((Arc::new(statuses), calls.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/responses"), calls)
    }

    fn sender(url: String) -> impl Fn() -> reqwest::RequestBuilder {
        let client = reqwest::Client::new();
        move || client.post(&url).json(&serde_json::json!({"stream": true}))
    }

    #[test]
    fn the_default_policy_retries_five_times_every_five_seconds() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.retries, 5);
        assert_eq!(policy.delay, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_non_success_response_is_retried_until_it_succeeds() {
        let (url, calls) = status_server(vec![429, 500, 200]).await;

        let attempt = send_with_retry(
            "Test",
            sender(url),
            fast(5),
            &CancellationToken::new(),
            None,
            serde_json::json!({}),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        let Attempt::Response(response) = attempt else {
            panic!("expected a response");
        };
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn the_last_non_success_response_fails_after_the_retry_budget() {
        let (url, calls) = status_server(vec![429]).await;

        let error = send_with_retry(
            "Test",
            sender(url),
            fast(5),
            &CancellationToken::new(),
            None,
            serde_json::json!({}),
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(&error, Error::Provider(message) if message.contains("Test 429")),
            "unexpected error: {error}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn every_retry_waits_for_the_configured_delay() {
        let (url, _) = status_server(vec![429, 429, 200]).await;
        let started = std::time::Instant::now();

        send_with_retry(
            "Test",
            sender(url),
            RetryPolicy {
                retries: 5,
                delay: Duration::from_millis(150),
            },
            &CancellationToken::new(),
            None,
            serde_json::json!({}),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "retries did not wait: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn cancellation_during_the_retry_delay_stops_the_attempts() {
        let (url, calls) = status_server(vec![429]).await;
        let cancellation = CancellationToken::new();
        let deadline = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            deadline.cancel();
        });

        let attempt = send_with_retry(
            "Test",
            sender(url),
            RetryPolicy {
                retries: 5,
                delay: Duration::from_millis(500),
            },
            &cancellation,
            None,
            serde_json::json!({}),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        assert!(matches!(attempt, Attempt::Cancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_success_response_is_returned_without_any_retry() {
        let (url, calls) = status_server(vec![200]).await;

        let attempt = send_with_retry(
            "Test",
            sender(url),
            fast(5),
            &CancellationToken::new(),
            None,
            serde_json::json!({}),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        assert!(matches!(attempt, Attempt::Response(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
