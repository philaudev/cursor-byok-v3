//! Tracks running Tool executions and coordinates cancellation and cleanup.
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use tokio::sync::Mutex;

use crate::{cursor::protocol::proto::agent::v1 as pb, model::ToolCall, Error, Result};

use super::edit::EditWrite;

pub(crate) const DEFAULT_SHELL_BLOCK_UNTIL_MS: u64 = 30_000;
pub(crate) const MAX_SHELL_BLOCK_UNTIL_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackgroundShellStatus {
    Backgrounded,
    Running,
    Completed,
    Rejected,
    PermissionDenied,
    TransportClosed,
}

impl BackgroundShellStatus {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Rejected | Self::PermissionDenied | Self::TransportClosed
        )
    }
}

#[derive(Clone, Debug)]
pub struct BackgroundShellState {
    pub shell_id: String,
    pub pid: Option<u32>,
    pub status: BackgroundShellStatus,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[cfg(windows)]
pub fn is_pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            let error = GetLastError();
            // If access is denied, the process exists and is alive (we just lack permissions to query).
            // Otherwise (e.g. ERROR_INVALID_PARAMETER / not found), the PID does not exist.
            return error == ERROR_ACCESS_DENIED;
        }
        let mut exit_code: u32 = 0;
        let success = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        if success != 0 {
            exit_code == STILL_ACTIVE as u32
        } else {
            false
        }
    }
}

#[cfg(unix)]
pub fn is_pid_alive(pid: u32) -> bool {
    let res = unsafe { libc::kill(pid as i32, 0) };
    if res == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(any(windows, unix)))]
pub fn is_pid_alive(_pid: u32) -> bool {
    true
}

#[derive(Clone, Default)]
pub struct CursorToolRuntime {
    next_id: Arc<AtomicU32>,
    execs: Arc<Mutex<HashMap<u32, PendingExec>>>,
    background_shells: Arc<Mutex<HashMap<String, BackgroundShellState>>>,
    background_shell_execs: Arc<Mutex<HashMap<String, String>>>,
    background_shell_message_ids: Arc<Mutex<HashMap<u32, String>>>,
    interactions: Arc<Mutex<HashMap<u32, PendingInteraction>>>,
    completed: Arc<Mutex<HashMap<u32, String>>>,
    interrupted: Arc<Mutex<HashSet<u32>>>,
}

