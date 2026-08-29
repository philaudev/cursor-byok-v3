use std::{
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering},
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

struct Inner {
    store: Store,
    base_call: NewLlmCall,
    detailed: bool,
    attempt: Mutex<AttemptState>,
    next_attempt: AtomicU32,
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
                next_attempt: AtomicU32::new(0),
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

    pub async fn request(
        &self,
        headers: serde_json::Value,
        body: &serde_json::Value,
    ) -> Result<()> {
        let attempt = self.inner.attempt.lock().await;
        self.inner
            .store
            .record_llm_request(&attempt.call_id, &headers, body, self.inner.detailed)
            .await?;
        Ok(())
    }

    pub async fn response_headers(&self, status: u16) -> Result<()> {
        let attempt = self.inner.attempt.lock().await;
        self.inner
            .store
            .record_llm_response_headers(&attempt.call_id, elapsed_ms(attempt.started), status)
            .await
    }

    pub async fn response_chunk(&self, data: &[u8]) -> Result<()> {
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
        if attempt.chunks.chunks.len() >= MAX_BUFFERED_CHUNKS
            || attempt.chunks.bytes >= MAX_BUFFERED_BYTES
            || expired
        {
            self.flush_locked(&mut attempt).await?;
        }
        drop(attempt);
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
        let attempt = self.inner.attempt.lock().await;
        if is_valid_response_event(event)
            && attempt
                .first_valid_response_recorded
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            if let Err(error) = self
                .inner
                .store
                .record_llm_first_valid_response(&attempt.call_id, elapsed_ms(attempt.started))
                .await
            {
                attempt
                    .first_valid_response_recorded
                    .store(false, Ordering::Release);
                return Err(error);
            }
        }
        drop(attempt);

