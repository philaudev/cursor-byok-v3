//! Compiles rules, skills, MCP metadata, and environment context.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
};

use prost::Message;
use serde_json::Value;

use crate::{
    cursor::{
        protocol::proto::agent::v1 as pb, services::context_sync::RequestContextSynchronizer,
        tools::runtime::McpRoute,
    },
    model::{normalize_tool_name, ToolDefinition},
    store::BlobId,
    Error, Result,
};

pub async fn hydrate(
    request: &pb::AgentRunRequest,
    context_sync: &RequestContextSynchronizer,
) -> Result<pb::RequestContext> {
    let mut context = request_context(request).cloned().unwrap_or_default();
    let Some(parts) = request
        .action
        .as_ref()
        .and_then(|action| action.request_context_parts.as_ref())
    else {
        if is_background_completion(request) {
            return context_sync
                .load(request.conversation_id.as_deref().unwrap_or_default())
                .await;
        }
        return Ok(context);
    };

    if let Some(current) = context_sync
        .refresh_if_missing(
            parts,
            request.conversation_id.as_deref().unwrap_or_default(),
        )
        .await?
    {
        context.rules = current.rules;
        context.non_file_rules = current.non_file_rules;
        context.cloud_rule = current.cloud_rule;
        context.agent_skills = current.agent_skills;
        context.skill_options = current.skill_options;
        context.custom_subagents = current.custom_subagents;
        context.tools = current.tools;
        context.mcp_instructions = current.mcp_instructions;
        context.mcp_file_system_options = current.mcp_file_system_options;
        context.mcp_meta_tool_options = current.mcp_meta_tool_options;
        return Ok(context);
    }

    if let Some(part) = decode_part::<pb::RequestContextRulesPart>(
        "rules",
        &parts.rules_blob_id,
        parts.rules_byte_length,
        context_sync,
    )
    .await?
    {
        context.rules = part.rules;
        context.non_file_rules = part.non_file_rules;
        context.cloud_rule = part.cloud_rule;
    }
    if let Some(part) = decode_part::<pb::RequestContextSkillsPart>(
        "skills",
        &parts.skills_blob_id,
        parts.skills_byte_length,
        context_sync,
    )
    .await?
    {
        context.agent_skills = part.agent_skills;
        context.skill_options = part.skill_options;
    }
    if let Some(part) = decode_part::<pb::RequestContextSubagentsPart>(
        "subagents",
        &parts.subagents_blob_id,
        parts.subagents_byte_length,
        context_sync,
    )
    .await?
    {
        context.custom_subagents = part.custom_subagents;
    }
    if let Some(part) = decode_part::<pb::RequestContextMcpsPart>(
        "MCP",
        &parts.mcps_blob_id,
        parts.mcps_byte_length,
        context_sync,
    )
    .await?
    {
        context.tools = part.tools;
        context.mcp_instructions = part.mcp_instructions;
        context.mcp_file_system_options = part.mcp_file_system_options;
        context.mcp_meta_tool_options = part.mcp_meta_tool_options;
    }
    Ok(context)
}

fn is_background_completion(request: &pb::AgentRunRequest) -> bool {
    matches!(
        request
            .action
            .as_ref()
            .and_then(|action| action.action.as_ref()),
        Some(pb::conversation_action::Action::BackgroundTaskCompletionAction(_))
    )
}

async fn decode_part<T: Message + Default>(
    name: &str,
    raw_id: &[u8],
    expected_length: u32,
    context_sync: &RequestContextSynchronizer,
) -> Result<Option<T>> {
    if raw_id.is_empty() {
        if expected_length != 0 {
            return Err(Error::Protocol(format!(
                "{name} context has a byte length but no BlobID"
            )));
        }
        return Ok(None);
    }
    let id = BlobId::from_bytes(raw_id)?;
    let data = context_sync.get(&id).await?.ok_or_else(|| {
        Error::Protocol(format!(
            "{name} context Blob is missing: {}",
            id.to_base64()
        ))
    })?;
    if data.len() != expected_length as usize {
        return Err(Error::Protocol(format!(
            "{name} context Blob length mismatch: expected {expected_length}, got {}",
            data.len()
        )));
    }
    T::decode(data.as_slice())
        .map(Some)
        .map_err(|error| Error::Protocol(format!("invalid {name} context Blob: {error}")))
}

