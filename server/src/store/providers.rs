use std::{collections::HashSet, str::FromStr};

use sqlx::{Row, Sqlite, Transaction};

use crate::{
    model::{
        model_hash, normalize_base_url, ProviderEndpoint, ProviderEndpointInput,
        ProviderEndpointSecret, ProviderModel, ProviderModelInput, ProviderType,
    },
    Error, Result,
};

use super::{now_ms, Store};

impl Store {
    pub async fn create_provider_with_model(
        &self,
        provider: &ProviderEndpointInput,
        model: &ProviderModelInput,
    ) -> Result<(ProviderEndpoint, ProviderModel)> {
        let (provider, mut models) = self
            .create_provider_with_models(provider, std::slice::from_ref(model))
            .await?;
        Ok((provider, models.remove(0)))
    }

    pub async fn create_provider_with_models(
        &self,
        provider: &ProviderEndpointInput,
        models: &[ProviderModelInput],
    ) -> Result<(ProviderEndpoint, Vec<ProviderModel>)> {
        validate_endpoint(provider)?;
        validate_model_batch(models)?;
        let now = now_ms();
        let base_url = normalize_base_url(&provider.base_url)?;
        let mut hashes = Vec::with_capacity(models.len());
        let mut unique_hashes = HashSet::with_capacity(models.len());
        for model in models {
            let hash = model_hash(
                &base_url,
                provider.api_key.as_deref().unwrap_or_default(),
                model.endpoint_type,
                &model.model_id,
            )?;
            if !unique_hashes.insert(hash.clone()) {
                return Err(Error::Config(format!(
                    "8-character model hash collision: {hash}"
                )));
            }
            assert_hash_available(&self.pool, &hash, -1, model.model_id.trim()).await?;
            hashes.push(hash);
        }
        let mut transaction = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO provider_endpoints(name, provider_type, base_url, api_key, custom_headers_json, extra_params_json, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(provider.name.trim())
        .bind(provider.provider_type.as_str())
        .bind(&base_url)
        .bind(provider.api_key.as_deref().unwrap_or_default())
        .bind(serde_json::to_string(&provider.custom_headers)?)
        .bind(serde_json::to_string(&provider.extra_params)?)
        .bind(now).bind(now)
        .execute(&mut *transaction).await?;
        let provider_id = inserted.last_insert_rowid();
        for (model, hash) in models.iter().zip(&hashes) {
            insert_provider_model(&mut transaction, provider_id, hash, model, now).await?;
        }
        transaction.commit().await?;
        let mut saved = Vec::with_capacity(hashes.len());
        for hash in hashes {
            saved.push(self.provider_model(&hash).await?.unwrap());
        }
        Ok((self.provider(provider_id).await?.unwrap().endpoint, saved))
    }

    pub async fn providers(&self) -> Result<Vec<ProviderEndpoint>> {
        let rows = sqlx::query(
            "SELECT provider_id, name, provider_type, base_url, api_key, custom_headers_json, extra_params_json, created_at_ms, updated_at_ms FROM provider_endpoints ORDER BY provider_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(endpoint_from_row).collect()
    }

    pub async fn provider(&self, provider_id: i64) -> Result<Option<ProviderEndpointSecret>> {
        let row = sqlx::query(
            "SELECT provider_id, name, provider_type, base_url, api_key, custom_headers_json, extra_params_json, created_at_ms, updated_at_ms FROM provider_endpoints WHERE provider_id = ?",
        )
        .bind(provider_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(secret_from_row).transpose()
    }

    pub async fn create_provider(&self, input: &ProviderEndpointInput) -> Result<ProviderEndpoint> {
        validate_endpoint(input)?;
        let now = now_ms();
        let base_url = normalize_base_url(&input.base_url)?;
        let result = sqlx::query(
            "INSERT INTO provider_endpoints(name, provider_type, base_url, api_key, custom_headers_json, extra_params_json, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.name.trim())
        .bind(input.provider_type.as_str())
        .bind(base_url)
        .bind(input.api_key.as_deref().unwrap_or_default())
        .bind(serde_json::to_string(&input.custom_headers)?)
        .bind(serde_json::to_string(&input.extra_params)?)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(self
            .provider(result.last_insert_rowid())
            .await?
            .expect("inserted provider must exist")
            .endpoint)
    }

    pub async fn update_provider(
        &self,
        provider_id: i64,
        input: &ProviderEndpointInput,
    ) -> Result<ProviderEndpoint> {
        validate_endpoint(input)?;
        let current = self
            .provider(provider_id)
            .await?
            .ok_or_else(|| Error::RunNotFound(format!("provider {provider_id}")))?;
        let api_key = input
            .api_key
            .as_deref()
            .or(current.endpoint.api_key.as_deref())
            .unwrap_or_default();
        let custom_headers = merge_custom_headers(&current.custom_headers, &input.custom_headers)?;
        let base_url = normalize_base_url(&input.base_url)?;
        let identity_changed = base_url != current.endpoint.base_url
            || api_key != current.endpoint.api_key.as_deref().unwrap_or_default();
        let models = if identity_changed {
            sqlx::query("SELECT * FROM provider_models WHERE provider_id = ?")
                .bind(provider_id)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .map(model_from_row)
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let mut next_hashes = Vec::with_capacity(models.len());
        let mut unique_hashes = HashSet::with_capacity(models.len());
        for model in &models {
            let hash = model_hash(&base_url, api_key, model.endpoint_type, &model.model_id)?;
            if !unique_hashes.insert(hash.clone()) {
                return Err(Error::Config(format!(
                    "8-character model hash collision: {hash}"
                )));
            }
            assert_hash_available(&self.pool, &hash, provider_id, &model.model_id).await?;
            next_hashes.push(hash);
        }
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        if !models.is_empty() {
            sqlx::query(
                "UPDATE llm_calls SET model_hash = NULL WHERE model_hash IN (SELECT model_hash FROM provider_models WHERE provider_id = ?)",
            )
            .bind(provider_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query("DELETE FROM provider_models WHERE provider_id = ?")
                .bind(provider_id)
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query(
            "UPDATE provider_endpoints SET name = ?, provider_type = ?, base_url = ?, api_key = ?, custom_headers_json = ?, extra_params_json = ?, updated_at_ms = ? WHERE provider_id = ?",
        )
        .bind(input.name.trim())
        .bind(input.provider_type.as_str())
        .bind(&base_url)
        .bind(api_key)
        .bind(serde_json::to_string(&custom_headers)?)
        .bind(serde_json::to_string(&input.extra_params)?)
        .bind(now)
        .bind(provider_id)
        .execute(&mut *transaction)
        .await?;
        for (model, hash) in models.iter().zip(next_hashes) {
            sqlx::query(
                r#"INSERT INTO provider_models(
                    model_hash, provider_id, model_id, display_name, endpoint_type, request_url, enabled, sort_order,
                    context_window_tokens, max_output_tokens, reasoning_enabled, reasoning_effort,
                    supports_image_generation, created_at_ms, updated_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(hash)
            .bind(model.provider_id)
            .bind(&model.model_id)
            .bind(&model.display_name)
            .bind(model.endpoint_type.as_str())
            .bind(&model.request_url)
            .bind(model.enabled)
            .bind(model.sort_order)
            .bind(model.context_window_tokens.map(|value| value as i64))
            .bind(model.max_output_tokens.map(|value| value as i64))
            .bind(model.reasoning_enabled)
            .bind(&model.reasoning_effort)
            .bind(model.supports_image_generation)
            .bind(model.created_at_ms)
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(self.provider(provider_id).await?.unwrap().endpoint)
    }

    pub async fn delete_provider(&self, provider_id: i64) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE llm_calls SET model_hash = NULL WHERE model_hash IN (SELECT model_hash FROM provider_models WHERE provider_id = ?)",
        )
        .bind(provider_id)
        .execute(&mut *transaction)
        .await?;
        let result = sqlx::query("DELETE FROM provider_endpoints WHERE provider_id = ?")
            .bind(provider_id)
            .execute(&mut *transaction)
            .await?;
        if result.rows_affected() == 0 {
            transaction.rollback().await?;
            return Err(Error::RunNotFound(format!("provider {provider_id}")));
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn provider_models(&self, enabled_only: bool) -> Result<Vec<ProviderModel>> {
        let query = if enabled_only {
            "SELECT * FROM provider_models WHERE enabled = 1 ORDER BY sort_order, display_name"
        } else {
            "SELECT * FROM provider_models ORDER BY sort_order, display_name"
        };
        let rows = sqlx::query(query).fetch_all(&self.pool).await?;
        rows.into_iter().map(model_from_row).collect()
    }

    pub async fn provider_model(&self, hash: &str) -> Result<Option<ProviderModel>> {
        sqlx::query("SELECT * FROM provider_models WHERE model_hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?
            .map(model_from_row)
            .transpose()
    }

    pub async fn save_provider_model(
        &self,
        provider_id: i64,
        input: &ProviderModelInput,
    ) -> Result<ProviderModel> {
        let mut saved = self
            .save_provider_models(provider_id, std::slice::from_ref(input))
            .await?;
        Ok(saved.remove(0))
    }

    pub async fn save_provider_models(
        &self,
        provider_id: i64,
        inputs: &[ProviderModelInput],
    ) -> Result<Vec<ProviderModel>> {
        validate_model_batch(inputs)?;
        let provider = self
            .provider(provider_id)
            .await?
            .ok_or_else(|| Error::RunNotFound(format!("provider {provider_id}")))?;
        let mut hashes = Vec::with_capacity(inputs.len());
        let mut unique_hashes = HashSet::with_capacity(inputs.len());
        for input in inputs {
            let hash = model_hash(
                &provider.endpoint.base_url,
                provider.endpoint.api_key.as_deref().unwrap_or_default(),
                input.endpoint_type,
                &input.model_id,
            )?;
            if !unique_hashes.insert(hash.clone()) {
                return Err(Error::Config(format!(
                    "8-character model hash collision: {hash}"
                )));
            }
            assert_hash_available(&self.pool, &hash, provider_id, input.model_id.trim()).await?;
            let current_hash = sqlx::query_scalar::<_, String>(
                "SELECT model_hash FROM provider_models WHERE provider_id = ? AND model_id = ?",
            )
            .bind(provider_id)
            .bind(input.model_id.trim())
            .fetch_optional(&self.pool)
            .await?;
            if current_hash
                .as_deref()
                .is_some_and(|current| current != hash)
            {
                return Err(Error::Config(format!(
                    "model {} already exists with a different endpoint type; edit it instead",
                    input.model_id.trim()
                )));
            }
            hashes.push(hash);
        }
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        for (input, hash) in inputs.iter().zip(&hashes) {
            insert_provider_model(&mut transaction, provider_id, hash, input, now).await?;
        }
        transaction.commit().await?;
        let mut saved = Vec::with_capacity(hashes.len());
        for hash in hashes {
            saved.push(self.provider_model(&hash).await?.unwrap());
        }
        Ok(saved)
    }

    pub async fn update_provider_model(
        &self,
        current_hash: &str,
        input: &ProviderModelInput,
    ) -> Result<ProviderModel> {
        validate_model(input)?;
        let current = self
            .provider_model(current_hash)
            .await?
            .ok_or_else(|| Error::RunNotFound(format!("model {current_hash}")))?;
        let provider = self
            .provider(current.provider_id)
            .await?
            .expect("model provider must exist");
        let next_hash = model_hash(
            &provider.endpoint.base_url,
            provider.endpoint.api_key.as_deref().unwrap_or_default(),
            input.endpoint_type,
            &input.model_id,
        )?;
        if next_hash != current_hash {
            assert_hash_available(
                &self.pool,
                &next_hash,
                current.provider_id,
                input.model_id.trim(),
            )
            .await?;
        }
        let mut transaction = self.pool.begin().await?;
        if next_hash != current_hash {
            sqlx::query("UPDATE llm_calls SET model_hash = NULL WHERE model_hash = ?")
                .bind(current_hash)
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query(
            r#"UPDATE provider_models SET model_hash = ?, model_id = ?, display_name = ?, endpoint_type = ?, request_url = ?, enabled = ?,
                sort_order = ?, context_window_tokens = ?, max_output_tokens = ?, reasoning_enabled = ?,
                reasoning_effort = ?, supports_image_generation = ?, updated_at_ms = ?
                WHERE model_hash = ?"#,
        )
        .bind(&next_hash).bind(input.model_id.trim()).bind(input.display_name.trim())
        .bind(input.endpoint_type.as_str()).bind(input.request_url.trim())
        .bind(input.enabled).bind(input.sort_order)
        .bind(input.context_window_tokens.map(|value| value as i64))
        .bind(input.max_output_tokens.map(|value| value as i64))
        .bind(input.reasoning_enabled).bind(&input.reasoning_effort)
        .bind(input.supports_image_generation)
        .bind(now_ms()).bind(current_hash)
        .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(self.provider_model(&next_hash).await?.unwrap())
    }

    pub async fn delete_provider_model(&self, hash: &str) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE llm_calls SET model_hash = NULL WHERE model_hash = ?")
            .bind(hash)
            .execute(&mut *transaction)
            .await?;
        let result = sqlx::query("DELETE FROM provider_models WHERE model_hash = ?")
            .bind(hash)
            .execute(&mut *transaction)
            .await?;
        if result.rows_affected() == 0 {
            transaction.rollback().await?;
            return Err(Error::RunNotFound(format!("model {hash}")));
        }
        transaction.commit().await?;
        Ok(())
    }
}

async fn insert_provider_model(
    transaction: &mut Transaction<'_, Sqlite>,
    provider_id: i64,
    hash: &str,
    input: &ProviderModelInput,
    now: i64,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO provider_models(
            model_hash, provider_id, model_id, display_name, endpoint_type, request_url, enabled, sort_order,
            context_window_tokens, max_output_tokens, reasoning_enabled,
            reasoning_effort, supports_image_generation, created_at_ms, updated_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(provider_id, model_id) DO UPDATE SET
            display_name = excluded.display_name,
            endpoint_type = excluded.endpoint_type,
            request_url = excluded.request_url,
            enabled = excluded.enabled,
            sort_order = excluded.sort_order,
            context_window_tokens = excluded.context_window_tokens,
            max_output_tokens = excluded.max_output_tokens,
            reasoning_enabled = excluded.reasoning_enabled,
            reasoning_effort = excluded.reasoning_effort,
            supports_image_generation = excluded.supports_image_generation,
            updated_at_ms = excluded.updated_at_ms"#,
    )
    .bind(hash)
    .bind(provider_id)
    .bind(input.model_id.trim())
    .bind(input.display_name.trim())
    .bind(input.endpoint_type.as_str())
    .bind(input.request_url.trim())
    .bind(input.enabled)
    .bind(input.sort_order)
    .bind(input.context_window_tokens.map(|value| value as i64))
    .bind(input.max_output_tokens.map(|value| value as i64))
    .bind(input.reasoning_enabled)
    .bind(&input.reasoning_effort)
    .bind(input.supports_image_generation)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn validate_endpoint(input: &ProviderEndpointInput) -> Result<()> {
    if input.name.trim().is_empty() {
        return Err(Error::Config("provider name cannot be empty".into()));
    }
    if !input.custom_headers.is_object() {
        return Err(Error::Config("custom headers must be a JSON object".into()));
    }
    if !input.extra_params.is_object() {
        return Err(Error::Config("extra params must be a JSON object".into()));
    }
    normalize_base_url(&input.base_url)?;
    Ok(())
}

fn validate_model(input: &ProviderModelInput) -> Result<()> {
    if input.display_name.trim().is_empty() {
        return Err(Error::Config("model display name cannot be empty".into()));
    }
    if input.model_id.trim().is_empty() {
        return Err(Error::Config("model id cannot be empty".into()));
    }
    crate::model::resolve_request_url(
        "https://validation.invalid",
        input.endpoint_type,
        &input.request_url,
    )?;
    Ok(())
}

fn validate_model_batch(inputs: &[ProviderModelInput]) -> Result<()> {
    if inputs.is_empty() {
        return Err(Error::Config("at least one model is required".into()));
    }
    let mut model_ids = HashSet::with_capacity(inputs.len());
    for input in inputs {
        validate_model(input)?;
        if !model_ids.insert(input.model_id.trim()) {
            return Err(Error::Config(format!(
                "duplicate model id: {}",
                input.model_id.trim()
            )));
        }
    }
    Ok(())
}

fn endpoint_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProviderEndpoint> {
    let api_key: String = row.try_get("api_key")?;
    let has_api_key = !api_key.is_empty();
    let headers: serde_json::Value = serde_json::from_str(row.try_get("custom_headers_json")?)?;
    let extra_params: serde_json::Value = serde_json::from_str(row.try_get("extra_params_json")?)?;
    Ok(ProviderEndpoint {
        provider_id: row.try_get("provider_id")?,
        name: row.try_get("name")?,
        provider_type: ProviderType::from_str(row.try_get("provider_type")?)?,
        base_url: row.try_get("base_url")?,
        api_key: has_api_key.then_some(api_key),
        has_api_key,
        custom_headers: redact_custom_headers(&headers),
        extra_params,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

fn secret_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProviderEndpointSecret> {
    let custom_headers: serde_json::Value =
        serde_json::from_str(row.try_get("custom_headers_json")?)?;
    Ok(ProviderEndpointSecret {
        endpoint: endpoint_from_row(row)?,
        custom_headers,
    })
}

fn redact_custom_headers(headers: &serde_json::Value) -> serde_json::Value {
    let mut headers = headers.clone();
    if let Some(object) = headers.as_object_mut() {
        for (name, value) in object {
            if crate::model::is_sensitive_header(name) {
                *value = serde_json::Value::Null;
            }
        }
    }
    headers
}

fn merge_custom_headers(
    current: &serde_json::Value,
    update: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut output = update
        .as_object()
        .ok_or_else(|| Error::Config("custom headers must be a JSON object".into()))?
        .clone();
    let current = current
        .as_object()
        .expect("stored custom headers are validated");
    for (name, value) in &mut output {
        if value.is_null() {
            *value = current.get(name).cloned().ok_or_else(|| {
                Error::Config(format!(
                    "custom header {name} has no existing value to retain"
                ))
            })?;
        }
    }
    Ok(serde_json::Value::Object(output))
}

fn model_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProviderModel> {
    Ok(ProviderModel {
        model_hash: row.try_get("model_hash")?,
        provider_id: row.try_get("provider_id")?,
        model_id: row.try_get("model_id")?,
        display_name: row.try_get("display_name")?,
        endpoint_type: ProviderType::from_str(row.try_get("endpoint_type")?)?,
        request_url: row.try_get("request_url")?,
        enabled: row.try_get("enabled")?,
        sort_order: row.try_get("sort_order")?,
        context_window_tokens: row
            .try_get::<Option<i64>, _>("context_window_tokens")?
            .map(|value| value as u64),
        max_output_tokens: row
            .try_get::<Option<i64>, _>("max_output_tokens")?
            .map(|value| value as u64),
        reasoning_enabled: row.try_get("reasoning_enabled")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        supports_image_generation: row.try_get("supports_image_generation")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

async fn assert_hash_available(
    pool: &sqlx::SqlitePool,
    hash: &str,
    provider_id: i64,
    model_id: &str,
) -> Result<()> {
    let existing =
        sqlx::query("SELECT provider_id, model_id FROM provider_models WHERE model_hash = ?")
            .bind(hash)
            .fetch_optional(pool)
            .await?;
    if let Some(row) = existing {
        if row.try_get::<i64, _>("provider_id")? != provider_id
            || row.try_get::<String, _>("model_id")? != model_id
        {
            return Err(Error::Config(format!(
                "8-character model hash collision: {hash}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> ProviderEndpointInput {
        ProviderEndpointInput {
            name: "Test".into(),
            provider_type: ProviderType::OpenAiResponses,
            base_url: "https://example.com/v1".into(),
            api_key: Some("secret".into()),
            custom_headers: serde_json::json!({}),
            extra_params: serde_json::json!({}),
        }
    }

    fn model(id: &str) -> ProviderModelInput {
        ProviderModelInput {
            model_id: id.into(),
            display_name: id.into(),
            endpoint_type: ProviderType::OpenAiResponses,
            request_url: String::new(),
            enabled: true,
            sort_order: 0,
            context_window_tokens: Some(200_000),
            max_output_tokens: Some(16_000),
            reasoning_enabled: true,
            reasoning_effort: Some("high".into()),
            supports_image_generation: false,
        }
    }

    #[tokio::test]
    async fn creates_parent_and_child_then_updates_model_identity() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("models.db").display()
        ))
        .await
        .unwrap();
        let (created_provider, original) = store
            .create_provider_with_model(&provider(), &model("model-a"))
            .await
            .unwrap();
        insert_call(&store, &created_provider, &original).await;
        assert_eq!(created_provider.provider_id, original.provider_id);
        let updated = store
            .update_provider_model(&original.model_hash, &model("model-b"))
            .await
            .unwrap();
        assert_ne!(updated.model_hash, original.model_hash);
        assert!(store
            .provider_model(&original.model_hash)
            .await
            .unwrap()
            .is_none());
        let detached: Option<String> =
            sqlx::query_scalar("SELECT model_hash FROM llm_calls WHERE call_id = ?")
                .bind("call-1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(detached, None);
        assert_eq!(store.provider_models(false).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn creates_multiple_models_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("batch-models.db").display()
        ))
        .await
        .unwrap();
        let inputs = [model("model-a"), model("model-b")];
        let (created_provider, models) = store
            .create_provider_with_models(&provider(), &inputs)
            .await
            .unwrap();
        assert_eq!(models.len(), 2);
        assert!(models
            .iter()
            .all(|model| model.provider_id == created_provider.provider_id));

        let duplicate = [model("duplicate"), model("duplicate")];
        assert!(store
            .create_provider_with_models(&provider(), &duplicate)
            .await
            .is_err());
        assert_eq!(store.providers().await.unwrap().len(), 1);
        assert_eq!(store.provider_models(false).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn allows_same_endpoint_and_model_with_different_api_keys() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("credential-models.db").display()
        ))
        .await
        .unwrap();
        let first_provider = provider();
        let mut second_provider = provider();
        second_provider.name = "Second".into();
        second_provider.api_key = Some("different-secret".into());

        let (_, first_model) = store
            .create_provider_with_model(&first_provider, &model("model-a"))
            .await
            .unwrap();
        let (_, second_model) = store
            .create_provider_with_model(&second_provider, &model("model-a"))
            .await
            .unwrap();

        assert_ne!(first_model.model_hash, second_model.model_hash);
        assert_eq!(store.provider_models(false).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn adds_multiple_models_to_existing_provider_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("existing-batch-models.db").display()
        ))
        .await
        .unwrap();
        let provider = store.create_provider(&provider()).await.unwrap();
        let mut invalid = model("invalid");
        invalid.display_name.clear();
        assert!(store
            .save_provider_models(provider.provider_id, &[model("model-a"), invalid])
            .await
            .is_err());
        assert!(store.provider_models(false).await.unwrap().is_empty());

        let saved = store
            .save_provider_models(provider.provider_id, &[model("model-a"), model("model-b")])
            .await
            .unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(store.provider_models(false).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn updating_provider_identity_rehashes_models_and_preserves_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("provider-update.db").display()
        ))
        .await
        .unwrap();
        let (created_provider, original) = store
            .create_provider_with_model(&provider(), &model("model-a"))
            .await
            .unwrap();
        insert_call(&store, &created_provider, &original).await;

        let mut input = provider();
        input.name = "Updated".into();
        input.provider_type = ProviderType::Anthropic;
        input.base_url = "https://new.example.com/v2".into();
        let updated_provider = store
            .update_provider(created_provider.provider_id, &input)
            .await
            .unwrap();

        assert_eq!(updated_provider.name, "Updated");
        assert_eq!(updated_provider.provider_type, ProviderType::Anthropic);
        assert_eq!(updated_provider.base_url, "https://new.example.com/v2");
        assert!(store
            .provider_model(&original.model_hash)
            .await
            .unwrap()
            .is_none());
        let models = store.provider_models(false).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, original.model_id);
        assert_eq!(models[0].display_name, original.display_name);
        assert_eq!(models[0].endpoint_type, original.endpoint_type);
        assert_eq!(models[0].request_url, original.request_url);
        assert_eq!(models[0].reasoning_enabled, original.reasoning_enabled);
        assert_ne!(models[0].model_hash, original.model_hash);
        assert_eq!(
            models[0].model_hash,
            model_hash(
                &updated_provider.base_url,
                input.api_key.as_deref().unwrap(),
                models[0].endpoint_type,
                &models[0].model_id,
            )
            .unwrap()
        );
        let detached: Option<String> =
            sqlx::query_scalar("SELECT model_hash FROM llm_calls WHERE call_id = ?")
                .bind("call-1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(detached, None);
    }

    #[tokio::test]
    async fn updating_provider_api_key_rehashes_its_models() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("provider-key-update.db").display()
        ))
        .await
        .unwrap();
        let (created_provider, original) = store
            .create_provider_with_model(&provider(), &model("model-a"))
            .await
            .unwrap();
        insert_call(&store, &created_provider, &original).await;

        let mut input = provider();
        input.api_key = Some("different-secret".into());
        store
            .update_provider(created_provider.provider_id, &input)
            .await
            .unwrap();

        assert!(store
            .provider_model(&original.model_hash)
            .await
            .unwrap()
            .is_none());
        let models = store.provider_models(false).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].model_hash,
            model_hash(
                &created_provider.base_url,
                "different-secret",
                models[0].endpoint_type,
                &models[0].model_id,
            )
            .unwrap()
        );
        let detached: Option<String> =
            sqlx::query_scalar("SELECT model_hash FROM llm_calls WHERE call_id = ?")
                .bind("call-1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(detached, None);
    }

    #[tokio::test]
    async fn updating_provider_without_changing_api_key_preserves_model_hashes() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("provider-key-keep.db").display()
        ))
        .await
        .unwrap();
        let (created_provider, original) = store
            .create_provider_with_model(&provider(), &model("model-a"))
            .await
            .unwrap();

        // Editor keeps the configured key: sending it back must not rehash models.
        store
            .update_provider(created_provider.provider_id, &provider())
            .await
            .unwrap();
        assert!(store
            .provider_model(&original.model_hash)
            .await
            .unwrap()
            .is_some());

        // Editor cleared the field: keep the current key, still no rehash.
        let mut without_key = provider();
        without_key.api_key = None;
        store
            .update_provider(created_provider.provider_id, &without_key)
            .await
            .unwrap();
        assert!(store
            .provider_model(&original.model_hash)
            .await
            .unwrap()
            .is_some());
        assert_eq!(store.provider_models(false).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn providers_expose_the_configured_api_key_for_editing() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("provider-key-echo.db").display()
        ))
        .await
        .unwrap();
        let created = store.create_provider(&provider()).await.unwrap();
        assert_eq!(created.api_key.as_deref(), Some("secret"));
        let listed = store.providers().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].api_key.as_deref(), Some("secret"));
        assert!(listed[0].has_api_key);

        let without_key = ProviderEndpointInput {
            api_key: None,
            ..provider()
        };
        let empty = store.create_provider(&without_key).await.unwrap();
        assert_eq!(empty.api_key, None);
        assert!(!empty.has_api_key);
        assert_eq!(store.providers().await.unwrap().len(), 2);
    }

    async fn insert_call(store: &Store, provider: &ProviderEndpoint, model: &ProviderModel) {
        sqlx::query(
            "INSERT INTO llm_calls(call_id, run_id, conversation_id, provider_call_index, model_hash, provider_type, provider_url, request_type, request_url, model_id, display_name, status, created_at_ms, message_count, tool_count, detailed) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("call-1")
        .bind("run-1")
        .bind("conversation-1")
        .bind(0_i64)
        .bind(&model.model_hash)
        .bind("openai-responses")
        .bind(&provider.base_url)
        .bind(model.endpoint_type.as_str())
        .bind(crate::model::resolve_request_url(
            &provider.base_url,
            model.endpoint_type,
            &model.request_url,
        ).unwrap())
        .bind(&model.model_id)
        .bind(&model.display_name)
        .bind("completed")
        .bind(0_i64)
        .bind(0_i64)
        .bind(0_i64)
        .bind(false)
        .execute(store.pool())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn deleting_provider_detaches_call_history_before_cascade() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("models.db").display()
        ))
        .await
        .unwrap();
        let (provider, model) = store
            .create_provider_with_model(&provider(), &model("model-a"))
            .await
            .unwrap();
        insert_call(&store, &provider, &model).await;

        store.delete_provider(provider.provider_id).await.unwrap();

        let detached: Option<String> =
            sqlx::query_scalar("SELECT model_hash FROM llm_calls WHERE call_id = ?")
                .bind("call-1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(detached, None);
        assert!(store
            .provider_model(&model.model_hash)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn deleting_model_detaches_call_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("models.db").display()
        ))
        .await
        .unwrap();
        let (provider, model) = store
            .create_provider_with_model(&provider(), &model("model-a"))
            .await
            .unwrap();
        insert_call(&store, &provider, &model).await;

        store
            .delete_provider_model(&model.model_hash)
            .await
            .unwrap();

        let detached: Option<String> =
            sqlx::query_scalar("SELECT model_hash FROM llm_calls WHERE call_id = ?")
                .bind("call-1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(detached, None);
    }
}
