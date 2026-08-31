#[path = "support/fixtures.rs"]
mod fixtures;

use std::collections::BTreeMap;

use cursor_server::{
    cursor::prompting::{Mode, PromptAssets, PromptCompiler},
    model::{project_messages, ProjectedContent},
    model::{
        CanonicalMessage, MessageContent, ModelSpec, Origin, Role, ToolCallContent, ToolDefinition,
        ToolResultContent,
    },
};
use sha2::{Digest, Sha256};

#[test]
fn projecting_an_append_only_context_preserves_the_complete_prefix() {
    let first = vec![fixtures::user("u1", "one")];
    let mut second = first.clone();
    second.push(fixtures::user("u2", "two"));
    let projected_first = project_messages(&first).unwrap();
    let projected_second = project_messages(&second).unwrap();
    assert_eq!(projected_first, projected_second[..projected_first.len()]);
}

#[test]
fn every_tool_result_is_projected_as_string_content() {
    let object = serde_json::json!({"merge": false, "todos": []});
    let messages = vec![
        tool_result("object", object.clone()),
        tool_result("string", serde_json::Value::String("plain text".into())),
    ];
    let projected = project_messages(&messages).unwrap();

    let ProjectedContent::ToolResult(object_result) = &projected[0].content else {
        panic!("expected tool result")
    };
    let object_text = &object_result.content;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(object_text).unwrap(),
        object
    );
    let ProjectedContent::ToolResult(string_result) = &projected[1].content else {
        panic!("expected tool result")
    };
    assert_eq!(string_result.content, "plain text");
}

#[test]
fn projected_tool_result_prefixes_remain_stable() {
    let first = vec![named_tool_result("Grep", &"x".repeat(64 * 1024))];
    let mut second = first.clone();
    second.push(fixtures::user("u2", "continue"));

    let projected_first = project_messages(&first).unwrap();
    let projected_second = project_messages(&second).unwrap();

    assert_eq!(projected_first, projected_second[..projected_first.len()]);
}

#[test]
fn unbounded_tool_results_are_not_rewritten() {
    let original = "x".repeat(64 * 1024);
    let projected = project_messages(&[named_tool_result("Delete", &original)]).unwrap();
    let ProjectedContent::ToolResult(result) = &projected[0].content else {
        panic!("expected tool result")
    };
    assert_eq!(result.content, original);
}

#[test]
fn assistant_text_and_thinking_remain_separate_during_projection() {
    let messages = vec![CanonicalMessage {
        message_id: "assistant".into(),
        role: Role::Assistant,
        origin: Origin::Assistant,
        content: MessageContent::Assistant {
            text: "visible answer".into(),
            thinking: "private reasoning".into(),
            tool_round_id: Some("round".into()),
            replay_state: None,
            tool_calls: Vec::new(),
        },
        runtime_event_id: None,
    }];

    let projected = project_messages(&messages).unwrap();
    let ProjectedContent::Assistant { text, thinking, .. } = &projected[0].content else {
        panic!("expected assistant")
    };
    assert_eq!(text, "visible answer");
    assert_eq!(thinking, "private reasoning");
}

#[test]
fn split_tool_pairs_reconstruct_the_original_provider_assistant_message() {
    let messages = vec![
        assistant_tool_pair(
            "assistant-second",
            "model-call",
            1,
            "call-second",
            "visible answer",
            "complete reasoning",
        ),
        tool_result_with_call("result-second", "call-second", "second"),
        assistant_tool_pair("assistant-first", "model-call", 0, "call-first", "", ""),
        tool_result_with_call("result-first", "call-first", "first"),
    ];

    let projected = project_messages(&messages).unwrap();

    assert_eq!(projected.len(), 3);
    assert_eq!(projected[0].role, Role::Assistant);
    let ProjectedContent::Assistant {
        thinking, calls, ..
    } = &projected[0].content
    else {
        panic!("expected assistant")
    };
    assert_eq!(thinking, "complete reasoning");
    assert_eq!(calls[0].call_id, "call-first");
    assert_eq!(calls[1].call_id, "call-second");
    let ProjectedContent::ToolResult(second) = &projected[1].content else {
        panic!("expected tool result")
    };
    let ProjectedContent::ToolResult(first) = &projected[2].content else {
        panic!("expected tool result")
    };
    assert_eq!(second.call_id, "call-second");
    assert_eq!(first.call_id, "call-first");
}