/// 把本地 md 规则目录(rules 服务的存储)合并进请求上下文,
/// 使 BYOK 运行在 IDE 未携带这些规则时也能消费它们。
/// 与 IDE 已发规则按内容去重;读取失败只告警,不影响运行。
pub fn merge_local_rules(context: &mut pb::RequestContext, rules_dir: &Path) {
    let records = match crate::cursor::services::knowledge::RuleStore::open(rules_dir.into())
        .and_then(|store| store.list())
    {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(%error, "cannot read local rules; continuing without them");
            return;
        }
    };
    let existing = context
        .rules
        .iter()
        .chain(context.non_file_rules.iter())
        .map(|rule| rule.content.trim().to_owned())
        .chain(context.cloud_rule.iter().map(|rule| rule.trim().to_owned()))
        .collect::<HashSet<_>>();
    for record in records {
        if record.knowledge.trim().is_empty() || existing.contains(record.knowledge.trim()) {
            continue;
        }
        context.non_file_rules.push(pb::CursorRule {
            content: record.knowledge,
            ..Default::default()
        });
    }
}

#[derive(serde::Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default, alias = "disable-model-invocation")]
    disable_model_invocation: bool,
}

fn parse_frontmatter(content: &str) -> (Option<SkillFrontmatter>, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, content);
    }
    let rest = &trimmed[3..];
    let end_idx = rest.find("\n---").or_else(|| rest.find("\r\n---"));
    let Some(idx) = end_idx else {
        return (None, content);
    };
    let yaml_str = &rest[..idx];
    let after_closing = &rest[idx..];
    let body_start = after_closing.find("---").map(|i| i + 3).unwrap_or(0);
    let body = after_closing[body_start..].trim_start_matches(|c| c == '\r' || c == '\n');
    let frontmatter = serde_yaml::from_str::<SkillFrontmatter>(yaml_str).ok();
    (frontmatter, body)
}

fn extract_fallback_description(body: &str, path: &Path) -> String {
    for line in body.lines() {
        let trimmed = line.trim().trim_start_matches('#').trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    path.parent()
        .and_then(|p| p.file_name())
        .or_else(|| path.file_stem())
        .and_then(|n| n.to_str())
        .unwrap_or("Local Skill")
        .to_string()
}

struct ParsedSkill {
    skill: pb::AgentSkill,
    name: Option<String>,
}

fn parse_skill_file(path: &Path) -> Option<ParsedSkill> {
    let content = std::fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    let full_path = path.to_string_lossy().to_string();
    let (frontmatter, body) = parse_frontmatter(&content);
    let (description, disable_model_invocation, name) = if let Some(fm) = frontmatter {
        let desc = fm
            .description
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| extract_fallback_description(body, path));
        let name = fm.name.and_then(|n| {
            let trimmed = n.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_lowercase())
            }
        });
        (desc, fm.disable_model_invocation, name)
    } else {
        (extract_fallback_description(body, path), false, None)
    };

    Some(ParsedSkill {
        skill: pb::AgentSkill {
            full_path,
            content,
            description,
            disable_model_invocation,
            ..Default::default()
        },
        name,
    })
}

fn resolve_skill_identifier(parsed_name: Option<&str>, path: &Path) -> String {
    if let Some(name) = parsed_name {
        return name.to_string();
    }
    if path.is_dir() {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_lowercase()
    } else {
        path.file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_lowercase()
    }
}

fn skill_identifier(skill: &pb::AgentSkill, path: &Path) -> String {
    if let (Some(frontmatter), _) = parse_frontmatter(&skill.content) {
        if let Some(name) = frontmatter.name {
            if !name.trim().is_empty() {
                return name.trim().to_lowercase();
            }
        }
    }
    if path.is_dir() {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_lowercase()
    } else {
        path.file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_lowercase()
    }
}

