//! Records provider requests, responses, usage, and timing.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use tokio::sync::Mutex;

use crate::{
    model::{NewLlmCall, Usage},
    store::{BufferedLlmChunk, Store},
    Result,
};

use super::{is_valid_response_event, FinishReason, ModelEvent};

pub(crate) fn recorded_headers(
    config: &crate::config::ProviderConfig,
    defaults: &[(&str, &str)],
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for (name, value) in defaults {
        output.insert((*name).into(), (*value).into());
    }
    for (name, value) in &config.custom_headers {
        if crate::model::is_sensitive_header(name.as_str()) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            output.insert(name.as_str().into(), value.into());
        }
    }
    serde_json::Value::Object(output)
}

#[derive(Clone)]
pub struct CallRecorder {
    inner: Arc<Inner>,
}

pub(super) struct CancelOnDrop {
    recorder: CallRecorder,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.recorder.is_finished() {
            return;
        }
        let recorder = self.recorder.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                call_id = recorder.call_id(),
                "unfinished LLM call dropped outside Tokio runtime"
            );
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = recorder.cancelled().await {
                tracing::warn!(call_id = recorder.call_id(), %error, "failed to mark dropped LLM call cancelled");
            }
        });
    }
}

struct Inner {
    store: Store,
    base_call: NewLlmCall,
    detailed: bool,
    attempt: Mutex<AttemptState>,
    flush_gate: Mutex<()>,
    next_generation: AtomicU64,
    finished: AtomicBool,
}

struct AttemptState {
    call_id: String,
    started: Instant,
    next_chunk: AtomicI64,
    chunks: ChunkBuffer,
    first_text_recorded: AtomicBool,
    first_valid_response_recorded: AtomicBool,
}

impl AttemptState {
    fn new(call_id: String) -> Self {
        Self {
            call_id,
            started: Instant::now(),
            next_chunk: AtomicI64::new(0),
            chunks: ChunkBuffer::default(),
            first_text_recorded: AtomicBool::new(false),
            first_valid_response_recorded: AtomicBool::new(false),
        }
    }
}

#[derive(Default)]
struct ChunkBuffer {
    chunks: Vec<BufferedLlmChunk>,
    bytes: usize,
    first_chunk_at: Option<Instant>,
    generation: u64,
}

const MAX_BUFFERED_CHUNKS: usize = 32;
const MAX_BUFFERED_BYTES: usize = 256 * 1024;
const MAX_BUFFER_AGE: std::time::Duration = std::time::Duration::from_millis(50);