#[test]
fn every_prompt_mode_loads_the_captured_tool_set() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    assert_eq!(assets.mode(Mode::Agent).tools.len(), 26);
    assert_eq!(
        assets
            .mode(Mode::Agent)
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Shell",
            "Grep",
            "Delete",
            "WebSearch",
            "WebFetch",
            "GenerateImage",
            "InspectChanges",
            "EditNotebook",
            "TodoWrite",
            "StrReplace",
            "Write",
            "Read",
            "ReadLints",
            "Glob",
            "Ls",
            "AskQuestion",
            "Task",
            "AwaitShell",
            "GetMcpTools",
            "FetchMcpResource",
            "SwitchMode",
            "CallMcpTool",
            "SembleSearch",
            "SembleFindRelated",
            "CreatePlan",
            "UpdateCurrentStep",
        ]
    );
    assert_mode(
        &assets,
        Mode::Ask,
        &[
            "AskQuestion",
            "CallMcpTool",
            "Delete",
            "FetchMcpResource",
            "InspectChanges",
            "Glob",
            "Ls",
            "Grep",
            "Read",
            "ReadLints",
            "Shell",
            "StrReplace",
            "Task",
            "TodoWrite",
            "WebFetch",
            "WebSearch",
            "Write",
            "SembleSearch",
            "SembleFindRelated",
        ],
        "10fc1e912315761b00298eacafabf734cae4966b1e83d3706f8d5c07b392becf",
    );
    assert_mode(
        &assets,
        Mode::Plan,
        &[
            "Shell",
            "Glob",
            "Ls",
            "Grep",
            "Read",
            "InspectChanges",
            "Write",
            "StrReplace",
            "Delete",
            "TodoWrite",
            "ReadLints",
            "WebSearch",
            "WebFetch",
            "AskQuestion",
            "CreatePlan",
            "Task",
            "FetchMcpResource",
            "CallMcpTool",
            "SwitchMode",
            "SembleSearch",
            "SembleFindRelated",
        ],
        "edb505745a12e0892c22990135312940ba0f64b7af40f7220492b16bbf947aac",
    );
    assert_mode(
        &assets,
        Mode::Debug,
        &[
            "AskQuestion",
            "CallMcpTool",
            "Delete",
            "FetchMcpResource",
            "InspectChanges",
            "Glob",
            "Ls",
            "Grep",
            "Read",
            "ReadLints",
            "Shell",
            "StrReplace",
            "Task",
            "TodoWrite",
            "WebFetch",
            "WebSearch",
            "Write",
            "SembleSearch",
            "SembleFindRelated",
        ],
        "10fc1e912315761b00298eacafabf734cae4966b1e83d3706f8d5c07b392becf",
    );
    assert_mode(
        &assets,
        Mode::Multitask,
        &[
            "AskQuestion",
            "CallMcpTool",
            "Delete",
            "FetchMcpResource",
            "InspectChanges",
            "Glob",
            "Ls",
            "Grep",
            "Read",
            "ReadLints",
            "Shell",
            "StrReplace",
            "SwitchMode",
            "Task",
            "TodoWrite",
            "WebFetch",
            "WebSearch",
            "Write",
            "GenerateImage",
            "SembleSearch",
            "SembleFindRelated",
        ],
        "5c39beb609b3298f81338907a3e0ce92148979c10b7bf757295e0863d6bec993",
    );
    assert_mode(
        &assets,
        Mode::Subagent,
        &[
            "Shell",
            "Grep",
            "Delete",
            "WebSearch",
            "WebFetch",
            "GenerateImage",
            "InspectChanges",
            "ReadLints",
            "EditNotebook",
            "TodoWrite",
            "StrReplace",
            "Write",
            "Read",
            "Glob",
            "Ls",
            "AwaitShell",
            "GetMcpTools",
            "FetchMcpResource",
            "SwitchMode",
            "UpdateCurrentStep",
            "CallMcpTool",
            "SembleSearch",
            "SembleFindRelated",
        ],
        "8e9f4a013b9ecc90643b8ef2ab6aaca082412722fdf54017af592be1b37a2fb5",
    );
    assert_mode(
        &assets,
        Mode::Compaction,
        &[],
        "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
    );
    assert_eq!(
        schema_digest(&assets.mode(Mode::Agent).tools),
        "2958862c44387b3cba98cbc3fb2e9e7dbd9b6ae55389424c5168b365640e0fd4"
    );
    let task = assets
        .mode(Mode::Agent)
        .tools
        .iter()
        .find(|tool| tool.name == "Task")
        .unwrap();
    assert!(task.description.contains("SUBAGENT & DELEGATION POLICY"));
    assert!(task.description.contains("Launch up to 2-3 concurrent subagents"));
    assert!(!task
        .description
        .chars()
        .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character)));
    let shell = assets
        .mode(Mode::Agent)
        .tools
        .iter()
        .find(|tool| tool.name == "Shell")
        .unwrap();
    assert!(
        shell.parameters["properties"]["block_until_ms"]["description"]
            .as_str()
            .unwrap()
            .contains("do not combine it with `nohup`, `&`, `disown`")
    );
    for mode in [
        Mode::Agent,
        Mode::Ask,
        Mode::Debug,
        Mode::Multitask,
        Mode::Subagent,
        Mode::Compaction,
    ] {
        assert!(!assets
            .mode(mode)
            .tools
            .iter()
            .any(|tool| tool.name == "PatchEdit"));
    }
}

