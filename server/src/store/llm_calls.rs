use std::str::FromStr;

use sqlx::Row;

use crate::{
    model::{
        ConversationId, LlmCallRequest, LlmCallResponseChunk, LlmCallSummary, LlmCallUsageAnchor,
        NewLlmCall, ProviderType, Usage,
    },
    Result,
};

use super::{now_ms, Store};

impl Store {
    pub async fn detailed_logging(&self) -> Result<bool> {
        let value: String = sqlx::query_scalar(
            "SELECT value_json FROM service_settings WHERE setting_key = 'llm_detailed_logging'",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(serde_json::from_str(&value)?)
    }

    pub async fn set_detailed_logging(&self, enabled: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES ('llm_detailed_logging', ?, ?) ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at_ms = excluded.updated_at_ms",
        )
        .bind(serde_json::to_string(&enabled)?)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn start_llm_call(&self, call: &NewLlmCall) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO llm_calls(
                call_id, run_id, conversation_id, provider_call_index, model_hash,
                provider_type, provider_url, request_type, request_url, model_id, display_name,
                reasoning_effort, fast, status,
                created_at_ms, request_started_at_ms, queue_ms, message_count, tool_count, detailed
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'running', ?, ?, 0, ?, ?, ?)"#,
        )
        .bind(&call.call_id)
        .bind(&call.run_id)
        .bind(&call.conversation_id)
        .bind(call.provider_call_index)
        .bind(&call.model_hash)
        .bind(call.provider_type.as_str())
        .bind(&call.provider_url)
        .bind(call.request_type.as_str())
        .bind(&call.request_url)
        .bind(&call.model_id)
        .bind(&call.display_name)
        .bind(&call.reasoning_effort)
        .bind(call.fast)
        .bind(now)
        .bind(now)
        .bind(call.message_count as i64)
        .bind(call.tool_count as i64)
        .bind(call.detailed)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_llm_request(
        &self,
        call_id: &str,
        headers: &serde_json::Value,
        body: &serde_json::Value,
        detailed: bool,
    ) -> Result<()> {
        let body_json = serde_json::to_string(body)?;
        if detailed {
            sqlx::query("INSERT INTO llm_call_requests(call_id, headers_json, body_json, byte_count) SELECT ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM llm_calls WHERE call_id = ?)")
                .bind(call_id)
                .bind(serde_json::to_string(headers)?)
                .bind(&body_json)
                .bind(body_json.len() as i64)
                .bind(call_id)
                .execute(&self.pool)
                .await?;
        }
        sqlx::query("UPDATE llm_calls SET request_bytes = ? WHERE call_id = ?")
            .bind(body_json.len() as i64)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_llm_response_headers(
        &self,
        call_id: &str,
        elapsed_ms: i64,
        http_status: u16,
    ) -> Result<()> {
        sqlx::query("UPDATE llm_calls SET response_headers_at_ms = ?, ttfb_ms = ?, http_status = ? WHERE call_id = ?")
            .bind(now_ms())
            .bind(elapsed_ms)
            .bind(http_status as i64)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_llm_chunk(
        &self,
        call_id: &str,
        seq: i64,
        elapsed_ms: i64,
        data: &[u8],
        detailed: bool,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        if detailed {
            sqlx::query("INSERT INTO llm_call_response_chunks(call_id, seq, received_offset_ms, data, byte_count) SELECT ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM llm_calls WHERE call_id = ?)")
                .bind(call_id)
                .bind(seq)
                .bind(elapsed_ms)
                .bind(data)
                .bind(data.len() as i64)
                .bind(call_id)
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query("UPDATE llm_calls SET first_event_at_ms = COALESCE(first_event_at_ms, ?), response_bytes = response_bytes + ?, stream_event_count = stream_event_count + 1 WHERE call_id = ?")
            .bind(now_ms())
            .bind(data.len() as i64)
            .bind(call_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn record_llm_first_text(&self, call_id: &str, elapsed_ms: i64) -> Result<()> {
        sqlx::query("UPDATE llm_calls SET first_text_at_ms = COALESCE(first_text_at_ms, ?), ttft_ms = COALESCE(ttft_ms, ?) WHERE call_id = ?")
            .bind(now_ms())
            .bind(elapsed_ms)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_llm_usage(&self, call_id: &str, usage: Usage) -> Result<()> {
        sqlx::query("UPDATE llm_calls SET input_tokens = ?, output_tokens = ?, total_tokens = ?, cache_read_tokens = ?, cache_write_tokens = ?, reasoning_tokens = ?, usage_json = ? WHERE call_id = ?")
            .bind(as_i64(usage.input_tokens))
            .bind(as_i64(usage.output_tokens))
            .bind(as_i64(usage.total_tokens))
            .bind(as_i64(usage.cache_read_tokens))
            .bind(as_i64(usage.cache_write_tokens))
            .bind(as_i64(usage.reasoning_tokens))
            .bind(serde_json::to_string(&usage)?)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish_llm_call(
        &self,
        call_id: &str,
        status: &str,
        finish_reason: Option<&str>,
        elapsed_ms: i64,
        error_kind: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE llm_calls SET status = ?, finish_reason = ?, finished_at_ms = ?, duration_ms = ?, error_kind = ?, error_message = ? WHERE call_id = ? AND status = 'running'")
            .bind(status)
            .bind(finish_reason)
            .bind(now_ms())
            .bind(elapsed_ms)
            .bind(error_kind)
            .bind(error_message)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn llm_calls(&self, limit: i64) -> Result<Vec<LlmCallSummary>> {
        let rows = sqlx::query("SELECT * FROM llm_calls ORDER BY created_at_ms DESC LIMIT ?")
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(summary_from_row).collect()
    }

    pub async fn llm_call(&self, call_id: &str) -> Result<Option<LlmCallSummary>> {
        sqlx::query("SELECT * FROM llm_calls WHERE call_id = ?")
            .bind(call_id)
            .fetch_optional(&self.pool)
            .await?
            .map(summary_from_row)
            .transpose()
    }

    pub(crate) async fn latest_llm_call_usage_anchor(
        &self,
        conversation_id: &ConversationId,
        model_hash: &str,
    ) -> Result<Option<LlmCallUsageAnchor>> {
        let row = sqlx::query(
            r#"SELECT request_type, usage_json, message_count, tool_count
               FROM llm_calls
               WHERE conversation_id = ?
                 AND model_hash = ?
                 AND status = 'completed'
                 AND input_tokens IS NOT NULL
                 AND usage_json IS NOT NULL
               ORDER BY rowid DESC
               LIMIT 1"#,
        )
        .bind(conversation_id.as_str())
        .bind(model_hash)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let message_count =
                usize::try_from(row.try_get::<i64, _>("message_count")?).unwrap_or(usize::MAX);
            let tool_count =
                usize::try_from(row.try_get::<i64, _>("tool_count")?).unwrap_or(usize::MAX);
            Ok(LlmCallUsageAnchor {
                request_type: ProviderType::from_str(row.try_get("request_type")?)?,
                usage: serde_json::from_str(row.try_get("usage_json")?)?,
                message_count,
                tool_count,
            })
        })
        .transpose()
    }

    pub async fn llm_call_request(&self, call_id: &str) -> Result<Option<LlmCallRequest>> {
        let row = sqlx::query(
            "SELECT headers_json, body_json, byte_count FROM llm_call_requests WHERE call_id = ?",
        )
        .bind(call_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(LlmCallRequest {
                headers: serde_json::from_str(row.try_get("headers_json")?)?,
                body: serde_json::from_str(row.try_get("body_json")?)?,
                byte_count: row.try_get("byte_count")?,
            })
        })
        .transpose()
    }

    pub async fn llm_call_chunks(&self, call_id: &str) -> Result<Vec<LlmCallResponseChunk>> {
        let rows = sqlx::query("SELECT seq, received_offset_ms, data, byte_count FROM llm_call_response_chunks WHERE call_id = ? ORDER BY seq")
            .bind(call_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(LlmCallResponseChunk {
                    seq: row.try_get("seq")?,
                    received_offset_ms: row.try_get("received_offset_ms")?,
                    data: String::from_utf8_lossy(&row.try_get::<Vec<u8>, _>("data")?).into_owned(),
                    byte_count: row.try_get("byte_count")?,
                })
            })
            .collect()
    }
}

fn as_i64(value: Option<u64>) -> Option<i64> {
    value.map(|value| value.min(i64::MAX as u64) as i64)
}

fn summary_from_row(row: sqlx::sqlite::SqliteRow) -> Result<LlmCallSummary> {
    let usage = row.try_get::<Option<String>, _>("usage_json")?;
    Ok(LlmCallSummary {
        call_id: row.try_get("call_id")?,
        run_id: row.try_get("run_id")?,
        conversation_id: row.try_get("conversation_id")?,
        provider_call_index: row.try_get("provider_call_index")?,
        model_hash: row.try_get("model_hash")?,
        provider_type: row.try_get("provider_type")?,
        provider_url: row.try_get("provider_url")?,
        request_type: row.try_get("request_type")?,
        request_url: row.try_get("request_url")?,
        model_id: row.try_get("model_id")?,
        display_name: row.try_get("display_name")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        fast: Some(row.try_get("fast")?),
        status: row.try_get("status")?,
        finish_reason: row.try_get("finish_reason")?,
        created_at_ms: row.try_get("created_at_ms")?,
        request_started_at_ms: row.try_get("request_started_at_ms")?,
        response_headers_at_ms: row.try_get("response_headers_at_ms")?,
        first_event_at_ms: row.try_get("first_event_at_ms")?,
        first_text_at_ms: row.try_get("first_text_at_ms")?,
        finished_at_ms: row.try_get("finished_at_ms")?,
        queue_ms: row.try_get("queue_ms")?,
        ttfb_ms: row.try_get("ttfb_ms")?,
        ttft_ms: row.try_get("ttft_ms")?,
        duration_ms: row.try_get("duration_ms")?,
        input_tokens: row.try_get("input_tokens")?,
        output_tokens: row.try_get("output_tokens")?,
        total_tokens: row.try_get("total_tokens")?,
        cache_read_tokens: row.try_get("cache_read_tokens")?,
        cache_write_tokens: row.try_get("cache_write_tokens")?,
        reasoning_tokens: row.try_get("reasoning_tokens")?,
        usage: usage
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        message_count: row.try_get("message_count")?,
        tool_count: row.try_get("tool_count")?,
        request_bytes: row.try_get("request_bytes")?,
        response_bytes: row.try_get("response_bytes")?,
        stream_event_count: row.try_get("stream_event_count")?,
        http_status: row.try_get("http_status")?,
        error_kind: row.try_get("error_kind")?,
        error_message: row.try_get("error_message")?,
        detailed: row.try_get("detailed")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelConfigInput, ModelType};

    #[tokio::test]
    async fn latest_usage_anchor_uses_the_latest_completed_call_for_the_same_conversation_and_model(
    ) {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let model = store
            .create_model(&ModelConfigInput {
                model_id: "model".into(),
                display_name: "Model".into(),
                model_type: ModelType::OpenAi,
                base_url: "https://example.com/v1/responses".into(),
                use_full_url: true,
                api_key: "secret".into(),
                tooltip_data: "Model".into(),
                sort_order: 0,
                reasoning_effort: None,
                openai_endpoint: "/v1/responses".into(),
                openai_extra_params_enabled: false,
                openai_extra_params: serde_json::json!({}),
                custom_headers_enabled: false,
                custom_headers: serde_json::json!({}),
                anthropic_extra_params_enabled: false,
                anthropic_extra_params: serde_json::json!({}),
                context_window_tokens: Some(200_000),
                max_completion_tokens: Some(16_000),
                anthropic_max_tokens: None,
                anthropic_thinking_effort: None,
                thinking_budget_tokens: None,
            })
            .await
            .unwrap();
        let conversation_id = ConversationId::new("conversation");

        for (call_id, status, input_tokens, message_count) in [
            ("completed-old", "completed", 120_000, 10),
            ("failed-newer", "error", 180_000, 11),
            ("completed-latest", "completed", 140_649, 12),
        ] {
            store
                .start_llm_call(&NewLlmCall {
                    call_id: call_id.into(),
                    run_id: format!("run-{call_id}"),
                    conversation_id: conversation_id.to_string(),
                    provider_call_index: 0,
                    model_hash: model.model_hash.clone(),
                    provider_type: model.provider_type(),
                    provider_url: model.base_url.clone(),
                    request_type: model.provider_type(),
                    request_url: model.request_url().unwrap(),
                    model_id: model.model_id.clone(),
                    display_name: model.display_name.clone(),
                    reasoning_effort: None,
                    fast: false,
                    message_count,
                    tool_count: 7,
                    detailed: false,
                })
                .await
                .unwrap();
            store
                .record_llm_usage(
                    call_id,
                    Usage {
                        input_tokens: Some(input_tokens),
                        cache_read_tokens: Some(100_000),
                        ..Usage::default()
                    },
                )
                .await
                .unwrap();
            store
                .finish_llm_call(call_id, status, None, 1, None, None)
                .await
                .unwrap();
        }

        let anchor = store
            .latest_llm_call_usage_anchor(&conversation_id, &model.model_hash)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(anchor.request_type, ProviderType::OpenAiResponses);
        assert_eq!(anchor.usage.input_tokens, Some(140_649));
        assert_eq!(anchor.message_count, 12);
        assert_eq!(anchor.tool_count, 7);
    }
}
