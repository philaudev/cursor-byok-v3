//! Decides when to compact context and builds a stable fallback summary.

use std::collections::HashSet;

use crate::model::{
    CanonicalMessage, LlmCallUsageAnchor, PreparedRun, ProjectedMessage, RunAction,
};

const FALLBACK_CHARS: usize = 12_000;
pub(super) const COMPACTION_RESERVE_TOKENS: u64 = 10_000;

pub(super) const OUTPUT_TOKENS: u64 = 4_096;
pub(super) const INSTRUCTIONS: &str = "Summarize the conversation for the next model turn. Preserve goals, constraints, decisions, files, commands, errors, results, and unfinished work. Do not call tools. Return only the concise durable summary.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ContextUsageAnchor {
    input_tokens: u64,
    message_count: usize,
    tool_count: usize,
}

impl ContextUsageAnchor {
    pub(super) fn from_llm_call(anchor: LlmCallUsageAnchor) -> Option<Self> {
        Some(Self {
            input_tokens: anchor.usage.context_input_tokens(anchor.request_type)?,
            message_count: anchor.message_count,
            tool_count: anchor.tool_count,
        })
    }
}

pub(super) fn should_compact(
    prepared: &PreparedRun,
    messages: &[CanonicalMessage],
    projected_messages: &[ProjectedMessage],
    anchor: Option<ContextUsageAnchor>,
    checkpoint_context_tokens: Option<u64>,
) -> bool {
    if prepared.action != RunAction::Start {
        return false;
    }
    let Some(context_window) = prepared.model.context_window_tokens else {
        return false;
    };
    if context_window == 0 || messages.len() <= prepared.initial_messages.len() {
        return false;
    }
    let budget = context_window.saturating_sub(COMPACTION_RESERVE_TOKENS);
    let estimated_input = anchor
        .filter(|anchor| {
            anchor.message_count <= projected_messages.len()
                && anchor.tool_count == prepared.prompt.tools.len()
        })
        .map(|anchor| {
            anchor
                .input_tokens
                .saturating_add(estimate_serialized_tokens(
                    &serde_json::to_string(&projected_messages[anchor.message_count..])
                        .unwrap_or_default(),
                ))
        })
        .or_else(|| {
            checkpoint_context_tokens.map(|tokens| {
                tokens.saturating_add(estimate_serialized_tokens(
                    &serde_json::to_string(&prepared.initial_messages).unwrap_or_default(),
                ))
            })
        })
        .unwrap_or_else(|| {
            estimate_serialized_tokens(
                &serde_json::to_string(&(&prepared.prompt, messages)).unwrap_or_default(),
            )
        });
    estimated_input >= budget
}

pub(super) fn partition(
    messages: &[CanonicalMessage],
    current_ids: &HashSet<&str>,
) -> (Vec<CanonicalMessage>, Option<CanonicalMessage>) {
    let latest_request_context = messages
        .iter()
        .rposition(|message| message.message_id.starts_with("request-context:"));
    let compactable = messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            Some(*index) != latest_request_context
                && !current_ids.contains(message.message_id.as_str())
        })
        .map(|(_, message)| message.clone())
        .collect();
    let retained = latest_request_context
        .and_then(|index| messages.get(index))
        .filter(|message| !current_ids.contains(message.message_id.as_str()))
        .cloned();
    (compactable, retained)
}

pub(super) fn fallback_summary(messages: &[CanonicalMessage]) -> String {
    let serialized = serde_json::to_string(messages).unwrap_or_default();
    let start = serialized
        .char_indices()
        .rev()
        .nth(FALLBACK_CHARS.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    format!(
        "Durable recent conversation state:\n{}",
        &serialized[start..]
    )
}

fn estimate_serialized_tokens(serialized: &str) -> u64 {
    serialized
        .chars()
        .fold(0_u64, |units, character| {
            units.saturating_add(if character.is_ascii() { 273 } else { 550 })
        })
        .div_ceil(1_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        project_messages, CheckpointId, ConversationId, ModelSpec, Origin, PromptSpec, Role, RunId,
        RunKind,
    };

    #[test]
    fn automatic_compaction_starts_only_after_the_context_window_is_exceeded() {
        let mut model = ModelSpec::new("model");
        model.context_window_tokens = Some(200_000);
        let prepared = PreparedRun {
            run_id: RunId::new("run"),
            cursor_request_id: None,
            conversation_id: ConversationId::new("conversation"),
            kind: RunKind::Root,
            model,
            checkpoint_context_tokens: None,
            prompt: PromptSpec {
                instructions: String::new(),
                tools: Vec::new(),
            },
            compaction_prompt: PromptSpec {
                instructions: String::new(),
                tools: Vec::new(),
            },
            initial_messages: Vec::new(),
            action: RunAction::Start,
            base_checkpoint_id: CheckpointId(1),
        };
        let messages = vec![CanonicalMessage::text(
            "user",
            Role::User,
            Origin::Runtime,
            "hello",
        )];
        let projected = project_messages(&messages).unwrap();
        let tail_tokens = estimate_serialized_tokens(&serde_json::to_string(&projected).unwrap());
        let anchor = |estimated_input| {
            Some(ContextUsageAnchor {
                input_tokens: estimated_input - tail_tokens,
                message_count: 0,
                tool_count: 0,
            })
        };

        assert!(!should_compact(
            &prepared,
            &messages,
            &projected,
            anchor(189_999),
            None,
        ));
        assert!(should_compact(
            &prepared,
            &messages,
            &projected,
            anchor(190_000),
            None,
        ));
        assert!(should_compact(
            &prepared,
            &messages,
            &projected,
            anchor(200_001),
            None,
        ));
    }
}
