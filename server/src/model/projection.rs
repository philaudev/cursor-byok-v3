//! Projects canonical Messages into provider-visible model input.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::{
    CanonicalMessage, ContentPart, MessageContent, ProviderReplayState, Role, ToolCallContent,
    ToolResultContent,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ProjectedContent {
    Parts(Vec<ContentPart>),
    Assistant {
        text: String,
        thinking: String,
        replay_state: Option<ProviderReplayState>,
        calls: Vec<ToolCallContent>,
    },
    ToolResult(ToolResultContent),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProjectedMessage {
    pub message_id: String,
    pub role: Role,
    pub content: ProjectedContent,
}

pub fn project_messages(messages: &[CanonicalMessage]) -> Result<Vec<ProjectedMessage>> {
    let mut projected = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        if let Some((group, next)) = project_tool_round(messages, index)? {
            projected.extend(group);
            index = next;
        } else {
            projected.push(project_message(&messages[index]));
            index += 1;
        }
    }
    Ok(projected)
}

pub fn project_compaction_messages(messages: &[CanonicalMessage]) -> Result<Vec<ProjectedMessage>> {
    if messages.is_empty() {
        return Ok(vec![ProjectedMessage {
            message_id: "compaction:input".into(),
            role: Role::User,
            content: ProjectedContent::Parts(vec![ContentPart::Text {
                text: "History to compact:\n(empty conversation)\n\nReturn only the replacement summary text.".into(),
            }]),
        }]);
    }

    let mut sections = Vec::new();
    let mut lines = Vec::new();
    let mut turn_idx = 1;

    for message in messages {
        match &message.content {
            MessageContent::Parts { parts } => {
                let mut text = String::new();
                for part in parts {
                    if let ContentPart::Text { text: t } = part {
                        text.push_str(t);
                    }
                }
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let role_str = match message.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::System => "system",
                        Role::Tool => "tool",
                    };
                    lines.push(format!("{turn_idx}. {role_str}={trimmed}"));
                    turn_idx += 1;
                }
            }
            MessageContent::Assistant {
                text,
                thinking,
                tool_calls,
                ..
            } => {
                let mut parts = Vec::new();
                if !text.trim().is_empty() {
                    parts.push(format!("assistant={}", text.trim()));
                }
                if !thinking.trim().is_empty() {
                    parts.push(format!("thinking={}", thinking.trim()));
                }
                for call in tool_calls {
                    parts.push(format!("tool_call={}", call.name));
                }
                if !parts.is_empty() {
                    lines.push(format!("{turn_idx}. {}", parts.join(" | ")));
                    turn_idx += 1;
                }
            }
            MessageContent::ToolResult(result) => {
                let tool_name = if result.name.is_empty() {
                    "tool_result"
                } else {
                    &result.name
                };
                let content = result.content.trim();
                let summary_content = if content.len() > 300 {
                    let end = content
                        .char_indices()
                        .map(|(index, character)| index + character.len_utf8())
                        .take_while(|end| *end <= 300)
                        .last()
                        .unwrap_or(0);
                    format!("{}...", &content[..end])
                } else if content.is_empty() {
                    "completed".into()
                } else {
                    content.into()
                };
                lines.push(format!("{turn_idx}. {tool_name}={summary_content}"));
                turn_idx += 1;
            }
        }
    }

    if !lines.is_empty() {
        sections.push(format!("History to compact:\n{}", lines.join("\n")));
    }
    sections.push("Return only the replacement summary text.".into());

    Ok(vec![ProjectedMessage {
        message_id: "compaction:input".into(),
        role: Role::User,
        content: ProjectedContent::Parts(vec![ContentPart::Text {
            text: sections.join("\n\n"),
        }]),
    }])
}

