//! Codex app-server control adapter.
//!
//! Codex app-server speaks JSON-RPC over stdio, WebSocket, or WebSocket over a
//! Unix socket. Fleet uses `codex app-server proxy --sock PATH` as its stdio
//! bridge to the shared Unix endpoint. This keeps framing inside Codex while
//! preserving exact JSON-RPC request IDs for structured responses.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ApprovalDecision, ProviderError, ProviderReceipt, QuestionAnswer, QuestionOption,
    StructuredQuestion,
};

const METHOD_REQUEST_USER_INPUT: &str = "item/tool/requestUserInput";
const METHOD_COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
const METHOD_FILE_APPROVAL: &str = "item/fileChange/requestApproval";
const METHOD_PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";
const METHOD_THREAD_ARCHIVE: &str = "thread/archive";

/// JSON-RPC request ID retained without string or number coercion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RpcRequestId(Value);

impl RpcRequestId {
    /// Parse valid string or numeric JSON-RPC request ID.
    pub fn new(value: Value) -> Result<Self, ProviderError> {
        if value.is_string() || value.is_number() {
            Ok(Self(value))
        } else {
            Err(ProviderError::Protocol(
                "Codex request id must be string or number".into(),
            ))
        }
    }

    /// Borrow exact JSON value for response routing.
    pub const fn as_value(&self) -> &Value {
        &self.0
    }
}

/// Exact identity carried by Codex item-level server requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexItemRequestIdentity {
    /// JSON-RPC ID to echo in response.
    pub request_id: RpcRequestId,
    /// Exact app-server thread ID.
    pub thread_id: String,
    /// Exact app-server turn ID.
    pub turn_id: String,
    /// Exact app-server item ID.
    pub item_id: String,
}

/// Complete Codex request-user-input payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexQuestionRequest {
    /// Exact item request identity.
    pub identity: CodexItemRequestIdentity,
    /// Questions in app-server order.
    pub questions: Vec<StructuredQuestion>,
    /// Optional server-side auto-resolution window.
    pub auto_resolution_ms: Option<u64>,
}

/// App-server approval request family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexApprovalKind {
    /// Command execution approval.
    CommandExecution,
    /// File change approval.
    FileChange,
    /// Additional permission profile approval.
    Permissions,
}

/// Complete Codex approval request with raw schema-compatible params.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexApprovalRequest {
    /// Exact item request identity.
    pub identity: CodexItemRequestIdentity,
    /// Approval response schema family.
    pub kind: CodexApprovalKind,
    /// Original params for display and permission-grant response construction.
    pub params: Value,
}

/// Capability result from installed Codex version and generated schema.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct CodexCapabilities {
    /// Installed CLI version string.
    pub cli_version: String,
    /// Running managed daemon version payload when available.
    pub daemon_version: Option<Value>,
    /// Whether app-server schema generation succeeded.
    pub app_server: bool,
    /// Whether installed CLI exposes stdio proxy for Unix app-server.
    pub stdio_proxy: bool,
    /// Whether exact request-user-input method exists in generated schema.
    pub request_user_input: bool,
    /// Whether current approval request methods exist in generated schema.
    pub approvals: bool,
    /// Whether exact thread archive exists in generated schema.
    pub thread_archive: bool,
}

impl CodexCapabilities {
    /// Conservative unavailable capability set.
    pub const fn unavailable() -> Self {
        Self {
            cli_version: String::new(),
            daemon_version: None,
            app_server: false,
            stdio_proxy: false,
            request_user_input: false,
            approvals: false,
            thread_archive: false,
        }
    }
}

/// Executable command represented without shell interpolation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Executable path.
    pub program: OsString,
    /// Ordered argv excluding executable.
    pub args: Vec<OsString>,
}

impl CommandSpec {
    /// Convert specification to process command.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }
}

/// Build shared app-server Unix listener command.
pub fn app_server_command(codex_binary: &OsStr, socket: &Path) -> CommandSpec {
    CommandSpec {
        program: codex_binary.to_os_string(),
        args: vec![
            "app-server".into(),
            "--listen".into(),
            unix_endpoint(socket).into(),
        ],
    }
}