pub fn merge_skills_from_directories(context: &mut pb::RequestContext, directories: &[PathBuf]) {
    let mut known_paths = context
        .agent_skills
        .iter()
        .map(|s| s.full_path.clone())
        .collect::<HashSet<_>>();
    let mut known_names = HashSet::new();

    for skill in &context.agent_skills {
        known_names.insert(skill_identifier(skill, Path::new(&skill.full_path)));
    }

    for dir in directories {
        if !dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!(path = %dir.display(), %error, "cannot read skills directory");
                continue;
            }
        };

        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                let skill_md = [
                    path.join("SKILL.md"),
                    path.join("skill.md"),
                    path.join("Skill.md"),
                ]
                .into_iter()
                .find(|p| p.is_file());

                if let Some(skill_file) = skill_md {
                    if let Some(parsed) = parse_skill_file(&skill_file) {
                        let name_key = resolve_skill_identifier(parsed.name.as_deref(), &path);
                        let path_key = parsed.skill.full_path.clone();
                        if !known_paths.contains(&path_key) && !known_names.contains(&name_key) {
                            known_paths.insert(path_key);
                            known_names.insert(name_key);
                            context.agent_skills.push(parsed.skill);
                        }
                    }
                }
            } else if path.is_file() && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("md")) {
                if let Some(parsed) = parse_skill_file(&path) {
                    let name_key = resolve_skill_identifier(parsed.name.as_deref(), &path);
                    let path_key = parsed.skill.full_path.clone();
                    if !known_paths.contains(&path_key) && !known_names.contains(&name_key) {
                        known_paths.insert(path_key);
                        known_names.insert(name_key);
                        context.agent_skills.push(parsed.skill);
                    }
                }
            }
        }
    }
}

/// 把工作区及用户主目录下的 skills 目录自动合并进请求上下文,
/// 使本地定义的 skills 在 IDE 未下发时也能被 AI 发现与使用。
pub fn merge_local_skills(context: &mut pb::RequestContext) {
    let mut candidate_roots = Vec::new();

    if let Some(env) = &context.env {
        for path in &env.workspace_paths {
            if !path.trim().is_empty() {
                candidate_roots.push(PathBuf::from(path));
            }
        }
        if !env.project_folder.trim().is_empty() {
            candidate_roots.push(PathBuf::from(&env.project_folder));
        }
    }
    for repo in &context.git_repos {
        if !repo.path.trim().is_empty() {
            candidate_roots.push(PathBuf::from(&repo.path));
        }
    }

    let mut skill_dirs = Vec::new();
    for root in &candidate_roots {
        skill_dirs.push(root.join(".agents/skills"));
        skill_dirs.push(root.join(".cursor/skills"));
        skill_dirs.push(root.join(".claude/skills"));
    }

    if let Some(home) = dirs::home_dir() {
        skill_dirs.push(home.join(".agents/skills"));
        skill_dirs.push(home.join(".cursor/skills"));
        skill_dirs.push(home.join(".claude/skills"));
    }
    if let Ok(managed_dir) = crate::config::managed_data_dir() {
        skill_dirs.push(managed_dir.join("skills"));
    }

    merge_skills_from_directories(context, &skill_dirs);
}

pub fn request_context(request: &pb::AgentRunRequest) -> Option<&pb::RequestContext> {
    let action = request.action.as_ref()?;
    action
        .request_context_parts
        .as_ref()
        .and_then(|parts| parts.dynamic_context.as_ref())
        .or_else(|| match action.action.as_ref()? {
            pb::conversation_action::Action::UserMessageAction(action) => {
                action.request_context.as_ref()
            }
            pb::conversation_action::Action::ExecutePlanAction(action) => {
                action.request_context.as_ref()
            }
            _ => None,
        })
}