#[test]
fn every_captured_mode_owns_and_renders_its_runtime_template() {
    let compiler = PromptCompiler::new(
        PromptAssets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("prompt/cursor")
                .as_path(),
        )
        .unwrap(),
    );
    let values = BTreeMap::from([
        ("OPEN_FILES", String::new()),
        ("SELECTED_CONTEXT", String::new()),
        ("ACTION_CONTEXT", String::new()),
        ("TIMESTAMP", "Sunday, Aug 16, 2026, 11:31 PM (UTC+8)".into()),
        ("USER_QUERY", "question".into()),
        ("DEBUG_SERVER_ENDPOINT", "http://debug".into()),
        ("DEBUG_LOG_PATH", "/tmp/debug.log".into()),
        ("DEBUG_SESSION_ID", "session".into()),
    ]);
    for (mode, marker) in [
        (Mode::Agent, "You are still in **Agent Mode**"),
        (Mode::Ask, "Ask mode is active."),
        (Mode::Plan, "You are still in **Plan Mode**"),
        (Mode::Debug, "You are now in **DEBUG MODE**"),
        (Mode::Multitask, "The user has engaged **Multitask Mode**"),
    ] {
        let rendered = compiler.runtime_message(mode, &values).unwrap();
        assert!(rendered.contains(marker), "missing {mode:?} marker");
        assert!(rendered.contains("<user_query>") && rendered.contains("question") && rendered.contains("</user_query>"));
        assert_eq!(rendered.matches("<user_query>").count(), 1);
    }
}

fn assert_mode(assets: &PromptAssets, mode: Mode, expected: &[&str], digest: &str) {
    assert_eq!(
        assets
            .mode(mode)
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(schema_digest(&assets.mode(mode).tools), digest);
}

fn schema_digest(tools: &[ToolDefinition]) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(tools).unwrap()))
}

#[test]
fn dynamic_mcp_tools_are_appended_after_the_stable_mode_tool_prefix() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let base = compiler
        .prompt_spec(Mode::Agent, &ModelSpec::new("model"), &[], false)
        .unwrap();
    let dynamic = compiler
        .prompt_spec(
            Mode::Agent,
            &ModelSpec::new("model"),
            &[ToolDefinition {
                name: "mcp_repo_lookup".into(),
                description: "lookup".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            false,
        )
        .unwrap();
    assert_eq!(base.tools, dynamic.tools[..base.tools.len()]);
    assert_eq!(dynamic.tools.last().unwrap().name, "mcp_repo_lookup");
}

#[test]
fn dynamic_mcp_tool_cannot_replace_a_mode_tool() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let error = compiler
        .prompt_spec(
            Mode::Agent,
            &ModelSpec::new("model"),
            &[ToolDefinition {
                name: "Read".into(),
                description: "replacement".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            false,
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("dynamic MCP tool conflicts with a mode tool: Read"));
}

#[test]
fn image_generation_capability_controls_only_the_generate_image_definition() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let without = compiler
        .prompt_spec(Mode::Agent, &ModelSpec::new("model"), &[], false)
        .unwrap();
    let mut model = ModelSpec::new("model");
    model.supports_image_generation = true;
    let with = compiler
        .prompt_spec(Mode::Agent, &model, &[], false)
        .unwrap();

    assert!(!without
        .tools
        .iter()
        .any(|tool| tool.name == "GenerateImage"));
    assert!(with.tools.iter().any(|tool| tool.name == "GenerateImage"));
    assert_eq!(with.tools.len(), without.tools.len() + 1);
}

#[test]
fn agent_system_prompt_is_static_and_substitutes_the_model_name() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let mut model = ModelSpec::new("test-model-hash");
    model.display_name = Some("Test Model".into());
    let request = compiler
        .prompt_spec(Mode::Agent, &model, &[], false)
        .unwrap();
    let prompt = &request.instructions;
    assert!(prompt.contains("powered by Test Model"));
    assert!(!prompt.contains("test-model-hash"));
    assert!(!prompt.contains("{{FAKE_MODEL_NAME}}"));
    assert!(!prompt.contains("<user_info>"));
}

