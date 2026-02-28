use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

use dap_types::{WsCommand, WsEnvelope};

// ─────────────────────────────────────────────
//  Session state (reactive signals)
// ─────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Thread {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StackFrame {
    pub id: u32,
    pub name: String,
    pub line: u32,
    pub file: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionState {
    pub id: String,
    pub status: String, // "running" | "paused" | "ended"
    pub threads: Vec<Thread>,
    pub active_thread_id: u32,
    pub stack_frames: Vec<StackFrame>,
    pub variables: Vec<Variable>,
    pub source_path: Option<String>,
    pub source_line: Option<u32>,
    pub console_logs: Vec<ConsoleLog>,
}

#[derive(Clone, Debug)]
pub struct ConsoleLog {
    pub tag: String,
    pub message: String,
    pub class: String,
}

// ─────────────────────────────────────────────
//  App root
// ─────────────────────────────────────────────

#[component]
pub fn App() -> impl IntoView {
    let sessions: RwSignal<Vec<String>> = RwSignal::new(vec![]);
    let active_session: RwSignal<Option<String>> = RwSignal::new(None);
    let session_data: RwSignal<std::collections::HashMap<String, SessionState>> =
        RwSignal::new(Default::default());
    let ws_sender: RwSignal<Option<js_sys::Function>> = RwSignal::new(None);

    // Connect WebSocket
    let ws_url = {
        let loc = web_sys::window().unwrap().location();
        let host = loc.host().unwrap();
        format!("ws://{host}/ws?session=default")
    };

    let sessions_c = sessions.clone();
    let active_c = active_session.clone();
    let data_c = session_data.clone();
    let ws_sender_c = ws_sender.clone();

    Effect::new(move |_| {
        let ws = WebSocket::new(&ws_url).unwrap();
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

        // Store send fn
        let ws2 = ws.clone();
        let send_fn = Closure::wrap(Box::new(move |msg: String| {
            let _ = ws2.send_with_str(&msg);
        }) as Box<dyn Fn(String)>);
        ws_sender_c.set(Some(send_fn.as_ref().unchecked_ref::<js_sys::Function>().clone()));
        send_fn.forget();

        let sessions_h = sessions_c.clone();
        let active_h = active_c.clone();
        let data_h = data_c.clone();

        let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Some(text) = e.data().as_string() {
                if let Ok(envelope) = serde_json::from_str::<WsEnvelope>(&text) {
                    handle_envelope(envelope, &sessions_h, &active_h, &data_h);
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    });

    let send_command = move |command: String, args: Value| {
        if let Some(session_id) = active_session.get() {
            let cmd = WsCommand {
                session_id,
                command,
                arguments: args,
            };
            if let (Ok(json), Some(f)) = (
                serde_json::to_string(&cmd),
                ws_sender.get(),
            ) {
                let _ = f.call1(&JsValue::NULL, &JsValue::from_str(&json));
            }
        }
    };

    let send_cmd = send_command.clone();

    view! {
        <div id="app">
            <Header send=Callback::new(move |(cmd, args)| send_command(cmd, args)) />
            <div class="dashboard-wrapper">
                <SessionsPanel
                    sessions=sessions.read_only()
                    active=active_session
                />
                <main class="center-content">
                    <SourcePanel
                        session_data=session_data.read_only()
                        active_session=active_session.read_only()
                    />
                    <ConsolePanel
                        session_data=session_data.read_only()
                        active_session=active_session.read_only()
                    />
                </main>
                <aside class="sidebar sidebar-right">
                    <StackPanel
                        session_data=session_data.read_only()
                        active_session=active_session.read_only()
                    />
                    <VariablesPanel
                        session_data=session_data.read_only()
                        active_session=active_session.read_only()
                    />
                </aside>
            </div>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Message handler
// ─────────────────────────────────────────────

fn handle_envelope(
    envelope: WsEnvelope,
    sessions: &RwSignal<Vec<String>>,
    active_session: &RwSignal<Option<String>>,
    data: &RwSignal<std::collections::HashMap<String, SessionState>>,
) {
    let id = envelope.session_id.clone();

    // Register new session
    sessions.update(|s| {
        if !s.contains(&id) {
            s.push(id.clone());
        }
    });

    if active_session.get_untracked().is_none() {
        active_session.set(Some(id.clone()));
    }

    data.update(|map| {
        let state = map.entry(id.clone()).or_insert_with(|| SessionState {
            id: id.clone(),
            status: "running".into(),
            ..Default::default()
        });

        let msg = &envelope.msg;
        let kind = msg.get("type").and_then(Value::as_str).unwrap_or("");

        match kind {
            "event" => {
                let event = msg.get("event").and_then(Value::as_str).unwrap_or("");
                match event {
                    "stopped" => {
                        state.status = "paused".into();
                        if let Some(body) = msg.get("body") {
                            if let Some(tid) = body.get("threadId").and_then(Value::as_u64) {
                                state.active_thread_id = tid as u32;
                            }
                        }
                    }
                    "continued" => state.status = "running".into(),
                    "terminated" => state.status = "ended".into(),
                    "output" => {
                        if let Some(body) = msg.get("body") {
                            let output = body.get("output").and_then(Value::as_str).unwrap_or("").to_string();
                            let cat = body.get("category").and_then(Value::as_str).unwrap_or("console");
                            if cat != "telemetry" {
                                state.console_logs.push(ConsoleLog {
                                    tag: "out".into(),
                                    message: output.trim().to_string(),
                                    class: "log-text".into(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            "response" => {
                let command = msg.get("command").and_then(Value::as_str).unwrap_or("");
                let success = msg.get("success").and_then(Value::as_bool).unwrap_or(false);
                if !success { return; }

                state.console_logs.push(ConsoleLog {
                    tag: "✓".into(),
                    message: format!("{command} OK"),
                    class: "log-response".into(),
                });

                if command == "threads" {
                    if let Some(threads) = msg.get("body").and_then(|b| b.get("threads")) {
                        state.threads = serde_json::from_value(threads.clone()).unwrap_or_default();
                    }
                }

                if command == "stackTrace" {
                    if let Some(frames) = msg.get("body").and_then(|b| b.get("stackFrames")) {
                        let raw: Vec<Value> = serde_json::from_value(frames.clone()).unwrap_or_default();
                        state.stack_frames = raw.iter().map(|f| StackFrame {
                            id: f.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                            name: f.get("name").and_then(Value::as_str).unwrap_or("?").to_string(),
                            line: f.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                            file: f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str).unwrap_or("").to_string(),
                        }).collect();
                        if let Some(top) = state.stack_frames.first() {
                            state.source_path = Some(top.file.clone());
                            state.source_line = Some(top.line);
                        }
                    }
                }

                if command == "variables" {
                    if let Some(vars) = msg.get("body").and_then(|b| b.get("variables")) {
                        state.variables = serde_json::from_value(vars.clone()).unwrap_or_default();
                    }
                }
            }
            _ => {}
        }
    });
}

// ─────────────────────────────────────────────
//  Header component
// ─────────────────────────────────────────────

#[component]
fn Header(send: Callback<(String, Value)>) -> impl IntoView {
    let s = send.clone();
    view! {
        <header>
            <div class="header-left">
                <h1>"Debugium " <span class="badge">"Live"</span></h1>
            </div>
            <div class="header-controls">
                <button class="debug-btn btn-continue"
                    on:click=move |_| s.call(("continue".into(), serde_json::json!({ "threadId": 1 })))>
                    <span class="btn-icon">"▶"</span>" Continue"
                </button>
                <button class="debug-btn btn-step"
                    on:click=move |_| send.call(("stepIn".into(), serde_json::json!({ "threadId": 1 })))>
                    <span class="btn-icon">"↓"</span>" Step In"
                </button>
                <button class="debug-btn btn-over"
                    on:click=move |_| send.call(("next".into(), serde_json::json!({ "threadId": 1 })))>
                    <span class="btn-icon">"↷"</span>" Step Over"
                </button>
                <button class="debug-btn"
                    on:click=move |_| send.call(("stepOut".into(), serde_json::json!({ "threadId": 1 })))>
                    <span class="btn-icon">"↑"</span>" Step Out"
                </button>
            </div>
        </header>
    }
}

// ─────────────────────────────────────────────
//  Sessions sidebar
// ─────────────────────────────────────────────

#[component]
fn SessionsPanel(
    sessions: ReadSignal<Vec<String>>,
    active: RwSignal<Option<String>>,
) -> impl IntoView {
    view! {
        <aside class="sidebar sidebar-left">
            <div class="panel">
                <div class="panel-header"><h2>"Sessions"</h2></div>
                <div class="panel-content scrollable">
                    <ul class="list-view">
                        <For
                            each=move || sessions.get()
                            key=|id| id.clone()
                            children=move |id| {
                                let id_c = id.clone();
                                let is_active = move || active.get().as_deref() == Some(&id_c);
                                let id_click = id.clone();
                                view! {
                                    <li
                                        class:active-item=is_active
                                        on:click=move |_| active.set(Some(id_click.clone()))>
                                        <span class="session-icon">
                                            {if is_active() { "⏸" } else { "▶" }}
                                        </span>
                                        {id.clone()}
                                    </li>
                                }
                            }
                        />
                        <Show when=move || sessions.get().is_empty()>
                            <li class="empty-state">"No sessions"</li>
                        </Show>
                    </ul>
                </div>
            </div>
        </aside>
    }
}

// ─────────────────────────────────────────────
//  Source panel (CodeMirror via JS interop)
// ─────────────────────────────────────────────

#[component]
fn SourcePanel(
    session_data: ReadSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let file_label = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .and_then(|s| s.source_path.map(|p| format!("{}:{}", basename(&p), s.source_line.unwrap_or(0))))
            .unwrap_or_else(|| "No file mapped".into())
    };

    view! {
        <div class="panel source-panel">
            <div class="panel-header">
                <h2>"Source"</h2>
                <span class="file-path">{file_label}</span>
            </div>
            <div class="panel-content" id="code-view-container">
                // CodeMirror is initialized via JS interop after mount
                // The JS shim in index.html handles the editor lifecycle
            </div>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Stack panel
// ─────────────────────────────────────────────

#[component]
fn StackPanel(
    session_data: ReadSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let frames = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.stack_frames)
            .unwrap_or_default()
    };

    view! {
        <div class="panel">
            <div class="panel-header"><h2>"Threads & Stack"</h2></div>
            <div class="panel-content scrollable">
                <ul class="list-view">
                    <For
                        each=frames
                        key=|f| f.id
                        children=move |f| {
                            let is_top = f.id == frames().first().map(|f| f.id).unwrap_or(0);
                            view! {
                                <li class:frame-active=move || is_top class:frame-subtle=move || !is_top>
                                    <span class="frame-icon">{if is_top { "→" } else { " " }}</span>
                                    " " {f.name.clone()}
                                    <span class="frame-location">{basename(&f.file)}":"{ f.line}</span>
                                </li>
                            }
                        }
                    />
                    <Show when=move || frames().is_empty()>
                        <li class="empty-state">"No active threads"</li>
                    </Show>
                </ul>
            </div>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Variables panel
// ─────────────────────────────────────────────

#[component]
fn VariablesPanel(
    session_data: ReadSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let vars = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.variables)
            .unwrap_or_default()
    };

    view! {
        <div class="panel">
            <div class="panel-header"><h2>"Variables"</h2></div>
            <div class="panel-content scrollable">
                <ul class="list-view">
                    <For
                        each=vars
                        key=|v| v.name.clone()
                        children=move |v| {
                            let type_class = match v.kind.as_deref() {
                                Some("int") | Some("float") => "var-number",
                                Some("str") => "var-string",
                                Some("bool") => "var-bool",
                                _ => "var-value",
                            };
                            view! {
                                <li class="var-item">
                                    <span class="var-name">{v.name}</span>
                                    <span class="var-sep">"="</span>
                                    <span class={type_class}>{v.value}</span>
                                </li>
                            }
                        }
                    />
                    <Show when=move || vars().is_empty()>
                        <li class="empty-state">"No variables in scope"</li>
                    </Show>
                </ul>
            </div>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Console panel
// ─────────────────────────────────────────────

#[component]
fn ConsolePanel(
    session_data: ReadSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let logs = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.console_logs)
            .unwrap_or_default()
    };

    view! {
        <div class="panel console-panel">
            <div class="panel-header"><h2>"Debug Console"</h2></div>
            <div class="panel-content scrollable" id="console-logs">
                <For
                    each=logs
                    key=|l| l.message.clone()
                    children=move |l| {
                        view! {
                            <div class={format!("log-entry {}", l.class)}>
                                <span class="log-tag">{l.tag}</span>
                                " " {l.message}
                            </div>
                        }
                    }
                />
            </div>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Utils
// ─────────────────────────────────────────────

fn basename(path: &str) -> String {
    path.split('/').last().unwrap_or(path).to_string()
}

// ─────────────────────────────────────────────
//  WASM entry point
// ─────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