impl CallRecorder {
    pub async fn start(store: Store, mut call: NewLlmCall) -> Result<Self> {
        call.detailed = store.detailed_logging().await?;
        store.start_llm_call(&call).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                base_call: call.clone(),
                detailed: call.detailed,
                attempt: Mutex::new(AttemptState::new(call.call_id.clone())),
                flush_gate: Mutex::new(()),
                next_generation: AtomicU64::new(0),
                finished: AtomicBool::new(false),
            }),
        })
    }

    pub fn detailed(&self) -> bool {
        self.inner.detailed
    }

    pub fn is_finished(&self) -> bool {
        self.inner.finished.load(Ordering::Acquire)
    }

    pub(super) fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop {
            recorder: self.clone(),
        }
    }

    pub async fn request(
        &self,
        headers: serde_json::Value,
        body: &serde_json::Value,
    ) -> Result<()> {
        let call_id = {
            let attempt = self.inner.attempt.lock().await;
            attempt.call_id.clone()
        };
        self.inner
            .store
            .record_llm_request(&call_id, &headers, body, self.inner.detailed)
            .await?;
        Ok(())
    }

    pub async fn response_headers(&self, status: u16) -> Result<()> {
        let (call_id, elapsed) = {
            let attempt = self.inner.attempt.lock().await;
            (attempt.call_id.clone(), elapsed_ms(attempt.started))
        };
        self.inner
            .store
            .record_llm_response_headers(&call_id, elapsed, status)
            .await
    }

    pub async fn response_chunk(&self, data: &[u8]) -> Result<()> {
        let _flush_gate = self.inner.flush_gate.lock().await;
        if self.is_finished() {
            return Ok(());
        }
        let (call_id, to_flush, schedule_flush) = {
            let mut attempt = self.inner.attempt.lock().await;
            if self.is_finished() {
                return Ok(());
            }
            let seq = attempt.next_chunk.fetch_add(1, Ordering::Relaxed);
            let schedule_flush = if attempt.chunks.chunks.is_empty() {
                attempt.chunks.generation = self
                    .inner
                    .next_generation
                    .fetch_add(1, Ordering::Relaxed)
                    .wrapping_add(1);
                attempt.chunks.first_chunk_at = Some(Instant::now());
                Some(attempt.chunks.generation)
            } else {
                None
            };
            attempt.chunks.bytes += data.len();
            let elapsed = elapsed_ms(attempt.started);
            attempt.chunks.chunks.push(if self.inner.detailed {
                BufferedLlmChunk::new(seq, elapsed, data)
            } else {
                BufferedLlmChunk::metrics(seq, elapsed, data.len())
            });
            let expired = attempt
                .chunks
                .first_chunk_at
                .is_some_and(|started| started.elapsed() >= MAX_BUFFER_AGE);
            let to_flush = if attempt.chunks.chunks.len() >= MAX_BUFFERED_CHUNKS
                || attempt.chunks.bytes >= MAX_BUFFERED_BYTES
                || expired
            {
                let chunks = std::mem::take(&mut attempt.chunks.chunks);
                attempt.chunks.bytes = 0;
                attempt.chunks.first_chunk_at = None;
                Some(chunks)
            } else {
                None
            };
            (attempt.call_id.clone(), to_flush, schedule_flush)
        };

        if let Some(chunks) = to_flush {
            self.inner
                .store
                .record_llm_chunks(&call_id, &chunks, self.inner.detailed)
                .await?;
        }

        if let Some(generation) = schedule_flush {
            let recorder = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(MAX_BUFFER_AGE).await;
                if let Err(error) = recorder.flush_generation(generation).await {
                    tracing::warn!(call_id = recorder.call_id(), %error, "failed to flush LLM response chunks");
                }
            });
        }
        Ok(())
    }

    pub async fn event(&self, event: &ModelEvent) -> Result<()> {
        let first_valid = if is_valid_response_event(event) {
            let attempt = self.inner.attempt.lock().await;
            if attempt
                .first_valid_response_recorded
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                Some((attempt.call_id.clone(), elapsed_ms(attempt.started)))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((call_id, elapsed)) = first_valid {
            if let Err(error) = self
                .inner
                .store
                .record_llm_first_valid_response(&call_id, elapsed)
                .await
            {
                let attempt = self.inner.attempt.lock().await;
                attempt
                    .first_valid_response_recorded
                    .store(false, Ordering::Release);
                return Err(error);
            }
        }

        match event {
            ModelEvent::TextDelta(delta) if !delta.trim().is_empty() => {
                let first_text = {
                    let attempt = self.inner.attempt.lock().await;
                    if attempt
                        .first_text_recorded
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        Some((attempt.call_id.clone(), elapsed_ms(attempt.started)))
                    } else {
                        None
                    }
                };
                if let Some((call_id, elapsed)) = first_text {
                    if let Err(error) = self
                        .inner
                        .store
                        .record_llm_first_text(&call_id, elapsed)
                        .await
                    {
                        let attempt = self.inner.attempt.lock().await;
                        attempt.first_text_recorded.store(false, Ordering::Release);
                        return Err(error);
                    }
                }
            }
            ModelEvent::Usage(usage) => self.usage(*usage).await?,
            ModelEvent::Done(reason) => self.completed(*reason).await?,
            _ => {}
        }
        Ok(())
    }

    pub async fn usage(&self, usage: Usage) -> Result<()> {
        let call_id = {
            let attempt = self.inner.attempt.lock().await;
            attempt.call_id.clone()
        };
        self.inner
            .store
            .record_llm_usage(&call_id, usage)
            .await
    }

    pub async fn completed(&self, reason: FinishReason) -> Result<()> {
        self.finish("completed", Some(finish_reason(reason)), None, None)
            .await
    }

    pub async fn failed(&self, error: &crate::Error) -> Result<()> {
        self.finish(
            "error",
            None,
            Some(error_kind(error)),
            Some(&error.to_string()),
        )
        .await
    }

    pub async fn cancelled(&self) -> Result<()> {
        self.finish("cancelled", None, None, None).await
    }

    async fn finish(
        &self,
        status: &str,
        reason: Option<&str>,
        error_kind: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        let _flush_gate = self.inner.flush_gate.lock().await;
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (call_id, elapsed, remaining_chunks) = {
            let mut attempt = self.inner.attempt.lock().await;
            let remaining = if !attempt.chunks.chunks.is_empty() {
                let chunks = std::mem::take(&mut attempt.chunks.chunks);
                attempt.chunks.bytes = 0;
                attempt.chunks.first_chunk_at = None;
                Some(chunks)
            } else {
                None
            };
            (attempt.call_id.clone(), elapsed_ms(attempt.started), remaining)
        };

        if let Some(chunks) = remaining_chunks {
            if let Err(error) = self
                .inner
                .store
                .record_llm_chunks(&call_id, &chunks, self.inner.detailed)
                .await
            {
                self.inner.finished.store(false, Ordering::Release);
                return Err(error);
            }
        }

        if let Err(error) = self
            .inner
            .store
            .finish_llm_call(
                &call_id,
                status,
                reason,
                elapsed,
                error_kind,
                error_message,
            )
            .await
        {
            self.inner.finished.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    async fn flush_generation(&self, generation: u64) -> Result<()> {
        let _flush_gate = self.inner.flush_gate.lock().await;
        if self.is_finished() {
            return Ok(());
        }
        let (call_id, to_flush) = {
            let mut attempt = self.inner.attempt.lock().await;
            if attempt.chunks.generation != generation || attempt.chunks.chunks.is_empty() {
                (attempt.call_id.clone(), None)
            } else {
                let chunks = std::mem::take(&mut attempt.chunks.chunks);
                attempt.chunks.bytes = 0;
                attempt.chunks.first_chunk_at = None;
                (attempt.call_id.clone(), Some(chunks))
            }
        };
        if let Some(chunks) = to_flush {
            self.inner
                .store
                .record_llm_chunks(&call_id, &chunks, self.inner.detailed)
                .await?;
        }
        Ok(())
    }

    fn call_id(&self) -> String {
        self.inner
            .attempt
            .try_lock()
            .map(|attempt| attempt.call_id.clone())
            .unwrap_or_else(|_| self.inner.base_call.call_id.clone())
    }
}

fn elapsed_ms(started: Instant) -> i64 {
    started.elapsed().as_millis().min(i64::MAX as u128) as i64
}

fn finish_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolUse => "tool_use",
    }
}

