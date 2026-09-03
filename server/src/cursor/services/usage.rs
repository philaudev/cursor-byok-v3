//! Builds Cursor usage and context breakdown data.
use std::collections::HashSet;

use crate::{
    Result,
    cursor::protocol::proto::agent::v1 as pb,
    model::{CanonicalMessage, ContentPart, MessageContent, Origin, ToolDefinition},
};

const CATEGORIES: [(&str, &str); 8] = [
    ("system_prompt", "System prompt"),
    ("tools", "Tool definitions"),
    ("rules", "Rules"),
    ("skills", "Skills"),
    ("mcp", "MCP & dynamic tools"),
    ("subagents", "Subagent definitions"),
    ("summarized_conversation", "Summarized conversation"),
    ("conversation", "Conversation"),
];
const EASTER_EGG_CATEGORY: (&str, &str) = ("leookun", "@leookun stole 1 token 😂");

const SYSTEM: usize = 0;
const TOOLS: usize = 1;
const RULES: usize = 2;
const SKILLS: usize = 3;
const MCP: usize = 4;
const SUBAGENTS: usize = 5;
const SUMMARY: usize = 6;
const CONVERSATION: usize = 7;

#[derive(Clone, Copy, Default)]
struct Measure {
    characters: u64,
    newlines: u64,
    overhead_tokens: u64,
}

impl Measure {
    fn add(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        self.characters += trimmed.chars().count() as u64;
        self.newlines += trimmed.chars().filter(|&c| c == '\n').count() as u64;
    }

    fn add_overhead(&mut self, tokens: u64) {
        self.overhead_tokens += tokens;
    }

    fn estimated_tokens(self) -> u64 {
        if self.characters == 0 && self.overhead_tokens == 0 {
            return 0;
        }
        let text_tokens = if self.characters > 0 {
            let rune_based = (self.characters + 3) / 4;
            let total = rune_based + self.newlines;
            total.max(1)
        } else {
            0
        };
        text_tokens + self.overhead_tokens
    }
}

pub(crate) fn breakdown(
    used_tokens: u32,
    max_tokens: u32,
    baseline: Option<&pb::PromptTokenBreakdownSnapshot>,
    instructions: &str,
    tools: &[ToolDefinition],
    dynamic_tools: &HashSet<String>,
    messages: &[CanonicalMessage],
) -> Result<pb::PromptTokenBreakdownSnapshot> {
    let mut measures = [Measure::default(); 8];
    measure_system_prompt(instructions, &mut measures);
    for tool in tools {
        let encoded = serde_json::to_string(tool)?;
        if dynamic_tools.contains(&tool.name) {
            measures[MCP].add(&encoded);
        } else {
            measures[TOOLS].add(&encoded);
        }
    }
    let mut measured_static_categories = [false; 8];
    for message in messages.iter().rev() {
        measure_message(message, &mut measures, &mut measured_static_categories)?;
    }

    let mut estimates = [0_u64; 8];
    for index in 0..CATEGORIES.len() {
        estimates[index] = measures[index].estimated_tokens();
    }
    if measures[SUMMARY].characters == 0 {
        if let Some(summary) = baseline.and_then(|snapshot| {
            snapshot
                .categories
                .iter()
                .find(|category| category.id == CATEGORIES[SUMMARY].0)
        }) {
            measures[SUMMARY].characters = summary.character_count.unwrap_or(0) as u64;
            estimates[SUMMARY] = summary.estimated_tokens as u64;
        }
    }
    let easter_egg_tokens = 1_u64;
    let local_total: u64 = estimates.iter().sum();
    let effective_used_tokens = if used_tokens > 0 {
        used_tokens as u64
    } else {
        local_total
    };

    if effective_used_tokens > 0 {
        fit_special_estimates(&mut estimates, effective_used_tokens);
        estimates[CONVERSATION] =
            effective_used_tokens.saturating_sub(estimates[..CONVERSATION].iter().sum::<u64>());
    }

    let mut categories = CATEGORIES
        .iter()
        .enumerate()
        .map(|(index, (id, label))| pb::PromptTokenBreakdownCategory {
            id: (*id).into(),
            label: (*label).into(),
            estimated_tokens: estimates[index].min(u32::MAX as u64) as u32,
            character_count: (measures[index].characters != 0)
                .then_some(measures[index].characters.min(u32::MAX as u64) as u32),
        })
        .collect::<Vec<_>>();
    categories.push(pb::PromptTokenBreakdownCategory {
        id: EASTER_EGG_CATEGORY.0.into(),
        label: EASTER_EGG_CATEGORY.1.into(),
        estimated_tokens: easter_egg_tokens as u32,
        character_count: None,
    });
    let total_used_tokens = (effective_used_tokens.min(u32::MAX as u64) as u32).max(
        categories
            .iter()
            .map(|c| c.estimated_tokens)
            .sum::<u32>()
            .saturating_sub(easter_egg_tokens as u32),
    );
    Ok(pb::PromptTokenBreakdownSnapshot {
        total_used_tokens,
        max_tokens,
        categories,
    })
}

