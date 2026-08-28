use cursor_server::{
    client::ClientEvent,
    config::{ProviderConfig, ProviderKind},
    model::{
        ContentPart, ModelInvocation, ModelLatency, ModelRequest, ModelSpec, ProjectedContent,
        ProjectedMessage, PromptSpec, Role, Usage,
    },
    provider::{
        FinishReason, ModelEvent, OpenAiChatProvider, OpenAiResponsesProvider, Provider,
        ProviderStream,
    },
    run::{consume_model_cycle, RunFailure},
};
use futures_util::{stream, StreamExt};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn provider_stream(events: Vec<ModelEvent>) -> ProviderStream {
    Box::pin(stream::iter(events.into_iter().map(Ok)))
}

#[tokio::test]
async fn complete_tool_stream_is_validated_and_projected() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let result = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::ThinkingStart,
            ModelEvent::ThinkingDelta("why".into()),
            ModelEvent::ThinkingEnd,
            ModelEvent::ToolCallStart {
                index: 0,
                call_id: "call".into(),
                name: "Read".into(),
            },
            ModelEvent::ToolCallArgumentsDelta {
                index: 0,
                delta: r#"{"path":"/tmp/a"}"#.into(),
            },
            ModelEvent::ToolCallEnd { index: 0 },
            ModelEvent::Usage(Usage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                ..Usage::default()
            }),
            ModelEvent::Done(FinishReason::ToolUse),
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    drop(sender);

    assert_eq!(result.reasoning, "why");
    assert_eq!(result.calls[0].arguments["path"], "/tmp/a");
    assert_eq!(result.usage.unwrap().input_tokens, Some(10));
    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
        events.push(event);
    }
    assert!(events
        .iter()
        .any(|event| matches!(event, ClientEvent::ThinkingEnd { .. })));
}

#[tokio::test]
async fn eof_and_half_a_tool_call_are_failures_and_keep_only_diagnostics() {
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    let failure = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::ToolCallStart {
                index: 0,
                call_id: "call".into(),
                name: "Read".into(),
            },
            ModelEvent::ToolCallArgumentsDelta {
                index: 0,
                delta: "{".into(),
            },
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(failure.failure, RunFailure::Provider(_)));
}

#[tokio::test]
async fn done_with_open_blocks_and_events_after_done_are_rejected() {
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    let open = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::TextStart,
            ModelEvent::Done(FinishReason::Stop),
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(open.failure, RunFailure::Protocol(_)));

    let after = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::Done(FinishReason::Stop),
            ModelEvent::Usage(Usage::default()),
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(after.failure, RunFailure::Protocol(_)));
}

#[tokio::test]
async fn duplicate_usage_is_rejected_instead_of_guessing_which_total_is_final() {
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    let failure = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::Usage(Usage::default()),
            ModelEvent::Usage(Usage::default()),
            ModelEvent::Done(FinishReason::Stop),
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();

    assert!(matches!(failure.failure, RunFailure::Protocol(_)));
}

#[tokio::test]
async fn duplicate_tool_call_ids_are_rejected_across_distinct_indexes() {
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    let failure = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::ToolCallStart {
                index: 0,
                call_id: "call-1".into(),
                name: "Read".into(),
            },
            ModelEvent::ToolCallEnd { index: 0 },
            ModelEvent::ToolCallStart {
                index: 1,
                call_id: "call-1".into(),
                name: "Read".into(),
            },
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();

    assert!(matches!(failure.failure, RunFailure::Protocol(_)));
}

#[tokio::test]
async fn openai_chat_raw_stream_and_request_projection_match_the_endpoint() {
    let (base_url, mut requests, server) = fixture_server(
        "/v1/chat/completions",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"wh\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"y\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        ),
    )
    .await;
    let provider = OpenAiChatProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiChat, base_url, None),
    );
    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    let body = requests.recv().await.unwrap();
    let continued = continued_invocation(&events);
    let _ = collect(provider.stream(continued, CancellationToken::new())).await;
    let second_body = requests.recv().await.unwrap();
    server.abort();

    assert_eq!(body["messages"][0]["content"], "system");
    assert_eq!(body["messages"][1]["content"][0]["text"], "hello");
    assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
    assert_eq!(
        body["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
    assert!(body.get("max_completion_tokens").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("service_tier").is_none());
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>(),
        "why"
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::Usage(usage)
        if usage.input_tokens == Some(7)
            && usage.output_tokens == Some(2)
            && usage.cache_read_tokens.is_none()
            && usage.cache_write_tokens.is_none()
            && usage.reasoning_tokens.is_none())));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
    assert_array_prefix(&body["messages"], &second_body["messages"]);
    assert_eq!(
        second_body["messages"].as_array().unwrap().last().unwrap()["reasoning_content"],
        "why"
    );
}

