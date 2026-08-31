//! Decodes Tool execution responses received from Cursor.
use crate::{
    cursor::{
        protocol::{events, proto::agent::v1 as pb},
        tools::{
            edit,
            runtime::{BackgroundShellStatus, CursorToolRuntime, ExecStage, PendingExec},
            tool_call_result::{self as result, ToolCompletion},
        },
    },
    model::ToolCall,
    Error, Result,
};

use super::request::{await_read_request, edit_write_request};

pub const NON_STREAMING_CLOSE_GRACE: std::time::Duration =
    std::time::Duration::from_millis(1500);

pub enum ClientExecEvent {
    Delta(Box<pb::AgentServerMessage>),
    Message(Box<pb::AgentServerMessage>),
    Completed(Box<ToolCompletion>),
    Pending,
}

pub async fn client_event(
    message: &pb::ExecClientMessage,
    pending: &CursorToolRuntime,
) -> Result<ClientExecEvent> {
    if pending.is_interrupted(message.id).await {
        if message.message.as_ref().is_some_and(is_terminal) {
            pending.discard_exec(message.id).await;
        }
        return Ok(ClientExecEvent::Pending);
    }
    let call = match pending.exec_call(message.id).await {
        Some(call) => call,
        None if pending
            .background_shell_for_event(message.id, &message.exec_id)
            .await
            .is_some() =>
        {
            return background_shell_event(message, pending).await;
        }
        None if pending.completed_call(message.id).await.is_some() => {
            return Err(Error::Protocol(format!(
                "duplicate terminal ExecClientMessage id: {}",
                message.id
            )))
        }
        None => {
            return Err(Error::Protocol(format!(
                "unknown ExecClientMessage id: {}",
                message.id
            )))
        }
    };
    let Some(wire_result) = &message.message else {
        return Ok(ClientExecEvent::Pending);
    };
    let pb::exec_client_message::Message::ShellStream(stream) = wire_result else {
        let entry = take(message.id, pending).await?;
        return match entry.stage {
            ExecStage::EditRead => advance_edit(entry, wire_result, pending).await,
            ExecStage::Await(_) => advance_await(entry, wire_result, pending).await,
            ExecStage::Direct | ExecStage::DynamicMcp(_) | ExecStage::EditWrite(_) => {
                completed(entry, wire_result.clone())
            }
        };
    };
    use pb::shell_stream::Event;
    let event = match &stream.event {
        Some(Event::Stdout(stdout)) => {
            if pending.append_stdout(message.id, &stdout.data).await {
                if let Some(shell_id) = pending.background_shell_for_exec(&message.exec_id).await {
                    pending
                        .background_shell_stdout(&shell_id, &stdout.data)
                        .await;
                }
                ClientExecEvent::Delta(Box::new(shell_delta(&call, true, &stdout.data)))
            } else {
                ClientExecEvent::Pending
            }
        }
        Some(Event::Stderr(stderr)) => {
            if pending.append_stderr(message.id, &stderr.data).await {
                if let Some(shell_id) = pending.background_shell_for_exec(&message.exec_id).await {
                    pending
                        .background_shell_stderr(&shell_id, &stderr.data)
                        .await;
                }
                ClientExecEvent::Delta(Box::new(shell_delta(&call, false, &stderr.data)))
            } else {
                ClientExecEvent::Pending
            }
        }
        Some(Event::Start(_)) | Some(Event::HookContext(_)) => ClientExecEvent::Pending,
        Some(Event::Exit(exit)) => {
            if let Some(shell_id) = pending.background_shell_for_exec(&message.exec_id).await {
                pending
                    .background_shell_exit(&shell_id, exit.code as i32)
                    .await;
            }
            let entry = take(message.id, pending).await?;
            let result = shell_exit_result(message, exit, &entry.stdout, &entry.stderr);
            completed(entry, pb::exec_client_message::Message::ShellResult(result))?
        }
        Some(Event::Backgrounded(backgrounded)) => {
            let entry = take(message.id, pending).await?;
            pending
                .background_shell_backgrounded(
                    backgrounded.shell_id,
                    backgrounded.pid.map(|p| p as u32),
                    message.id,
                    &message.exec_id,
                    entry.stdout.clone(),
                    entry.stderr.clone(),
                )
                .await;
            let result = shell_backgrounded_result(
                backgrounded,
                &entry.stdout,
                &entry.stderr,
                &entry.context.terminals_folder,
            );
            completed(entry, pb::exec_client_message::Message::ShellResult(result))?
        }
        Some(Event::Rejected(value)) => {
            let result = pb::ShellResult {
                result: Some(pb::shell_result::Result::Rejected(value.clone())),
                ..Default::default()
            };
            complete(
                message.id,
                pending,
                pb::exec_client_message::Message::ShellResult(result),
            )
            .await?
        }
        Some(Event::PermissionDenied(value)) => {
            let result = pb::ShellResult {
                result: Some(pb::shell_result::Result::PermissionDenied(value.clone())),
                ..Default::default()
            };
            complete(
                message.id,
                pending,
                pb::exec_client_message::Message::ShellResult(result),
            )
            .await?
        }
        Some(Event::SandboxUnsupported(value)) => {
            let result = pb::ShellResult {
                result: Some(pb::shell_result::Result::SpawnError(pb::ShellSpawnError {
                    command: value.command.clone(),
                    working_directory: value.working_directory.clone(),
                    error: value.reason.clone(),
                })),
                ..Default::default()
            };
            complete(
                message.id,
                pending,
                pb::exec_client_message::Message::ShellResult(result),
            )
            .await?
        }
        None => ClientExecEvent::Pending,
    };
    Ok(event)
}

