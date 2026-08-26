use std::{
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
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

use super::{FinishReason, ModelEvent};

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
    call_id: String,
    started: Instant,
    detailed: bool,
    next_chunk: AtomicI64,
    chunks: Mutex<ChunkBuffer>,
    first_text_recorded: AtomicBool,
    finished: AtomicBool,
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
                call_id: call.call_id,
                started: Instant::now(),
                detailed: call.detailed,
                next_chunk: AtomicI64::new(0),
                chunks: Mutex::new(ChunkBuffer::default()),
                first_text_recorded: AtomicBool::new(false),
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
        self.inner
            .store
            .record_llm_request(&self.inner.call_id, &headers, body, self.inner.detailed)
            .await?;
        Ok(())
    }

    pub async fn response_headers(&self, status: u16) -> Result<()> {
        self.inner
            .store
            .record_llm_response_headers(&self.inner.call_id, self.elapsed_ms(), status)
            .await
    }

    pub async fn response_chunk(&self, data: &[u8]) -> Result<()> {
        let mut buffer = self.inner.chunks.lock().await;
        if self.is_finished() {
            return Ok(());
        }
        let seq = self.inner.next_chunk.fetch_add(1, Ordering::Relaxed);
        let schedule_flush = if buffer.chunks.is_empty() {
            buffer.generation = buffer.generation.wrapping_add(1);
            buffer.first_chunk_at = Some(Instant::now());
            Some(buffer.generation)
        } else {
            None
        };
        buffer.bytes += data.len();
        buffer.chunks.push(if self.inner.detailed {
            BufferedLlmChunk::new(seq, self.elapsed_ms(), data)
        } else {
            BufferedLlmChunk::metrics(seq, self.elapsed_ms(), data.len())
        });
        let expired = buffer
            .first_chunk_at
            .is_some_and(|started| started.elapsed() >= MAX_BUFFER_AGE);
        if buffer.chunks.len() >= MAX_BUFFERED_CHUNKS
            || buffer.bytes >= MAX_BUFFERED_BYTES
            || expired
        {
            self.flush_locked(&mut buffer).await?;
        }
        drop(buffer);
        if let Some(generation) = schedule_flush {
            let recorder = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(MAX_BUFFER_AGE).await;
                if let Err(error) = recorder.flush_generation(generation).await {
                    tracing::warn!(call_id = recorder.inner.call_id, %error, "failed to flush LLM response chunks");
                }
            });
        }
        Ok(())
    }

    pub async fn event(&self, event: &ModelEvent) -> Result<()> {
        match event {
            ModelEvent::TextDelta(_) => {
                if self
                    .inner
                    .first_text_recorded
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    if let Err(error) = self
                        .inner
                        .store
                        .record_llm_first_text(&self.inner.call_id, self.elapsed_ms())
                        .await
                    {
                        self.inner
                            .first_text_recorded
                            .store(false, Ordering::Release);
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
        self.inner
            .store
            .record_llm_usage(&self.inner.call_id, usage)
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
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Err(error) = self.flush_chunks().await {
            self.inner.finished.store(false, Ordering::Release);
            return Err(error);
        }
        if let Err(error) = self
            .inner
            .store
            .finish_llm_call(
                &self.inner.call_id,
                status,
                reason,
                self.elapsed_ms(),
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

    async fn flush_chunks(&self) -> Result<()> {
        let mut buffer = self.inner.chunks.lock().await;
        self.flush_locked(&mut buffer).await
    }

    async fn flush_generation(&self, generation: u64) -> Result<()> {
        let mut buffer = self.inner.chunks.lock().await;
        if buffer.generation != generation {
            return Ok(());
        }
        self.flush_locked(&mut buffer).await
    }

    async fn flush_locked(&self, buffer: &mut ChunkBuffer) -> Result<()> {
        if buffer.chunks.is_empty() {
            return Ok(());
        }
        let chunks = std::mem::take(&mut buffer.chunks);
        buffer.bytes = 0;
        buffer.first_chunk_at = None;
        if let Err(error) = self
            .inner
            .store
            .record_llm_chunks(&self.inner.call_id, &chunks, self.inner.detailed)
            .await
        {
            buffer.bytes = chunks.iter().map(|chunk| chunk.byte_count).sum();
            buffer.first_chunk_at = Some(Instant::now());
            buffer.chunks = chunks;
            return Err(error);
        }
        Ok(())
    }

    fn elapsed_ms(&self) -> i64 {
        self.inner
            .started
            .elapsed()
            .as_millis()
            .min(i64::MAX as u128) as i64
    }
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
                call_id: call_id.into(),
                started: Instant::now(),
                detailed,
                next_chunk: AtomicI64::new(0),
                chunks: Mutex::new(ChunkBuffer::default()),
                first_text_recorded: AtomicBool::new(false),
                finished: AtomicBool::new(false),
            }),
        }
    }

    #[tokio::test]
    async fn a_partial_chunk_batch_flushes_after_the_deadline() {
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
}