pub fn compile_context(context: &pb::RequestContext, today: &str) -> String {
    let mut sections = Vec::new();
    let mut transcripts = None;
    if let Some(env) = &context.env {
        let workspace = env
            .workspace_paths
            .first()
            .map(String::as_str)
            .unwrap_or("");
        let repo = context.git_repos.iter().find(|repo| repo.path == workspace);
        sections.push(format!(
            "<user_info>\nOS Version: {}\n\nShell: {}\n\nWorkspace Path: {}\n\nIs directory a git repo: {}\n\nTerminals folder: {}\n\nToday's date: {}\n\nNote: Prefer using absolute paths over relative paths as tool call args when possible.\n</user_info>",
            env.os_version,
            env.shell,
            workspace,
            repo.map(|repo| format!("Yes, at {}", repo.path)).unwrap_or_else(|| "No".into()),
            env.terminals_folder,
            today,
        ));
        if !env.agent_transcripts_folder.is_empty() {
            transcripts = Some(format!(
                "<agent_transcripts>\nAgent transcripts (past chats) live in {}. They have names like <uuid>.jsonl, cite parent chat transcripts to the user as [<title for chat <=6 words>\n](<uuid excluding .jsonl>). Don't discuss the folder structure.\n</agent_transcripts>",
                env.agent_transcripts_folder
            ));
        }
    }
    sections.extend(context.git_repos.iter().map(|repo| {
        format!(
            "<git_status>\nThis is the git status at the start of the conversation. Note that this status is a snapshot in time, and will not update during the conversation.\n\n\nGit repo: {}\n\n```\n{}\n```\n</git_status>",
            repo.path, repo.status
        )
    }));
    sections.extend(transcripts);
    let skill_contents = context
        .agent_skills
        .iter()
        .map(|skill| skill.content.as_str())
        .filter(|content| !content.is_empty())
        .collect::<HashSet<_>>();
    let mut rules = context
        .rules
        .iter()
        .chain(context.non_file_rules.iter())
        .filter(|rule| {
            !rule.content.trim().is_empty()
                && !is_skill_rule(rule)
                && !skill_contents.contains(rule.content.as_str())
        })
        .map(|rule| format!("<user_rule>\n{}\n</user_rule>", rule.content))
        .collect::<Vec<_>>();
    rules.extend(
        context
            .cloud_rule
            .iter()
            .map(|rule| format!("<user_rule>\n{rule}\n</user_rule>")),
    );
    if !rules.is_empty() {
        sections.push(format!("<rules>\n{}\n</rules>", rules.join("\n")));
    }
    let skills = context
        .agent_skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .map(|skill| {
            format!(
                "<agent_skill fullPath=\"{}\">{}</agent_skill>",
                xml(&skill.full_path),
                xml(&skill.description),
            )
        })
        .collect::<Vec<_>>();
    if !skills.is_empty() {
        sections.push(format!(
            "<agent_skills>\n<available_skills>\n{}\n</available_skills>\n</agent_skills>",
            skills.join("\n")
        ));
    }
    let subagents = context
        .custom_subagents
        .iter()
        .map(|agent| {
            format!(
                "<subagent name=\"{}\">{}</subagent>",
                xml(&agent.name),
                agent.description
            )
        })
        .collect::<Vec<_>>();
    if !subagents.is_empty() {
        sections.push(format!(
            "<subagents>\n{}\n</subagents>",
            subagents.join("\n")
        ));
    }
    {
        let servers = context
            .mcp_meta_tool_options
            .as_ref()
            .into_iter()
            .flat_map(|options| &options.mcp_descriptors)
            .filter_map(compile_mcp_descriptor)
            .collect::<Vec<_>>();
        if !servers.is_empty() {
            sections.push(format!(
                "<mcp_meta_tools>\nThe following MCP tools are available. Call a listed tool directly with CallMcpTool without calling GetMcpTools first. If a call returns an error, use it to correct the arguments or authentication and retry when appropriate.\n<mcp_meta_tool_servers>\n{}\n</mcp_meta_tool_servers>\n</mcp_meta_tools>",
                servers.join("\n")
            ));
        }
    }
    sections.join("\n\n")
}