async fn background_shell_event(
    message: &pb::ExecClientMessage,
    runtime: &CursorToolRuntime,
) -> Result<ClientExecEvent> {
    let Some(pb::exec_client_message::Message::ShellStream(stream)) = &message.message else {
        return Ok(ClientExecEvent::Pending);
    };
    let Some(shell_id) = runtime
        .background_shell_for_event(message.id, &message.exec_id)
        .await
    else {
        return Ok(ClientExecEvent::Pending);
    };
    use pb::shell_stream::Event;
    match &stream.event {
        Some(Event::Stdout(stdout)) => {
            runtime
                .background_shell_stdout(&shell_id, &stdout.data)
                .await
        }
        Some(Event::Stderr(stderr)) => {
            runtime
                .background_shell_stderr(&shell_id, &stderr.data)
                .await
        }
        Some(Event::Exit(exit)) => {
            runtime
                .background_shell_exit(&shell_id, exit.code as i32)
                .await
        }
        Some(Event::Rejected(_)) => {
            runtime
                .background_shell_terminal(&shell_id, BackgroundShellStatus::Rejected)
                .await
        }
        Some(Event::PermissionDenied(_)) => {
            runtime
                .background_shell_terminal(&shell_id, BackgroundShellStatus::PermissionDenied)
                .await
        }
        Some(Event::Start(_))
        | Some(Event::HookContext(_))
        | Some(Event::Backgrounded(_))
        | Some(Event::SandboxUnsupported(_))
        | None => {}
    }
    Ok(ClientExecEvent::Pending)
}

pub async fn recover_transport_closed(
    id: u32,
    pending: &CursorToolRuntime,
) -> Result<Option<ToolCompletion>> {
    let Some(entry) = pending.take_transport_closed(id).await else {
        return Ok(None);
    };
    let error = format!(
        "{} transport closed before terminal result arrived",
        entry.call.name
    );
    let rendered = match &entry.stage {
        ExecStage::DynamicMcp(definition) => {
            super::render::render_dynamic_mcp(&entry.call, definition, false)
        }
        _ => super::render_tool_call(&entry.call, false)?,
    };
    Ok(Some(ToolCompletion::from_rendered(
        &entry.call,
        entry.started_at_ms,
        error,
        true,
        rendered,
    )?))
}

pub async fn stream_closed(id: u32, pending: &CursorToolRuntime) -> Result<Option<ToolCompletion>> {
    if pending.is_interrupted(id).await {
        pending.discard_exec(id).await;
        return Ok(None);
    }
    if pending
        .exec_call(id)
        .await
        .is_some_and(|call| call.name.eq_ignore_ascii_case("Task"))
    {
        pending.mark_transport_closed(id).await;
        return Ok(None);
    }
    stream_closed_immediate(id, pending).await
}