#[tokio::test]
async fn openai_chat_done_marker_can_terminate_without_finish_reason() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/chat/completions",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n",
        ),
    )
    .await;
    let provider = OpenAiChatProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiChat, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn openai_chat_accepts_content_filter_finish_reason() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/chat/completions",
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"content_filter\"}]}\n\n",
    )
    .await;
    let provider = OpenAiChatProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiChat, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn openai_chat_buffers_split_tool_metadata() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/chat/completions",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"arguments\":\"{\\\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"Read\",\"arguments\":\"path\\\":\\\"a\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        ),
    )
    .await;
    let provider = OpenAiChatProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiChat, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events.iter().any(|event| matches!(event, ModelEvent::ToolCallStart { call_id, name, .. } if call_id == "call-1" && name == "Read")));
    assert_eq!(
        events.last(),
        Some(&ModelEvent::Done(FinishReason::ToolUse))
    );
}

#[tokio::test]
async fn openai_responses_raw_stream_does_not_invent_reasoning_effort() {
    let (base_url, mut requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"why\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"r1\",\"encrypted_content\":\"opaque-1\"}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"r2\",\"encrypted_content\":\"opaque-2\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "data: {\"type\":\"response.output_text.done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":8,\"output_tokens\":3}}}\n\n",
        ),
    )
    .await;
    let mut request = invocation();
    request.request.model.reasoning.enabled = true;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, Some(4096)),
    );
    let events = collect(provider.stream(request, CancellationToken::new())).await;
    let body = requests.recv().await.unwrap();
    let continued = continued_invocation(&events);
    let _ = collect(provider.stream(continued, CancellationToken::new())).await;
    let second_body = requests.recv().await.unwrap();
    server.abort();

    assert_eq!(body["reasoning"]["summary"], "auto");
    assert!(body["reasoning"].get("effort").is_none());
    assert!(body.get("service_tier").is_none());
    assert_eq!(body["max_output_tokens"], 4096);
    assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
    assert_eq!(body["input"][0]["content"][1]["detail"], "auto");
    assert_eq!(
        body["input"][0]["content"][1]["image_url"],
        "data:image/png;base64,AQID"
    );
    assert!(events.iter().any(|event| matches!(event, ModelEvent::ProviderReplayState(state) if state.provider_kind == "openai_responses")));
    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::Usage(usage)
        if usage.input_tokens == Some(8)
            && usage.output_tokens == Some(3)
            && usage.cache_read_tokens.is_none()
            && usage.cache_write_tokens.is_none()
            && usage.reasoning_tokens.is_none())));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
    assert_array_prefix(&body["input"], &second_body["input"]);
    let replayed = second_body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(replayed, ["opaque-1", "opaque-2"]);
}

#[tokio::test]
async fn openai_responses_streams_openrouter_reasoning_text_events() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"still working\"}\n\n",
            "data: {\"type\":\"response.reasoning_text.done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        ),
    )
    .await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::ThinkingStart)));
    assert!(events.iter().any(
        |event| matches!(event, ModelEvent::ThinkingDelta(delta) if delta == "still working")
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::ThinkingEnd)));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn openai_responses_reasoning_item_done_closes_an_open_summary() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"why\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"r1\",\"encrypted_content\":\"opaque\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        ),
    )
    .await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::ThinkingEnd)));
    assert!(events.iter().any(
        |event| matches!(event, ModelEvent::ProviderReplayState(state)
        if state.value["items"][0]["encrypted_content"] == "opaque")
    ));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn openai_responses_item_done_closes_text_and_tool_arguments() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"Read\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"path\\\":\\\"a\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        ),
    )
    .await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::TextEnd)));
    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::ToolCallEnd { index: 1 })));
    assert_eq!(
        events.last(),
        Some(&ModelEvent::Done(FinishReason::ToolUse))
    );
}