static MCP_DESCRIPTION_CACHE: LazyLock<RwLock<HashMap<(String, String), String>>> =
    LazyLock::new(|| {
        let mut map = HashMap::new();
        let fast_context_desc = "Fast semantic codebase search and context discovery. Locates relevant files, exact line ranges, and code regions from natural language descriptions in a single step, without needing manual grep trials or knowing exact filenames.\n\nRecommended for:\n- Finding feature implementations, business logic, and API flows (e.g. 'where is payment webhook handled', 'auth token refresh flow').\n- Exploring unfamiliar codebases or locating where conceptual logic lives.\n- Getting high-relevance entry points, line ranges, and suggested grep keywords before reading code.\n\nUse exact-match grep instead only when searching for a known, specific symbol name or literal string.\n\nFor best semantic search quality, write the query primarily in English; add local-language business terms only when needed.\nUse tree_depth/max_turns/max_results for task-level tuning; use exclude_paths to reduce payload or noise.\n- include_code_snippets: Default false (lightweight mode, ~2-5KB output). Set to true to include full code snippets in the response (~45KB output).\nResponse includes a [config] line showing actual parameters used — use this to decide adjustments on retry.";

        map.insert(
            ("user-fast-context".to_string(), "fast_context_search".to_string()),
            fast_context_desc.to_string(),
        );
        map.insert(
            ("fast-context".to_string(), "fast_context_search".to_string()),
            fast_context_desc.to_string(),
        );
        RwLock::new(map)
    });

pub fn get_mcp_tool_description(server_identifier: &str, tool_name: &str) -> Option<String> {
    let cache = MCP_DESCRIPTION_CACHE.read().ok()?;
    cache
        .get(&(server_identifier.to_string(), tool_name.to_string()))
        .cloned()
        .or_else(|| {
            let stripped = server_identifier.strip_prefix("user-").unwrap_or(server_identifier);
            cache.get(&(stripped.to_string(), tool_name.to_string())).cloned()
        })
}

pub fn cache_mcp_tool_description(server_identifier: &str, tool_name: &str, description: &str) {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Ok(mut cache) = MCP_DESCRIPTION_CACHE.write() {
        cache.insert(
            (server_identifier.to_string(), tool_name.to_string()),
            trimmed.to_string(),
        );
        if let Some(stripped) = server_identifier.strip_prefix("user-") {
            cache.insert(
                (stripped.to_string(), tool_name.to_string()),
                trimmed.to_string(),
            );
        }
    }
}

fn compile_mcp_descriptor(server: &pb::McpDescriptor) -> Option<String> {
    if server.server_identifier.trim().is_empty() {
        return None;
    }
    let tools = server
        .tools
        .iter()
        .filter(|tool| !tool.tool_name.trim().is_empty())
        .map(|tool| {
            let mut lines = vec![format!("<mcp_tool name=\"{}\">", xml(&tool.tool_name))];
            if let Some(path) = tool
                .definition_path
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                lines.push(format!("<definition_path>{}</definition_path>", xml(path)));
            }
            let description = tool
                .description
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|d| {
                    cache_mcp_tool_description(&server.server_identifier, &tool.tool_name, d);
                    d.to_string()
                })
                .or_else(|| get_mcp_tool_description(&server.server_identifier, &tool.tool_name))
                .or_else(|| get_mcp_tool_description(&server.server_name, &tool.tool_name));

            if let Some(desc) = description {
                lines.push(format!("<description>{}</description>", xml(&desc)));
            }
            if let Some(schema) = mcp_input_schema(tool) {
                lines.push(format!("<input_schema>{}</input_schema>", xml(&schema)));
            }
            lines.push("</mcp_tool>".into());
            lines.join("\n")
        })
        .collect::<Vec<_>>();
    if tools.is_empty() {
        return None;
    }
    Some(format!(
        "<mcp_meta_tool_server name=\"{}\" identifier=\"{}\">\n<tools>\n{}\n</tools>\n</mcp_meta_tool_server>",
        xml(if server.server_name.trim().is_empty() {
            &server.server_identifier
        } else {
            &server.server_name
        }),
        xml(&server.server_identifier),
        tools.join("\n"),
    ))
}