pub async fn stream_closed_immediate(
    id: u32,
    pending: &CursorToolRuntime,
) -> Result<Option<ToolCompletion>> {
    let Some(entry) = pending.take_exec(id).await else {
        return Ok(None);
    };
    let error = "Cursor Exec stream closed before returning a terminal result";
    if entry.call.name.eq_ignore_ascii_case("Shell") {
        let command = entry
            .call
            .arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let working_directory = entry
            .call
            .arguments
            .get("working_directory")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(Some(result::from_exec(
            entry,
            &pb::exec_client_message::Message::ShellResult(pb::ShellResult {
                result: Some(pb::shell_result::Result::SpawnError(pb::ShellSpawnError {
                    command,
                    working_directory,
                    error: error.into(),
                })),
                ..Default::default()
            }),
        )?));
    }
    let rendered = match &entry.stage {
        ExecStage::DynamicMcp(definition) => {
            super::render_dynamic_mcp(&entry.call, definition, false)
        }
        _ => super::render_tool_call(&entry.call, false)?,
    };
    Ok(Some(ToolCompletion::from_rendered(
        &entry.call,
        entry.started_at_ms,
        error.to_string(),
        true,
        rendered,
    )?))
}

async fn advance_await(
    entry: PendingExec,
    result: &pb::exec_client_message::Message,
    registry: &CursorToolRuntime,
) -> Result<ClientExecEvent> {
    let read = match result {
        pb::exec_client_message::Message::ReadResult(result)
        | pb::exec_client_message::Message::RedactedReadResult(result) => result,
        _ => return Err(Error::Protocol("AwaitShell expected ReadResult".into())),
    };
    let ExecStage::Await(state) = &entry.stage else {
        return Err(Error::Protocol(
            "AwaitShell result reached a non-await execution stage".into(),
        ));
    };

    // Prefer reading the freshest terminal file directly from disk if available,
    // as Desktop UI RPC may return a stale/cached ReadResult buffer.
    let disk_content = if !state.output_file_path.is_empty() {
        std::fs::read_to_string(&state.output_file_path).ok()
    } else {
        None
    };

    let rpc_content = match read.result.as_ref() {
        Some(pb::read_result::Result::Success(success)) => match success.output.as_ref() {
            Some(pb::read_success::Output::Content(content)) => content.as_str(),
            _ => "",
        },
        Some(pb::read_result::Result::FileNotFound(_)) => "",
        Some(pb::read_result::Result::Error(error)) => {
            tracing::warn!(
                task_id = %state.task_id,
                error = %error.error,
                "advance_await ReadResult error"
            );
            return Ok(ClientExecEvent::Completed(Box::new(result::await_error(
                entry,
                &error.error,
            )?)));
        }
        _ => "",
    };

    let content: &str = match &disk_content {
        Some(disk_str) if !disk_str.is_empty() => disk_str.as_str(),
        _ => rpc_content,
    };

    let shell_state = registry.background_shell(&state.task_id).await;
    if let Some(shell) = shell_state
        .as_ref()
        .filter(|shell| shell.status.is_terminal())
    {
        let combined_output = if !content.is_empty() {
            content.to_string()
        } else {
            format!("{}{}", shell.stdout, shell.stderr)
        };
        let regex_match = state
            .regex
            .as_ref()
            .map(|pattern| regex::Regex::new(pattern))
            .transpose()
            .map_err(|error| Error::Protocol(format!("invalid AwaitShell pattern: {error}")))?
            .and_then(|pattern| {
                pattern
                    .find(&combined_output)
                    .map(|found| found.as_str().to_string())
            });
        let file_exit_code = content.lines().find_map(|line| {
            line.strip_prefix("exit_code:")
                .and_then(|value| value.trim().parse::<i32>().ok())
        });
        let completed = matches!(shell.status, BackgroundShellStatus::Completed);
        if completed {
            return Ok(ClientExecEvent::Completed(Box::new(result::await_result(
                entry,
                combined_output.len() as u64,
                regex_match,
                shell.exit_code.or(file_exit_code),
                true,
            )?)));
        }
        let message = match shell.status {
            BackgroundShellStatus::Rejected => "background shell was rejected",
            BackgroundShellStatus::PermissionDenied => "background shell permission was denied",
            BackgroundShellStatus::TransportClosed => {
                "background shell transport closed before exit"
            }
            BackgroundShellStatus::Completed => "background shell completed without an exit code",
            BackgroundShellStatus::Backgrounded | BackgroundShellStatus::Running => unreachable!(),
        };
        return Ok(ClientExecEvent::Completed(Box::new(result::await_error(
            entry, message,
        )?)));
    }
    let regex_match = state
        .regex
        .as_ref()
        .map(|pattern| regex::Regex::new(pattern))
        .transpose()
        .map_err(|error| Error::Protocol(format!("invalid AwaitShell pattern: {error}")))?
        .and_then(|pattern| {
            pattern
                .find(content)
                .map(|found| found.as_str().to_string())
        });
    let mut exit_code = content.lines().find_map(|line| {
        line.strip_prefix("exit_code:")
            .and_then(|value| value.trim().parse::<i32>().ok())
    });

    // Check header status metadata from terminal file (e.g. status: succeeded / status: failed / status: completed)
    let header_status_completed = content.lines().find_map(|line| {
        let val = line.strip_prefix("status:")?.trim();
        if val.eq_ignore_ascii_case("succeeded") || val.eq_ignore_ascii_case("completed") {
            Some(0)
        } else if val.eq_ignore_ascii_case("failed") || val.eq_ignore_ascii_case("error") {
            Some(1)
        } else {
            None
        }
    });

    if exit_code.is_none() && header_status_completed.is_some() {
        exit_code = header_status_completed;
    }

    // Parse PID from terminal header if background shell state doesn't have it
    let file_pid = content.lines().find_map(|line| {
        line.strip_prefix("pid:")
            .and_then(|val| val.trim().parse::<u32>().ok())
    });

    // PID probing fallback: if file still says running or has no exit code, check if PID has terminated in OS
    let mut pid_terminated = false;
    if exit_code.is_none() {
        let target_pid = shell_state
            .as_ref()
            .and_then(|s| s.pid)
            .or(file_pid);

        if let Some(pid) = target_pid {
            if !crate::cursor::tools::runtime::is_pid_alive(pid) {
                pid_terminated = true;
                exit_code = Some(0);
            }
        }
    }

    if regex_match.is_some()
        || exit_code.is_some()
        || header_status_completed.is_some()
        || pid_terminated
        || std::time::Instant::now() >= state.deadline
    {
        return Ok(ClientExecEvent::Completed(Box::new(result::await_result(
            entry,
            content.len() as u64,
            regex_match,
            exit_code,
            exit_code.is_some() || header_status_completed.is_some() || pid_terminated,
        )?)));
    }
    let state = match entry.stage {
        ExecStage::Await(state) => state,
        _ => {
            return Err(Error::Protocol(
                "AwaitShell result changed execution stage".into(),
            ))
        }
    };
    let wait = state
        .deadline
        .saturating_duration_since(std::time::Instant::now())
        .min(std::time::Duration::from_millis(50));
    tokio::time::sleep(wait).await;
    let call = entry.call.clone();
    let context = entry.context.clone();
    let id = registry
        .reserve_await_again(&call, &context, state, entry.started_at_ms)
        .await?;
    Ok(ClientExecEvent::Message(Box::new(await_read_request(
        id, &call, &context,
    )?)))
}