#[tokio::test]
async fn openai_responses_preserves_delta_that_repeats_the_streamed_suffix() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"Shell\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"block_until_ms\\\":300\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"00\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"block_until_ms\\\":30000}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"Shell\",\"arguments\":\"{\\\"block_until_ms\\\":30000}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        ),
    )
    .await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );
    let (sender, _receiver) = tokio::sync::mpsc::channel(32);

    let result = consume_model_cycle(
        provider.stream(invocation(), CancellationToken::new()),
        &sender,
        &CancellationToken::new(),
    )
    .await;
    server.abort();

    assert_eq!(result.unwrap().calls[0].arguments["block_until_ms"], 30000);
}

#[tokio::test]
async fn openai_responses_accepts_empty_arguments_done_and_eof_after_completed_tool() {
    let arguments =
        r#"{"merge":false,"todos":[{"id":"first","content":"First","status":"pending"}]}"#;
    let stream = format!(
        concat!(
            "data: {{\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"TodoWrite\"}}}}\n\n",
            "data: {{\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":{0:?}}}\n\n",
            "data: {{\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"\"}}\n\n",
            "data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"TodoWrite\",\"arguments\":{0:?}}}}}\n\n",
        ),
        arguments,
    );
    let stream = Box::leak(stream.into_boxed_str());
    let (base_url, _requests, server) = fixture_server("/v1/responses", stream).await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );
    let (sender, _receiver) = tokio::sync::mpsc::channel(32);

    let cycle = consume_model_cycle(
        provider.stream(invocation(), CancellationToken::new()),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(cycle.calls[0].name, "TodoWrite");
    assert_eq!(cycle.calls[0].arguments["todos"][0]["content"], "First");
}

#[tokio::test]
async fn openai_responses_completed_snapshot_does_not_reindex_streamed_tool() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"reasoning-1\",\"encrypted_content\":\"opaque\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"Read\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[",
            "{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}",
            "]}}\n\n",
        ),
    )
    .await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );
    let (sender, _receiver) = tokio::sync::mpsc::channel(32);

    let cycle = consume_model_cycle(
        provider.stream(invocation(), CancellationToken::new()),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(cycle.calls.len(), 1);
    assert_eq!(cycle.calls[0].index, 1);
    assert_eq!(cycle.calls[0].call_id, "call-1");
    assert_eq!(cycle.calls[0].arguments["path"], "a");
}

#[tokio::test]
async fn openai_responses_done_marker_accepts_completed_items() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}}\n\n",
            "data: [DONE]\n\n",
        ),
    )
    .await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn openai_chat_fast_is_projected_as_service_tier_fast() {
    let (base_url, mut requests, server) = fixture_server(
        "/v1/chat/completions",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ),
    )
    .await;
    let provider = OpenAiChatProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiChat, base_url, None),
    );
    let mut request = invocation();
    request.request.model.model_id = "GPT-5.6-sol".into();
    request.request.model.latency = ModelLatency::Fast;

    let _ = collect(provider.stream(request, CancellationToken::new())).await;
    let body = requests.recv().await.unwrap();
    server.abort();

    assert_eq!(body["service_tier"], "fast");
    assert_eq!(body["prompt_cache_key"], "cursor-byok");
}

#[tokio::test]
async fn openai_responses_fast_is_projected_as_service_tier_fast() {
    let (base_url, mut requests, server) = fixture_server(
        "/v1/responses",
        concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "data: {\"type\":\"response.output_text.done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        ),
    )
    .await;
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::OpenAiResponses, base_url, None),
    );
    let mut request = invocation();
    request.request.model.model_id = "gpt-5.6-sol".into();
    request.request.model.latency = ModelLatency::Fast;

    let _ = collect(provider.stream(request, CancellationToken::new())).await;
    let body = requests.recv().await.unwrap();
    server.abort();

    assert_eq!(body["service_tier"], "fast");
    assert_eq!(body["prompt_cache_key"], "cursor-byok");
}