fn mcp_input_schema(tool: &pb::McpToolDescriptor) -> Option<String> {
    tool.input_schema_json
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            serde_json::from_str::<Value>(value)
                .map(|value| value.to_string())
                .unwrap_or_else(|_| value.to_string())
        })
        .or_else(|| {
            tool.input_schema
                .as_ref()
                .map(prost_value)
                .map(|value| value.to_string())
        })
}

pub fn meta_mcp_routes(context: &pb::RequestContext) -> HashMap<(String, String), McpRoute> {
    context
        .mcp_meta_tool_options
        .as_ref()
        .into_iter()
        .flat_map(|options| &options.mcp_descriptors)
        .filter(|server| !server.server_identifier.trim().is_empty())
        .flat_map(|server| {
            server.tools.iter().filter_map(move |tool| {
                if tool.tool_name.trim().is_empty() {
                    return None;
                }
                let description = tool
                    .description
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| get_mcp_tool_description(&server.server_identifier, &tool.tool_name))
                    .or_else(|| get_mcp_tool_description(&server.server_name, &tool.tool_name))
                    .unwrap_or_default();
                Some((
                    (server.server_identifier.clone(), tool.tool_name.clone()),
                    McpRoute {
                        name: format!("{}-{}", server.server_identifier, tool.tool_name),
                        provider_identifier: server.server_identifier.clone(),
                        server_identifier: server.server_name.clone(),
                        tool_name: tool.tool_name.clone(),
                        description,
                    },
                ))
            })
        })
        .collect()
}

fn is_skill_rule(rule: &pb::CursorRule) -> bool {
    let path = Path::new(&rule.full_path);
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
        || rule.full_path.replace('\\', "/").contains("/skills/")
}

#[allow(dead_code)]
pub fn selected_context(user: &pb::UserMessage) -> Option<String> {
    let selected = user.selected_context.as_ref()?;
    let mut sections = selected.extra_context.clone();
    sections.extend(
        selected
            .files
            .iter()
            .map(|file| format!("<file path=\"{}\">\n{}\n</file>", file.path, file.content)),
    );
    sections.extend(
        selected
            .code_selections
            .iter()
            .map(|value| format!("<code path=\"{}\">\n{}\n</code>", value.path, value.content)),
    );
    sections.extend(selected.terminals.iter().map(|value| {
        format!(
            "<terminal title=\"{}\">\n{}\n</terminal>",
            value.title.as_deref().unwrap_or_default(),
            value.content
        )
    }));
    sections.extend(selected.terminal_selections.iter().map(|value| {
        format!(
            "<terminal_selection title=\"{}\">\n{}\n</terminal_selection>",
            value.title.as_deref().unwrap_or_default(),
            value.content
        )
    }));
    sections.extend(selected.cursor_rules.iter().filter_map(|value| {
        value.rule.as_ref().map(|rule| {
            format!(
                "<rule path=\"{}\">\n{}\n</rule>",
                rule.full_path, rule.content
            )
        })
    }));
    sections.extend(selected.cursor_commands.iter().map(|value| {
        format!(
            "<command name=\"{}\">\n{}\n</command>",
            value.name, value.content
        )
    }));
    sections.extend(selected.selected_skills.iter().map(|value| {
        format!(
            "<skill path=\"{}\">\n{}\n{}\n</skill>",
            value.full_path, value.description, value.content
        )
    }));
    sections.extend(selected.external_links.iter().map(|value| {
        format!(
            "External link: {}{}",
            value.url,
            value
                .pdf_content
                .as_deref()
                .map(|content| format!("\n{content}"))
                .unwrap_or_default()
        )
    }));
    Some(sections.join("\n\n"))
}