fn is_terminal(message: &pb::exec_client_message::Message) -> bool {
    use pb::{exec_client_message::Message, shell_stream::Event};

    match message {
        Message::ShellStream(stream) => matches!(
            stream.event.as_ref(),
            Some(Event::Exit(_))
                | Some(Event::Backgrounded(_))
                | Some(Event::Rejected(_))
                | Some(Event::PermissionDenied(_))
                | Some(Event::SandboxUnsupported(_))
        ),
        _ => true,
    }
}

async fn advance_edit(
    entry: PendingExec,
    result: &pb::exec_client_message::Message,
    registry: &CursorToolRuntime,
) -> Result<ClientExecEvent> {
    let read = match result {
        pb::exec_client_message::Message::ReadResult(result)
        | pb::exec_client_message::Message::RedactedReadResult(result) => result,
        _ => {
            return Err(Error::Protocol(format!(
                "expected ReadResult for edit tool {}",
                entry.call.name
            )))
        }
    };
    let write = match edit::after_read(&entry.call, read) {
        Ok(write) => write,
        Err(error) => {
            return Ok(ClientExecEvent::Completed(Box::new(result::edit_failure(
                entry, error,
            )?)))
        }
    };
    let id = registry
        .reserve_edit_write(
            &entry.call,
            &entry.context,
            write.clone(),
            entry.started_at_ms,
        )
        .await?;
    Ok(ClientExecEvent::Message(Box::new(edit_write_request(
        id,
        &entry.call,
        &write,
    )?)))
}

