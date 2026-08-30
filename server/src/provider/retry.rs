//! Applies provider retry and backoff behavior.
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
