use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{CanonicalMessage, MessageContent};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DerivedState {
    pub todos: Option<Value>,
    pub plan: Option<Value>,
}

pub fn fold_derived_state(messages: &[CanonicalMessage]) -> DerivedState {
    let mut state = DerivedState::default();
    let mut calls = std::collections::HashMap::<String, (String, Value)>::new();
    for message in messages {
        match &message.content {
            MessageContent::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    calls.insert(
                        call.call_id.clone(),
                        (call.name.clone(), call.arguments.clone()),
                    );
                }
            }
            MessageContent::ToolResult(result) if !result.is_error => {
                let Some((name, input)) = calls.get(&result.call_id).cloned() else {
                    continue;
                };
                match normalize(&name).as_str() {
                    "todowrite" | "updatetodos" => {
                        state.todos = Some(apply_todo_write(state.todos.take(), input));
                    }
                    "createplan" | "updateplan" | "writeplan" => state.plan = Some(input),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    state
}

fn apply_todo_write(current: Option<Value>, mut input: Value) -> Value {
    if !input.get("merge").and_then(Value::as_bool).unwrap_or(false) {
        return input;
    }
    let mut todos = current
        .as_ref()
        .and_then(|value| value.get("todos"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let patches = input
        .get("todos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for mut patch in patches {
        if let Value::Object(ref mut patch_obj) = patch {
            if !patch_obj.contains_key("status") || patch_obj["status"].is_null() {
                patch_obj.insert("status".into(), Value::String("pending".into()));
            }
        }
        let existing = patch.get("id").and_then(Value::as_str).and_then(|id| {
            todos
                .iter_mut()
                .find(|todo| todo.get("id").and_then(Value::as_str) == Some(id))
        });
        match (existing, patch) {
            (Some(Value::Object(todo)), Value::Object(patch)) => todo.extend(patch),
            (_, patch) => todos.push(patch),
        }
    }
    if let Some(object) = input.as_object_mut() {
        object.insert("merge".into(), Value::Bool(false));
        object.insert("todos".into(), Value::Array(todos));
    }
    input
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Origin, Role, ToolCallContent, ToolResultContent};

    #[test]
    fn todo_write_merge_materializes_complete_existing_items_and_appends_new_ids() {
        let messages = vec![
            assistant_call(
                "create",
                serde_json::json!({
                    "merge": false,
                    "todos": [
                        {"id": "first", "content": "First", "status": "in_progress"},
                        {"id": "second", "content": "Second", "status": "pending"}
                    ]
                }),
            ),
            successful_result("create"),
            assistant_call(
                "merge",
                serde_json::json!({
                    "merge": true,
                    "todos": [
                        {"id": "first", "status": "completed"},
                        {"id": "second", "content": "Second updated"},
                        {"id": "third", "content": "Third", "status": "cancelled"}
                    ]
                }),
            ),
            successful_result("merge"),
        ];

        let state = fold_derived_state(&messages);

        assert_eq!(
            state.todos.unwrap()["todos"],
            serde_json::json!([
                {"id": "first", "content": "First", "status": "completed"},
                {"id": "second", "content": "Second updated", "status": "pending"},
                {"id": "third", "content": "Third", "status": "cancelled"}
            ])
        );
    }

    #[test]
    fn todo_write_merge_supplies_default_pending_status_for_new_items_without_status() {
        let messages = vec![
            assistant_call(
                "create",
                serde_json::json!({
                    "merge": false,
                    "todos": [
                        {"id": "task-1", "content": "Task 1", "status": "in_progress"}
                    ]
                }),
            ),
            successful_result("create"),
            assistant_call(
                "merge",
                serde_json::json!({
                    "merge": true,
                    "todos": [
                        {"id": "task-2", "content": "Task 2"}
                    ]
                }),
            ),
            successful_result("merge"),
        ];

        let state = fold_derived_state(&messages);

        assert_eq!(
            state.todos.unwrap()["todos"],
            serde_json::json!([
                {"id": "task-1", "content": "Task 1", "status": "in_progress"},
                {"id": "task-2", "content": "Task 2", "status": "pending"}
            ])
        );
    }

    fn assistant_call(call_id: &str, arguments: Value) -> CanonicalMessage {
        CanonicalMessage {
            message_id: format!("assistant-{call_id}"),
            role: Role::Assistant,
            origin: Origin::Assistant,
            content: MessageContent::Assistant {
                text: String::new(),
                thinking: String::new(),
                tool_round_id: Some(format!("round-{call_id}").into()),
                replay_state: None,
                tool_calls: vec![ToolCallContent {
                    index: 0,
                    call_id: call_id.into(),
                    name: "TodoWrite".into(),
                    arguments,
                }],
            },
            runtime_event_id: None,
        }
    }

    fn successful_result(call_id: &str) -> CanonicalMessage {
        CanonicalMessage {
            message_id: format!("result-{call_id}"),
            role: Role::Tool,
            origin: Origin::Tool,
            content: MessageContent::ToolResult(ToolResultContent {
                call_id: call_id.into(),
                name: "TodoWrite".into(),
                content: "{}".into(),
                is_error: false,
                image: None,
                provider_parts: Vec::new(),
            }),
            runtime_event_id: None,
        }
    }
}