#[tokio::test]
async fn anthropic_raw_stream_uses_explicit_and_default_token_limits() {
    let (base_url, mut requests, server) = fixture_server(
        "/v1/messages",
        concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"first\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"signature-1\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"second\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"signature-2\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
            "event: content_block_start\ndata: {\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":2}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        ),
    )
    .await;
    let provider = cursor_server::provider::AnthropicProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::Anthropic, base_url.clone(), Some(1234)),
    );
    let mut request = invocation();
    request.request.model.latency = ModelLatency::Fast;
    let events = collect(provider.stream(request, CancellationToken::new())).await;
    let body = requests.recv().await.unwrap();
    let continued = continued_invocation(&events);
    let _ = collect(provider.stream(continued, CancellationToken::new())).await;
    let second_body = requests.recv().await.unwrap();
    let default_provider = cursor_server::provider::AnthropicProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::Anthropic, base_url, None),
    );
    let _ = collect(default_provider.stream(invocation(), CancellationToken::new())).await;
    let default_body = requests.recv().await.unwrap();
    server.abort();

    assert_eq!(body["max_tokens"], 1234);
    assert_eq!(default_body["max_tokens"], 65_000);
    assert!(body.get("service_tier").is_none());
    assert_eq!(body["messages"][0]["content"][1]["type"], "image");
    assert_eq!(
        body["messages"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(body["messages"][0]["content"][1]["source"]["data"], "AQID");
    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::Usage(usage)
        if usage.input_tokens == Some(9)
            && usage.output_tokens == Some(2)
            && usage.cache_read_tokens.is_none()
            && usage.cache_write_tokens.is_none()
            && usage.reasoning_tokens.is_none())));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
    assert_array_prefix(&body["messages"], &second_body["messages"]);
    let signatures = second_body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter_map(|block| block.get("signature").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(signatures, ["signature-1", "signature-2"]);
}

#[tokio::test]
async fn anthropic_unsigned_thinking_is_displayed_but_not_replayed() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/messages",
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"why\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        ),
    )
    .await;
    let provider = cursor_server::provider::AnthropicProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::Anthropic, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::ThinkingDelta(text) if text == "why")));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ModelEvent::ProviderReplayState(_))));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn anthropic_redacted_thinking_is_preserved_for_replay() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/messages",
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        ),
    )
    .await;
    let provider = cursor_server::provider::AnthropicProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::Anthropic, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events.iter().any(
        |event| matches!(event, ModelEvent::ProviderReplayState(state)
        if state.value["blocks"][0]["type"] == "redacted_thinking"
            && state.value["blocks"][0]["data"] == "opaque")
    ));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn anthropic_message_stop_closes_blocks_and_infers_finish_reason() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/messages",
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        ),
    )
    .await;
    let provider = cursor_server::provider::AnthropicProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::Anthropic, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::TextEnd)));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn anthropic_final_message_delta_can_terminate_without_message_stop() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/messages",
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"refusal\"}}\n\n",
        ),
    )
    .await;
    let provider = cursor_server::provider::AnthropicProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::Anthropic, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn anthropic_accepts_event_type_from_data() {
    let (base_url, _requests, server) = fixture_server(
        "/v1/messages",
        concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ),
    )
    .await;
    let provider = cursor_server::provider::AnthropicProvider::new(
        reqwest::Client::new(),
        config(ProviderKind::Anthropic, base_url, None),
    );

    let events = collect(provider.stream(invocation(), CancellationToken::new())).await;
    server.abort();

    assert!(events
        .iter()
        .any(|event| matches!(event, ModelEvent::TextDelta(text) if text == "ok")));
    assert_eq!(events.last(), Some(&ModelEvent::Done(FinishReason::Stop)));
}

#[tokio::test]
async fn empty_tool_arguments_are_normalized_but_nonempty_invalid_json_is_rejected() {
    let (sender, _receiver) = tokio::sync::mpsc::channel(16);
    let empty = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::ToolCallStart {
                index: 0,
                call_id: "call".into(),
                name: "NoArgs".into(),
            },
            ModelEvent::ToolCallEnd { index: 0 },
            ModelEvent::Done(FinishReason::ToolUse),
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(empty.calls[0].arguments, serde_json::json!({}));

    let invalid = consume_model_cycle(
        provider_stream(vec![
            ModelEvent::Start {
                model_call_id: "model-call".into(),
            },
            ModelEvent::ToolCallStart {
                index: 0,
                call_id: "call".into(),
                name: "Broken".into(),
            },
            ModelEvent::ToolCallArgumentsDelta {
                index: 0,
                delta: "{".into(),
            },
            ModelEvent::ToolCallEnd { index: 0 },
        ]),
        &sender,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(invalid.failure, RunFailure::Protocol(_)));
}