/// Build stdio proxy command for shared app-server Unix endpoint.
pub fn proxy_command(codex_binary: &OsStr, socket: &Path) -> CommandSpec {
    CommandSpec {
        program: codex_binary.to_os_string(),
        args: vec![
            "app-server".into(),
            "proxy".into(),
            "--sock".into(),
            socket.as_os_str().to_os_string(),
        ],
    }
}

/// Build Fleet-managed Codex TUI launch using shared Unix app-server.
pub fn managed_tui_command(
    codex_binary: &OsStr,
    socket: &Path,
    additional_args: impl IntoIterator<Item = OsString>,
) -> CommandSpec {
    let mut args = vec!["--remote".into(), unix_endpoint(socket).into()];
    args.extend(additional_args);
    CommandSpec {
        program: codex_binary.to_os_string(),
        args,
    }
}

fn unix_endpoint(socket: &Path) -> String {
    format!("unix://{}", socket.display())
}

/// Probe installed Codex binary and its generated experimental schema.
pub fn probe_codex(codex_binary: &OsStr) -> CodexCapabilities {
    let cli_version = Command::new(codex_binary)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();

    let daemon_version = Command::new(codex_binary)
        .args(["app-server", "daemon", "version"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice(&output.stdout).ok());
    let stdio_proxy = Command::new(codex_binary)
        .args(["app-server", "proxy", "--help"])
        .output()
        .is_ok_and(|output| output.status.success());

    let schema_dir = probe_dir();
    let generated = fs::create_dir_all(&schema_dir).is_ok()
        && Command::new(codex_binary)
            .args([
                "app-server",
                "generate-json-schema",
                "--experimental",
                "--out",
            ])
            .arg(&schema_dir)
            .output()
            .is_ok_and(|output| output.status.success());
    let schema = generated
        .then(|| fs::read_to_string(schema_dir.join("codex_app_server_protocol.schemas.json")))
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default();
    let _ = fs::remove_dir_all(&schema_dir);
    let (request_user_input, approvals, thread_archive) = capabilities_from_schema(&schema);

    CodexCapabilities {
        cli_version,
        daemon_version,
        app_server: generated,
        stdio_proxy,
        request_user_input,
        approvals,
        thread_archive,
    }
}

fn probe_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("ainb-codex-schema-{}-{nonce}", std::process::id()))
}

/// Detect version-specific methods from generated app-server schema.
pub fn capabilities_from_schema(schema: &str) -> (bool, bool, bool) {
    let request_user_input = schema.contains(METHOD_REQUEST_USER_INPUT)
        && schema.contains("ToolRequestUserInputResponse");
    let approvals = schema.contains(METHOD_COMMAND_APPROVAL)
        && schema.contains(METHOD_FILE_APPROVAL)
        && schema.contains(METHOD_PERMISSIONS_APPROVAL);
    let thread_archive =
        schema.contains(METHOD_THREAD_ARCHIVE) && schema.contains("ThreadArchiveParams");
    (request_user_input, approvals, thread_archive)
}

/// JSON-RPC transport used by Codex adapter.
pub trait CodexTransport {
    /// Send request and return its result value.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, ProviderError>;
    /// Send notification without request ID.
    fn notify(&mut self, method: &str, params: Value) -> Result<(), ProviderError>;
    /// Respond to exact server request ID.
    fn respond(&mut self, request_id: &RpcRequestId, result: Value) -> Result<(), ProviderError>;
    /// Read queued server request or notification.
    fn next_inbound(&mut self) -> Result<Value, ProviderError>;
}

/// Stdio proxy transport to a Unix-socket app-server.
pub struct CodexProxyTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    inbound: VecDeque<Value>,
}

impl CodexProxyTransport {
    /// Spawn `codex app-server proxy --sock PATH` with piped JSON-RPC stdio.
    pub fn connect(codex_binary: &OsStr, socket: &Path) -> Result<Self, ProviderError> {
        let mut command = proxy_command(codex_binary, socket).command();
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::Transport("Codex proxy stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::Transport("Codex proxy stdout unavailable".into()))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            inbound: VecDeque::new(),
        })
    }

    fn write_message(&mut self, message: &Value) -> Result<(), ProviderError> {
        serde_json::to_writer(&mut self.stdin, message)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_message(&mut self) -> Result<Value, ProviderError> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Err(ProviderError::Transport(
                "Codex app-server proxy closed stdout".into(),
            ));
        }
        serde_json::from_str(&line).map_err(ProviderError::from)
    }
}