impl CursorToolRuntime {
    pub fn new() -> Self {
        Self {
            next_id: Arc::new(AtomicU32::new(0)),
            execs: Arc::new(Mutex::new(HashMap::new())),
            background_shells: Arc::new(Mutex::new(HashMap::new())),
            background_shell_execs: Arc::new(Mutex::new(HashMap::new())),
            background_shell_message_ids: Arc::new(Mutex::new(HashMap::new())),
            interactions: Arc::new(Mutex::new(HashMap::new())),
            completed: Arc::new(Mutex::new(HashMap::new())),
            interrupted: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn with_shared_ids(next_id: Arc<AtomicU32>) -> Self {
        Self {
            next_id,
            execs: Arc::new(Mutex::new(HashMap::new())),
            background_shells: Arc::new(Mutex::new(HashMap::new())),
            background_shell_execs: Arc::new(Mutex::new(HashMap::new())),
            background_shell_message_ids: Arc::new(Mutex::new(HashMap::new())),
            interactions: Arc::new(Mutex::new(HashMap::new())),
            completed: Arc::new(Mutex::new(HashMap::new())),
            interrupted: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn with_shared_background_state(
        next_id: Arc<AtomicU32>,
        background_shells: Arc<Mutex<HashMap<String, BackgroundShellState>>>,
        background_shell_execs: Arc<Mutex<HashMap<String, String>>>,
        background_shell_message_ids: Arc<Mutex<HashMap<u32, String>>>,
    ) -> Self {
        Self {
            next_id,
            execs: Arc::new(Mutex::new(HashMap::new())),
            background_shells,
            background_shell_execs,
            background_shell_message_ids,
            interactions: Arc::new(Mutex::new(HashMap::new())),
            completed: Arc::new(Mutex::new(HashMap::new())),
            interrupted: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

pub(crate) struct PendingExec {
    pub call: ToolCall,
    pub context: ExecContext,
    pub started_at_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stage: ExecStage,
    pub transport_closed: bool,
}

pub(crate) enum ExecStage {
    Direct,
    DynamicMcp(pb::McpToolDefinition),
    EditRead,
    EditWrite(EditWrite),
    Await(AwaitState),
}

pub(crate) struct AwaitState {
    pub deadline: Instant,
    pub output_file_path: String,
    pub task_id: String,
    pub regex: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExecContext {
    pub workspace_paths: Vec<String>,
    pub conversation_id: String,
    pub root_conversation_id: String,
    pub default_subagent_model: String,
    pub subagent_model: Option<SubagentModel>,
    pub subagent_models: HashMap<String, SubagentModel>,
    pub custom_subagents: Vec<pb::CustomSubagent>,
    pub allow_subagents: bool,
    pub subagents_disabled: bool,
    pub terminals_folder: String,
    pub admin_command_denylist: Vec<String>,
    pub mcp_routes: HashMap<(String, String), McpRoute>,
}

#[derive(Clone, Debug)]
pub struct McpRoute {
    pub name: String,
    pub provider_identifier: String,
    pub server_identifier: String,
    pub tool_name: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub enum SubagentModel {
    Model(String),
    Disabled,
}

impl ExecContext {
    pub fn task_disabled(&self, call: &ToolCall) -> bool {
        if !call.name.eq_ignore_ascii_case("Task") {
            return false;
        }
        if self.subagents_disabled || matches!(self.subagent_model, Some(SubagentModel::Disabled)) {
            return true;
        }
        let subagent_type = call
            .arguments
            .get("subagent_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("generalPurpose");
        matches!(
            self.subagent_models.get(subagent_type),
            Some(SubagentModel::Disabled)
        )
    }

    pub fn prepare_call(&self, call: &ToolCall) -> Result<ToolCall> {
        let mut prepared = call.clone();
        if prepared.name.eq_ignore_ascii_case("Shell")
            || prepared.name.eq_ignore_ascii_case("AwaitShell")
        {
            normalize_shell_block_until_ms(&mut prepared)?;
        }
        if !prepared.name.eq_ignore_ascii_case("Task") {
            return Ok(prepared);
        }
        let arguments = prepared
            .arguments
            .as_object()
            .ok_or_else(|| Error::Protocol("Task arguments must be a JSON object".into()))?;
        let subagent_type = arguments
            .get("subagent_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("generalPurpose");
        if self.task_disabled(&prepared) {
            return Ok(prepared);
        }
        let custom_subagent = self
            .custom_subagents
            .iter()
            .find(|agent| agent.name == subagent_type);
        let model = match self.subagent_models.get(subagent_type) {
            Some(SubagentModel::Model(model)) => model.clone(),
            Some(SubagentModel::Disabled) => return Ok(prepared),
            None => match self.subagent_models.get("explore") {
                Some(SubagentModel::Model(model)) => model.clone(),
                _ => match custom_subagent {
                    Some(agent)
                        if !agent.force_default_model && valid_custom_model(&agent.model) =>
                    {
                        agent.model.clone()
                    }
                    _ => match &self.subagent_model {
                        Some(SubagentModel::Model(model)) => model.clone(),
                        Some(SubagentModel::Disabled) => {
                            unreachable!("disabled Task returned above")
                        }
                        None => arguments
                            .get("model")
                            .and_then(serde_json::Value::as_str)
                            .filter(|model| *model != "inherit")
                            .unwrap_or(&self.default_subagent_model)
                            .to_string(),
                    },
                },
            },
        };
        if model.is_empty() {
            return Err(Error::Protocol(format!(
                "Task subagent type {subagent_type} has no model"
            )));
        }
        prepared
            .arguments
            .as_object_mut()
            .expect("Task arguments were validated")
            .insert("model".into(), serde_json::Value::String(model));
        Ok(prepared)
    }
}

fn valid_custom_model(value: &str) -> bool {
    !value.trim().is_empty() && value != "inherit"
}

fn normalize_shell_block_until_ms(call: &mut ToolCall) -> Result<()> {
    let tool_name = call.name.clone();
    let arguments = call
        .arguments
        .as_object_mut()
        .ok_or_else(|| Error::Protocol(format!("{tool_name} arguments must be a JSON object")))?;
    let Some(value) = arguments.get("block_until_ms") else {
        return Ok(());
    };
    let value = value.as_u64().ok_or_else(|| {
        Error::Protocol(format!(
            "{tool_name} block_until_ms must be a non-negative integer"
        ))
    })?;
    if value > MAX_SHELL_BLOCK_UNTIL_MS {
        arguments.insert(
            "block_until_ms".into(),
            serde_json::Value::from(MAX_SHELL_BLOCK_UNTIL_MS),
        );
    }
    Ok(())
}

pub(crate) struct PendingInteraction {
    pub call: ToolCall,
    pub started_at_ms: u64,
}

impl CursorToolRuntime {
    pub(crate) fn next_run(&self) -> Self {
        Self {
            next_id: self.next_id.clone(),
            execs: Arc::new(Mutex::new(HashMap::new())),
            background_shells: self.background_shells.clone(),
            background_shell_execs: self.background_shell_execs.clone(),
            background_shell_message_ids: self.background_shell_message_ids.clone(),
            interactions: Arc::new(Mutex::new(HashMap::new())),
            completed: Arc::new(Mutex::new(HashMap::new())),
            interrupted: self.interrupted.clone(),
        }
    }

    pub async fn reserve_exec(&self, call: &ToolCall, context: &ExecContext) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::Direct, None)
            .await
    }

    pub(crate) async fn reserve_dynamic_mcp(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        definition: &pb::McpToolDefinition,
    ) -> Result<u32> {
        self.reserve_exec_stage(
            call,
            context,
            ExecStage::DynamicMcp(definition.clone()),
            None,
        )
        .await
    }

    pub(crate) async fn reserve_edit_read(
        &self,
        call: &ToolCall,
        context: &ExecContext,
    ) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::EditRead, None)
            .await
    }

    pub(crate) async fn reserve_edit_write(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        write: EditWrite,
        started_at_ms: u64,
    ) -> Result<u32> {
        self.reserve_exec_stage(
            call,
            context,
            ExecStage::EditWrite(write),
            Some(started_at_ms),
        )
        .await
    }

    pub(crate) async fn reserve_await(
        &self,
        call: &ToolCall,
        context: &ExecContext,
    ) -> Result<u32> {
        let task_id = call
            .arguments
            .get("shell_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::Protocol("AwaitShell is missing shell_id".into()))?;
        let block_ms = call
            .arguments
            .get("block_until_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(DEFAULT_SHELL_BLOCK_UNTIL_MS);
        if block_ms > MAX_SHELL_BLOCK_UNTIL_MS {
            return Err(Error::Protocol(format!(
                "AwaitShell block_until_ms exceeds {MAX_SHELL_BLOCK_UNTIL_MS}"
            )));
        }
        let output_file_path = format!(
            "{}/{}.txt",
            context.terminals_folder.trim_end_matches('/'),
            task_id
        );
        self.reserve_exec_stage(
            call,
            context,
            ExecStage::Await(AwaitState {
                deadline: Instant::now() + std::time::Duration::from_millis(block_ms),
                output_file_path,
                task_id: task_id.to_string(),
                regex: call
                    .arguments
                    .get("pattern")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }),
            None,
        )
        .await
    }

    pub(crate) async fn reserve_await_again(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        state: AwaitState,
        started_at_ms: u64,
    ) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::Await(state), Some(started_at_ms))
            .await
    }

    async fn reserve_exec_stage(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        stage: ExecStage,
        started_at_ms: Option<u64>,
    ) -> Result<u32> {
        let id = self.next_id()?;
        self.execs.lock().await.insert(
            id,
            PendingExec {
                call: call.clone(),
                context: context.clone(),
                started_at_ms: started_at_ms.unwrap_or_else(now_ms),
                stdout: String::new(),
                stderr: String::new(),
                stage,
                transport_closed: false,
            },
        );
        Ok(id)
    }

    pub async fn reserve_interaction(&self, call: &ToolCall) -> Result<u32> {
        let id = self.next_id()?;
        self.interactions.lock().await.insert(
            id,
            PendingInteraction {
                call: call.clone(),
                started_at_ms: now_ms(),
            },
        );
        Ok(id)
    }

    pub async fn mark_background_shell_transport_closed(&self, message_id: u32) -> bool {
        let Some(shell_id) = self.background_shell_for_message(message_id).await else {
            return false;
        };
        let mut shells = self.background_shells.lock().await;
        let Some(shell) = shells.get_mut(&shell_id) else {
            return false;
        };
        if !matches!(shell.status, BackgroundShellStatus::Running) {
            return false;
        }
        shell.status = BackgroundShellStatus::TransportClosed;
        true
    }

    pub async fn observe_background_task_completion(
        &self,
        action: &pb::BackgroundTaskCompletionAction,
    ) {
        for completion in &action.completions {
            if completion.kind != pb::BackgroundTaskKind::Shell as i32
                || completion.task_id.is_empty()
            {
                continue;
            }
            let status = match pb::BackgroundTaskStatus::try_from(completion.status) {
                Ok(pb::BackgroundTaskStatus::Success) => BackgroundShellStatus::Completed,
                Ok(pb::BackgroundTaskStatus::Error) | Ok(pb::BackgroundTaskStatus::Aborted) => {
                    BackgroundShellStatus::TransportClosed
                }
                Ok(pb::BackgroundTaskStatus::Unspecified) | Err(_) => continue,
            };
            let detail = completion.detail.as_deref().unwrap_or_default();
            self.update_background_shell(&completion.task_id, |state| {
                if matches!(status, BackgroundShellStatus::TransportClosed) {
                    state.stderr.push_str(detail);
                } else {
                    state.stdout.push_str(detail);
                }
                state.status = status;
            })
            .await;
        }
    }

    pub(crate) async fn background_shell_for_event(
        &self,
        message_id: u32,
        exec_id: &str,
    ) -> Option<String> {
        if let Some(shell_id) = self.background_shell_for_exec(exec_id).await {
            return Some(shell_id);
        }
        self.background_shell_for_message(message_id).await
    }

    pub(crate) async fn background_shell_for_message(&self, message_id: u32) -> Option<String> {
        self.background_shell_message_ids
            .lock()
            .await
            .get(&message_id)
            .cloned()
    }

    pub(crate) async fn background_shell_for_exec(&self, exec_id: &str) -> Option<String> {
        self.background_shell_execs
            .lock()
            .await
            .get(exec_id)
            .cloned()
    }

    pub async fn background_shell(&self, shell_id: &str) -> Option<BackgroundShellState> {
        self.background_shells.lock().await.get(shell_id).cloned()
    }

    pub async fn background_shell_backgrounded(
        &self,
        shell_id: u32,
        pid: Option<u32>,
        message_id: u32,
        exec_id: &str,
        stdout: String,
        stderr: String,
    ) {
        let shell_id = shell_id.to_string();
        self.background_shell_execs
            .lock()
            .await
            .insert(exec_id.into(), shell_id.clone());
        self.background_shell_message_ids
            .lock()
            .await
            .insert(message_id, shell_id.clone());
        self.background_shells
            .lock()
            .await
            .entry(shell_id.clone())
            .or_insert(BackgroundShellState {
                shell_id,
                pid,
                status: BackgroundShellStatus::Backgrounded,
                stdout,
                stderr,
                exit_code: None,
            });
    }

    pub async fn background_shell_stdout(&self, shell_id: &str, data: &str) {
        self.update_background_shell(shell_id, |state| {
            state.stdout.push_str(data);
            if !state.status.is_terminal() {
                state.status = BackgroundShellStatus::Running;
            }
        })
        .await;
    }

    pub(crate) async fn background_shell_stderr(&self, shell_id: &str, data: &str) {
        self.update_background_shell(shell_id, |state| {
            state.stderr.push_str(data);
            if !state.status.is_terminal() {
                state.status = BackgroundShellStatus::Running;
            }
        })
        .await;
    }

    pub async fn background_shell_exit(&self, shell_id: &str, exit_code: i32) {
        self.update_background_shell(shell_id, |state| {
            state.exit_code = Some(exit_code);
            state.status = BackgroundShellStatus::Completed;
        })
        .await;
    }

    pub(crate) async fn background_shell_terminal(
        &self,
        shell_id: &str,
        status: BackgroundShellStatus,
    ) {
        self.update_background_shell(shell_id, |state| state.status = status)
            .await;
    }

    async fn update_background_shell(
        &self,
        shell_id: &str,
        update: impl FnOnce(&mut BackgroundShellState),
    ) {
        let mut shells = self.background_shells.lock().await;
        let state = shells
            .entry(shell_id.to_string())
            .or_insert_with(|| BackgroundShellState {
                shell_id: shell_id.into(),
                pid: None,
                status: BackgroundShellStatus::Running,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            });
        update(state);
    }

    pub async fn exec_call(&self, id: u32) -> Option<ToolCall> {
        self.execs
            .lock()
            .await
            .get(&id)
            .map(|entry| entry.call.clone())
    }

    pub(crate) async fn mark_transport_closed(&self, id: u32) -> bool {
        let mut entries = self.execs.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        if entry.transport_closed {
            return false;
        }
        entry.transport_closed = true;
        true
    }

    pub(crate) async fn take_transport_closed(&self, id: u32) -> Option<PendingExec> {
        let mut entries = self.execs.lock().await;
        let should_take = entries.get(&id).is_some_and(|entry| entry.transport_closed);
        if !should_take {
            return None;
        }
        let pending = entries.remove(&id);
        drop(entries);
        if let Some(pending) = &pending {
            self.completed
                .lock()
                .await
                .insert(id, pending.call.call_id.clone());
        }
        pending
    }
    pub async fn append_stdout(&self, id: u32, data: &str) -> bool {
        let mut entries = self.execs.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stdout.push_str(data);
        true
    }

    pub async fn append_stderr(&self, id: u32, data: &str) -> bool {
        let mut entries = self.execs.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stderr.push_str(data);
        true
    }

    pub(crate) async fn take_exec(&self, id: u32) -> Option<PendingExec> {
        let pending = self.execs.lock().await.remove(&id);
        if let Some(pending) = &pending {
            self.completed
                .lock()
                .await
                .insert(id, pending.call.call_id.clone());
        }
        pending
    }

    pub(crate) async fn take_interaction(&self, id: u32) -> Option<PendingInteraction> {
        let pending = self.interactions.lock().await.remove(&id);
        if let Some(pending) = &pending {
            self.completed
                .lock()
                .await
                .insert(id, pending.call.call_id.clone());
        }
        pending
    }

    pub async fn completed_call(&self, id: u32) -> Option<String> {
        self.completed.lock().await.get(&id).cloned()
    }

    pub async fn is_interrupted(&self, id: u32) -> bool {
        self.interrupted.lock().await.contains(&id)
    }

    pub async fn clear_completed(&self) {
        self.completed.lock().await.clear();
    }

    pub async fn discard_exec(&self, id: u32) {
        self.execs.lock().await.remove(&id);
    }

    pub async fn discard_interaction(&self, id: u32) {
        self.interactions.lock().await.remove(&id);
    }

    pub async fn drain_running(&self) -> Vec<u32> {
        let mut entries = self.execs.lock().await;
        let mut ids = entries.drain().map(|(id, _)| id).collect::<Vec<_>>();
        ids.sort_unstable();
        self.interactions.lock().await.clear();
        self.completed.lock().await.clear();
        self.interrupted.lock().await.clear();
        ids
    }

    pub async fn interrupt_for_run_replacement(&self) -> Vec<u32> {
        let mut execs = self.execs.lock().await;
        let mut abort_ids = execs.keys().copied().collect::<Vec<_>>();
        let mut interrupted_ids = abort_ids.clone();
        execs.clear();
        drop(execs);

        let mut interactions = self.interactions.lock().await;
        interrupted_ids.extend(interactions.keys().copied());
        interactions.clear();
        drop(interactions);

        self.completed.lock().await.clear();
        self.interrupted.lock().await.extend(interrupted_ids);
        abort_ids.sort_unstable();
        abort_ids
    }

    pub async fn interrupt_for_message(&self) -> Vec<u32> {
        let (abort_ids, interrupted_ids) = {
            let mut entries = self.execs.lock().await;
            let mut abort_ids = Vec::new();
            let mut interrupted_ids = Vec::new();
            entries.retain(|id, entry| {
                interrupted_ids.push(*id);
                let keep_running = entry.call.name.eq_ignore_ascii_case("Task");
                if !keep_running {
                    abort_ids.push(*id);
                }
                keep_running
            });
            (abort_ids, interrupted_ids)
        };
        let interaction_ids = {
            let mut interactions = self.interactions.lock().await;
            let ids = interactions.keys().copied().collect::<Vec<_>>();
            interactions.clear();
            ids
        };
        let mut interrupted = self.interrupted.lock().await;
        interrupted.extend(interrupted_ids);
        interrupted.extend(interaction_ids);
        let mut abort_ids = abort_ids;
        abort_ids.sort_unstable();
        abort_ids
    }

    pub async fn running_exec_ids(&self) -> Vec<u32> {
        let mut ids = self.execs.lock().await.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    pub async fn running_task_exec_id(&self, call_id: &str) -> Option<u32> {
        self.execs
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| {
                (entry.call.call_id == call_id && entry.call.name.eq_ignore_ascii_case("Task"))
                    .then_some(*id)
            })
            .min()
    }

    fn next_id(&self) -> Result<u32> {
        self.next_id
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("Cursor message id space exhausted".into()))
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[cfg(test)]
mod tests {
    use super::*;

    fn task(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            index: 0,
            call_id: "task-1".into(),
            model_call_id: "model-call-1".into(),
            name: "Task".into(),
            arguments_text: arguments.to_string(),
            arguments,
        }
    }

    #[tokio::test]
    async fn shared_runtime_ids_are_unique_across_sessions() {
        let next_id = Arc::new(AtomicU32::new(0));
        let first = CursorToolRuntime::with_shared_ids(next_id.clone());
        let second = CursorToolRuntime::with_shared_ids(next_id);
        let call = task(serde_json::json!({"prompt":"inspect", "model":"child-model"}));
        let context = ExecContext::default();

        let id1 = first.reserve_exec(&call, &context).await.unwrap();
        let id2 = second.reserve_interaction(&call).await.unwrap();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);

        // State isolation test: each session has its own pending execs map
        assert!(first.exec_call(id1).await.is_some());
        assert!(first.exec_call(id2).await.is_none());
        assert!(second.exec_call(id1).await.is_none());
    }

    #[test]
    fn shell_timeouts_are_clamped_without_affecting_background_execution() {
        let context = ExecContext::default();
        let oversized_shell = ToolCall {
            index: 0,
            call_id: "shell-1".into(),
            model_call_id: "model-call-1".into(),
            name: "Shell".into(),
            arguments_text: "{}".into(),
            arguments: serde_json::json!({"command": "cargo test", "block_until_ms": 7_140_000}),
        };
        let oversized_await = ToolCall {
            name: "AwaitShell".into(),
            arguments: serde_json::json!({"shell_id": "42", "block_until_ms": 7_140_000}),
            ..oversized_shell.clone()
        };

        assert_eq!(
            context.prepare_call(&oversized_shell).unwrap().arguments["block_until_ms"],
            MAX_SHELL_BLOCK_UNTIL_MS
        );
        assert_eq!(
            context.prepare_call(&oversized_await).unwrap().arguments["block_until_ms"],
            MAX_SHELL_BLOCK_UNTIL_MS
        );

        let background_shell = ToolCall {
            arguments: serde_json::json!({"command": "npm run dev", "block_until_ms": 0}),
            ..oversized_shell
        };
        assert_eq!(
            context.prepare_call(&background_shell).unwrap().arguments["block_until_ms"],
            0
        );
    }

    #[test]
    fn task_model_defaults_to_parent_and_honors_an_explicit_model() {
        let context = ExecContext {
            default_subagent_model: "parent-model".into(),
            ..ExecContext::default()
        };
        let inherited = context
            .prepare_call(&task(serde_json::json!({"prompt":"inspect"})))
            .unwrap();
        let explicit = context
            .prepare_call(&task(serde_json::json!({
                "prompt":"inspect",
                "model":"child-model"
            })))
            .unwrap();

        assert_eq!(inherited.arguments["model"], "parent-model");
        assert_eq!(explicit.arguments["model"], "child-model");
    }

    #[test]
    fn global_subagent_model_applies_to_every_task_type() {
        let context = ExecContext {
            default_subagent_model: "parent-model".into(),
            subagent_model: Some(SubagentModel::Model("child-model".into())),
            ..ExecContext::default()
        };
        let call = task(serde_json::json!({
            "prompt":"inspect",
            "subagent_type":"test-subagent"
        }));

        assert_eq!(
            context.prepare_call(&call).unwrap().arguments["model"],
            "child-model"
        );
    }

    #[test]
    fn custom_subagent_model_and_named_override_take_precedence() {
        let context = ExecContext {
            default_subagent_model: "parent-model".into(),
            custom_subagents: vec![pb::CustomSubagent {
                name: "advisor".into(),
                model: "advisor-model".into(),
                ..Default::default()
            }],
            ..ExecContext::default()
        };
        let call = task(serde_json::json!({"prompt":"inspect", "subagent_type":"advisor"}));
        assert_eq!(
            context.prepare_call(&call).unwrap().arguments["model"],
            "advisor-model"
        );

        let context = ExecContext {
            subagent_models: HashMap::from([(
                "explore".into(),
                SubagentModel::Model("explore-model".into()),
            )]),
            ..context
        };
        assert_eq!(
            context.prepare_call(&call).unwrap().arguments["model"],
            "explore-model"
        );

        let context = ExecContext {
            subagent_models: HashMap::from([(
                "advisor".into(),
                SubagentModel::Model("override-model".into()),
            )]),
            ..context
        };
        assert_eq!(
            context.prepare_call(&call).unwrap().arguments["model"],
            "override-model"
        );
    }

    #[test]
    fn disabled_named_subagent_is_not_prepared() {
        let context = ExecContext {
            subagent_models: HashMap::from([("advisor".into(), SubagentModel::Disabled)]),
            ..ExecContext::default()
        };
        let call = task(serde_json::json!({"prompt":"inspect", "subagent_type":"advisor"}));
        assert!(context.task_disabled(&call));
        assert!(context
            .prepare_call(&call)
            .unwrap()
            .arguments
            .get("model")
            .is_none());
    }

    #[test]
    fn disabled_subagents_disable_every_task_type() {
        let context = ExecContext {
            default_subagent_model: "parent-model".into(),
            subagent_model: Some(SubagentModel::Disabled),
            ..ExecContext::default()
        };
        let call = task(serde_json::json!({
            "prompt":"inspect",
            "subagent_type":"test-subagent"
        }));

        assert!(context.task_disabled(&call));
        assert!(context
            .prepare_call(&call)
            .unwrap()
            .arguments
            .get("model")
            .is_none());
    }
}
