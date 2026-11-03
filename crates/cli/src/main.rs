use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Map, Value};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

use composable_agents::types::{
    AgentEvent, AgentFinishReason, AgentRequest, Message, ModelFinishReason, Part, PartContent,
    Role, TaskState, TaskStatus, TokenUsage,
};
use composable_agents::{AgentHost, AgentService};
use composable_otel::OtelService;
use composable_runtime::Runtime;

#[derive(Parser)]
#[command(name = "agents")]
#[command(about = "List and invoke agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the metadata of every agent, by component name
    List {
        /// Component definition files (.toml) and standalone .wasm files
        #[arg(required = true)]
        definitions: Vec<PathBuf>,
    },
    /// Invoke an agent, streaming its artifacts to stdout
    Invoke {
        /// Component definition files (.toml) and standalone .wasm files
        #[arg(required = true)]
        definitions: Vec<PathBuf>,

        /// The agent's component name, then the message (default stdin), passed after --
        #[arg(last = true, required = true, num_args = 1..=2, value_names = ["AGENT", "MESSAGE"])]
        invoke: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let (Command::List { definitions } | Command::Invoke { definitions, .. }) = &cli.command;

    // The message is read before the runtime starts, so that if it's missing,
    // the process fails without loading any components.
    let invocation = match &cli.command {
        Command::List { .. } => None,
        Command::Invoke { invoke, .. } => Some((invoke[0].clone(), message(invoke.get(1))?)),
    };

    let runtime = Runtime::builder()
        .from_paths(definitions)
        .with_service::<OtelService>()
        .with_service::<AgentService>()
        .build()
        .await?;
    runtime.start()?;

    let host = AgentHost::new(runtime.host(), None);
    let exit = match invocation {
        None => list(&host).await,
        Some((agent, text)) => invoke(&host, &agent, text).await,
    };
    runtime.shutdown().await;
    exit
}

// Each agent's metadata, keyed by its component name.
async fn list(host: &AgentHost) -> Result<ExitCode> {
    let agents: Map<String, Value> = host.list().await?.into_iter().collect();
    println!("{}", serde_json::to_string_pretty(&agents)?);
    Ok(ExitCode::SUCCESS)
}