pub fn dynamic_mcp(
    request: &pb::AgentRunRequest,
    context: &pb::RequestContext,
) -> Result<BTreeMap<String, (pb::McpToolDefinition, ToolDefinition)>> {
    let direct = request
        .mcp_tools
        .iter()
        .flat_map(|tools| tools.mcp_tools.iter());
    let contextual = context.tools.iter();
    let mut output = BTreeMap::new();
    for wire in direct.chain(contextual) {
        if wire.name.is_empty() {
            return Err(Error::Protocol(
                "MCP tool definition is missing name".into(),
            ));
        }
        let parameters = match wire.input_schema_json.as_deref() {
            Some(json) if !json.trim().is_empty() => serde_json::from_str(json)?,
            _ => prost_value(wire.input_schema.as_ref().ok_or_else(|| {
                Error::Protocol(format!("MCP tool {} is missing input schema", wire.name))
            })?),
        };
        let parameters = normalize_mcp_parameters(&wire.name, parameters)?;
        let name = normalize_tool_name(&wire.name);
        let definition = ToolDefinition {
            name: name.clone(),
            description: wire.description.clone(),
            parameters,
        };
        if output
            .insert(name.clone(), (wire.clone(), definition))
            .is_some()
        {
            return Err(Error::Protocol(format!(
                "duplicate MCP tool name after normalization: {name}"
            )));
        }
    }
    Ok(output)
}

fn normalize_mcp_parameters(tool_name: &str, mut parameters: Value) -> Result<Value> {
    let schema = parameters
        .as_object_mut()
        .ok_or_else(|| invalid_mcp_parameters(tool_name))?;
    match schema.get("type") {
        Some(Value::String(schema_type)) if schema_type == "object" => return Ok(parameters),
        Some(_) => return Err(invalid_mcp_parameters(tool_name)),
        None => {}
    }
    let object_only_union = ["anyOf", "oneOf"].into_iter().any(|keyword| {
        schema
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                !branches.is_empty()
                    && branches.iter().all(|branch| {
                        branch
                            .as_object()
                            .and_then(|branch| branch.get("type"))
                            .and_then(Value::as_str)
                            == Some("object")
                    })
            })
    });
    if !object_only_union {
        return Err(invalid_mcp_parameters(tool_name));
    }
    // OpenAI-compatible function schemas (and the corresponding schema
    // validators used by other providers) require the root schema to declare
    // an object type. Cursor's app-control MCP sometimes sends an object-only
    // `anyOf`/`oneOf` schema without that root annotation. Preserve the union
    // while adding the annotation to the model-facing copy.
    schema.insert("type".into(), Value::String("object".into()));
    Ok(parameters)
}

fn invalid_mcp_parameters(tool_name: &str) -> Error {
    Error::Protocol(format!(
        "MCP tool {tool_name} input schema must describe an object"
    ))
}