fn error_kind(error: &crate::Error) -> &'static str {
    match error {
        crate::Error::Provider(_) | crate::Error::Http(_) => "provider",
        crate::Error::Cancelled => "cancelled",
        crate::Error::Database(_) | crate::Error::Store(_) => "store",
        _ => "internal",
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{
        ModelConfigInput, ModelType, NewLlmCall, ProviderType, OPENAI_CHAT_ENDPOINT,
    };

    use super::*;

    #[tokio::test]
    async fn dropping_an_unfinished_call_guard_marks_the_call_cancelled() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("test.db").display()
        ))
        .await
        .unwrap();
        let model = store
            .create_model(&ModelConfigInput {
                sort_order: 0,
                display_name: "Test Model".into(),
                group_name: None,
                model_type: ModelType::OpenAi,
                base_url: "https://example.com/v1/chat/completions".into(),
                use_full_url: true,
                api_key: "test-key".into(),
                tooltip_data: "Test Model".into(),
                model_id: "test-model".into(),
                reasoning_effort: None,
                openai_endpoint: OPENAI_CHAT_ENDPOINT.into(),
                openai_extra_params_enabled: false,
                openai_extra_params: serde_json::json!({}),
                custom_headers_enabled: false,
                custom_headers: serde_json::json!({}),
                anthropic_extra_params_enabled: false,
                anthropic_extra_params: serde_json::json!({}),
                context_window_tokens: None,
                max_completion_tokens: None,
                anthropic_max_tokens: None,
                anthropic_thinking_effort: None,
                thinking_budget_tokens: None,
            })
            .await
            .unwrap();
        let recorder = CallRecorder::start(
            store.clone(),
            NewLlmCall {
                call_id: "cancel-on-drop".into(),
                run_id: "run".into(),
                conversation_id: "conversation".into(),
                provider_call_index: 0,
                model_hash: model.model_hash,
                provider_type: ProviderType::OpenAiChat,
                provider_url: "https://example.com".into(),
                request_type: ProviderType::OpenAiChat,
                request_url: "https://example.com/v1/chat/completions".into(),
                model_id: "test-model".into(),
                display_name: "Test Model".into(),
                reasoning_effort: None,
                fast: false,
                message_count: 1,
                projected_message_count: 1,
                history_fingerprint: "fingerprint".into(),
                tool_count: 0,
                detailed: false,
            },
        )
        .await
        .unwrap();

        drop(recorder.cancel_on_drop());

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let status: String =
                sqlx::query_scalar("SELECT status FROM llm_calls WHERE call_id = ?")
                    .bind("cancel-on-drop")
                    .fetch_one(store.pool())
                    .await
                    .unwrap();
            if status == "cancelled" {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "unfinished recorder stayed running after its stream was dropped"
            );
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn recorder_flush_preserves_chunk_sequence_and_byte_counts() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("test.db").display()
        ))
        .await
        .unwrap();
        store.set_detailed_logging(true).await.unwrap();

        let model = store
            .create_model(&ModelConfigInput {
                sort_order: 0,
                display_name: "Test Model".into(),
                group_name: None,
                model_type: ModelType::OpenAi,
                base_url: "https://example.com/v1/chat/completions".into(),
                use_full_url: true,
                api_key: "test-key".into(),
                tooltip_data: "Test Model".into(),
                model_id: "test-model".into(),
                reasoning_effort: None,
                openai_endpoint: OPENAI_CHAT_ENDPOINT.into(),
                openai_extra_params_enabled: false,
                openai_extra_params: serde_json::json!({}),
                custom_headers_enabled: false,
                custom_headers: serde_json::json!({}),
                anthropic_extra_params_enabled: false,
                anthropic_extra_params: serde_json::json!({}),
                context_window_tokens: None,
                max_completion_tokens: None,
                anthropic_max_tokens: None,
                anthropic_thinking_effort: None,
                thinking_budget_tokens: None,
            })
            .await
            .unwrap();

        let recorder = CallRecorder::start(
            store.clone(),
            NewLlmCall {
                call_id: "detailed-call-1".into(),
                run_id: "run-1".into(),
                conversation_id: "conv-1".into(),
                provider_call_index: 0,
                model_hash: model.model_hash,
                provider_type: ProviderType::OpenAiChat,
                provider_url: "https://example.com".into(),
                request_type: ProviderType::OpenAiChat,
                request_url: "https://example.com/v1/chat/completions".into(),
                model_id: "test-model".into(),
                display_name: "Test Model".into(),
                reasoning_effort: None,
                fast: false,
                message_count: 1,
                projected_message_count: 1,
                history_fingerprint: "fingerprint".into(),
                tool_count: 0,
                detailed: true,
            },
        )
        .await
        .unwrap();

        recorder.response_chunk(b"chunk-0").await.unwrap();
        recorder.response_chunk(b"chunk-1-payload").await.unwrap();
        recorder.completed(FinishReason::Stop).await.unwrap();

        let rows: Vec<(i64, i64, Vec<u8>)> = sqlx::query_as(
            "SELECT seq, byte_count, data FROM llm_call_response_chunks WHERE call_id = ? ORDER BY seq ASC"
        )
        .bind("detailed-call-1")
        .fetch_all(store.pool())
        .await
        .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[0].1, 7);
        assert_eq!(rows[0].2, b"chunk-0");
        assert_eq!(rows[1].0, 1);
        assert_eq!(rows[1].1, 15);
        assert_eq!(rows[1].2, b"chunk-1-payload");

        let call_status: (String, Option<String>, i64, i64) = sqlx::query_as(
            "SELECT status, finish_reason, response_bytes, stream_event_count FROM llm_calls WHERE call_id = ?"
        )
        .bind("detailed-call-1")
        .fetch_one(store.pool())
        .await
        .unwrap();

        assert_eq!(call_status.0, "completed");
        assert_eq!(call_status.1.as_deref(), Some("stop"));
        assert_eq!(call_status.2, 22);
        assert_eq!(call_status.3, 2);
    }
}