async fn complete(
    id: u32,
    pending: &CursorToolRuntime,
    result: pb::exec_client_message::Message,
) -> Result<ClientExecEvent> {
    completed(take(id, pending).await?, result)
}

async fn take(id: u32, pending: &CursorToolRuntime) -> Result<PendingExec> {
    pending
        .take_exec(id)
        .await
        .ok_or_else(|| Error::Protocol(format!("unknown terminal Exec id: {id}")))
}

fn completed(
    pending: PendingExec,
    result: pb::exec_client_message::Message,
) -> Result<ClientExecEvent> {
    Ok(ClientExecEvent::Completed(Box::new(result::from_exec(
        pending, &result,
    )?)))
}

fn shell_exit_result(
    message: &pb::ExecClientMessage,
    exit: &pb::ShellStreamExit,
    stdout: &str,
    stderr: &str,
) -> pb::ShellResult {
    let result = if exit.code == 0 && !exit.aborted {
        pb::shell_result::Result::Success(pb::ShellSuccess {
            working_directory: exit.cwd.clone(),
            exit_code: exit.code as i32,
            stdout: stdout.into(),
            stderr: stderr.into(),
            interleaved_output: Some(format!("{stdout}{stderr}")),
            local_execution_time_ms: exit
                .local_execution_time_ms
                .or(message.local_execution_time_ms),
            ..Default::default()
        })
    } else {
        pb::shell_result::Result::Failure(pb::ShellFailure {
            working_directory: exit.cwd.clone(),
            exit_code: exit.code as i32,
            stdout: stdout.into(),
            stderr: stderr.into(),
            interleaved_output: Some(format!("{stdout}{stderr}")),
            abort_reason: exit.abort_reason,
            aborted: exit.aborted,
            local_execution_time_ms: exit
                .local_execution_time_ms
                .or(message.local_execution_time_ms),
            ..Default::default()
        })
    };
    pb::ShellResult {
        result: Some(result),
        is_background: Some(false),
        ..Default::default()
    }
}

fn shell_backgrounded_result(
    backgrounded: &pb::ShellStreamBackgrounded,
    stdout: &str,
    stderr: &str,
    terminals_folder: &str,
) -> pb::ShellResult {
    pb::ShellResult {
        result: Some(pb::shell_result::Result::Success(pb::ShellSuccess {
            command: backgrounded.command.clone(),
            working_directory: backgrounded.working_directory.clone(),
            stdout: stdout.into(),
            stderr: stderr.into(),
            shell_id: Some(backgrounded.shell_id),
            pid: backgrounded.pid,
            ms_to_wait: backgrounded.ms_to_wait,
            background_reason: backgrounded.reason,
            interleaved_output: Some(format!("{stdout}{stderr}")),
            ..Default::default()
        })),
        is_background: Some(true),
        terminals_folder: (!terminals_folder.is_empty()).then(|| terminals_folder.into()),
        pid: backgrounded.pid,
        ..Default::default()
    }
}

fn shell_delta(call: &ToolCall, stdout: bool, content: &str) -> pb::AgentServerMessage {
    let delta = if stdout {
        pb::shell_tool_call_delta::Delta::Stdout(pb::ShellToolCallStdoutDelta {
            content: content.into(),
        })
    } else {
        pb::shell_tool_call_delta::Delta::Stderr(pb::ShellToolCallStderrDelta {
            content: content.into(),
        })
    };
    events::server_interaction(pb::interaction_update::Message::ToolCallDelta(Box::new(
        pb::ToolCallDeltaUpdate {
            call_id: call.call_id.clone(),
            tool_call_delta: Some(Box::new(pb::ToolCallDelta {
                delta: Some(pb::tool_call_delta::Delta::ShellToolCallDelta(
                    pb::ShellToolCallDelta { delta: Some(delta) },
                )),
            })),
            model_call_id: call.model_call_id.clone(),
        },
    )))
}