fn prost_value(value: &prost_types::Value) -> Value {
    use prost_types::value::Kind;
    match value.kind.as_ref() {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::NumberValue(value)) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Some(Kind::StringValue(value)) => Value::String(value.clone()),
        Some(Kind::BoolValue(value)) => Value::Bool(*value),
        Some(Kind::StructValue(value)) => Value::Object(
            value
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), prost_value(value)))
                .collect(),
        ),
        Some(Kind::ListValue(value)) => {
            Value::Array(value.values.iter().map(prost_value).collect())
        }
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(content: &str) -> pb::CursorRule {
        pb::CursorRule {
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn merge_local_rules_appends_and_dedupes_by_content() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.md"), "shared rule").unwrap();
        std::fs::write(directory.path().join("b.md"), "local only rule").unwrap();
        std::fs::write(directory.path().join("c.md"), "   \n").unwrap();

        let mut context = pb::RequestContext {
            non_file_rules: vec![rule("  shared rule  ")],
            ..Default::default()
        };
        merge_local_rules(&mut context, directory.path());

        let contents = context
            .non_file_rules
            .iter()
            .map(|rule| rule.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            contents,
            ["  shared rule  ", "local only rule"],
            "IDE-sent duplicate is kept once and blank local rules are skipped"
        );
    }

    #[test]
    fn merge_local_rules_survives_a_missing_directory() {
        let directory = tempfile::tempdir().unwrap();
        let mut context = pb::RequestContext::default();
        merge_local_rules(&mut context, &directory.path().join("nested/rules"));
        assert!(context.non_file_rules.is_empty());
    }

    #[test]
    fn merge_local_skills_discovers_and_parses_frontmatter() {
        let directory = tempfile::tempdir().unwrap();
        let skill_dir = directory.path().join("frontend");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: frontend\ndescription: Frontend skill description\ndisable-model-invocation: false\n---\n# Frontend Content\nBody here",
        )
        .unwrap();

        let standalone_file = directory.path().join("debug.md");
        std::fs::write(
            standalone_file,
            "---\nname: debug\ndescription: Debug skill description\n---\nBody here",
        )
        .unwrap();

        let mut context = pb::RequestContext::default();
        merge_skills_from_directories(&mut context, &[directory.path().to_path_buf()]);

        assert_eq!(context.agent_skills.len(), 2);
        let names = context
            .agent_skills
            .iter()
            .map(|s| s.description.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"Frontend skill description"));
        assert!(names.contains(&"Debug skill description"));
    }

    #[test]
    fn merge_local_skills_dedupes_by_name_and_path() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        let skill_dir1 = dir1.path().join("custom");
        std::fs::create_dir_all(&skill_dir1).unwrap();
        std::fs::write(
            skill_dir1.join("SKILL.md"),
            "---\nname: custom\ndescription: Custom 1\n---\nBody 1",
        )
        .unwrap();

        let skill_dir2 = dir2.path().join("custom");
        std::fs::create_dir_all(&skill_dir2).unwrap();
        std::fs::write(
            skill_dir2.join("SKILL.md"),
            "---\nname: custom\ndescription: Custom 2\n---\nBody 2",
        )
        .unwrap();

        let mut context = pb::RequestContext::default();
        merge_skills_from_directories(
            &mut context,
            &[dir1.path().to_path_buf(), dir2.path().to_path_buf()],
        );

        assert_eq!(context.agent_skills.len(), 1);
        assert_eq!(context.agent_skills[0].description, "Custom 1");
    }

    #[test]
    fn compile_mcp_descriptor_uses_cached_description_when_missing_in_wire() {
        let server = pb::McpDescriptor {
            server_identifier: "user-fast-context".into(),
            server_name: "fast-context".into(),
            tools: vec![pb::McpToolDescriptor {
                tool_name: "fast_context_search".into(),
                description: None,
                definition_path: None,
                input_schema: None,
                input_schema_json: None,
                annotations_json: None,
            }],
            ..Default::default()
        };

        let compiled = compile_mcp_descriptor(&server).expect("descriptor should compile");
        assert!(compiled.contains("<mcp_tool name=\"fast_context_search\">"));
        assert!(compiled.contains("<description>Fast semantic codebase search and context discovery."));
    }

    #[test]
    fn dynamic_caching_and_lookup_of_mcp_tool_description() {
        cache_mcp_tool_description("custom-mcp", "my_custom_tool", "Description of custom tool");
        let desc = get_mcp_tool_description("custom-mcp", "my_custom_tool");
        assert_eq!(desc.as_deref(), Some("Description of custom tool"));

        let server = pb::McpDescriptor {
            server_identifier: "custom-mcp".into(),
            server_name: "custom-mcp".into(),
            tools: vec![pb::McpToolDescriptor {
                tool_name: "my_custom_tool".into(),
                description: None,
                ..Default::default()
            }],
            ..Default::default()
        };

        let compiled = compile_mcp_descriptor(&server).unwrap();
        assert!(compiled.contains("<description>Description of custom tool</description>"));
    }
}