fn project_tool_round(
    messages: &[CanonicalMessage],
    start: usize,
) -> Result<Option<(Vec<ProjectedMessage>, usize)>> {
    let MessageContent::Assistant {
        tool_round_id: Some(group_id),
        tool_calls,
        ..
    } = &messages[start].content
    else {
        return Ok(None);
    };
    if tool_calls.is_empty() {
        return Ok(None);
    }

    let mut cursor = start;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut replay_state = None;
    let mut calls = Vec::new();
    let mut results = Vec::new();
    let mut result_ids = HashSet::new();

    while cursor < messages.len() {
        let MessageContent::Assistant {
            text: part_text,
            thinking: part_thinking,
            tool_round_id: Some(candidate_group),
            replay_state: part_replay,
            tool_calls: part_calls,
        } = &messages[cursor].content
        else {
            break;
        };
        if candidate_group != group_id || part_calls.is_empty() {
            break;
        }
        text.push_str(part_text);
        thinking.push_str(part_thinking);
        if replay_state.is_none() {
            replay_state = part_replay.clone();
        } else if part_replay.is_some() {
            return Err(Error::Protocol(
                "tool round repeats provider replay state".into(),
            ));
        }
        calls.extend(part_calls.iter().cloned());
        cursor += 1;

        while cursor < messages.len() {
            let MessageContent::ToolResult(result) = &messages[cursor].content else {
                break;
            };
            if !calls.iter().any(|call| call.call_id == result.call_id) {
                break;
            }
            if !result_ids.insert(result.call_id.clone()) {
                return Err(Error::Protocol(format!(
                    "duplicate tool result call_id: {}",
                    result.call_id
                )));
            }
            results.push((messages[cursor].message_id.clone(), result.clone()));
            cursor += 1;
        }
    }

    calls.sort_by_key(|call| call.index);
    for call in &calls {
        if !result_ids.contains(&call.call_id) {
            return Err(Error::Protocol(format!(
                "assistant tool call has no result call_id: {}",
                call.call_id
            )));
        }
    }

    let mut output = Vec::with_capacity(results.len() + 1);
    output.push(ProjectedMessage {
        message_id: messages[start].message_id.clone(),
        role: Role::Assistant,
        content: ProjectedContent::Assistant {
            text,
            thinking,
            replay_state,
            calls,
        },
    });
    output.extend(
        results
            .into_iter()
            .map(|(message_id, result)| ProjectedMessage {
                message_id,
                role: Role::Tool,
                content: ProjectedContent::ToolResult(result),
            }),
    );
    Ok(Some((output, cursor)))
}

fn project_message(message: &CanonicalMessage) -> ProjectedMessage {
    let content = match &message.content {
        MessageContent::Parts { parts } => ProjectedContent::Parts(parts.clone()),
        MessageContent::Assistant {
            text,
            thinking,
            replay_state,
            tool_calls,
            ..
        } => ProjectedContent::Assistant {
            text: text.clone(),
            thinking: thinking.clone(),
            replay_state: replay_state.clone(),
            calls: tool_calls.clone(),
        },
        MessageContent::ToolResult(result) => ProjectedContent::ToolResult(result.clone()),
    };
    ProjectedMessage {
        message_id: message.message_id.clone(),
        role: message.role.clone(),
        content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Origin;

    #[test]
    fn compaction_messages_formats_history_into_single_user_message() {
        let messages = vec![
            CanonicalMessage::text("u1", Role::User, Origin::User, "Hello"),
            CanonicalMessage {
                message_id: "a1".into(),
                role: Role::Assistant,
                origin: Origin::Assistant,
                content: MessageContent::Assistant {
                    text: "I can help".into(),
                    thinking: "".into(),
                    tool_round_id: None,
                    replay_state: None,
                    tool_calls: vec![ToolCallContent {
                        index: 0,
                        call_id: "call-1".into(),
                        name: "Read".into(),
                        arguments: serde_json::json!({"path": "foo.rs"}),
                    }],
                },
                runtime_event_id: None,
            },
            CanonicalMessage {
                message_id: "unicode-tool-result".into(),
                role: Role::Tool,
                origin: Origin::Tool,
                content: MessageContent::ToolResult(ToolResultContent {
                    call_id: "call-unicode".into(),
                    name: "Read".into(),
                    content: format!("{}ỉ", "x".repeat(298)),
                    is_error: false,
                    image: None,
                    provider_parts: Vec::new(),
                }),
                runtime_event_id: None,
            },
        ];

        let projected = project_compaction_messages(&messages).unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].role, Role::User);

        let ProjectedContent::Parts(parts) = &projected[0].content else {
            panic!("expected parts content");
        };
        let ContentPart::Text { text } = &parts[0] else {
            panic!("expected text part");
        };

        assert!(text.contains("History to compact:"));
        assert!(text.contains("1. user=Hello"));
        assert!(text.contains("assistant=I can help"));
        assert!(text.contains("tool_call=Read"));
        assert!(text.contains("Read=xxxxxxxx"));
        assert!(text.contains("..."));
        assert!(text.contains("Return only the replacement summary text."));
    }
}
