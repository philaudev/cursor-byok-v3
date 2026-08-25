use cursor_server::{
    model::{
        ConversationId, ModelSpec, NewLlmCall, PreparedRun, PromptSpec, ProviderEndpointInput,
        ProviderModelInput, ProviderType, RunAction, RunId, RunKind, Usage,
    },
    store::{RunStatus, Store},
};

async fn store() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", directory.path().join("test.db").display());
    let store = Store::connect(&url).await.unwrap();
    (directory, store)
}

#[tokio::test]
async fn provider_secret_is_write_only_and_model_hash_is_stable() {
    let (_directory, store) = store().await;
    let provider = store
        .create_provider(&ProviderEndpointInput {
            name: "Local".into(),
            provider_type: ProviderType::OpenAiChat,
            base_url: "https://example.com/v1/".into(),
            api_key: Some("secret".into()),
            custom_headers: serde_json::json!({"x-route":"one", "authorization":"header-secret"}),
            extra_params: serde_json::json!({"temperature":0}),
        })
        .await
        .unwrap();
    assert!(provider.has_api_key);
    assert!(!serde_json::to_string(&provider).unwrap().contains("secret"));
    assert_eq!(
        provider.custom_headers["authorization"],
        serde_json::Value::Null
    );
    let updated = store
        .update_provider(
            provider.provider_id,
            &ProviderEndpointInput {
                name: "Renamed".into(),
                provider_type: provider.provider_type,
                base_url: provider.base_url.clone(),
                api_key: None,
                custom_headers: provider.custom_headers.clone(),
                extra_params: provider.extra_params.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "Renamed");
    assert_eq!(
        store
            .provider(provider.provider_id)
            .await
            .unwrap()
            .unwrap()
            .custom_headers["authorization"],
        "header-secret"
    );

    let model = store
        .save_provider_model(
            provider.provider_id,
            &ProviderModelInput {
                model_id: "model-a".into(),
                display_name: "Model A".into(),
                endpoint_type: ProviderType::OpenAiChat,
                request_url: String::new(),
                enabled: true,
                sort_order: 0,
                context_window_tokens: None,
                max_output_tokens: None,
                reasoning_enabled: false,
                reasoning_effort: None,
                supports_image_generation: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(model.model_hash, "bab5019a");
    assert!(model.supports_image_generation);
}

#[tokio::test]
async fn call_summary_is_always_stored_and_payloads_follow_detailed_setting() {
    let (_directory, store) = store().await;
    let provider = store
        .create_provider(&ProviderEndpointInput {
            name: "Local".into(),
            provider_type: ProviderType::OpenAiChat,
            base_url: "https://example.com/v1".into(),
            api_key: None,
            custom_headers: serde_json::json!({}),
            extra_params: serde_json::json!({}),
        })
        .await
        .unwrap();
    let model = store
        .save_provider_model(
            provider.provider_id,
            &ProviderModelInput {
                model_id: "model-a".into(),
                display_name: "Model A".into(),
                endpoint_type: ProviderType::OpenAiChat,
                request_url: String::new(),
                enabled: true,
                sort_order: 0,
                context_window_tokens: None,
                max_output_tokens: None,
                reasoning_enabled: false,
                reasoning_effort: None,
                supports_image_generation: false,
            },
        )
        .await
        .unwrap();
    let call = NewLlmCall {
        call_id: "call-1".into(),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        provider_call_index: 0,
        model_hash: model.model_hash,
        provider_type: ProviderType::OpenAiChat,
        provider_url: provider.base_url,
        request_type: ProviderType::OpenAiChat,
        request_url: "https://example.com/v1/chat/completions".into(),
        model_id: model.model_id,
        display_name: model.display_name,
        reasoning_effort: Some("high".into()),
        fast: true,
        message_count: 2,
        tool_count: 3,
        detailed: false,
    };
    let conversation_id = ConversationId::new("conversation-1");
    let base_revision_id = store.ensure_conversation(&conversation_id).await.unwrap();
    store
        .claim_run(&PreparedRun {
            run_id: RunId::new("run-1"),
            conversation_id,
            kind: RunKind::Root,
            model: ModelSpec::new(call.model_hash.clone()),
            prompt: PromptSpec {
                instructions: String::new(),
                tools: Vec::new(),
            },
            compaction_prompt: PromptSpec {
                instructions: String::new(),
                tools: Vec::new(),
            },
            initial_messages: Vec::new(),
            action: RunAction::Resume {
                pending_tool_round: None,
            },
            base_revision_id,
        })
        .await
        .unwrap();
    store.start_llm_call(&call).await.unwrap();
    store
        .record_llm_request(
            "call-1",
            &serde_json::json!({}),
            &serde_json::json!({"model":"model-a"}),
            false,
        )
        .await
        .unwrap();
    store
        .record_llm_chunk("call-1", 0, 4, b"data", false)
        .await
        .unwrap();
    store
        .record_llm_usage(
            "call-1",
            Usage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .finish_llm_call("call-1", "completed", Some("stop"), 9, None, None)
        .await
        .unwrap();

    let summary = store.llm_call("call-1").await.unwrap().unwrap();
    assert_eq!(summary.total_tokens, Some(15));
    assert_eq!(summary.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(summary.fast, Some(true));
    assert_eq!(summary.request_bytes, Some(19));
    assert_eq!(summary.response_bytes, 4);
    assert!(store.llm_call_request("call-1").await.unwrap().is_none());
    assert!(store.llm_call_chunks("call-1").await.unwrap().is_empty());

    let abandoned = NewLlmCall {
        call_id: "call-2".into(),
        provider_call_index: 1,
        ..call
    };
    store.start_llm_call(&abandoned).await.unwrap();
    store
        .finish_run(&RunId::new("run-1"), RunStatus::Cancelled, None, None)
        .await
        .unwrap();
    let abandoned = store.llm_call("call-2").await.unwrap().unwrap();
    assert_eq!(abandoned.status, "cancelled");
    assert!(abandoned.finished_at_ms.is_some());
    assert!(abandoned.duration_ms.is_some());
    assert_eq!(
        store.llm_call("call-1").await.unwrap().unwrap().status,
        "completed"
    );
}