#[test]
fn subagent_uses_the_subagent_prompt_and_only_the_captured_tool_delta() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let subagent_prompt = compiler
        .prompt_spec(Mode::Subagent, &ModelSpec::new("model"), &[], false)
        .unwrap();
    assert!(subagent_prompt
        .instructions
        .contains("<handoff_return_contract>"));

    let request = compiler
        .prompt_spec(Mode::Subagent, &ModelSpec::new("model"), &[], false)
        .unwrap();
    assert_eq!(
        request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Shell",
            "Grep",
            "Delete",
            "WebSearch",
            "WebFetch",
            "InspectChanges",
            "ReadLints",
            "EditNotebook",
            "TodoWrite",
            "StrReplace",
            "Write",
            "Read",
            "Glob",
            "Ls",
            "AwaitShell",
            "GetMcpTools",
            "FetchMcpResource",
            "SwitchMode",
            "UpdateCurrentStep",
            "CallMcpTool",
            "SembleSearch",
            "SembleFindRelated",
        ]
    );
    assert!(!request.tools.iter().any(|tool| tool.name == "Task"));

    let suppressed = compiler
        .prompt_spec(Mode::Subagent, &ModelSpec::new("model"), &[], true)
        .unwrap();
    assert!(!suppressed
        .tools
        .iter()
        .any(|tool| tool.name == "UpdateCurrentStep"));
}

fn tool_result(id: &str, output: serde_json::Value) -> CanonicalMessage {
    tool_result_with_call(id, &format!("call-{id}"), output)
}

fn tool_result_with_call(
    id: &str,
    call_id: &str,
    output: impl Into<serde_json::Value>,
) -> CanonicalMessage {
    let output = output.into();
    CanonicalMessage {
        message_id: id.into(),
        role: Role::Tool,
        origin: Origin::Tool,
        content: MessageContent::ToolResult(ToolResultContent {
            call_id: call_id.into(),
            name: "Tool".into(),
            content: output
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| output.to_string()),
            is_error: false,
            image: None,
            provider_parts: Vec::new(),
        }),
        runtime_event_id: None,
    }
}

fn named_tool_result(name: &str, output: &str) -> CanonicalMessage {
    CanonicalMessage {
        message_id: format!("result-{name}"),
        role: Role::Tool,
        origin: Origin::Tool,
        content: MessageContent::ToolResult(ToolResultContent {
            call_id: format!("call-{name}"),
            name: name.into(),
            content: output.into(),
            is_error: false,
            image: None,
            provider_parts: Vec::new(),
        }),
        runtime_event_id: None,
    }
}

fn assistant_tool_pair(
    id: &str,
    tool_round_id: &str,
    index: usize,
    call_id: &str,
    text: &str,
    thinking: &str,
) -> CanonicalMessage {
    CanonicalMessage {
        message_id: id.into(),
        role: Role::Assistant,
        origin: Origin::Assistant,
        content: MessageContent::Assistant {
            text: text.into(),
            thinking: thinking.into(),
            tool_round_id: Some(tool_round_id.into()),
            replay_state: None,
            tool_calls: vec![ToolCallContent {
                index,
                call_id: call_id.into(),
                name: "Tool".into(),
                arguments: serde_json::json!({}),
            }],
        },
        runtime_event_id: None,
    }
}