#[tokio::test]
async fn every_provider_can_be_cancelled_while_waiting_for_response_headers() {
    for kind in [
        ProviderKind::OpenAiChat,
        ProviderKind::OpenAiResponses,
        ProviderKind::Anthropic,
    ] {
        let (base_url, accepted, server) = hanging_server().await;
        let config = config(kind.clone(), base_url, Some(1024));
        let provider: Arc<dyn Provider> = match kind {
            ProviderKind::OpenAiChat => {
                Arc::new(OpenAiChatProvider::new(reqwest::Client::new(), config))
            }
            ProviderKind::OpenAiResponses => {
                Arc::new(OpenAiResponsesProvider::new(reqwest::Client::new(), config))
            }
            ProviderKind::Anthropic => Arc::new(cursor_server::provider::AnthropicProvider::new(
                reqwest::Client::new(),
                config,
            )),
        };
        let cancellation = CancellationToken::new();
        let stream = provider.stream(invocation(), cancellation.clone());
        let collect = tokio::spawn(async move { collect(stream).await });
        accepted.await.unwrap();
        cancellation.cancel();
        let events = tokio::time::timeout(Duration::from_secs(1), collect)
            .await
            .expect("provider did not cancel while waiting for headers")
            .unwrap();
        assert!(events.is_empty());
        server.abort();
    }
}

fn invocation() -> ModelInvocation {
    ModelInvocation {
        call_id: "call-1".into(),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        provider_call_index: 0,
        canonical_message_count: 1,
        request: ModelRequest {
            prompt: PromptSpec {
                instructions: "system".into(),
                tools: Vec::new(),
            },
            model: ModelSpec::new("model"),
            history: vec![ProjectedMessage {
                message_id: "user-1".into(),
                role: Role::User,
                content: ProjectedContent::Parts(vec![
                    ContentPart::Text {
                        text: "hello".into(),
                    },
                    ContentPart::Image {
                        mime_type: "image/png".into(),
                        data: vec![1, 2, 3],
                    },
                ]),
            }],
        },
    }
}

fn continued_invocation(events: &[ModelEvent]) -> ModelInvocation {
    let mut invocation = invocation();
    let text = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let thinking = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ThinkingDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let replay_state = events.iter().find_map(|event| match event {
        ModelEvent::ProviderReplayState(state) => Some(state.clone()),
        _ => None,
    });
    invocation.request.history.push(ProjectedMessage {
        message_id: "assistant-1".into(),
        role: Role::Assistant,
        content: ProjectedContent::Assistant {
            text,
            thinking,
            replay_state,
            calls: Vec::new(),
        },
    });
    invocation.call_id = "call-2".into();
    invocation
}

fn assert_array_prefix(first: &Value, second: &Value) {
    let first = first.as_array().unwrap();
    let second = second.as_array().unwrap();
    assert_eq!(first.as_slice(), &second[..first.len()]);
}

fn config(kind: ProviderKind, base_url: String, max_output_tokens: Option<u64>) -> ProviderConfig {
    let path = match kind {
        ProviderKind::OpenAiChat => "/chat/completions",
        ProviderKind::OpenAiResponses => "/responses",
        ProviderKind::Anthropic => "/messages",
    };
    ProviderConfig {
        kind,
        request_url: format!("{base_url}{path}"),
        api_key: "test".into(),
        custom_headers: Default::default(),
        max_output_tokens,
        request_timeout: Duration::from_secs(5),
    }
}

async fn collect(stream: ProviderStream) -> Vec<ModelEvent> {
    stream.map(|event| event.unwrap()).collect::<Vec<_>>().await
}

#[derive(Clone)]
struct FixtureState {
    response: Arc<str>,
    requests: tokio::sync::mpsc::UnboundedSender<Value>,
}

async fn fixture_server(
    path: &'static str,
    response: &'static str,
) -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<Value>,
    tokio::task::JoinHandle<()>,
) {
    async fn endpoint(
        axum::extract::State(state): axum::extract::State<FixtureState>,
        axum::Json(body): axum::Json<Value>,
    ) -> impl axum::response::IntoResponse {
        let _ = state.requests.send(body);
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            state.response.to_string(),
        )
    }

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let app = axum::Router::new()
        .route(path, axum::routing::post(endpoint))
        .with_state(FixtureState {
            response: response.into(),
            requests: sender,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), receiver, server)
}

async fn hanging_server() -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, receiver) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        let _ = accepted.send(());
        std::future::pending::<()>().await;
    });
    (format!("http://{address}/v1"), receiver, server)
}
