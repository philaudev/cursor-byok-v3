use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{
    model::{ModelSpec, PromptSpec, ToolDefinition},
    Error, Result,
};

use super::{assets::runtime_expression, Mode, PromptAssets};

#[derive(Clone)]
pub struct PromptCompiler {
    assets: PromptAssets,
    compaction_prompt_path: Option<PathBuf>,
}

impl PromptCompiler {
    pub fn new(assets: PromptAssets) -> Self {
        Self {
            assets,
            compaction_prompt_path: None,
        }
    }

    pub fn with_compaction_prompt_path(assets: PromptAssets, path: impl Into<PathBuf>) -> Self {
        Self {
            assets,
            compaction_prompt_path: Some(path.into()),
        }
    }

    pub fn runtime_message(&self, mode: Mode, values: &BTreeMap<&str, String>) -> Result<String> {
        render(&self.assets.mode(mode).runtime, values)
    }

    pub fn prompt_spec(
        &self,
        mode: Mode,
        model: &ModelSpec,
        dynamic_tools: &[ToolDefinition],
        suppress_subagent_progress: bool,
    ) -> Result<PromptSpec> {
        let mut tools = self.tools(mode, suppress_subagent_progress);
        let mut dynamic_tools = dynamic_tools.to_vec();
        dynamic_tools.sort_by(|left, right| left.name.cmp(&right.name));
        append_dynamic_tools(&mut tools, dynamic_tools)?;
        if !model.supports_image_generation {
            tools.retain(|tool| tool.name != "GenerateImage");
        }
        let fake_model_name = model
            .display_name
            .as_deref()
            .unwrap_or(model.model_id.as_str());
        Ok(PromptSpec {
            instructions: self.instructions(mode, fake_model_name)?,
            tools,
        })
    }

    fn instructions(&self, mode: Mode, fake_model_name: &str) -> Result<String> {
        let prompt = match (mode, &self.compaction_prompt_path) {
            (Mode::Compaction, Some(path)) => {
                let prompt = read_compaction_prompt(path)?;
                if prompt.trim().is_empty() {
                    self.assets.mode(Mode::Compaction).prompt.clone()
                } else {
                    prompt
                }
            }
            _ => self.assets.mode(mode).prompt.clone(),
        };
        Ok(prompt.replace("{{FAKE_MODEL_NAME}}", fake_model_name))
    }

    fn tools(&self, mode: Mode, suppress_subagent_progress: bool) -> Vec<ToolDefinition> {
        let mut tools = self.assets.mode(mode).tools.clone();
        if mode == Mode::Subagent && suppress_subagent_progress {
            tools.retain(|tool| tool.name != "UpdateCurrentStep");
        }
        tools
    }
}

fn read_compaction_prompt(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|error| {
        Error::Config(format!(
            "cannot read compaction prompt at {}: {error}",
            path.display()
        ))
    })
}

fn render(template: &str, values: &BTreeMap<&str, String>) -> Result<String> {
    let expression = runtime_expression();
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    for capture in expression.captures_iter(template) {
        let token = capture.get(0).expect("runtime template token");
        let name = &capture[1];
        let value = values
            .get(name)
            .ok_or_else(|| Error::Protocol(format!("runtime template value is missing: {name}")))?;
        output.push_str(&template[cursor..token.start()]);
        output.push_str(value);
        cursor = token.end();
    }
    output.push_str(&template[cursor..]);
    Ok(output.trim().to_string())
}

fn append_dynamic_tools(
    tools: &mut Vec<ToolDefinition>,
    additions: Vec<ToolDefinition>,
) -> Result<()> {
    for tool in additions {
        if tools.iter().any(|existing| existing.name == tool.name) {
            return Err(Error::Protocol(format!(
                "dynamic MCP tool conflicts with a mode tool: {}",
                tool.name
            )));
        }
        tools.push(tool);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn compaction_prompt_error_identifies_the_configured_file() {
        let path = std::path::PathBuf::from("missing-compaction.md");
        let error = read_compaction_prompt(&path).unwrap_err();
        assert!(error.to_string().contains("missing-compaction.md"));
    }

    #[test]
    fn render_replaces_runtime_variables() {
        let values = BTreeMap::from([("USER_QUERY", "hello".to_string())]);
        assert_eq!(
            render("before {{USER_QUERY}} after", &values).unwrap(),
            "before hello after"
        );
    }

    #[test]
    fn empty_runtime_compaction_prompt_uses_embedded_default() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("compaction.md");
        fs::write(&path, " \n\t").unwrap();
        let assets = PromptAssets::embedded().unwrap();
        let expected = assets.mode(Mode::Compaction).prompt.clone();
        let compiler = PromptCompiler::with_compaction_prompt_path(assets, path);

        assert_eq!(
            compiler.instructions(Mode::Compaction, "model").unwrap(),
            expected.replace("{{FAKE_MODEL_NAME}}", "model")
        );
    }

    #[test]
    fn compaction_prompt_content_can_change_between_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("compaction.md");
        fs::write(&path, "first").unwrap();
        assert_eq!(read_compaction_prompt(&path).unwrap(), "first");
        fs::write(&path, "second").unwrap();
        assert_eq!(read_compaction_prompt(&path).unwrap(), "second");
    }
}