// Invokes `agent` with `text`, writing its artifacts to stdout as they stream,
// and its status changes, final status, finish reason, and usage to stderr.
// Succeeds only if the task completes.
async fn invoke(host: &AgentHost, agent: &str, text: String) -> Result<ExitCode> {
    let mut stdout = std::io::stdout();
    let terminal = stdout.is_terminal();
    let mut needs_newline = false;
    let outcome = host
        .invoke(agent, request(text), |event| {
            show_event(event, &mut stdout, terminal, &mut needs_newline)
        })
        .await?;
    // If stdout has failed, the task was canceled, so the error is ignored.
    if needs_newline {
        let _ = writeln!(stdout);
    }

    let response = match outcome {
        Ok(response) => response,
        Err(error) => {
            eprintln!("Error: {} (code {})", error.message, error.code);
            if let Some(data) = error.data {
                eprintln!("{data}");
            }
            return Ok(ExitCode::FAILURE);
        }
    };
    eprintln!("{}", status_line(&response.status));
    if let Some(reason) = &response.finish_reason {
        eprintln!("finish reason: {}", finish_reason(reason));
    }
    eprintln!("usage: {}", usage(&response.usage));
    if response.status.state == TaskState::Completed {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

// The message to send, either the provided argument or stdin (only if not a
// terminal). The message must not be empty.
fn message(argument: Option<&String>) -> Result<String> {
    let mut text = match argument {
        Some(text) => text.clone(),
        None => {
            let mut stdin = std::io::stdin();
            if stdin.is_terminal() {
                bail!("provide MESSAGE after AGENT, or pipe it to stdin");
            }
            let mut text = String::new();
            stdin.read_to_string(&mut text)?;
            text
        }
    };
    text.truncate(text.trim_end().len());
    if text.is_empty() {
        bail!("the message is empty");
    }
    Ok(text)
}

// A request for a new task in a new context, with `text` as a user message.
fn request(text: String) -> AgentRequest {
    AgentRequest {
        message: Message {
            message_id: uuid::Uuid::new_v4().to_string(),
            context_id: None,
            task_id: None,
            role: Role::User,
            parts: vec![Part {
                content: PartContent::Text(text),
                metadata: None,
                filename: None,
                media_type: None,
            }],
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        },
        task_id: None,
        context_id: None,
        accepted_output_modes: Vec::new(),
        requested_extensions: Vec::new(),
        metadata: None,
        task_status: None,
        related_tasks: Vec::new(),
        history: Vec::new(),
        token_budget: None,
    }
}

// Writes each artifact part to `stdout` as text, starting a new artifact or a
// later part of the same event on a new line. Writes a status change to
// stderr. If stdout is the terminal, and its text so far doesn't end in a
// newline, the status starts a new line, so that it doesn't share a line with
// the artifact's text. Returns false if `stdout` fails, so that the task is
// canceled.
fn show_event(
    event: AgentEvent,
    stdout: &mut impl Write,
    terminal: bool,
    needs_newline: &mut bool,
) -> bool {
    match event {
        AgentEvent::Artifact(update) => {
            let appends = update.append == Some(true);
            for (index, part) in update.artifact.parts.into_iter().enumerate() {
                let text = part_text(part);
                if text.is_empty() {
                    continue;
                }
                let separator = if *needs_newline && (index > 0 || !appends) {
                    "\n"
                } else {
                    ""
                };
                if write!(stdout, "{separator}{text}")
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    return false;
                }
                *needs_newline = !text.ends_with('\n');
            }
        }
        AgentEvent::Status(update) => {
            if terminal && std::mem::take(needs_newline) {
                eprintln!();
            }
            eprintln!("{}", status_line(&update.status));
        }
    }
    true
}

// A part as its text, its data's JSON, or its URL. Raw bytes are shown as a
// placeholder with the part's filename and media type, if given.
fn part_text(part: Part) -> String {
    match part.content {
        PartContent::Text(text) | PartContent::Data(text) | PartContent::Url(text) => text,
        PartContent::Raw(_) => {
            let details: Vec<String> = [part.filename, part.media_type]
                .into_iter()
                .flatten()
                .collect();
            if details.is_empty() {
                "[raw]".to_string()
            } else {
                format!("[raw: {}]", details.join(", "))
            }
        }
    }
}

// A status's state, then the text of its message, if any.
fn status_line(status: &TaskStatus) -> String {
    let state = state_name(status.state);
    let text = status
        .message
        .as_ref()
        .map(message_text)
        .unwrap_or_default();
    if text.is_empty() {
        format!("status: {state}")
    } else {
        format!("status: {state}: {text}")
    }
}

// The WIT case name of a task state.
fn state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Unspecified => "unspecified",
        TaskState::Submitted => "submitted",
        TaskState::Working => "working",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Canceled => "canceled",
        TaskState::InputRequired => "input-required",
        TaskState::Rejected => "rejected",
        TaskState::AuthRequired => "auth-required",
    }
}