impl CodexTransport for CodexProxyTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, ProviderError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;

        loop {
            let message = self.read_message()?;
            if message.get("id") == Some(&json!(id))
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                if let Some(error) = message.get("error") {
                    return Err(ProviderError::Protocol(format!(
                        "Codex {method} failed: {error}"
                    )));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            self.inbound.push_back(message);
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), ProviderError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn respond(&mut self, request_id: &RpcRequestId, result: Value) -> Result<(), ProviderError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": request_id.as_value(),
            "result": result,
        }))
    }

    fn next_inbound(&mut self) -> Result<Value, ProviderError> {
        if let Some(message) = self.inbound.pop_front() {
            return Ok(message);
        }
        self.read_message()
    }
}

impl Drop for CodexProxyTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parsed app-server inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexInbound {
    /// Structured request-user-input request.
    RequestUserInput(CodexQuestionRequest),
    /// Structured approval request.
    Approval(CodexApprovalRequest),
    /// Server notification.
    Notification {
        /// Notification method.
        method: String,
        /// Notification params.
        params: Value,
    },
    /// Server request not yet understood by Fleet.
    OtherRequest {
        /// Exact request ID.
        request_id: RpcRequestId,
        /// Request method.
        method: String,
        /// Request params.
        params: Value,
    },
}

/// Parse one app-server request or notification without discarding unknown fields.
pub fn parse_inbound(message: &Value) -> Result<CodexInbound, ProviderError> {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::Protocol("Codex inbound message has no method".into()))?
        .to_owned();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = message.get("id").cloned() else {
        return Ok(CodexInbound::Notification { method, params });
    };
    let request_id = RpcRequestId::new(id)?;

    match method.as_str() {
        METHOD_REQUEST_USER_INPUT => Ok(CodexInbound::RequestUserInput(parse_question_request(
            request_id, &params,
        )?)),
        METHOD_COMMAND_APPROVAL => Ok(CodexInbound::Approval(parse_approval_request(
            request_id,
            CodexApprovalKind::CommandExecution,
            params,
        )?)),
        METHOD_FILE_APPROVAL => Ok(CodexInbound::Approval(parse_approval_request(
            request_id,
            CodexApprovalKind::FileChange,
            params,
        )?)),
        METHOD_PERMISSIONS_APPROVAL => Ok(CodexInbound::Approval(parse_approval_request(
            request_id,
            CodexApprovalKind::Permissions,
            params,
        )?)),
        _ => Ok(CodexInbound::OtherRequest {
            request_id,
            method,
            params,
        }),
    }
}