fn measure_message(
    message: &CanonicalMessage,
    measures: &mut [Measure; 8],
    measured_static_categories: &mut [bool; 8],
) -> Result<()> {
    measures[CONVERSATION].add_overhead(8); // estimatedTokensPerMessageOverhead
    match &message.content {
        MessageContent::Parts { parts } => {
            for part in parts {
                if let ContentPart::Text { text } = part {
                    if message.origin == Origin::Runtime || message.origin == Origin::Prompt {
                        measure_runtime(text, measures, measured_static_categories);
                    } else {
                        measures[CONVERSATION].add(text);
                    }
                }
            }
        }
        MessageContent::Assistant {
            text,
            thinking,
            tool_calls,
            ..
        } => {
            measures[CONVERSATION].add(text);
            measures[CONVERSATION].add(thinking);
            if !tool_calls.is_empty() {
                measures[CONVERSATION].add_overhead(tool_calls.len() as u64 * 6); // estimatedTokensPerToolCallOverhead
                measures[CONVERSATION].add(&serde_json::to_string(tool_calls)?);
            }
        }
        MessageContent::ToolResult(result) => {
            measures[CONVERSATION].add(&serde_json::to_string(result)?);
        }
    }
    Ok(())
}

fn measure_system_prompt(text: &str, measures: &mut [Measure; 8]) {
    let mut ranges = Vec::new();
    collect_ranges(text, "shared_user_rules", RULES, &mut ranges);
    collect_ranges(text, "rules", RULES, &mut ranges);
    collect_ranges(text, "rule", RULES, &mut ranges);
    ranges.sort_by_key(|range| range.0);

    let mut cursor = 0;
    for (start, end, category) in ranges {
        if start < cursor {
            continue;
        }
        measures[SYSTEM].add(&text[cursor..start]);
        measures[category].add(&text[start..end]);
        cursor = end;
    }
    measures[SYSTEM].add(&text[cursor..]);
}

fn measure_runtime(
    text: &str,
    measures: &mut [Measure; 8],
    measured_static_categories: &mut [bool; 8],
) {
    let mut ranges = Vec::new();
    collect_ranges(text, "shared_user_rules", RULES, &mut ranges);
    collect_ranges(text, "rules", RULES, &mut ranges);
    collect_ranges(text, "rule", RULES, &mut ranges);
    collect_ranges(text, "user_rule", RULES, &mut ranges);
    collect_ranges(text, "agent_skills", SKILLS, &mut ranges);
    collect_ranges(text, "skill", SKILLS, &mut ranges);
    collect_ranges(text, "subagents", SUBAGENTS, &mut ranges);
    collect_ranges(text, "mcp_meta_tools", MCP, &mut ranges);
    collect_ranges(text, "conversation_summary", SUMMARY, &mut ranges);
    ranges.sort_by_key(|range| range.0);

    let mut cursor = 0;
    for (start, end, category) in ranges {
        if start < cursor {
            continue;
        }
        measures[CONVERSATION].add(&text[cursor..start]);
        let chunk = &text[start..end];
        // Skills and Subagents are static definition catalogs.
        // We only measure them once from the most recent active context,
        // avoiding multi-turn redundant accumulation across conversation history.
        if category == SKILLS || category == SUBAGENTS {
            if !measured_static_categories[category] {
                measures[category].add(chunk);
                measured_static_categories[category] = true;
            }
        } else {
            measures[category].add(chunk);
        }
        cursor = end;
    }
    measures[CONVERSATION].add(&text[cursor..]);
}