        match event {
            ModelEvent::TextDelta(delta) if !delta.trim().is_empty() => {
                let attempt = self.inner.attempt.lock().await;
                if attempt
                    .first_text_recorded
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    if let Err(error) = self
                        .inner
                        .store
                        .record_llm_first_text(&attempt.call_id, elapsed_ms(attempt.started))
                        .await
                    {
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
        let attempt = self.inner.attempt.lock().await;
        self.inner
            .store
            .record_llm_usage(&attempt.call_id, usage)
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

    pub async fn retry(
        &self,
        error: &crate::Error,
        headers: serde_json::Value,
        body: &serde_json::Value,
    ) -> Result<()> {
        self.failed(error).await?;

        let attempt_number = self.inner.next_attempt.fetch_add(1, Ordering::Relaxed) + 1;
        let mut call = self.inner.base_call.clone();
        call.call_id = format!("{}:retry-{attempt_number}", self.inner.base_call.call_id);
        self.inner.store.start_llm_call(&call).await?;

        {
            let mut attempt = self.inner.attempt.lock().await;
            *attempt = AttemptState::new(call.call_id);
            self.inner.finished.store(false, Ordering::Release);
        }

        if let Err(error) = self.request(headers, body).await {
            self.failed(&error).await?;
            return Err(error);
        }
        Ok(())
    }

    async fn finish(
        &self,
        status: &str,
        reason: Option<&str>,
        error_kind: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut attempt = self.inner.attempt.lock().await;
        if let Err(error) = self.flush_locked(&mut attempt).await {
            self.inner.finished.store(false, Ordering::Release);
            return Err(error);
        }
        if let Err(error) = self
            .inner
            .store
            .finish_llm_call(
                &attempt.call_id,
                status,
                reason,
                elapsed_ms(attempt.started),
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
        let mut attempt = self.inner.attempt.lock().await;
        if attempt.chunks.generation != generation {
            return Ok(());
        }
        self.flush_locked(&mut attempt).await
    }

    async fn flush_locked(&self, attempt: &mut AttemptState) -> Result<()> {
        let buffer = &mut attempt.chunks;
        if buffer.chunks.is_empty() {
            return Ok(());
        }
        let chunks = std::mem::take(&mut buffer.chunks);
        buffer.bytes = 0;
        buffer.first_chunk_at = None;
        if let Err(error) = self
            .inner
            .store
            .record_llm_chunks(&attempt.call_id, &chunks, self.inner.detailed)
            .await
        {
            buffer.bytes = chunks.iter().map(|chunk| chunk.byte_count).sum();
            buffer.first_chunk_at = Some(Instant::now());
            buffer.chunks = chunks;
            return Err(error);
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
    use super::*;

    async fn test_recorder(store: &Store, call_id: &str, detailed: bool) -> CallRecorder {
        let call = NewLlmCall {
            call_id: call_id.into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
            provider_call_index: 0,
            model_hash: "hash".into(),
            provider_type: crate::model::ProviderType::OpenAiChat,
            provider_url: "https://example.com".into(),
            request_type: crate::model::ProviderType::OpenAiChat,
            request_url: "https://example.com".into(),
            model_id: "model".into(),
            display_name: "Model".into(),
            reasoning_effort: None,
            fast: false,
            message_count: 0,
            tool_count: 0,
            detailed,
        };
        sqlx::query(
            "INSERT INTO model_configs(
                model_hash, display_name, model_type, base_url, api_key,
                tooltip_data, model_id, created_at_ms, updated_at_ms
             ) VALUES ('hash', 'Model', 'openai', 'https://example.com',
                'key', 'Model', 'model', 1, 1)",
        )
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO llm_calls(
                call_id, run_id, conversation_id, provider_call_index, provider_type,
                provider_url, request_type, request_url, model_id, display_name, status,
                created_at_ms, message_count, tool_count, detailed
             ) VALUES (?, 'run', 'conversation', 0, 'openai-chat',
                'https://example.com', 'openai-chat', 'https://example.com',
                'model', 'Model', 'running', 1, 0, 0, ?)",
        )
        .bind(call_id)
        .bind(detailed)
        .execute(store.pool())
        .await
        .unwrap();
        CallRecorder {
            inner: Arc::new(Inner {
                store: store.clone(),
                base_call: call.clone(),
                detailed,
                attempt: Mutex::new(AttemptState::new(call_id.into())),
                next_attempt: AtomicU32::new(0),
                next_generation: AtomicU64::new(0),
                finished: AtomicBool::new(false),
            }),
        }
    }

    #[tokio::test]
    async fn a_partial_chunk_flushes_after_the_deadline() {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let recorder = test_recorder(&store, "timed-flush-call", true).await;

        recorder.response_chunk(b"chunk").await.unwrap();
        assert_eq!(
            store
                .llm_call("timed-flush-call")
                .await
                .unwrap()
                .unwrap()
                .stream_event_count,
            0
        );

        tokio::time::sleep(MAX_BUFFER_AGE + std::time::Duration::from_millis(100)).await;

        let call = store.llm_call("timed-flush-call").await.unwrap().unwrap();
        assert_eq!(call.response_bytes, 5);
        assert_eq!(call.stream_event_count, 1);
        assert_eq!(
            store
                .llm_call_chunks("timed-flush-call")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn first_text_is_persisted_only_once() {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let recorder = test_recorder(&store, "first-text-call", false).await;
        sqlx::query("CREATE TABLE first_text_updates(count INTEGER NOT NULL)")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO first_text_updates(count) VALUES (0)")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER count_first_text_updates
             AFTER UPDATE OF first_text_at_ms ON llm_calls
             BEGIN
                 UPDATE first_text_updates SET count = count + 1;
             END",
        )
        .execute(store.pool())
        .await
        .unwrap();
        for text in ["one", "two", "three"] {
            recorder
                .event(&ModelEvent::TextDelta(text.into()))
                .await
                .unwrap();
        }

        let count: i64 = sqlx::query_scalar("SELECT count FROM first_text_updates")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn first_valid_response_includes_empty_text_and_reasoning_events() {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let recorder = test_recorder(&store, "first-valid-response-call", false).await;

        recorder
            .event(&ModelEvent::Start {
                model_call_id: "call".into(),
            })
            .await
            .unwrap();
        recorder.event(&ModelEvent::TextStart).await.unwrap();
        recorder
            .event(&ModelEvent::TextDelta(String::new()))
            .await
            .unwrap();

        let call = store
            .llm_call("first-valid-response-call")
            .await
            .unwrap()
            .unwrap();
        assert!(call.ttfr_ms.is_some());
        assert!(call.ttft_ms.is_none());

        recorder
            .event(&ModelEvent::TextDelta("text".into()))
            .await
            .unwrap();
        let call = store
            .llm_call("first-valid-response-call")
            .await
            .unwrap()
            .unwrap();
        assert!(call.ttft_ms.is_some());

        recorder
            .event(&ModelEvent::ThinkingDelta("reasoning".into()))
            .await
            .unwrap();
        let call = store
            .llm_call("first-valid-response-call")
            .await
            .unwrap()
            .unwrap();
        assert!(call.first_valid_response_at_ms.is_some());
    }

    #[tokio::test]
    async fn retry_finishes_the_old_call_and_records_the_new_request() {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let recorder = test_recorder(&store, "retry-call", true).await;
        let error = crate::Error::Provider("OpenAI Chat 429: rate limited".into());
        let body = serde_json::json!({"model": "model", "stream": true});

        recorder
            .retry(
                &error,
                serde_json::json!({"content-type": "application/json"}),
                &body,
            )
            .await
            .unwrap();

        let old = store.llm_call("retry-call").await.unwrap().unwrap();
        assert_eq!(old.status, "error");
        assert_eq!(
            old.error_message.as_deref(),
            Some(error.to_string().as_str())
        );

        let new = store.llm_call("retry-call:retry-1").await.unwrap().unwrap();
        assert_eq!(new.status, "running");
        assert_eq!(new.request_bytes, Some(31));
        assert!(store
            .llm_call_request("retry-call:retry-1")
            .await
            .unwrap()
            .is_some());

        recorder.completed(FinishReason::Stop).await.unwrap();
        assert_eq!(
            store
                .llm_call("retry-call:retry-1")
                .await
                .unwrap()
                .unwrap()
                .status,
            "completed"
        );
    }
}
