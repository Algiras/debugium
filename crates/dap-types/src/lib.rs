use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─────────────────────────────────────────────
//  Top-level DAP envelope
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapMessage {
    pub seq: u32,
    #[serde(rename = "type")]
    pub kind: String, // "request" | "response" | "event"
    #[serde(flatten)]
    pub body: DapBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DapBody {
    Request(DapRequest),
    Response(DapResponse),
    Event(DapEvent),
}

// ─────────────────────────────────────────────
//  Requests
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapRequest {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeArgs {
    #[serde(rename = "clientID")]
    pub client_id: String,
    #[serde(rename = "clientName")]
    pub client_name: String,
    #[serde(rename = "adapterID")]
    pub adapter_id: String,
    #[serde(rename = "pathFormat")]
    pub path_format: String,
    #[serde(rename = "linesStartAt1")]
    pub lines_start_at1: bool,
    #[serde(rename = "columnsStartAt1")]
    pub columns_start_at1: bool,
    #[serde(rename = "supportsVariableType", default)]
    pub supports_variable_type: bool,
}

impl Default for InitializeArgs {
    fn default() -> Self {
        Self {
            client_id: "debugium".into(),
            client_name: "Debugium".into(),
            adapter_id: "".into(),
            path_format: "path".into(),
            lines_start_at1: true,
            columns_start_at1: true,
            supports_variable_type: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchArgs {
    #[serde(rename = "type")]
    pub kind: String,
    pub request: String,
    pub program: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(rename = "stopOnEntry", skip_serializing_if = "Option::is_none")]
    pub stop_on_entry: Option<bool>,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetBreakpointsArgs {
    pub source: Source,
    pub breakpoints: Vec<SourceBreakpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceBreakpoint {
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadsArgs {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackTraceArgs {
    #[serde(rename = "threadId")]
    pub thread_id: u32,
    #[serde(rename = "startFrame", skip_serializing_if = "Option::is_none")]
    pub start_frame: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub levels: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopesArgs {
    #[serde(rename = "frameId")]
    pub frame_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariablesArgs {
    #[serde(rename = "variablesReference")]
    pub variables_reference: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinueArgs {
    #[serde(rename = "threadId")]
    pub thread_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextArgs {
    #[serde(rename = "threadId")]
    pub thread_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepInArgs {
    #[serde(rename = "threadId")]
    pub thread_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutArgs {
    #[serde(rename = "threadId")]
    pub thread_id: u32,
}

// ─────────────────────────────────────────────
//  Responses
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapResponse {
    #[serde(rename = "request_seq")]
    pub request_seq: u32,
    pub success: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadsBody {
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackTraceBody {
    #[serde(rename = "stackFrames")]
    pub stack_frames: Vec<StackFrame>,
    #[serde(rename = "totalFrames", skip_serializing_if = "Option::is_none")]
    pub total_frames: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub id: u32,
    pub name: String,
    pub line: u32,
    pub column: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(rename = "presentationHint", skip_serializing_if = "Option::is_none")]
    pub presentation_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "sourceReference", skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopesBody {
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub name: String,
    #[serde(rename = "variablesReference")]
    pub variables_reference: u64,
    pub expensive: bool,
    #[serde(rename = "presentationHint", skip_serializing_if = "Option::is_none")]
    pub presentation_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariablesBody {
    pub variables: Vec<Variable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(rename = "variablesReference")]
    pub variables_reference: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetBreakpointsBody {
    pub breakpoints: Vec<Breakpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakpoint {
    pub id: Option<u32>,
    pub verified: bool,
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ─────────────────────────────────────────────
//  Events
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapEvent {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoppedEventBody {
    pub reason: String,
    #[serde(rename = "threadId", skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<u32>,
    #[serde(rename = "allThreadsStopped", skip_serializing_if = "Option::is_none")]
    pub all_threads_stopped: Option<bool>,
    #[serde(rename = "hitBreakpointIds", skip_serializing_if = "Option::is_none")]
    pub hit_breakpoint_ids: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputEventBody {
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadEventBody {
    #[serde(rename = "threadId")]
    pub thread_id: u32,
    pub reason: String,
}

// ─────────────────────────────────────────────
//  WebSocket envelope (server → UI)
// ─────────────────────────────────────────────

/// Wraps a raw DAP message with the originating session ID so
/// the UI can route it to the correct session panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsEnvelope {
    pub session_id: String,
    pub msg: Value,
}

/// Commands from UI → server over WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsCommand {
    pub session_id: String,
    pub command: String,
    #[serde(default)]
    pub arguments: Value,
}