// The text parts of a message, joined.
fn message_text(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match &part.content {
            PartContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// The WIT case name of a finish reason, with the provider's raw value for a
// model's reason.
fn finish_reason(reason: &AgentFinishReason) -> String {
    let (case, raw) = match reason {
        AgentFinishReason::TurnLimit => return "turn-limit".to_string(),
        AgentFinishReason::Budget => return "budget".to_string(),
        AgentFinishReason::Model(reason) => match reason {
            ModelFinishReason::Completed(raw) => ("completed", raw),
            ModelFinishReason::ToolRequested(raw) => ("tool-requested", raw),
            ModelFinishReason::Truncated(raw) => ("truncated", raw),
            ModelFinishReason::Stopped(raw) => ("stopped", raw),
            ModelFinishReason::Blocked(raw) => ("blocked", raw),
            ModelFinishReason::Paused(raw) => ("paused", raw),
            ModelFinishReason::Other(raw) => ("other", raw),
        },
    };
    format!("{case} ({raw})")
}

// The reported token counts, then how many calls reported none.
fn usage(usage: &TokenUsage) -> String {
    let counts = [
        ("input", usage.input_tokens),
        ("output", usage.output_tokens),
        ("total", usage.total_tokens),
        ("cache-read", usage.cache_read_tokens),
        ("cache-write", usage.cache_write_tokens),
        ("reasoning", usage.reasoning_tokens),
    ];
    let mut reported: Vec<String> = counts
        .into_iter()
        .filter_map(|(name, count)| count.map(|count| format!("{name} {count}")))
        .collect();
    match usage.unreported_calls {
        0 => {}
        1 => reported.push("1 call unreported".to_string()),
        calls => reported.push(format!("{calls} calls unreported")),
    }
    if reported.is_empty() {
        "none reported".to_string()
    } else {
        reported.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use composable_agents::types::{Artifact, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};

    use super::*;

    fn part(content: PartContent) -> Part {
        Part {
            content,
            metadata: None,
            filename: None,
            media_type: None,
        }
    }

    fn artifact_event(parts: Vec<Part>, append: bool) -> AgentEvent {
        AgentEvent::Artifact(TaskArtifactUpdateEvent {
            task_id: "task-1".to_string(),
            context_id: "context-1".to_string(),
            artifact: Artifact {
                artifact_id: "artifact-1".to_string(),
                name: None,
                description: None,
                parts,
                metadata: None,
                extensions: Vec::new(),
            },
            append: Some(append),
            last_chunk: None,
            metadata: None,
        })
    }

    fn status(state: TaskState, text: Option<&str>) -> TaskStatus {
        let mut message = request(text.unwrap_or_default().to_string()).message;
        message.role = Role::Agent;
        TaskStatus {
            state,
            message: text.map(|_| message),
            timestamp: None,
        }
    }

    fn no_usage() -> TokenUsage {
        TokenUsage {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            unreported_calls: 0,
        }
    }

    #[test]
    fn a_request_is_a_user_message_with_one_text_part_for_a_new_task() {
        let request = request("Hello".to_string());
        assert_eq!(request.message.role, Role::User);
        assert!(uuid::Uuid::parse_str(&request.message.message_id).is_ok());
        let [
            Part {
                content: PartContent::Text(text),
                ..
            },
        ] = request.message.parts.as_slice()
        else {
            panic!("expected one text part");
        };
        assert_eq!(text, "Hello");
        assert_eq!(request.task_id, None);
        assert_eq!(request.context_id, None);
        assert!(request.history.is_empty());
        assert_eq!(request.token_budget, None);
    }

    #[test]
    fn an_argument_is_the_message_with_trailing_whitespace_trimmed() {
        assert_eq!(message(Some(&"Hello\n".to_string())).unwrap(), "Hello");
        let error = message(Some(&" \n".to_string())).unwrap_err().to_string();
        assert_eq!(error, "the message is empty");
    }

    #[test]
    fn artifact_text_is_written_as_it_streams() {
        let mut stdout = Vec::new();
        let mut needs_newline = false;
        for (index, chunk) in ["Hel", "lo"].into_iter().enumerate() {
            let text = part(PartContent::Text(chunk.to_string()));
            let event = artifact_event(vec![text], index > 0);
            assert!(show_event(event, &mut stdout, false, &mut needs_newline));
        }
        assert_eq!(String::from_utf8(stdout).unwrap(), "Hello");
        assert!(needs_newline);
    }

    #[test]
    fn each_part_is_written_to_stdout_on_its_own_line_but_a_status_change_is_not() {
        let mut stdout = Vec::new();
        let mut needs_newline = false;
        let mut raw = part(PartContent::Raw("AAAA".to_string()));
        raw.media_type = Some("application/pdf".to_string());
        let parts = vec![
            part(PartContent::Text("Here:".to_string())),
            part(PartContent::Data(r#"{"a":1}"#.to_string())),
            part(PartContent::Url("https://example.com".to_string())),
            raw,
        ];
        assert!(show_event(
            artifact_event(parts, false),
            &mut stdout,
            false,
            &mut needs_newline
        ));
        let working = AgentEvent::Status(TaskStatusUpdateEvent {
            task_id: "task-1".to_string(),
            context_id: "context-1".to_string(),
            status: status(TaskState::Working, None),
            metadata: None,
        });
        assert!(show_event(working, &mut stdout, false, &mut needs_newline));
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "Here:\n{\"a\":1}\nhttps://example.com\n[raw: application/pdf]"
        );
    }

    #[test]
    fn a_status_line_includes_the_text_of_its_message() {
        assert_eq!(
            status_line(&status(TaskState::Completed, None)),
            "status: completed"
        );
        assert_eq!(
            status_line(&status(TaskState::Failed, Some("the model call failed"))),
            "status: failed: the model call failed"
        );
    }

    #[test]
    fn a_finish_reason_includes_the_providers_value() {
        let reason = AgentFinishReason::Model(ModelFinishReason::Completed("stop".to_string()));
        assert_eq!(finish_reason(&reason), "completed (stop)");
        assert_eq!(finish_reason(&AgentFinishReason::TurnLimit), "turn-limit");
    }

    #[test]
    fn usage_shows_only_reported_counts_and_unreported_calls() {
        assert_eq!(usage(&no_usage()), "none reported");
        let reported = TokenUsage {
            input_tokens: Some(12),
            output_tokens: Some(34),
            total_tokens: Some(46),
            unreported_calls: 1,
            ..no_usage()
        };
        assert_eq!(
            usage(&reported),
            "input 12, output 34, total 46, 1 call unreported"
        );
    }
}