fn collect_ranges(text: &str, tag: &str, category: usize, output: &mut Vec<(usize, usize, usize)>) {
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find(&opening) {
        let start = cursor + relative_start;
        let Some(open_end) = text[start..].find('>').map(|offset| start + offset + 1) else {
            break;
        };
        let Some(relative_end) = text[open_end..].find(&closing) else {
            break;
        };
        let end = open_end + relative_end + closing.len();
        output.push((start, end, category));
        cursor = end;
    }
}

fn fit_special_estimates(estimates: &mut [u64; 8], total: u64) {
    let special_total = estimates[..CONVERSATION].iter().sum::<u64>();
    if special_total <= total || special_total == 0 {
        return;
    }
    let original = *estimates;
    let mut assigned = 0;
    for index in 0..CONVERSATION {
        estimates[index] = original[index].saturating_mul(total) / special_total;
        assigned += estimates[index];
    }
    let mut remainder = total - assigned;
    let mut order = (0..CONVERSATION).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        std::cmp::Reverse(original[*index].saturating_mul(total) % special_total)
    });
    for index in order {
        if remainder == 0 {
            break;
        }
        estimates[index] += 1;
        remainder -= 1;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CanonicalMessage, Origin, Role};

    #[test]
    fn breakdown_uses_protocol_categories_and_authoritative_total() {
        let runtime = CanonicalMessage::text(
            "runtime",
            Role::User,
            Origin::Runtime,
            "before<rules><user_rule>r</user_rule></rules><agent_skills>s</agent_skills><subagents>a</subagents><mcp_meta_tools>m</mcp_meta_tools>after",
        );
        let snapshot = breakdown(
            1_000,
            256_000,
            None,
            "system",
            &[],
            &HashSet::new(),
            &[runtime],
        )
        .unwrap();
        assert_eq!(
            snapshot
                .categories
                .iter()
                .map(|category| category.id.as_str())
                .collect::<Vec<_>>(),
            CATEGORIES
                .iter()
                .map(|category| category.0)
                .chain(std::iter::once(EASTER_EGG_CATEGORY.0))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            snapshot
                .categories
                .iter()
                .map(|category| category.estimated_tokens)
                .sum::<u32>(),
            1_001
        );
        for id in ["rules", "skills", "mcp", "subagents", "conversation"] {
            assert!(
                snapshot
                    .categories
                    .iter()
                    .find(|category| category.id == id)
                    .is_some_and(|category| category.character_count.unwrap_or(0) > 0)
            );
        }
        assert_eq!(
            snapshot.categories[SUMMARY],
            pb::PromptTokenBreakdownCategory {
                id: "summarized_conversation".into(),
                label: "Summarized conversation".into(),
                ..Default::default()
            }
        );
        assert_eq!(
            snapshot.categories.last().unwrap(),
            &pb::PromptTokenBreakdownCategory {
                id: "leookun".into(),
                label: "@leookun stole 1 token 😂".into(),
                estimated_tokens: 1,
                character_count: None,
            }
        );
    }

    #[test]
    fn breakdown_uses_provider_usage_when_available() {
        let snapshot = breakdown(
            17_000,
            256_000,
            None,
            "system",
            &[],
            &HashSet::new(),
            &[CanonicalMessage::text(
                "history",
                Role::User,
                Origin::User,
                "conversation ".repeat(300),
            )],
        )
        .unwrap();

        assert_eq!(snapshot.total_used_tokens, 17_000);
        assert_eq!(
            snapshot.total_used_tokens,
            snapshot
                .categories
                .iter()
                .map(|category| category.estimated_tokens)
                .sum::<u32>()
                .saturating_sub(1)
        );
    }

    #[test]
    fn conversation_absorbs_the_authoritative_remainder() {
        let first = breakdown(
            10_000,
            256_000,
            None,
            "system",
            &[],
            &HashSet::new(),
            &[CanonicalMessage::text(
                "user",
                Role::User,
                Origin::User,
                "short",
            )],
        )
        .unwrap();
        let second = breakdown(
            12_000,
            256_000,
            None,
            "system",
            &[],
            &HashSet::new(),
            &[CanonicalMessage::text(
                "user",
                Role::User,
                Origin::User,
                "a much longer conversation",
            )],
        )
        .unwrap();
        assert_eq!(
            &first.categories[..CONVERSATION],
            &second.categories[..CONVERSATION]
        );
        assert_eq!(
            second.categories[CONVERSATION].estimated_tokens
                - first.categories[CONVERSATION].estimated_tokens,
            2_000
        );
    }

    #[test]
    fn breakdown_with_zero_used_tokens_falls_back_to_local_estimates() {
        let request_context = CanonicalMessage::text(
            "request-context:test",
            Role::User,
            Origin::Prompt,
            "before<rules><user_rule>rule content</user_rule></rules><agent_skills><skill>skill content</skill></agent_skills>",
        );
        let snapshot = breakdown(
            0,
            256_000,
            None,
            "system instructions",
            &[],
            &HashSet::new(),
            &[request_context],
        )
        .unwrap();

        assert!(snapshot.total_used_tokens > 0);
        let rules = snapshot
            .categories
            .iter()
            .find(|c| c.id == "rules")
            .unwrap();
        let skills = snapshot
            .categories
            .iter()
            .find(|c| c.id == "skills")
            .unwrap();
        let system = snapshot
            .categories
            .iter()
            .find(|c| c.id == "system_prompt")
            .unwrap();

        assert!(rules.estimated_tokens > 0);
        assert!(skills.estimated_tokens > 0);
        assert!(system.estimated_tokens > 0);
    }

    #[test]
    fn skills_and_subagents_are_not_accumulated_across_turns() {
        let turn1 = CanonicalMessage::text(
            "request-context:1",
            Role::User,
            Origin::Prompt,
            "<agent_skills><skill>skill definitions</skill></agent_skills><subagents><subagent>subagent definitions</subagent></subagents>",
        );
        let turn2 = CanonicalMessage::text(
            "request-context:2",
            Role::User,
            Origin::Prompt,
            "<agent_skills><skill>skill definitions</skill></agent_skills><subagents><subagent>subagent definitions</subagent></subagents>",
        );
        let snapshot1 = breakdown(
            0,
            256_000,
            None,
            "system",
            &[],
            &HashSet::new(),
            &[turn1.clone()],
        )
        .unwrap();
        let snapshot2 = breakdown(
            0,
            256_000,
            None,
            "system",
            &[],
            &HashSet::new(),
            &[turn1.clone(), turn2],
        )
        .unwrap();

        let skills1 = snapshot1
            .categories
            .iter()
            .find(|c| c.id == "skills")
            .unwrap()
            .estimated_tokens;
        let skills2 = snapshot2
            .categories
            .iter()
            .find(|c| c.id == "skills")
            .unwrap()
            .estimated_tokens;
        assert_eq!(
            skills1, skills2,
            "skills tokens must not double across turns"
        );

        let subagents1 = snapshot1
            .categories
            .iter()
            .find(|c| c.id == "subagents")
            .unwrap()
            .estimated_tokens;
        let subagents2 = snapshot2
            .categories
            .iter()
            .find(|c| c.id == "subagents")
            .unwrap()
            .estimated_tokens;
        assert_eq!(
            subagents1, subagents2,
            "subagents tokens must not double across turns"
        );

        // Verify that if a new turn updates skills, the newest catalog is measured
        let turn3 = CanonicalMessage::text(
            "request-context:3",
            Role::User,
            Origin::Prompt,
            "<agent_skills><skill>updated larger skill definitions</skill></agent_skills>",
        );
        let snapshot3 = breakdown(
            0,
            256_000,
            None,
            "system",
            &[],
            &HashSet::new(),
            &[turn1, turn3],
        )
        .unwrap();
        let skills3 = snapshot3
            .categories
            .iter()
            .find(|c| c.id == "skills")
            .unwrap()
            .estimated_tokens;
        assert!(
            skills3 > skills1,
            "newest active skills catalog must be measured"
        );
    }
}