fn parse_question_request(
    request_id: RpcRequestId,
    params: &Value,
) -> Result<CodexQuestionRequest, ProviderError> {
    let identity = parse_item_identity(request_id, params)?;
    let raw_questions = params
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Protocol("Codex RUI questions missing".into()))?;
    let questions = raw_questions
        .iter()
        .map(|question| {
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            Ok(QuestionOption {
                                label: required_string(option, "label")?,
                                description: required_string(option, "description")?,
                            })
                        })
                        .collect::<Result<Vec<_>, ProviderError>>()
                })
                .transpose()?
                .unwrap_or_default();
            Ok(StructuredQuestion {
                id: required_string(question, "id")?,
                header: required_string(question, "header")?,
                question: required_string(question, "question")?,
                options,
                multi_select: question
                    .get("multiSelect")
                    .or_else(|| question.get("multi_select"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                is_other: question.get("isOther").and_then(Value::as_bool).unwrap_or(false),
                is_secret: question.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    Ok(CodexQuestionRequest {
        identity,
        questions,
        auto_resolution_ms: params.get("autoResolutionMs").and_then(Value::as_u64),
    })
}

fn parse_approval_request(
    request_id: RpcRequestId,
    kind: CodexApprovalKind,
    params: Value,
) -> Result<CodexApprovalRequest, ProviderError> {
    let identity = parse_item_identity(request_id, &params)?;
    Ok(CodexApprovalRequest {
        identity,
        kind,
        params,
    })
}

fn parse_item_identity(
    request_id: RpcRequestId,
    params: &Value,
) -> Result<CodexItemRequestIdentity, ProviderError> {
    Ok(CodexItemRequestIdentity {
        request_id,
        thread_id: required_string(params, "threadId")?,
        turn_id: required_string(params, "turnId")?,
        item_id: required_string(params, "itemId")?,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String, ProviderError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ProviderError::Protocol(format!("Codex field {field} missing")))
}

/// Version-gated app-server client.
pub struct CodexClient<T> {
    transport: T,
    capabilities: CodexCapabilities,
    initialized: bool,
}

impl<T: CodexTransport> CodexClient<T> {
    /// Create client around an app-server transport and version probe result.
    pub const fn new(transport: T, capabilities: CodexCapabilities) -> Self {
        Self {
            transport,
            capabilities,
            initialized: false,
        }
    }

    /// Initialize app-server connection and opt into experimental API.
    pub fn initialize(&mut self, client_version: &str) -> Result<Value, ProviderError> {
        if !self.capabilities.app_server {
            return Err(ProviderError::Unsupported(
                "Codex app-server unavailable in installed version".into(),
            ));
        }
        let result = self.transport.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "agents-in-a-box-fleet",
                    "title": "Fleet",
                    "version": client_version,
                },
                "capabilities": {
                    "experimentalApi": true,
                },
            }),
        )?;
        self.transport.notify("initialized", json!({}))?;
        self.initialized = true;
        Ok(result)
    }

    /// Start new thread and return exact thread ID.
    pub fn thread_start(
        &mut self,
        cwd: &Path,
        model: Option<&str>,
    ) -> Result<String, ProviderError> {
        self.require_initialized()?;
        let result = self.transport.request(
            "thread/start",
            json!({
                "cwd": cwd,
                "model": model,
            }),
        )?;
        required_string(result.get("thread").unwrap_or(&Value::Null), "id")
    }

    /// Resume exact thread and return full schema-compatible result.
    pub fn thread_resume(&mut self, thread_id: &str) -> Result<Value, ProviderError> {
        self.require_initialized()?;
        self.transport.request("thread/resume", json!({ "threadId": thread_id }))
    }

    /// Read exact thread with turns included.
    pub fn thread_read(&mut self, thread_id: &str) -> Result<Value, ProviderError> {
        self.require_initialized()?;
        self.transport.request(
            "thread/read",
            json!({ "threadId": thread_id, "includeTurns": true }),
        )
    }

    /// Start turn with one text input and return exact turn ID.
    pub fn turn_start(&mut self, thread_id: &str, text: &str) -> Result<String, ProviderError> {
        self.require_initialized()?;
        let result = self.transport.request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{ "type": "text", "text": text }],
            }),
        )?;
        required_string(result.get("turn").unwrap_or(&Value::Null), "id")
    }

    /// Interrupt exact active turn.
    pub fn turn_interrupt(
        &mut self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Value, ProviderError> {
        self.require_initialized()?;
        self.transport.request(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
        )
    }

    /// Answer exact request-user-input server request.
    pub fn answer_request_user_input(
        &mut self,
        request: &CodexQuestionRequest,
        answers: &[QuestionAnswer],
    ) -> Result<ProviderReceipt, ProviderError> {
        self.require_initialized()?;
        if !self.capabilities.request_user_input {
            return Err(ProviderError::Unsupported(
                "Codex request-user-input absent from generated schema".into(),
            ));
        }
        validate_question_answers(request, answers)?;
        let answer_map = answers
            .iter()
            .map(|answer| {
                (
                    answer.question_id.clone(),
                    json!({ "answers": answer.answers }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        self.transport.respond(
            &request.identity.request_id,
            json!({ "answers": answer_map }),
        )?;
        Ok(ProviderReceipt {
            authoritative: true,
            transport: "codex-app-server",
        })
    }

    /// Decide exact app-server approval request.
    pub fn decide_approval(
        &mut self,
        request: &CodexApprovalRequest,
        decision: ApprovalDecision,
    ) -> Result<ProviderReceipt, ProviderError> {
        self.require_initialized()?;
        if !self.capabilities.approvals {
            return Err(ProviderError::Unsupported(
                "Codex approvals absent from generated schema".into(),
            ));
        }
        let result = approval_result(request, decision)?;
        self.transport.respond(&request.identity.request_id, result)?;
        if decision == ApprovalDecision::DenyAndInterrupt
            && request.kind == CodexApprovalKind::Permissions
        {
            self.turn_interrupt(&request.identity.thread_id, &request.identity.turn_id)?;
        }
        Ok(ProviderReceipt {
            authoritative: true,
            transport: "codex-app-server",
        })
    }

    /// Parse next server request or notification.
    pub fn next_inbound(&mut self) -> Result<CodexInbound, ProviderError> {
        self.require_initialized()?;
        let message = self.transport.next_inbound()?;
        parse_inbound(&message)
    }

    /// Return owned transport for integration or tests.
    pub fn into_inner(self) -> T {
        self.transport
    }

    fn require_initialized(&self) -> Result<(), ProviderError> {
        if !self.initialized {
            return Err(ProviderError::Protocol(
                "Codex app-server client not initialized".into(),
            ));
        }
        Ok(())
    }
}

fn validate_question_answers(
    request: &CodexQuestionRequest,
    answers: &[QuestionAnswer],
) -> Result<(), ProviderError> {
    if request.questions.len() != answers.len() {
        return Err(ProviderError::Protocol(format!(
            "expected {} Codex answers, got {}",
            request.questions.len(),
            answers.len()
        )));
    }
    for question in &request.questions {
        let count = answers
            .iter()
            .filter(|answer| answer.question_id == question.id && !answer.answers.is_empty())
            .count();
        if count != 1 {
            return Err(ProviderError::Protocol(format!(
                "Codex question {} must have one non-empty answer",
                question.id
            )));
        }
    }
    Ok(())
}

fn approval_result(
    request: &CodexApprovalRequest,
    decision: ApprovalDecision,
) -> Result<Value, ProviderError> {
    match request.kind {
        CodexApprovalKind::CommandExecution | CodexApprovalKind::FileChange => {
            let decision = match decision {
                ApprovalDecision::Approve => "accept",
                ApprovalDecision::ApproveForSession => "acceptForSession",
                ApprovalDecision::Deny => "decline",
                ApprovalDecision::DenyAndInterrupt => "cancel",
            };
            Ok(json!({ "decision": decision }))
        }
        CodexApprovalKind::Permissions => {
            let permissions = match decision {
                ApprovalDecision::Approve | ApprovalDecision::ApproveForSession => {
                    request.params.get("permissions").cloned().ok_or_else(|| {
                        ProviderError::Protocol(
                            "Codex permission request has no permissions profile".into(),
                        )
                    })?
                }
                ApprovalDecision::Deny | ApprovalDecision::DenyAndInterrupt => json!({}),
            };
            let scope = if decision == ApprovalDecision::ApproveForSession {
                "session"
            } else {
                "turn"
            };
            Ok(json!({ "permissions": permissions, "scope": scope }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeTransport {
        results: VecDeque<Value>,
        requests: Vec<(String, Value)>,
        notifications: Vec<(String, Value)>,
        responses: Vec<(RpcRequestId, Value)>,
        inbound: VecDeque<Value>,
    }

    impl CodexTransport for FakeTransport {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, ProviderError> {
            self.requests.push((method.to_owned(), params));
            self.results
                .pop_front()
                .ok_or_else(|| ProviderError::Transport("missing fake result".into()))
        }

        fn notify(&mut self, method: &str, params: Value) -> Result<(), ProviderError> {
            self.notifications.push((method.to_owned(), params));
            Ok(())
        }

        fn respond(
            &mut self,
            request_id: &RpcRequestId,
            result: Value,
        ) -> Result<(), ProviderError> {
            self.responses.push((request_id.clone(), result));
            Ok(())
        }

        fn next_inbound(&mut self) -> Result<Value, ProviderError> {
            self.inbound
                .pop_front()
                .ok_or_else(|| ProviderError::Transport("missing fake inbound".into()))
        }
    }

    fn capabilities(request_user_input: bool) -> CodexCapabilities {
        CodexCapabilities {
            cli_version: "codex-cli 0.144.6".into(),
            daemon_version: Some(json!({ "version": "0.144.6" })),
            app_server: true,
            stdio_proxy: true,
            request_user_input,
            approvals: true,
            thread_archive: true,
        }
    }

    fn initialized_client(transport: FakeTransport, rui: bool) -> CodexClient<FakeTransport> {
        let mut client = CodexClient::new(transport, capabilities(rui));
        client.initialize("1.2.3").expect("initialize");
        client
    }

    #[test]
    fn command_specs_use_shared_unix_endpoint_and_proxy() {
        let socket = Path::new("/tmp/fleet codex.sock");
        assert_eq!(
            app_server_command(OsStr::new("codex"), socket).args,
            vec![
                OsString::from("app-server"),
                OsString::from("--listen"),
                OsString::from("unix:///tmp/fleet codex.sock"),
            ]
        );
        assert_eq!(
            proxy_command(OsStr::new("codex"), socket).args,
            vec![
                OsString::from("app-server"),
                OsString::from("proxy"),
                OsString::from("--sock"),
                OsString::from("/tmp/fleet codex.sock"),
            ]
        );
        assert_eq!(
            managed_tui_command(
                OsStr::new("codex"),
                socket,
                [OsString::from("--model"), OsString::from("gpt-5")]
            )
            .args,
            vec![
                OsString::from("--remote"),
                OsString::from("unix:///tmp/fleet codex.sock"),
                OsString::from("--model"),
                OsString::from("gpt-5"),
            ]
        );
    }

    #[test]
    fn schema_probe_capability_gates_request_user_input() {
        let schema = format!(
            "{METHOD_REQUEST_USER_INPUT} ToolRequestUserInputResponse {METHOD_COMMAND_APPROVAL} {METHOD_FILE_APPROVAL} {METHOD_PERMISSIONS_APPROVAL} {METHOD_THREAD_ARCHIVE} ThreadArchiveParams"
        );
        assert_eq!(capabilities_from_schema(&schema), (true, true, true));
        assert_eq!(
            capabilities_from_schema(&schema.replace(METHOD_REQUEST_USER_INPUT, "missing")),
            (false, true, true)
        );
    }

    #[test]
    fn initialize_opts_into_experimental_api_then_notifies() {
        let mut transport = FakeTransport::default();
        transport.results.push_back(json!({ "server": "ok" }));
        let client = initialized_client(transport, true);
        let transport = client.into_inner();

        assert_eq!(transport.requests[0].0, "initialize");
        assert_eq!(
            transport.requests[0].1["capabilities"]["experimentalApi"],
            true
        );
        assert_eq!(
            transport.notifications,
            vec![("initialized".into(), json!({}))]
        );
    }

    #[test]
    fn parses_full_request_user_input_with_exact_ids() {
        let inbound = parse_inbound(&json!({
            "jsonrpc": "2.0",
            "id": "rpc-17",
            "method": METHOD_REQUEST_USER_INPUT,
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-2",
                "itemId": "item-3",
                "autoResolutionMs": 120000,
                "questions": [
                    {
                        "id": "editor",
                        "header": "Editor",
                        "question": "Pick editor",
                        "options": [
                            { "label": "Vim", "description": "Modal" },
                            { "label": "Emacs", "description": "Extensible" }
                        ]
                    },
                    {
                        "id": "reason",
                        "header": "Reason",
                        "question": "Why?",
                        "isOther": true,
                        "isSecret": true
                    }
                ]
            }
        }))
        .expect("parse RUI");

        let CodexInbound::RequestUserInput(request) = inbound else {
            panic!("expected request user input");
        };
        assert_eq!(request.identity.request_id.as_value(), &json!("rpc-17"));
        assert_eq!(request.identity.thread_id, "thread-1");
        assert_eq!(request.identity.turn_id, "turn-2");
        assert_eq!(request.identity.item_id, "item-3");
        assert_eq!(request.questions.len(), 2);
        assert_eq!(request.questions[0].options.len(), 2);
        assert!(request.questions[1].is_other);
        assert!(request.questions[1].is_secret);
        assert_eq!(request.auto_resolution_ms, Some(120_000));
    }

    #[test]
    fn answers_exact_request_id_and_all_question_ids() {
        let mut transport = FakeTransport::default();
        transport.results.push_back(json!({}));
        let mut client = initialized_client(transport, true);
        let request = CodexQuestionRequest {
            identity: CodexItemRequestIdentity {
                request_id: RpcRequestId::new(json!(42)).expect("rpc id"),
                thread_id: "thread-1".into(),
                turn_id: "turn-2".into(),
                item_id: "item-3".into(),
            },
            questions: vec![StructuredQuestion {
                id: "tools".into(),
                header: "Tools".into(),
                question: "Pick tools".into(),
                options: vec![],
                multi_select: true,
                is_other: false,
                is_secret: false,
            }],
            auto_resolution_ms: None,
        };

        client
            .answer_request_user_input(
                &request,
                &[QuestionAnswer {
                    question_id: "tools".into(),
                    answers: vec!["rg".into(), "ast-grep".into()],
                }],
            )
            .expect("answer RUI");
        let transport = client.into_inner();

        assert_eq!(transport.responses[0].0.as_value(), &json!(42));
        assert_eq!(
            transport.responses[0].1,
            json!({ "answers": { "tools": { "answers": ["rg", "ast-grep"] } } })
        );
    }

    #[test]
    fn rui_capability_gate_sends_no_response() {
        let mut transport = FakeTransport::default();
        transport.results.push_back(json!({}));
        let mut client = initialized_client(transport, false);
        let request = CodexQuestionRequest {
            identity: CodexItemRequestIdentity {
                request_id: RpcRequestId::new(json!(1)).expect("rpc id"),
                thread_id: "thread".into(),
                turn_id: "turn".into(),
                item_id: "item".into(),
            },
            questions: vec![],
            auto_resolution_ms: None,
        };

        let error = client.answer_request_user_input(&request, &[]).expect_err("capability gate");
        let transport = client.into_inner();

        assert!(matches!(error, ProviderError::Unsupported(_)));
        assert!(transport.responses.is_empty());
    }

    #[test]
    fn thread_and_turn_methods_preserve_exact_ids() {
        let mut transport = FakeTransport::default();
        transport.results.extend([
            json!({}),
            json!({ "thread": { "id": "thread-1" } }),
            json!({ "thread": { "id": "thread-1", "turns": [] } }),
            json!({ "thread": { "id": "thread-1", "turns": [] } }),
            json!({ "turn": { "id": "turn-2" } }),
            json!({}),
        ]);
        let mut client = initialized_client(transport, true);

        assert_eq!(
            client.thread_start(Path::new("/repo"), Some("gpt-5")).expect("thread start"),
            "thread-1"
        );
        client.thread_resume("thread-1").expect("thread resume");
        client.thread_read("thread-1").expect("thread read");
        assert_eq!(
            client.turn_start("thread-1", "hello").expect("turn start"),
            "turn-2"
        );
        client.turn_interrupt("thread-1", "turn-2").expect("turn interrupt");
        let transport = client.into_inner();

        assert_eq!(
            transport.requests.iter().map(|(method, _)| method.as_str()).collect::<Vec<_>>(),
            vec![
                "initialize",
                "thread/start",
                "thread/resume",
                "thread/read",
                "turn/start",
                "turn/interrupt",
            ]
        );
        assert_eq!(transport.requests[2].1["threadId"], "thread-1");
        assert_eq!(transport.requests[4].1["threadId"], "thread-1");
        assert_eq!(transport.requests[5].1["turnId"], "turn-2");
    }

    #[test]
    fn command_approval_responds_to_exact_rpc_id() {
        let mut transport = FakeTransport::default();
        transport.results.push_back(json!({}));
        let mut client = initialized_client(transport, true);
        let request = CodexApprovalRequest {
            identity: CodexItemRequestIdentity {
                request_id: RpcRequestId::new(json!("approve-9")).expect("rpc id"),
                thread_id: "thread-1".into(),
                turn_id: "turn-2".into(),
                item_id: "item-3".into(),
            },
            kind: CodexApprovalKind::CommandExecution,
            params: json!({}),
        };

        client
            .decide_approval(&request, ApprovalDecision::ApproveForSession)
            .expect("approve");
        let transport = client.into_inner();

        assert_eq!(transport.responses[0].0.as_value(), &json!("approve-9"));
        assert_eq!(
            transport.responses[0].1,
            json!({ "decision": "acceptForSession" })
        );
    }
}
