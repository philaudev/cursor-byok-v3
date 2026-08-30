use crate::model::{ProviderReplayState, Usage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolUse,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelEvent {
    Start {
        model_call_id: String,
    },
    TextStart,
    TextDelta(String),
    TextEnd,
    ThinkingStart,
    ThinkingDelta(String),
    ThinkingEnd,
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
    },
    ToolCallArgumentsDelta {
        index: usize,
        delta: String,
    },
    ToolCallEnd {
        index: usize,
    },
    ProviderReplayState(ProviderReplayState),
    Usage(Usage),
    Done(FinishReason),
}

/// Returns whether an event represents the first valid upstream response.
/// Transport markers, replay metadata, usage, completion, and provider heartbeats
/// are intentionally excluded; empty text/reasoning/tool deltas are valid events.
pub fn is_valid_response_event(event: &ModelEvent) -> bool {
    matches!(
        event,
        ModelEvent::TextDelta(_)
            | ModelEvent::ThinkingStart
            | ModelEvent::ThinkingDelta(_)
            | ModelEvent::ThinkingEnd
            | ModelEvent::ToolCallStart { .. }
            | ModelEvent::ToolCallArgumentsDelta { .. }
            | ModelEvent::ToolCallEnd { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_markers_include_empty_content_but_exclude_transport_events() {
        assert!(is_valid_response_event(&ModelEvent::TextDelta(
            String::new()
        )));
        assert!(is_valid_response_event(&ModelEvent::ThinkingDelta(
            String::new()
        )));
        assert!(is_valid_response_event(
            &ModelEvent::ToolCallArgumentsDelta {
                index: 0,
                delta: String::new(),
            }
        ));
        assert!(is_valid_response_event(&ModelEvent::ThinkingStart));
        assert!(is_valid_response_event(&ModelEvent::ToolCallStart {
            index: 0,
            call_id: "call".into(),
            name: "tool".into(),
        }));
        assert!(!is_valid_response_event(&ModelEvent::Start {
            model_call_id: "call".into(),
        }));
        assert!(!is_valid_response_event(&ModelEvent::TextStart));
        assert!(!is_valid_response_event(&ModelEvent::Usage(
            Usage::default()
        )));
        assert!(!is_valid_response_event(&ModelEvent::Done(
            FinishReason::Stop
        )));
    }
}
