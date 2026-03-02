use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, WebSocket};
use js_sys::Reflect;

use dap_types::{WsCommand, WsEnvelope};

mod editor;

// ─────────────────────────────────────────────
//  Session state types
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
    pub variables_reference: u64,
}

/// Breakpoint with optional condition / logMessage.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BreakpointSpec {
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionState {
    pub id: String,
    pub status: String,     // "running" | "paused" | "ended"
    pub threads: Vec<Thread>,
    pub active_thread_id: u32,
    pub stack_frames: Vec<StackFrame>,
    pub variables: Vec<Variable>,
    pub source_path: Option<String>,
    pub source_code: Option<String>,
    pub source_line: Option<u32>,
    pub console_logs: Vec<ConsoleLog>,
    pub event_seq: u32,     // increments on every event — drives animation re-triggers
    /// file path → breakpoint specs (with optional condition/logMessage)
    pub breakpoints: std::collections::HashMap<String, Vec<BreakpointSpec>>,
    /// variablesReference → expanded children
    pub expanded_vars: std::collections::HashMap<u64, Vec<Variable>>,
    /// tracks which variablesReference was last requested for expansion
    pub pending_var_ref: Option<u64>,
    /// tracks which file a pending setBreakpoints request is for
    pub pending_bp_file: Option<String>,
    /// completions dropdown items
    pub completions: Vec<String>,
    /// raw scopes from the last scopes response
    pub scopes: Vec<Value>,
    /// variablesReference of the scope we want to auto-expand
    pub pending_scope_var_ref: Option<i64>,
    /// watch expression results: (expression, result)
    pub watch_results: Vec<(String, String)>,
    /// files opened as tabs (ordered)
    pub open_files: Vec<String>,
    /// active stack frame id (for scopes/eval context)
    pub active_frame_id: Option<u32>,
    /// LLM-set gutter annotations: file → vec of (line, message, color)
    pub annotations: Vec<AnnotationEntry>,
    /// LLM-set findings: structured observations shown in findings panel
    pub findings: Vec<FindingEntry>,
    /// previous variable values for diff highlighting
    pub prev_variables: std::collections::HashMap<String, String>,
    /// set of variable names that changed at last stop
    pub changed_vars: std::collections::HashSet<String>,
    /// execution timeline (one entry per stopped event)
    pub timeline: Vec<TimelineEntryUi>,
    /// last LLM query tool + detail, shown in the status bar
    pub last_llm_query: String,
    /// per-session layout state (saved when switching away, restored when switching back)
    pub saved_layout: SavedLayoutState,
    /// most recently inspected variable reference (for visual emphasis)
    pub last_inspected_var_ref: Option<u64>,
    /// most recently verified breakpoint from adapter
    pub recent_verified_breakpoint: Option<(String, u32)>,
}

/// Snapshot of per-session layout fields (saved/restored on session switch).
#[derive(Clone, Debug, Default)]
pub struct SavedLayoutState {
    pub watches: Vec<String>,
    pub active_tab: Option<String>,
    pub var_filter: String,
    pub console_collapsed: bool,
    pub vars_collapsed: bool,
    pub bps_collapsed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AnnotationEntry {
    pub id: u32,
    pub file: String,
    pub line: u32,
    pub message: String,
    pub color: String,
}

#[derive(Clone, Debug, Default)]
pub struct FindingEntry {
    pub id: u32,
    pub message: String,
    pub level: String,
    pub timestamp: String,
}

#[derive(Clone, Debug, Default)]
pub struct TimelineEntryUi {
    pub id: u32,
    pub file: String,
    pub line: u32,
    pub timestamp: String,
    pub changed_vars: Vec<String>,
    pub stack_summary: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ConsoleLog {
    pub tag: String,
    pub message: String,
    pub class: String,
    pub seq: u32,  // unique key for animation identity
}

// ─────────────────────────────────────────────
//  Shared contexts
// ─────────────────────────────────────────────

/// Maps session_id → WS send function.
#[derive(Clone)]
pub struct WsSenders(pub RwSignal<std::collections::HashMap<String, js_sys::Function>>);

/// Maps session_id → WS connected state.
#[derive(Clone)]
pub struct WsConnected(pub RwSignal<std::collections::HashMap<String, bool>>);

/// Current command in flight: (session_id, label) or None.
#[derive(Clone)]
pub struct CommandInFlight(pub RwSignal<Option<(String, &'static str)>>);

/// Last completed command label (cleared after flash animation).
#[derive(Clone)]
pub struct LastCompleted(pub RwSignal<Option<&'static str>>);

/// Session that last received a DAP event (cleared after flash animation).
#[derive(Clone)]
pub struct LastEventSession(pub RwSignal<Option<String>>);

/// Global UI error message (toast)
#[derive(Clone)]
pub struct GlobalError(pub RwSignal<Option<String>>);

/// Left/right sidebar collapsed state.
#[derive(Clone)]
pub struct LayoutState {
    pub left_collapsed: RwSignal<bool>,
    pub right_collapsed: RwSignal<bool>,
    pub narrow_mode: RwSignal<bool>,
    pub console_collapsed: RwSignal<bool>,
    pub vars_collapsed: RwSignal<bool>,
    pub left_width: RwSignal<u32>,
    pub right_width: RwSignal<u32>,
    /// watch expressions entered by user
    pub watches: RwSignal<Vec<String>>,
    /// currently active tab in the source panel (file path)
    pub active_tab: RwSignal<Option<String>>,
    /// breakpoints panel collapsed state
    pub bps_collapsed: RwSignal<bool>,
    /// dark mode toggle
    pub dark_mode: RwSignal<bool>,
    /// overall UI density preset: slim | standard | full
    pub layout_preset: RwSignal<String>,
    /// variable name filter text
    pub var_filter: RwSignal<String>,
}

// ─────────────────────────────────────────────
//  Drag-to-resize handle
// ─────────────────────────────────────────────

#[component]
fn ResizeHandle(
    width: RwSignal<u32>,
    min_w: u32,
    max_w: u32,
    /// true = dragging left makes it wider (right sidebar), false = right makes wider (left)
    invert: bool,
) -> impl IntoView {
    let dragging: RwSignal<bool> = RwSignal::new(false);
    let start_x: RwSignal<i32> = RwSignal::new(0);
    let start_w: RwSignal<u32> = RwSignal::new(0);

    view! {
        <div
            class="resize-handle"
            on:pointerdown=move |e| {
                use wasm_bindgen::JsCast;
                if let Some(el) = e.current_target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                {
                    let _ = el.set_pointer_capture(e.pointer_id());
                }
                dragging.set(true);
                start_x.set(e.client_x());
                start_w.set(width.get_untracked());
                e.prevent_default();
            }
            on:pointermove=move |e| {
                if !dragging.get_untracked() { return; }
                let raw_delta = e.client_x() - start_x.get_untracked();
                let delta = if invert { -raw_delta } else { raw_delta };
                let new_w = (start_w.get_untracked() as i32 + delta)
                    .clamp(min_w as i32, max_w as i32) as u32;
                width.set(new_w);
            }
            on:pointerup=move |_| { dragging.set(false); }
            on:lostpointercapture=move |_| { dragging.set(false); }
        ></div>
    }
}

// ─────────────────────────────────────────────
//  App root
// ─────────────────────────────────────────────

#[component]
pub fn App() -> impl IntoView {
    let sessions: RwSignal<Vec<String>> = RwSignal::new(vec![]);
    let session_metas: RwSignal<std::collections::HashMap<String, Value>> = RwSignal::new(Default::default());
    let active_session: RwSignal<Option<String>> = RwSignal::new(None);
    let session_data: RwSignal<std::collections::HashMap<String, SessionState>> =
        RwSignal::new(Default::default());

    let ws_senders: RwSignal<std::collections::HashMap<String, js_sys::Function>> =
        RwSignal::new(Default::default());
    let ws_connected: RwSignal<std::collections::HashMap<String, bool>> =
        RwSignal::new(Default::default());
    let cmd_in_flight: RwSignal<Option<(String, &'static str)>> = RwSignal::new(None);
    let last_completed: RwSignal<Option<&'static str>> = RwSignal::new(None);
    let last_event_session: RwSignal<Option<String>> = RwSignal::new(None);
    let global_error: RwSignal<Option<String>> = RwSignal::new(None);
    let reconnect_tick: RwSignal<u32> = RwSignal::new(0);
    let onboarding_dismissed: RwSignal<bool> = RwSignal::new(false);
    let show_launch_modal: RwSignal<bool> = RwSignal::new(false);
    let layout = LayoutState {
        left_collapsed: RwSignal::new(false),
        right_collapsed: RwSignal::new(false),
        narrow_mode: RwSignal::new(false),
        console_collapsed: RwSignal::new(false),
        vars_collapsed: RwSignal::new(false),
        left_width: RwSignal::new(200u32),
        right_width: RwSignal::new(272u32),
        watches: RwSignal::new(vec![]),
        active_tab: RwSignal::new(None),
        bps_collapsed: RwSignal::new(true),
        dark_mode: RwSignal::new({
            // Restore from localStorage
            web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .and_then(|s| s.get_item("debugium_dark_mode").ok().flatten())
                .map(|v| v == "true")
                .unwrap_or(false)
        }),
        layout_preset: RwSignal::new({
            let saved = read_window_storage("debugium.layout_preset")
                .unwrap_or_else(|| "standard".to_string());
            match saved.as_str() {
                "slim" | "standard" | "full" => saved,
                _ => "standard".to_string(),
            }
        }),
        var_filter: RwSignal::new(String::new()),
    };

    provide_context(WsSenders(ws_senders));
    provide_context(WsConnected(ws_connected));
    provide_context(CommandInFlight(cmd_in_flight));
    provide_context(LastCompleted(last_completed));
    provide_context(LastEventSession(last_event_session));
    provide_context(GlobalError(global_error));
    provide_context(layout.clone());

    // Auto-clear global UI error toast after a short delay.
    {
        let ge = global_error;
        Effect::new(move |_| {
            if ge.get().is_some() {
                let ge_clear = ge;
                leptos::task::spawn_local(async move {
                    gloo_timers::future::sleep(std::time::Duration::from_millis(4500)).await;
                    ge_clear.set(None);
                });
            }
        });
    }

    // Persist onboarding dismissal across reloads.
    if let Some(saved) = read_window_storage("debugium.onboarding.dismissed") {
        onboarding_dismissed.set(saved == "1");
    }
    {
        let onboarding_dismissed_save = onboarding_dismissed;
        Effect::new(move |_| {
            let val = if onboarding_dismissed_save.get() { "1" } else { "0" };
            write_window_storage("debugium.onboarding.dismissed", val);
        });
    }

    // ── Save/restore per-session layout state on session switch ──
    {
        let layout_sr = layout.clone();
        let prev_session: RwSignal<Option<String>> = RwSignal::new(None);
        Effect::new(move |_| {
            let new_sid = active_session.get();
            let old_sid = prev_session.get_untracked();
            if new_sid == old_sid { return; }

            // Save current signals → old session's saved_layout
            if let Some(ref old) = old_sid {
                session_data.update(|m| {
                    if let Some(s) = m.get_mut(old) {
                        s.saved_layout = SavedLayoutState {
                            watches: layout_sr.watches.get_untracked(),
                            active_tab: layout_sr.active_tab.get_untracked(),
                            var_filter: layout_sr.var_filter.get_untracked(),
                            console_collapsed: layout_sr.console_collapsed.get_untracked(),
                            vars_collapsed: layout_sr.vars_collapsed.get_untracked(),
                            bps_collapsed: layout_sr.bps_collapsed.get_untracked(),
                        };
                    }
                });
            }

            // Restore from new session's saved_layout
            if let Some(ref new) = new_sid {
                let saved = session_data.get_untracked()
                    .get(new).map(|s| s.saved_layout.clone())
                    .unwrap_or_default();
                layout_sr.watches.set(saved.watches);
                layout_sr.active_tab.set(saved.active_tab);
                layout_sr.var_filter.set(saved.var_filter);
                layout_sr.console_collapsed.set(saved.console_collapsed);
                layout_sr.vars_collapsed.set(saved.vars_collapsed);
                layout_sr.bps_collapsed.set(saved.bps_collapsed);
            }

            prev_session.set(new_sid);
        });
    }

    let host = web_sys::window().unwrap().location().host().unwrap();

    // Keep narrow_mode in sync with viewport width for split-screen setups.
    {
        let nm_r = layout.narrow_mode;
        let lw_r = layout.left_width;
        let rw_r = layout.right_width;
        let apply_responsive = move || {
            let Some(win) = web_sys::window() else { return; };
            let Ok(width) = win.inner_width() else { return; };
            let Some(width_px) = width.as_f64() else { return; };
            let narrow = width_px <= 960.0;
            nm_r.set(narrow);
            if narrow {
                lw_r.update(|v| *v = (*v).clamp(140, 180));
                rw_r.update(|v| *v = (*v).clamp(170, 220));
            }
        };
        apply_responsive();
        let on_resize = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            apply_responsive();
        }) as Box<dyn FnMut(_)>);
        if let Some(win) = web_sys::window() {
            let _ = win.add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref());
        }
        on_resize.forget();
    }

    // ── Poll /sessions continuously ───────────────────
    leptos::task::spawn_local(async move {
        // Track which session IDs the polling loop has already seen,
        // so we can detect genuinely new ones even if handle_envelope added them first.
        let mut poll_known: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            if let Ok(resp) = gloo_net::http::Request::get("/sessions").send().await {
                if let Ok(data) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = data.get("sessions").and_then(|s| s.as_array()) {
                        let ids: Vec<String> = arr.iter().filter_map(|v| {
                            v.as_str().map(str::to_string)
                                .or_else(|| v.get("id").and_then(|id| id.as_str()).map(str::to_string))
                        }).collect();
                        if !ids.is_empty() {
                            // Detect new sessions vs what polling has seen before
                            let mut new_from_poll: Vec<String> = Vec::new();
                            for id in &ids {
                                if poll_known.insert(id.clone()) {
                                    new_from_poll.push(id.clone());
                                }
                            }
                            sessions.update(|s| {
                                for id in &ids {
                                    if !s.contains(id) { s.push(id.clone()); }
                                }
                            });
                            // Auto-switch to a genuinely new session (not the initial discovery)
                            if let Some(newest) = new_from_poll.last() {
                                if poll_known.len() > new_from_poll.len() {
                                    active_session.set(Some(newest.clone()));
                                }
                            }
                        }
                        // Store enriched meta per session
                        let metas_snap: Vec<(String, Value)> = arr.iter().filter_map(|v| {
                            v.get("id").and_then(|id| id.as_str()).map(|id| (id.to_string(), v.clone()))
                        }).collect();
                        if !metas_snap.is_empty() {
                            session_metas.update(|map| {
                                for (id, meta) in metas_snap { map.insert(id, meta); }
                            });
                        }
                    }
                }
            }
            gloo_timers::future::sleep(std::time::Duration::from_millis(2000)).await;
        }
    });

    // ── Open a WS per session as they appear ──────────
    let layout_for_ws = layout.clone();
    Effect::new({
        let sessions = sessions.clone();
        let active_session = active_session.clone();
        let session_data = session_data.clone();
        let ws_senders = ws_senders.clone();
        let ws_connected = ws_connected.clone();
        let cmd_in_flight = cmd_in_flight.clone();
        let last_event_session = last_event_session.clone();
        let host = host.clone();
        let layout = layout_for_ws;

        move |_| {
            let current_sessions = sessions.get();
            // Also track reconnect_tick so Effect re-runs when we need to reconnect
            let _tick = reconnect_tick.get();
            let connected: Vec<String> = ws_senders.get_untracked().keys().cloned().collect();

            for id in current_sessions {
                if connected.contains(&id) { continue; }

                // Mark as disconnected initially
                ws_connected.update(|m| { m.insert(id.clone(), false); });

                let ws_url = format!("ws://{}/ws?session={}", host, id);
                let ws = match WebSocket::new(&ws_url) { Ok(w) => w, Err(_) => continue };
                ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

                // onopen: mark connected; kick off data chain if session already paused
                {
                    let ws_connected_open = ws_connected.clone();
                    let ws_senders_open = ws_senders.clone();
                    let session_data_open = session_data.clone();
                    let id_open = id.clone();
                    let onopen = Closure::wrap(Box::new(move |_: JsValue| {
                        ws_connected_open.update(|m| { m.insert(id_open.clone(), true); });
                        // Fetch /state to replay the stopped event for late-joining clients
                        let session_data_state = session_data_open.clone();
                        let ws_senders_state = ws_senders_open.clone();
                        let id_state = id_open.clone();
                        leptos::task::spawn_local(async move {
                            if let Ok(resp) = gloo_net::http::Request::get(
                                &format!("/state?session={}", id_state)).send().await {
                                if let Ok(json) = resp.json::<serde_json::Value>().await {
                                    if json.get("paused").and_then(Value::as_bool).unwrap_or(false) {
                                        if let Some(ev) = json.get("stopped_event") {
                                            // Replay the stopped event through the normal handler
                                            let envelope = serde_json::json!({
                                                "session": id_state,
                                                "msg": ev
                                            });
                                            if let Ok(env) = serde_json::from_value::<WsEnvelope>(envelope) {
                                                // Get thread from stopped event and request stack
                                                let thread_id = ev.get("body")
                                                    .and_then(|b| b.get("threadId"))
                                                    .and_then(Value::as_u64)
                                                    .unwrap_or(1) as u32;
                                                // Update status to paused
                                                session_data_state.update(|map| {
                                                    let s = map.entry(id_state.clone()).or_insert_with(|| SessionState {
                                                        id: id_state.clone(), status: "running".into(), ..Default::default()
                                                    });
                                                    s.status = "paused".into();
                                                    s.active_thread_id = thread_id;
                                                });
                                                // Request stack trace
                                                send_cmd(&ws_senders_state, &id_state, "stackTrace",
                                                    serde_json::json!({ "threadId": thread_id, "levels": 20 }));
                                            }
                                        }
                                    }
                                }
                            }
                        });
                        // Fetch /annotations and /findings to populate initial state
                        let session_data_ann = session_data_open.clone();
                        let id_ann = id_open.clone();
                        leptos::task::spawn_local(async move {
                            if let Ok(resp) = gloo_net::http::Request::get(
                                &format!("/annotations?session={}", id_ann)).send().await {
                                if let Ok(json) = resp.json::<serde_json::Value>().await {
                                    if let Some(arr) = json.get("annotations").and_then(|v| v.as_array()) {
                                        let entries: Vec<AnnotationEntry> = arr.iter().filter_map(|a| Some(AnnotationEntry {
                                            id: a.get("id").and_then(Value::as_u64)? as u32,
                                            file: a.get("file").and_then(Value::as_str)?.to_string(),
                                            line: a.get("line").and_then(Value::as_u64)? as u32,
                                            message: a.get("message").and_then(Value::as_str)?.to_string(),
                                            color: a.get("color").and_then(Value::as_str).unwrap_or("blue").to_string(),
                                        })).collect();
                                        if !entries.is_empty() {
                                            session_data_ann.update(|map| {
                                                let state = map.entry(id_ann.clone()).or_insert_with(|| SessionState {
                                                    id: id_ann.clone(), status: "running".into(), ..Default::default()
                                                });
                                                state.annotations = entries;
                                            });
                                        }
                                    }
                                }
                            }
                            if let Ok(resp) = gloo_net::http::Request::get(
                                &format!("/findings?session={}", id_ann)).send().await {
                                if let Ok(json) = resp.json::<serde_json::Value>().await {
                                    if let Some(arr) = json.get("findings").and_then(|v| v.as_array()) {
                                        let entries: Vec<FindingEntry> = arr.iter().filter_map(|f| Some(FindingEntry {
                                            id: f.get("id").and_then(Value::as_u64)? as u32,
                                            message: f.get("message").and_then(Value::as_str)?.to_string(),
                                            level: f.get("level").and_then(Value::as_str).unwrap_or("info").to_string(),
                                            timestamp: f.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
                                        })).collect();
                                        if !entries.is_empty() {
                                            session_data_ann.update(|map| {
                                                let state = map.entry(id_ann.clone()).or_insert_with(|| SessionState {
                                                    id: id_ann.clone(), status: "running".into(), ..Default::default()
                                                });
                                                state.findings = entries;
                                            });
                                        }
                                    }
                                }
                            }
                        });

                        // Fetch /timeline to restore timeline history
                        let session_data_tl = session_data_open.clone();
                        let id_tl = id_open.clone();
                        leptos::task::spawn_local(async move {
                            if let Ok(resp) = gloo_net::http::Request::get(
                                &format!("/timeline?session={}&limit=100", id_tl)).send().await {
                                if let Ok(json) = resp.json::<serde_json::Value>().await {
                                    if let Some(arr) = json.get("timeline").and_then(|v| v.as_array()) {
                                        let entries: Vec<TimelineEntryUi> = arr.iter().filter_map(|e| Some(TimelineEntryUi {
                                            id: e.get("id").and_then(Value::as_u64)? as u32,
                                            file: e.get("file").and_then(Value::as_str).unwrap_or("").to_string(),
                                            line: e.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                                            timestamp: e.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
                                            changed_vars: e.get("changed_vars").and_then(Value::as_array)
                                                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                                                .unwrap_or_default(),
                                            stack_summary: e.get("stack_summary").and_then(Value::as_array)
                                                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                                                .unwrap_or_default(),
                                        })).collect();
                                        if !entries.is_empty() {
                                            session_data_tl.update(|map| {
                                                let state = map.entry(id_tl.clone()).or_insert_with(|| SessionState {
                                                    id: id_tl.clone(), status: "running".into(), ..Default::default()
                                                });
                                                state.timeline = entries;
                                            });
                                        }
                                    }
                                }
                            }
                        });

                        // Fetch /breakpoints to sync any breakpoints set before UI connected
                        let session_data_bp = session_data_open.clone();
                        let id_bp = id_open.clone();
                        leptos::task::spawn_local(async move {
                            if let Ok(resp) = gloo_net::http::Request::get(
                                &format!("/breakpoints?session={}", id_bp)).send().await {
                                if let Ok(json) = resp.json::<serde_json::Value>().await {
                                    if let Some(bps) = json.get("breakpoints").and_then(|v| v.as_object()) {
                                        session_data_bp.update(|map| {
                                            let state = map.entry(id_bp.clone()).or_insert_with(|| SessionState {
                                                id: id_bp.clone(), status: "running".into(), ..Default::default()
                                            });
                                            for (file, lines_val) in bps {
                                                if let Some(lines) = lines_val.as_array() {
                                                    let line_nums: Vec<u32> = lines.iter()
                                                        .filter_map(|v| v.as_u64().map(|l| l as u32))
                                                        .collect();
                                                    if !line_nums.is_empty() {
                                                        let specs: Vec<BreakpointSpec> = line_nums.iter()
                                                            .map(|&line| BreakpointSpec { line, ..Default::default() })
                                                            .collect();
                                                        state.breakpoints.insert(file.clone(), specs);
                                                        if !state.open_files.contains(file) {
                                                            state.open_files.push(file.clone());
                                                        }
                                                    }
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                        });
                    }) as Box<dyn Fn(JsValue)>);
                    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
                    onopen.forget();
                }

                // onclose: mark disconnected, remove sender, schedule reconnect with backoff
                // (but skip reconnect if the session has ended — process exited normally)
                {
                    let ws_connected_close = ws_connected.clone();
                    let ws_senders_close = ws_senders.clone();
                    let session_data_close = session_data.clone();
                    let global_error_close = global_error.clone();
                    let id_close = id.clone();
                    let onclose = Closure::wrap(Box::new(move |_: JsValue| {
                        ws_connected_close.update(|m| { m.insert(id_close.clone(), false); });
                        global_error_close.set(Some(format!("Session '{id_close}' disconnected, retrying...")));
                        let ws_senders_retry = ws_senders_close.clone();
                        let session_data_retry = session_data_close.clone();
                        let id_retry = id_close.clone();
                        leptos::task::spawn_local(async move {
                            gloo_timers::future::sleep(std::time::Duration::from_millis(1000)).await;
                            // Don't reconnect if the session ended normally
                            let is_ended = session_data_retry.get_untracked()
                                .get(&id_retry).map(|s| s.status == "ended").unwrap_or(false);
                            if !is_ended {
                                ws_senders_retry.update(|m| { m.remove(&id_retry); });
                                reconnect_tick.update(|n| *n = n.wrapping_add(1));
                            }
                        });
                    }) as Box<dyn Fn(JsValue)>);
                    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
                    onclose.forget();
                }

                let ws2 = ws.clone();
                let id2 = id.clone();
                let send_closure = Closure::wrap(Box::new(move |msg: JsValue| {
                    if let Some(s) = msg.as_string() { let _ = ws2.send_with_str(&s); }
                }) as Box<dyn Fn(JsValue)>);
                ws_senders.update(|map| {
                    map.insert(id2, send_closure.as_ref().unchecked_ref::<js_sys::Function>().clone());
                });
                send_closure.forget();

                if active_session.get_untracked().is_none() {
                    active_session.set(Some(id.clone()));
                }

                let onmessage = Closure::wrap(Box::new({
                    let sessions = sessions.clone();
                    let active_session = active_session.clone();
                    let session_data = session_data.clone();
                    let global_error_msg = global_error.clone();
                    let cmd_in_flight = cmd_in_flight.clone();
                    let last_event_session = last_event_session.clone();
                    let ws_senders_msg = ws_senders.clone();
                    let layout_msg = layout.clone();
                    let id_msg = id.clone();
                    move |e: MessageEvent| {
                        if let Some(text) = e.data().as_string() {
                            if let Ok(env) = serde_json::from_str::<WsEnvelope>(&text) {
                                // Flash the session entry when any event arrives
                                let les = last_event_session.clone();
                                let sid = id_msg.clone();
                                les.set(Some(sid.clone()));
                                leptos::task::spawn_local(async move {
                                    gloo_timers::future::sleep(std::time::Duration::from_millis(300)).await;
                                    les.update(|v| {
                                        if v.as_deref() == Some(&sid) { *v = None; }
                                    });
                                });
                                handle_envelope(
                                    env,
                                    &sessions,
                                    &active_session,
                                    &session_data,
                                    &cmd_in_flight,
                                    &ws_senders_msg,
                                    &layout_msg.watches,
                                    &layout_msg.console_collapsed,
                                    &global_error_msg,
                                );
                            } else {
                                global_error_msg.set(Some("Failed to parse websocket message.".to_string()));
                            }
                        }
                    }
                }) as Box<dyn FnMut(MessageEvent)>);
                ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
                onmessage.forget();
            }
        }
    });

    let lc = layout.left_collapsed;
    let rc = layout.right_collapsed;
    let nm = layout.narrow_mode;
    let lw = layout.left_width;
    let rw = layout.right_width;
    let preset = layout.layout_preset;
    let left_width_px = move || {
        let base = if nm.get() { lw.get().clamp(140, 180) } else { lw.get() };
        match preset.get().as_str() {
            "slim" => base.clamp(140, 190),
            "full" if !nm.get() => base.max(220).min(360),
            _ => base,
        }
    };
    let right_width_px = move || {
        let base = if nm.get() { rw.get().clamp(170, 220) } else { rw.get() };
        match preset.get().as_str() {
            "slim" => base.clamp(170, 230),
            "full" if !nm.get() => base.max(300).min(480),
            _ => base,
        }
    };

    // ── Dark mode: apply/remove light-mode class on <html> ────────
    {
        let dark_mode = layout.dark_mode;
        Effect::new(move |_| {
            // dark_mode=false → dark (default); dark_mode=true → light
            let light = dark_mode.get();
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                if let Some(el) = doc.document_element() {
                    if light { let _ = el.class_list().add_1("light-mode"); }
                    else     { let _ = el.class_list().remove_1("light-mode"); }
                }
            }
            // Persist to localStorage
            if let Some(storage) = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
            {
                let _ = storage.set_item("debugium_dark_mode", if light { "true" } else { "false" });
            }
        });
    }

    // ── Layout preset: persist + apply coarse layout behavior ───────
    {
        let preset = layout.layout_preset;
        let console_collapsed = layout.console_collapsed;
        let left_width = layout.left_width;
        let right_width = layout.right_width;
        let prev_preset: RwSignal<String> = RwSignal::new(String::new());
        Effect::new(move |_| {
            let next = preset.get();
            if prev_preset.get_untracked() == next {
                return;
            }
            write_window_storage("debugium.layout_preset", &next);
            let vars_collapsed = layout.vars_collapsed;
            let bps_collapsed = layout.bps_collapsed;
            let left_collapsed = layout.left_collapsed;
            let right_collapsed = layout.right_collapsed;
            match next.as_str() {
                "slim" => {
                    // Close everything: max source area
                    console_collapsed.set(true);
                    vars_collapsed.set(true);
                    bps_collapsed.set(true);
                    left_collapsed.set(true);
                    right_collapsed.set(true);
                }
                "full" => {
                    // Show everything
                    console_collapsed.set(false);
                    vars_collapsed.set(false);
                    bps_collapsed.set(false);
                    left_collapsed.set(false);
                    right_collapsed.set(false);
                    left_width.update(|v| *v = (*v).max(220).min(360));
                    right_width.update(|v| *v = (*v).max(300).min(480));
                }
                _ => {
                    // Standard: console collapsed, panels open
                    console_collapsed.set(true);
                    vars_collapsed.set(false);
                    bps_collapsed.set(false);
                    left_collapsed.set(false);
                    right_collapsed.set(false);
                    left_width.update(|v| *v = (*v).clamp(180, 260));
                    right_width.update(|v| *v = (*v).clamp(240, 340));
                }
            }
            prev_preset.set(next);
        });
    }

    // ── Keyboard shortcuts (document-level) ───────────────────────
    {
        let ws_kb = ws_senders.clone();
        let act_kb = active_session.clone();
        let dm_kb = layout.dark_mode;
        let keydown = Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
            // Don't intercept when focus is in an input/textarea
            if let Some(target) = e.target() {
                use wasm_bindgen::JsCast;
                if target.dyn_ref::<web_sys::HtmlInputElement>().is_some()
                    || target.dyn_ref::<web_sys::HtmlTextAreaElement>().is_some()
                { return; }
            }
            let sid = act_kb.get_untracked().unwrap_or_else(|| "default".into());
            match (e.key().as_str(), e.shift_key()) {
                ("F5",  false) => { e.prevent_default(); send_cmd(&ws_kb, &sid, "continue", serde_json::json!({})); }
                ("F10", false) => { e.prevent_default(); send_cmd(&ws_kb, &sid, "next",     serde_json::json!({ "threadId": 1 })); }
                ("F11", false) => { e.prevent_default(); send_cmd(&ws_kb, &sid, "stepIn",   serde_json::json!({ "threadId": 1 })); }
                ("F11", true)  => { e.prevent_default(); send_cmd(&ws_kb, &sid, "stepOut",  serde_json::json!({ "threadId": 1 })); }
                ("d",   false) if e.meta_key() || e.ctrl_key() => {
                    e.prevent_default();
                    dm_kb.update(|v| *v = !*v);
                }
                _ => {}
            }
        });
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
        }
        keydown.forget();
    }

    // Auto-collapse sessions sidebar when only 1 session
    let multi_session = move || sessions.get().len() > 1;

    view! {
        <div
            id="app"
            class:narrow-mode=move || nm.get()
            class:layout-slim=move || layout.layout_preset.get() == "slim"
            class:layout-full=move || layout.layout_preset.get() == "full"
        >
            <Header active_session=active_session session_data=session_data.read_only() />
            <ProcessInfoBar active_session=active_session.read_only() session_metas=session_metas.read_only() />
            <GettingStartedCard
                sessions=sessions.read_only()
                onboarding_dismissed=onboarding_dismissed
                show_launch_modal=show_launch_modal
            />
            <LaunchSessionModal
                show=show_launch_modal
                sessions=sessions
                active_session=active_session
                session_metas=session_metas
            />
            <div class="dashboard-wrapper">

                // Expand rail for collapsed left sidebar
                <Show when=move || lc.get() && multi_session()>
                    <div class="sidebar-rail sidebar-rail-left" title="Expand sessions"
                        on:click=move |_| lc.set(false)
                    >"▶"</div>
                </Show>

                <aside
                    class="sidebar sidebar-left"
                    class:collapsed=move || lc.get() || !multi_session()
                    style=move || format!("width: {}px", left_width_px())
                >
                    <SessionsPanel
                        sessions=sessions
                        session_metas=session_metas
                        active=active_session
                        session_data=session_data.read_only()
                        show_launch_modal=show_launch_modal
                    />
                </aside>

                // ── Left resize handle ──
                <Show when=move || !lc.get() && !nm.get()>
                    <ResizeHandle width=lw min_w=120 max_w=400 invert=false />
                </Show>
                <Show when=move || !lc.get() && nm.get()>
                    <ResizeHandle width=lw min_w=140 max_w=180 invert=false />
                </Show>

                <main class="center-content">
                    <SourcePanel session_data=session_data active_session=active_session.read_only() />
                    <FindingsPanel session_data=session_data active_session=active_session.read_only() />
                    <ConsolePanel session_data=session_data active_session=active_session.read_only() />
                </main>

                // ── Right resize handle ──
                <Show when=move || !rc.get() && !nm.get()>
                    <ResizeHandle width=rw min_w=160 max_w=480 invert=true />
                </Show>
                <Show when=move || !rc.get() && nm.get()>
                    <ResizeHandle width=rw min_w=170 max_w=220 invert=true />
                </Show>

                <aside
                    class="sidebar sidebar-right"
                    class:collapsed=move || rc.get()
                    style=move || format!("width: {}px", right_width_px())
                >
                    <StackPanel session_data=session_data active_session=active_session.read_only() />
                    <VariablesPanel session_data=session_data active_session=active_session.read_only() />
                    <BreakpointsPanel session_data=session_data active_session=active_session.read_only() />
                    <WatchPanel session_data=session_data active_session=active_session.read_only() />
                    <TimelinePanel session_data=session_data active_session=active_session.read_only() />
                </aside>

                // Expand rail for collapsed right sidebar
                <Show when=move || rc.get()>
                    <div class="sidebar-rail sidebar-rail-right" title="Expand panels"
                        on:click=move |_| rc.set(false)
                    >"◀"</div>
                </Show>

            </div>
            <GlobalErrorToast />
            // ── Status bar ──
            <div class="status-bar">
                <span>{move || active_session.get().unwrap_or_else(|| "No session".into())}</span>
                <span>{move || active_session.get()
                    .and_then(|id| session_data.get().get(&id).cloned())
                    .map(|s| s.status)
                    .unwrap_or_default()
                }</span>
                <span>{move || active_session.get()
                    .and_then(|id| session_data.get().get(&id).cloned())
                    .and_then(|s| s.source_path.map(|p| basename(&p)))
                    .unwrap_or_default()
                }</span>
                <span class="status-llm-query">{move || active_session.get()
                    .and_then(|id| session_data.get().get(&id).cloned())
                    .map(|s| s.last_llm_query)
                    .unwrap_or_default()
                }</span>
            </div>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Send command helper
// ─────────────────────────────────────────────

fn send_cmd(
    ws_senders: &RwSignal<std::collections::HashMap<String, js_sys::Function>>,
    session_id: &str,
    command: &str,
    arguments: Value,
) {
    let map = ws_senders.get_untracked();
    if let Some(f) = map.get(session_id) {
        let cmd = WsCommand { session_id: session_id.to_string(), command: command.to_string(), arguments };
        if let Ok(json) = serde_json::to_string(&cmd) {
            let _ = f.call1(&JsValue::NULL, &JsValue::from_str(&json));
        }
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
    cmd_in_flight: &RwSignal<Option<(String, &'static str)>>,
    ws_senders: &RwSignal<std::collections::HashMap<String, js_sys::Function>>,
    watches: &RwSignal<Vec<String>>,
    console_collapsed: &RwSignal<bool>,
    global_error: &RwSignal<Option<String>>,
) {
    // Will be set if we need to send a follow-up DAP command after the update
    // (can't borrow data mutably twice in the same closure)
    let mut post_scopes_request: Option<(String, u32)> = None;  // (session_id, frameId)
    let mut post_vars_from_scope: Option<(String, i64)> = None; // (session_id, variablesReference)
    let mut post_watch_eval: Option<(String, u32, Vec<String>)> = None; // (session_id, frameId, exprs)
    let mut post_session_ended: bool = false;
    let mut post_switch_to_session: Option<String> = None;
    let id = envelope.session_id.clone();

    sessions.update(|s| { if !s.contains(&id) { s.push(id.clone()); } });
    if active_session.get_untracked().is_none() {
        active_session.set(Some(id.clone()));
    }

    // Clear in-flight command when the session responds with stopped/continued
    let msg_event = envelope.msg.get("event").and_then(Value::as_str).unwrap_or("");
    if msg_event == "stopped" || msg_event == "continued" || msg_event == "terminated" {
        if cmd_in_flight.get_untracked().as_ref().map(|(s, _)| s.as_str()) == Some(&id) {
            cmd_in_flight.set(None);
        }
    }

    data.update(|map| {
        let seq = map.get(&id).map(|s| s.event_seq).unwrap_or(0) + 1;
        let state = map.entry(id.clone()).or_insert_with(|| SessionState {
            id: id.clone(), status: "running".into(), ..Default::default()
        });
        state.event_seq = seq;

        let msg = &envelope.msg;
        match msg.get("type").and_then(Value::as_str).unwrap_or("") {
            "event" => match msg.get("event").and_then(Value::as_str).unwrap_or("") {
                "stopped" => {
                    state.status = "paused".into();
                    state.stack_frames.clear();
                    state.variables.clear();
                    state.scopes.clear();
                    state.active_frame_id = None;
                    if let Some(b) = msg.get("body") {
                        if let Some(tid) = b.get("threadId").and_then(Value::as_u64) {
                            state.active_thread_id = tid as u32;
                        }
                        let reason = b.get("reason").and_then(Value::as_str).unwrap_or("breakpoint");
                        push_log(state, "⏸", &format!("Paused ({})", reason), "log-event");
                    }
                    // Watch eval is deferred until stackTrace response (when frame_id is known)
                }
                "continued" => {
                    state.status = "running".into();
                    push_log(state, "▶", "Running", "log-response");
                }
                "terminated" | "exited" => {
                    state.status = "ended".into();
                    push_log(state, "■", "Session ended", "log-error");
                    post_session_ended = true;
                }
                "output" => {
                    if let Some(b) = msg.get("body") {
                        let out = b.get("output").and_then(Value::as_str).unwrap_or("").trim().to_string();
                        let cat = b.get("category").and_then(Value::as_str).unwrap_or("console");
                        if cat != "telemetry" && !out.is_empty() {
                            let (tag, class) = match cat { "stderr" => ("err", "log-error"), _ => ("out", "log-text") };
                            push_log(state, tag, &out, class);
                        }
                    }
                }
                // Synthetic event from MCP / server: a DAP command was dispatched
                "commandSent" => {
                    if let Some(b) = msg.get("body") {
                        let cmd = b.get("command").and_then(Value::as_str).unwrap_or("?");
                        let origin = b.get("origin").and_then(Value::as_str).unwrap_or("ui");
                        let icon = if origin == "mcp" { "🤖" } else { "→" };
                        push_log(state, icon, &format!("[{}] {}", origin.to_uppercase(), cmd), "log-response");
                    }
                }
                "sourceLoaded" => {
                    if let Some(b) = msg.get("body") {
                        let path = b.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                        let lines: Vec<String> = b.get("lines")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                            .unwrap_or_default();
                        let current_line = b.get("currentLine").and_then(Value::as_u64).unwrap_or(0) as u32;
                        state.source_code = Some(lines.join("\n"));
                        state.source_path = Some(path.clone());
                        state.source_line = Some(current_line);
                        // Track as open tab
                        if !path.is_empty() && !state.open_files.contains(&path) {
                            state.open_files.push(path);
                        }
                    }
                }
                "breakpoints_changed" => {
                    if let Some(b) = msg.get("body") {
                        let file = b.get("file").and_then(Value::as_str).unwrap_or("").to_string();
                        let lines: Vec<u32> = b.get("breakpoints").and_then(Value::as_array)
                            .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(|l| l as u32)).collect())
                            .unwrap_or_default();
                        if file.is_empty() { return; }
                        if lines.is_empty() {
                            state.breakpoints.remove(&file);
                        } else {
                            let specs: Vec<BreakpointSpec> = lines.iter()
                                .map(|&line| BreakpointSpec { line, ..Default::default() })
                                .collect();
                            state.breakpoints.insert(file.clone(), specs);
                        }
                        // Open as tab if not already
                        if !state.open_files.contains(&file) {
                            state.open_files.push(file.clone());
                        }
                        push_log(state, "📌", &format!("BP: {}", basename(&file)), "log-response");
                    }
                }
                "exceptionInfo" => {
                    if let Some(b) = msg.get("body") {
                        let exc_id = b.get("exceptionId").and_then(Value::as_str).unwrap_or("Exception");
                        let desc = b.get("description").and_then(Value::as_str).unwrap_or("");
                        push_log(state, "💥", &format!("{}: {}", exc_id, desc), "log-error");
                        if let Some(stack) = b.get("details").and_then(|d| d.get("stackTrace")).and_then(Value::as_str) {
                            push_log(state, "  ", stack, "log-error");
                        }
                    }
                }
                "breakpoint" => {
                    // Adapter confirmed/updated a breakpoint (e.g. after source-map resolution)
                    if let Some(b) = msg.get("body") {
                        if let Some(bp) = b.get("breakpoint") {
                            let verified = bp.get("verified").and_then(Value::as_bool).unwrap_or(false);
                            let line = bp.get("line").and_then(Value::as_u64).map(|l| l as u32);
                            let source = bp.get("source").and_then(|s| s.get("path")).and_then(Value::as_str);
                            if let (Some(file), Some(ln)) = (source, line) {
                                let file = file.to_string();
                                if verified {
                                    let entry = state.breakpoints.entry(file).or_default();
                                    if !entry.iter().any(|s| s.line == ln) {
                                        entry.push(BreakpointSpec { line: ln, ..Default::default() });
                                    }
                                } else {
                                    if let Some(bps) = state.breakpoints.values_mut().next() {
                                        bps.retain(|s| s.line != ln);
                                    }
                                }
                                let all_lines: Vec<u32> = state.breakpoints.values()
                                    .flat_map(|v| v.iter().map(|s| s.line))
                                    .collect();
                                if let Ok(json) = serde_json::to_string(&all_lines) {
                                    editor::set_breakpoints(&json);
                                }
                            }
                        }
                    }
                }
                "invalidated" => {
                    // Adapter requests a data refresh — push a special log entry as a hint.
                    push_log(state, "↺", "Debug data invalidated — refresh pending", "log-response");
                }
                "llmQuery" => {
                    if let Some(b) = msg.get("body") {
                        let tool   = b.get("tool").and_then(Value::as_str).unwrap_or("?");
                        let detail = b.get("detail").and_then(Value::as_str).unwrap_or("");
                        let label = if detail.is_empty() {
                            format!("[LLM] {tool}")
                        } else {
                            format!("[LLM] {tool}({detail})")
                        };
                        push_log(state, "🔍", &label, "log-llm-query");
                        state.last_llm_query = if detail.is_empty() {
                            format!("🔍 {tool}")
                        } else {
                            format!("🔍 {tool}({detail})")
                        };
                    }
                }
                "annotation_added" => {
                    if let Some(b) = msg.get("body") {
                        let entry = AnnotationEntry {
                            id: b.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                            file: b.get("file").and_then(Value::as_str).unwrap_or("").to_string(),
                            line: b.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                            message: b.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                            color: b.get("color").and_then(Value::as_str).unwrap_or("blue").to_string(),
                        };
                        push_log(state, "📎", &format!("{}:{} {}", basename(&entry.file), entry.line, entry.message), "log-response");
                        state.annotations.push(entry);
                    }
                }
                "finding_added" => {
                    if let Some(b) = msg.get("body") {
                        let entry = FindingEntry {
                            id: b.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                            message: b.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                            level: b.get("level").and_then(Value::as_str).unwrap_or("info").to_string(),
                            timestamp: b.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
                        };
                        let icon = match entry.level.as_str() { "error" => "🔴", "warning" => "🟡", _ => "🔵" };
                        push_log(state, icon, &entry.message, "log-response");
                        state.findings.push(entry);
                    }
                }
                "timeline_entry" => {
                    if let Some(b) = msg.get("body") {
                        let entry = TimelineEntryUi {
                            id: b.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                            file: b.get("file").and_then(Value::as_str).unwrap_or("").to_string(),
                            line: b.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                            timestamp: b.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
                            changed_vars: b.get("changed_vars").and_then(Value::as_array)
                                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                                .unwrap_or_default(),
                            stack_summary: b.get("stack_summary").and_then(Value::as_array)
                                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                                .unwrap_or_default(),
                        };
                        state.timeline.push(entry);
                        if state.timeline.len() > 500 { state.timeline.remove(0); }
                    }
                }
                "watches_updated" => {
                    // Server evaluated MCP-managed watches at stop — update results
                    if let Some(b) = msg.get("body") {
                        if let Some(results) = b.get("results").and_then(Value::as_array) {
                            for r in results {
                                let expr = r.get("expression").and_then(Value::as_str).unwrap_or("").to_string();
                                let val = r.get("value").and_then(Value::as_str).unwrap_or("").to_string();
                                if let Some(existing) = state.watch_results.iter_mut().find(|(e, _)| e == &expr) {
                                    existing.1 = val;
                                } else {
                                    state.watch_results.push((expr, val));
                                }
                            }
                        }
                    }
                }
                "watches_list_changed" => {
                    // MCP added/removed a watch — sync the expression list in watch_results
                    if let Some(b) = msg.get("body") {
                        if let Some(watches) = b.get("watches").and_then(Value::as_array) {
                            let exprs: Vec<String> = watches.iter()
                                .filter_map(Value::as_str).map(str::to_string).collect();
                            // Remove results for deleted expressions
                            state.watch_results.retain(|(e, _)| exprs.contains(e));
                            // Add placeholders for new ones
                            for expr in &exprs {
                                if !state.watch_results.iter().any(|(e, _)| e == expr) {
                                    state.watch_results.push((expr.clone(), "…".to_string()));
                                }
                            }
                        }
                    }
                }
                "session_launched" => {
                    post_switch_to_session = Some(id.clone());
                }
                _ => {}
            },
            "response" => {
                let ok = msg.get("success").and_then(Value::as_bool).unwrap_or(false);
                if !ok {
                    let cmd = msg.get("command").and_then(Value::as_str).unwrap_or("unknown");
                    let err = msg.get("message").and_then(Value::as_str).unwrap_or("command failed");
                    global_error.set(Some(format!("{cmd}: {err}")));
                    push_log(state, "✗", &format!("{cmd} failed: {err}"), "log-error");
                    return;
                }
                let cmd = msg.get("command").and_then(Value::as_str).unwrap_or("");

                if cmd == "threads" {
                    if let Some(t) = msg.get("body").and_then(|b| b.get("threads")) {
                        state.threads = serde_json::from_value(t.clone()).unwrap_or_default();
                    }
                }
                if cmd == "stackTrace" {
                    let raw: Vec<Value> = msg.get("body")
                        .and_then(|b| b.get("stackFrames"))
                        .and_then(|f| serde_json::from_value(f.clone()).ok())
                        .unwrap_or_default();
                    state.stack_frames = raw.iter().map(|f| StackFrame {
                        id: f.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                        name: f.get("name").and_then(Value::as_str).unwrap_or("?").to_string(),
                        line: f.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                        file: f.get("source").and_then(|s| s.get("path"))
                              .and_then(Value::as_str).unwrap_or("").to_string(),
                    }).collect();
                    if state.source_path.is_none() {
                        if let Some(top) = state.stack_frames.first() {
                            state.source_path = Some(top.file.clone());
                            state.source_line = Some(top.line);
                        }
                    }
                    // Auto-request scopes for top frame and update active_frame_id
                    if let Some(top) = state.stack_frames.first() {
                        state.active_frame_id = Some(top.id);
                        post_scopes_request = Some((id.clone(), top.id));
                        // Evaluate watches now that we have the correct frame_id
                        let watch_exprs = watches.get_untracked();
                        if !watch_exprs.is_empty() {
                            post_watch_eval = Some((id.clone(), top.id, watch_exprs));
                        }
                    }
                }
                if cmd == "scopes" {
                    if let Some(arr) = msg.get("body").and_then(|b| b.get("scopes")).and_then(Value::as_array) {
                        state.scopes = arr.clone();
                        // Auto-expand the locals scope (skip "special variables" — it's usually empty)
                        // Prefer a scope whose name contains "local" or "function", fall back to first
                        let locals_scope = arr.iter().find(|s| {
                            let name = s.get("name").and_then(Value::as_str).unwrap_or("").to_lowercase();
                            name.contains("local") || name.contains("function")
                        }).or_else(|| arr.first());
                        if let Some(scope) = locals_scope {
                            if let Some(vref) = scope.get("variablesReference").and_then(Value::as_i64) {
                                state.pending_scope_var_ref = Some(vref);
                                post_vars_from_scope = Some((id.clone(), vref));
                            }
                        }
                    }
                }
                if cmd == "variables" {
                    if let Some(arr) = msg.get("body").and_then(|b| b.get("variables")).and_then(Value::as_array) {
                        let new_vars: Vec<Variable> = arr.iter().filter_map(|v| {
                            Some(Variable {
                                name: v.get("name").and_then(Value::as_str)?.to_string(),
                                value: v.get("value").and_then(Value::as_str)?.to_string(),
                                kind: v.get("type").and_then(Value::as_str).map(str::to_string),
                                variables_reference: v.get("variablesReference").and_then(Value::as_u64).unwrap_or(0),
                            })
                        }).collect();

                        if let Some(ref_id) = state.pending_var_ref.take() {
                            // This is a manual expansion response — store as children
                            state.last_inspected_var_ref = Some(ref_id);
                            state.expanded_vars.insert(ref_id, new_vars);
                        } else if let Some(scope_ref) = state.pending_scope_var_ref.take() {
                            // This is a scope auto-expand response — update flat variable list
                            // and store as expanded children of the scope group
                            // Compute diff against previous values
                            state.changed_vars.clear();
                            for v in &new_vars {
                                if let Some(prev) = state.prev_variables.get(&v.name) {
                                    if prev != &v.value { state.changed_vars.insert(v.name.clone()); }
                                }
                            }
                            // Save current values for next diff
                            state.prev_variables = new_vars.iter().map(|v| (v.name.clone(), v.value.clone())).collect();
                            state.variables = new_vars.clone();
                            state.expanded_vars.insert(scope_ref as u64, new_vars);
                        } else {
                            // Initial load — update flat variable list
                            for var in new_vars {
                                if let Some(existing) = state.variables.iter_mut().find(|v| v.name == var.name) {
                                    *existing = var;
                                } else {
                                    state.variables.push(var);
                                }
                            }
                        }
                    }
                }
                if cmd == "setBreakpoints" {
                    // Use pending_bp_file (set by UI-initiated commands), or fall back to
                    // source.path injected by server (for CLI/MCP-initiated commands).
                    let file = state.pending_bp_file.take()
                        .or_else(|| msg.get("body")
                            .and_then(|b| b.get("source"))
                            .and_then(|s| s.get("path"))
                            .and_then(Value::as_str)
                            .map(String::from));
                    if let Some(file) = file {
                        if let Some(arr) = msg.get("body").and_then(|b| b.get("breakpoints")).and_then(Value::as_array) {
                            let verified_lines: Vec<u32> = arr.iter()
                                .filter(|b| b.get("verified").and_then(Value::as_bool).unwrap_or(false))
                                .filter_map(|b| b.get("line").and_then(Value::as_u64).map(|l| l as u32))
                                .collect();
                            // Preserve existing specs but update to verified lines
                            let specs: Vec<BreakpointSpec> = verified_lines.iter()
                                .map(|&line| {
                                    // Re-use existing spec for this line if present
                                    state.breakpoints.get(&file)
                                        .and_then(|bps| bps.iter().find(|s| s.line == line))
                                        .cloned()
                                        .unwrap_or(BreakpointSpec { line, ..Default::default() })
                                })
                                .collect();
                            state.breakpoints.insert(file.clone(), specs);
                            if let Some(first_verified) = verified_lines.first().copied() {
                                state.recent_verified_breakpoint = Some((file, first_verified));
                            }
                            // Push verified lines back into the editor gutter
                            if let Ok(json) = serde_json::to_string(&verified_lines) {
                                editor::set_breakpoints(&json);
                            }
                        }
                    }
                }
                if cmd == "evaluate" {
                    if let Some(b) = msg.get("body") {
                        let result = b.get("result").and_then(Value::as_str).unwrap_or("?");
                        // expression is now injected into the body by the server
                        let expr = b.get("expression").and_then(Value::as_str).unwrap_or("");
                        if !expr.is_empty() {
                            let expr_s = expr.to_string();
                            let result_s = result.to_string();
                            let existing = state.watch_results.iter().position(|(e, _)| e == &expr_s);
                            match existing {
                                Some(i) => state.watch_results[i].1 = result_s,
                                None => state.watch_results.push((expr_s, result_s)),
                            }
                        }
                        push_log(state, "=", &format!("{}{}", if expr.is_empty() { String::new() } else { format!("{} = ", expr) }, result), "log-response");
                    }
                }
                if cmd == "completions" {
                    if let Some(items) = msg.get("body").and_then(|b| b.get("targets")).and_then(Value::as_array) {
                        state.completions = items.iter()
                            .filter_map(|v| v.get("label").and_then(Value::as_str).map(str::to_string))
                            .collect();
                    }
                }
                if cmd == "setVariable" {
                    if let Some(b) = msg.get("body") {
                        let new_val = b.get("value").and_then(Value::as_str).unwrap_or("?");
                        push_log(state, "✏", &format!("= {}", new_val), "log-response");
                        // Update the variable value in the flat list
                        // (We don't have the name here, so just log it; full sync on next stop)
                    }
                }
            }
            _ => {}
        }
    });

    // Auto-expand console when the LLM reads state so the human sees the activity
    if envelope.msg.get("event").and_then(Value::as_str) == Some("llmQuery") {
        console_collapsed.set(false);
    }

    // Clear in-flight spinner for any successful response (events already handled above)
    if envelope.msg.get("type").and_then(Value::as_str) == Some("response")
        && envelope.msg.get("success").and_then(Value::as_bool).unwrap_or(false)
    {
        if cmd_in_flight.get_untracked().as_ref().map(|(s, _)| s.as_str()) == Some(id.as_str()) {
            cmd_in_flight.set(None);
        }
    }

    // Follow-up: request scopes for the top frame after stackTrace response
    if let Some((sid, frame_id)) = post_scopes_request {
        send_cmd(ws_senders, &sid, "scopes", serde_json::json!({ "frameId": frame_id }));
    }
    // Follow-up: request variables for the first scope after scopes response
    if let Some((sid, vref)) = post_vars_from_scope {
        send_cmd(ws_senders, &sid, "variables", serde_json::json!({ "variablesReference": vref }));
    }
    // Follow-up: evaluate each watch expression after stopped event
    if let Some((sid, frame_id, exprs)) = post_watch_eval {
        for expr in exprs {
            send_cmd(ws_senders, &sid, "evaluate", serde_json::json!({
                "expression": expr,
                "frameId": frame_id,
                "context": "watch"
            }));
        }
    }
    // Auto-switch to newly launched session (must happen outside data.update)
    if let Some(new_sid) = post_switch_to_session {
        active_session.set(Some(new_sid));
    }
    // Session ended: keep it in the sidebar so the user can see the final state.
    // The status is already "ended" which drives the gray dot and dimmed styling.
    let _ = post_session_ended;
}

fn push_log(state: &mut SessionState, tag: &str, msg: &str, class: &str) {
    let seq = state.event_seq;
    state.console_logs.push(ConsoleLog {
        tag: tag.into(), message: msg.into(), class: class.into(), seq,
    });
    // Keep last 200 entries
    if state.console_logs.len() > 200 {
        state.console_logs.remove(0);
    }
}

// ─────────────────────────────────────────────
//  Header
// ─────────────────────────────────────────────

#[component]
fn Header(
    active_session: RwSignal<Option<String>>,
    session_data: ReadSignal<std::collections::HashMap<String, SessionState>>,
) -> impl IntoView {
    let ws = use_context::<WsSenders>().expect("no WsSenders");
    let ws_senders = ws.0;
    let cmd_ctx = use_context::<CommandInFlight>().expect("no CommandInFlight");
    let cmd_signal = cmd_ctx.0;
    let last_completed_ctx = use_context::<LastCompleted>().expect("no LastCompleted");
    let last_completed = last_completed_ctx.0;
    let layout = use_context::<LayoutState>().expect("no LayoutState");

    // Watch cmd_in_flight transitions: Some → None means command completed
    Effect::new({
        let cmd_signal = cmd_signal.clone();
        let last_completed = last_completed.clone();
        let prev_label: RwSignal<Option<&'static str>> = RwSignal::new(None);
        move |_| {
            let current = cmd_signal.get();
            let prev = prev_label.get_untracked();
            match (&prev, &current) {
                (Some(lbl), None) => {
                    // Transition from in-flight to done
                    let lbl = *lbl;
                    last_completed.set(Some(lbl));
                    let lc = last_completed.clone();
                    leptos::task::spawn_local(async move {
                        gloo_timers::future::sleep(std::time::Duration::from_millis(400)).await;
                        lc.update(|v| { if *v == Some(lbl) { *v = None; } });
                    });
                }
                _ => {}
            }
            prev_label.set(current.map(|(_, l)| l));
        }
    });

    let do_cmd = move |dap_cmd: &'static str, label: &'static str| {
        let id = active_session.get_untracked().unwrap_or_else(|| "default".into());
        let thread_id = session_data.get_untracked()
            .get(&id)
            .map(|s| s.active_thread_id)
            .unwrap_or(1);
        cmd_signal.set(Some((id.clone(), label)));
        let args = serde_json::json!({ "threadId": thread_id });
        send_cmd(&ws_senders, &id, dap_cmd, args);
    };

    // Is this session currently paused? (controls button availability)
    let is_paused = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.status == "paused")
            .unwrap_or(false)
    };

    // Is this session currently running? (for Pause button)
    let is_running = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.status == "running")
            .unwrap_or(false)
    };

    let in_flight_label = move || cmd_signal.get().map(|(_, l)| l);

    let btn_class = move |label: &'static str| {
        let inflight = cmd_signal.get()
            .map(|(_, l)| l == label)
            .unwrap_or(false);
        let just_done = last_completed.get() == Some(label);
        match (inflight, just_done) {
            (true, _) => "debug-btn btn-inflight",
            (_, true) => "debug-btn just-completed",
            _ => "debug-btn",
        }
    };

    view! {
        <header>
            <div class="header-left">
                {
                    let wsc_badge = use_context::<WsConnected>().map(|c| c.0).unwrap();
                    let les = use_context::<LastEventSession>().map(|c| c.0).unwrap();
                    let wsc_pulse = wsc_badge.clone();
                    view! {
                        <h1>"Debugium "
                            <span
                                class="badge"
                                class:badge-offline=move || wsc_badge.get().values().all(|&v| !v)
                            >
                                {move || if wsc_badge.get().values().any(|&v| v) { "Live" } else { "Off" }}
                            </span>
                        </h1>
                        // Server heartbeat pulse
                        <span
                            class="server-pulse"
                            class:server-pulse-active=move || les.get().is_some()
                            class:server-offline=move || wsc_pulse.get().values().all(|&v| !v)
                            title="Server connection"
                        ></span>
                    }
                }
                // Session status chip
                {
                    let chip = move || {
                        active_session.get()
                            .and_then(|id| session_data.get().get(&id).cloned())
                            .map(|s| match s.status.as_str() {
                                "paused"  => ("status-chip chip-paused",  "⏸ Paused"),
                                "ended"   => ("status-chip chip-ended",   "■ Ended"),
                                _         => ("status-chip chip-running", "▶ Running"),
                            })
                    };
                    view! {
                        <Show when=move || chip().is_some()>
                            <span class=move || chip().unwrap().0>{move || chip().unwrap().1}</span>
                        </Show>
                    }
                }
                // In-flight command toast
                <Show when=move || in_flight_label().is_some()>
                    <div class="cmd-toast">
                        <span class="cmd-spinner"></span>
                        {move || in_flight_label().unwrap_or("")}
                    </div>
                </Show>
                <Show when=move || active_session.get().is_none()>
                    <div class="onboarding-inline-hint">
                        <span class="onboarding-hint-title">"No active session."</span>
                        <span>" Run "</span>
                        <code>"debugium launch app.py --adapter python --breakpoint /abs/path/app.py:42"</code>
                    </div>
                </Show>
            </div>
            <div class="header-center">
                <div class="header-controls">
                    <button
                        class=move || format!("btn-continue icon-only {}", btn_class("Continue"))
                        title="Continue (F5)"
                        disabled=move || !is_paused() || cmd_signal.get().is_some()
                        on:click=move |_| do_cmd("continue", "Continue")
                    >
                        <span class="btn-icon">"▶"</span>
                    </button>
                    <button
                        class=move || format!("btn-step icon-only {}", btn_class("Pause"))
                        title="Pause"
                        disabled=move || !is_running() || cmd_signal.get().is_some()
                        on:click=move |_| do_cmd("pause", "Pause")
                    >
                        <span class="btn-icon">"⏸"</span>
                    </button>
                    <button
                        class=move || format!("btn-step icon-only {}", btn_class("Step In"))
                        title="Step Into (F11)"
                        disabled=move || !is_paused() || cmd_signal.get().is_some()
                        on:click=move |_| do_cmd("stepIn", "Step In")
                    >
                        <span class="btn-icon">"↓"</span>
                    </button>
                    <button
                        class=move || format!("btn-over icon-only {}", btn_class("Step Over"))
                        title="Step Over (F10)"
                        disabled=move || !is_paused() || cmd_signal.get().is_some()
                        on:click=move |_| do_cmd("next", "Step Over")
                    >
                        <span class="btn-icon">"↷"</span>
                    </button>
                    <button
                        class=move || format!("icon-only {}", btn_class("Step Out"))
                        title="Step Out (Shift+F11)"
                        disabled=move || !is_paused() || cmd_signal.get().is_some()
                        on:click=move |_| do_cmd("stepOut", "Step Out")
                    >
                        <span class="btn-icon">"↑"</span>
                    </button>
                    // Separator
                    <span style="width: 1px; height: 16px; background: var(--border); margin: 0 4px; display: inline-block; opacity: .5"></span>
                    // Restart session
                    <button
                        class="debug-btn btn-restart icon-only"
                        title="Restart session"
                        disabled=move || active_session.get().is_none()
                        on:click={
                            let ws_s2 = ws_senders.clone();
                            let act2 = active_session.clone();
                            move |_| {
                                let id = act2.get_untracked().unwrap_or_else(|| "default".into());
                                send_cmd(&ws_s2, &id, "restart", serde_json::json!({}));
                            }
                        }
                    >
                        <span class="btn-icon">"↺"</span>
                    </button>
                    // Stop session
                    <button
                        class="debug-btn btn-stop icon-only"
                        title="Stop session (terminate)"
                        disabled=move || active_session.get().is_none()
                        on:click={
                            let ws_s = ws_senders.clone();
                            let act = active_session.clone();
                            move |_| {
                                let id = act.get_untracked().unwrap_or_else(|| "default".into());
                                send_cmd(&ws_s, &id, "terminate", serde_json::json!({}));
                            }
                        }
                    >
                        <span class="btn-icon">"■"</span>
                    </button>
                </div>
            </div>
            
            <div class="header-right">
                // Layout preset
                <div class="layout-preset-group" role="group" aria-label="Layout preset">
                    <button
                        class=move || if layout.layout_preset.get() == "slim" { "layout-preset-btn is-active" } else { "layout-preset-btn" }
                        on:click=move |_| layout.layout_preset.set("slim".to_string())
                        title="Slim: close all panels, maximise source"
                    >"Slim"</button>
                    <button
                        class=move || if layout.layout_preset.get() == "standard" { "layout-preset-btn is-active" } else { "layout-preset-btn" }
                        on:click=move |_| layout.layout_preset.set("standard".to_string())
                        title="Standard: console collapsed, panels open"
                    >"Std"</button>
                    <button
                        class=move || if layout.layout_preset.get() == "full" { "layout-preset-btn is-active" } else { "layout-preset-btn" }
                        on:click=move |_| layout.layout_preset.set("full".to_string())
                        title="Full: show everything"
                    >"Full"</button>
                </div>
                // Dark mode toggle
                <button
                    class="debug-btn btn-theme"
                    title="Toggle dark mode (Ctrl/⌘+D)"
                    on:click=move |_| layout.dark_mode.update(|v| *v = !*v)
                >{move || if layout.dark_mode.get() { "Dark" } else { "Light" }}</button>
            </div>
        </header>
    }
}

// ─────────────────────────────────────────────
//  Sessions sidebar
// ─────────────────────────────────────────────

#[component]
fn SessionsPanel(
    sessions: RwSignal<Vec<String>>,
    session_metas: RwSignal<std::collections::HashMap<String, Value>>,
    active: RwSignal<Option<String>>,
    session_data: ReadSignal<std::collections::HashMap<String, SessionState>>,
    show_launch_modal: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <aside class="sidebar sidebar-left">
            <div class="panel" style="flex:1;overflow:hidden">
                <div class="panel-header">
                    <h2>"Sessions"</h2>
                    {
                        let layout_sp = use_context::<LayoutState>().expect("no LayoutState");
                        view! {
                            <div class="panel-header-actions">
                                <button
                                    class="launch-inline-btn"
                                    title="Launch a new debug session"
                                    on:click=move |_| show_launch_modal.set(true)
                                >"+ Launch"</button>
                                <button
                                    class="collapse-btn"
                                    title="Collapse sessions sidebar"
                                    on:click=move |_| layout_sp.left_collapsed.update(|v| *v = !*v)
                                >{move || if layout_sp.left_collapsed.get() { "▶" } else { "◀" }}</button>
                            </div>
                        }
                    }
                </div>
                <div class="panel-content scrollable">
                    <ul class="list-view">
                        <For
                            each=move || sessions.get()
                            key=|id| id.clone()
                            children={
                                let active = active.clone();
                                let session_metas = session_metas.clone();
                                move |id: String| {
                                    let id_click = id.clone();
                                    let id_check = id.clone();
                                    let id_ended = id.clone();
                                    let id_meta = id.clone();
                                    let metas = session_metas.clone();
                                    let metas2 = session_metas.clone();
                                    let id_meta2 = id.clone();
                                    let is_ended = Signal::derive(move || {
                                        session_data.get().get(&id_ended).map(|s| s.status == "ended").unwrap_or(false)
                                    });
                                    let adapter_label = Signal::derive(move || {
                                        metas2.get()
                                            .get(&id_meta2)
                                            .and_then(|m| m.get("adapter"))
                                            .and_then(|v| v.as_str())
                                            .map(str::to_string)
                                    });
                                    let program_label = Signal::derive(move || {
                                        metas.get()
                                            .get(&id_meta)
                                            .and_then(|m| m.get("program"))
                                            .and_then(|v| v.as_str())
                                            .filter(|s| !s.is_empty())
                                            .map(str::to_string)
                                    });
                                    view! {
                                        <li
                                            class:active-item=move || active.get().as_deref() == Some(&id_check)
                                            class:session-ended=move || is_ended.get()
                                            class:session-flash={
                                                let id_flash = id.clone();
                                                move || {
                                                    let les = use_context::<LastEventSession>()
                                                        .map(|c| c.0).unwrap();
                                                    les.get().as_deref() == Some(&id_flash)
                                                }
                                            }
                                            on:click=move |_| active.set(Some(id_click.clone()))
                                        >
                                            <span class="session-item">
                                                {id.clone()}
                                                <SessionDot session_id=id.clone() session_ended=is_ended />
                                            </span>
                                            <Show when=move || program_label.get().is_some()>
                                                <div class="session-details">
                                                    <small class="session-program">{move || program_label.get().unwrap_or_default()}</small>
                                                    <Show when=move || adapter_label.get().is_some()>
                                                        <span class="session-adapter-pill">{move || adapter_label.get().unwrap_or_default()}</span>
                                                    </Show>
                                                </div>
                                            </Show>
                                        </li>
                                    }
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

/// Small dot next to session name that reflects live status and WS connectivity.
#[component]
fn SessionDot(session_id: String, session_ended: Signal<bool>) -> impl IntoView {
    let cmd_ctx = use_context::<CommandInFlight>().expect("no CommandInFlight");
    let cmd = cmd_ctx.0;
    let ws_conn_ctx = use_context::<WsConnected>().expect("no WsConnected");
    let ws_connected = ws_conn_ctx.0;
    let id = session_id.clone();

    let dot_class = move || {
        if session_ended.get() {
            return "session-dot dot-ended";
        }
        let is_connected = ws_connected.get().get(&id).copied().unwrap_or(false);
        if !is_connected {
            "session-dot dot-disconnected"
        } else {
            let inflight = cmd.get().as_ref().map(|(s, _)| s.as_str() == id.as_str()).unwrap_or(false);
            if inflight { "session-dot dot-live" } else { "session-dot dot-connected" }
        }
    };

    let dot_title = move || {
        if session_ended.get() { return "Session ended"; }
        let is_connected = ws_connected.get().get(&session_id).copied().unwrap_or(false);
        if is_connected { "Connected" } else { "Disconnected / Reconnecting…" }
    };

    view! {
        <span class=dot_class title=dot_title></span>
    }
}

// ─────────────────────────────────────────────
//  Source panel
// ─────────────────────────────────────────────

#[component]
fn SourcePanel(
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let container_ref: NodeRef<leptos::html::Div> = NodeRef::new();
    let editor_initialized = RwSignal::new(false);
    let data = session_data;
    let layout = use_context::<LayoutState>().expect("no LayoutState");
    let active_tab = layout.active_tab;

    // Track previous source_line to detect changes for path-updated flash
    let prev_line: RwSignal<Option<u32>> = RwSignal::new(None);
    let line_changed = RwSignal::new(false);
    // Track the last source_path that was auto-followed by execution navigation
    let last_exec_source: RwSignal<Option<String>> = RwSignal::new(None);

    let file_label = move || {
        let tab = active_tab.get();
        let path = tab.or_else(|| active_session.get()
            .and_then(|id| data.get().get(&id).cloned())
            .and_then(|s| s.source_path));
        path.as_deref().map(basename).unwrap_or_else(|| "No file".to_string())
    };

    let ws_ctx = use_context::<WsSenders>().expect("no WsSenders");
    let ws_senders_bp = ws_ctx.0;

    Effect::new({
        let container_ref = container_ref.clone();
        let data_bp = session_data.clone();
        let active_bp = active_session.clone();
        move |_| {
            if let Some(el) = container_ref.get() {
                let html_el: &web_sys::HtmlElement = el.as_ref();
                editor::init_editor(html_el, "// Waiting for debugger...", "");
                editor_initialized.set(true);

                // Register breakpoint-change callback so gutter clicks reach the server
                let data_cb = data_bp.clone();
                let ws_cb = ws_senders_bp.clone();
                let active_cb = active_bp.clone();
                let cb = Closure::wrap(Box::new(move |file: String, lines_json: String| {
                    let Some(session_id) = active_cb.get_untracked() else { return; };
                    let lines: Vec<u32> = serde_json::from_str::<Vec<u32>>(&lines_json)
                        .unwrap_or_default();
                    // Record pending file so the response handler can update breakpoints
                    data_cb.update(|map| {
                        if let Some(s) = map.get_mut(&session_id) {
                            s.pending_bp_file = Some(file.clone());
                        }
                    });
                    // Build breakpoint specs, preserving existing conditions for unchanged lines
                    let existing = data_cb.get_untracked()
                        .get(&session_id)
                        .and_then(|s| s.breakpoints.get(&file).cloned())
                        .unwrap_or_default();
                    let bp_specs: Vec<Value> = lines.iter().map(|&l| {
                        let spec = existing.iter().find(|s| s.line == l);
                        let mut obj = serde_json::json!({ "line": l });
                        if let Some(cond) = spec.and_then(|s| s.condition.as_ref()) {
                            obj["condition"] = Value::String(cond.clone());
                        }
                        if let Some(log) = spec.and_then(|s| s.log_message.as_ref()) {
                            obj["logMessage"] = Value::String(log.clone());
                        }
                        obj
                    }).collect();
                    let bp_args = serde_json::json!({
                        "source": { "path": file },
                        "breakpoints": bp_specs
                    });
                    send_cmd(&ws_cb, &session_id, "setBreakpoints", bp_args);
                }) as Box<dyn Fn(String, String)>);

                let window = web_sys::window().unwrap();
                let _ = Reflect::set(
                    &window,
                    &JsValue::from_str("__cm_on_bp_change"),
                    cb.as_ref().unchecked_ref(),
                );
                cb.forget();
            }
        }
    });

    Effect::new(move |_| {
        if !editor_initialized.get() { return; }
        let Some(id) = active_session.get() else { return; };
        let Some(state) = session_data.get().get(&id).cloned() else { return; };

        // Determine which file to display: active_tab override or current execution file
        let display_path = active_tab.get()
            .or_else(|| state.source_path.clone());

        if let Some(path) = display_path {
            // If this is the execution file and we have cached code, use it
            let cached_code = if Some(&path) == state.source_path.as_ref() {
                state.source_code.clone()
            } else {
                None
            };

            if let Some(code) = cached_code {
                editor::set_content(&code, &path);
                // Apply breakpoints for this file
                let bps_for_file: Vec<u32> = state.breakpoints.get(&path)
                    .map(|specs| specs.iter().map(|s| s.line).collect())
                    .unwrap_or_default();
                if let Ok(json) = serde_json::to_string(&bps_for_file) {
                    editor::set_breakpoints(&json);
                }
                // Apply annotations for this file
                let anns: Vec<serde_json::Value> = state.annotations.iter()
                    .filter(|a| a.file == path)
                    .map(|a| serde_json::json!({ "line": a.line, "message": a.message, "color": a.color }))
                    .collect();
                if let Ok(json) = serde_json::to_string(&anns) {
                    editor::set_annotations(&json, &path);
                }
            } else {
                // Fetch source from server
                let id_fetch = id.clone();
                let path_fetch = path.clone();
                let data_fetch = data.clone();
                let active_tab_fetch = active_tab;
                leptos::task::spawn_local(async move {
                    if let Ok(resp) = gloo_net::http::Request::get(&format!("/source?path={}", path_fetch)).send().await {
                        if let Ok(json_resp) = resp.json::<serde_json::Value>().await {
                            if let Some(lines) = json_resp.get("lines").and_then(|l| l.as_array()) {
                                let code: String = lines.iter().filter_map(|v| v.as_str())
                                    .map(str::to_string).collect::<Vec<_>>().join("\n");
                                // Cache code only if it's the session's source_path
                                data_fetch.update(|map| {
                                    if let Some(s) = map.get_mut(&id_fetch) {
                                        if s.source_path.as_deref() == Some(&path_fetch) {
                                            s.source_code = Some(code.clone());
                                        }
                                    }
                                });
                                editor::set_content(&code, &path_fetch);
                                // Apply breakpoints for this file
                                let snap2 = data_fetch.get_untracked();
                                let session_snap = snap2.get(&id_fetch);
                                let bps: Vec<u32> = session_snap
                                    .and_then(|s| s.breakpoints.get(&path_fetch).cloned())
                                    .map(|specs| specs.iter().map(|s| s.line).collect())
                                    .unwrap_or_default();
                                if let Ok(json) = serde_json::to_string(&bps) {
                                    editor::set_breakpoints(&json);
                                }
                                // Apply annotations for this file
                                let anns: Vec<serde_json::Value> = session_snap
                                    .map(|s| s.annotations.iter()
                                        .filter(|a| a.file == path_fetch)
                                        .map(|a| serde_json::json!({ "line": a.line, "message": a.message, "color": a.color }))
                                        .collect())
                                    .unwrap_or_default();
                                if let Ok(json) = serde_json::to_string(&anns) {
                                    editor::set_annotations(&json, &path_fetch);
                                }
                                // Only set active_tab on initial load (when it's None).
                                // Never overwrite a tab the user navigated to after we started the fetch.
                                if active_tab_fetch.get_untracked().is_none() {
                                    active_tab_fetch.set(Some(path_fetch));
                                }
                            }
                        }
                    }
                });
            }
        }

        // Show execution arrow only if viewing the execution file
        let is_exec_file = active_tab.get().as_ref() == state.source_path.as_ref()
            || active_tab.get().is_none();
        if is_exec_file {
            if let Some(line) = state.source_line {
                let prev = prev_line.get_untracked();
                if prev != Some(line) {
                    prev_line.set(Some(line));
                    line_changed.set(true);
                    leptos::task::spawn_local(async move {
                        gloo_timers::future::sleep(std::time::Duration::from_millis(700)).await;
                        line_changed.set(false);
                    });
                }
                editor::set_exec_line(line);
            }
        } else {
            editor::set_exec_line(0);
        }
    });

    // Also sync active_tab when source_path changes (execution navigation)
    // Only auto-follow when execution moves to a NEW file (don't override manual tab clicks)
    Effect::new(move |_| {
        let Some(id) = active_session.get() else { return; };
        let snap = session_data.get();
        let Some(state) = snap.get(&id) else { return; };
        if let Some(path) = &state.source_path {
            let prev = last_exec_source.get_untracked();
            // Auto-follow only when execution jumps to a different file than before
            if prev.as_deref() != Some(path.as_str()) {
                last_exec_source.set(Some(path.clone()));
                active_tab.set(Some(path.clone()));
            }
        }
    });

    let open_files = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.open_files)
            .unwrap_or_default()
    };

    let show_source_onboarding = move || {
        match active_session.get() {
            None => true,
            Some(id) => session_data
                .get()
                .get(&id)
                .and_then(|s| s.source_path.as_ref())
                .is_none(),
        }
    };
    let source_read_highlight = move || {
        active_session.get()
            .and_then(|id| data.get().get(&id).cloned())
            .map(|s| s.last_llm_query.contains("get_source"))
            .unwrap_or(false)
    };

    view! {
        <div class="panel source-panel" class:mcp-source-reading=source_read_highlight>
            <div class="panel-header">
                <h2>"Source"</h2>
                <span
                    class="file-path"
                    class:path-updated=move || line_changed.get()
                >
                    {file_label}
                </span>
            </div>
            // Tab bar
            <div class="tab-bar">
                <For
                    each=open_files
                    key=|f| f.clone()
                    children={
                        let active_tab_tb = active_tab;
                        let session_data_tb = session_data;
                        let active_session_tb = active_session;
                        move |file: String| {
                            let file_close = file.clone();
                            let file_tab = file.clone();
                            let file_name = basename(&file);
                            let is_active = {
                                let f = file.clone();
                                move || active_tab_tb.get().as_deref() == Some(&f)
                            };
                            let has_bp = {
                                let f = file.clone();
                                move || {
                                    active_session_tb.get()
                                        .and_then(|id| session_data_tb.get().get(&id).cloned())
                                        .map(|s| s.breakpoints.contains_key(&f))
                                        .unwrap_or(false)
                                }
                            };
                            // Show execution arrow when debugger is paused in this file
                            let exec_file = file.clone();
                            let is_exec_class = {
                                let f = exec_file.clone();
                                move || {
                                    active_session_tb.get()
                                        .and_then(|id| session_data_tb.get().get(&id).cloned())
                                        .map(|s| s.source_path.as_deref() == Some(&f))
                                        .unwrap_or(false)
                                }
                            };
                            let is_exec_show = {
                                let f = exec_file.clone();
                                move || {
                                    active_session_tb.get()
                                        .and_then(|id| session_data_tb.get().get(&id).cloned())
                                        .map(|s| s.source_path.as_deref() == Some(&f))
                                        .unwrap_or(false)
                                }
                            };
                            let on_click = {
                                let f = file_tab.clone();
                                move |_| { active_tab_tb.set(Some(f.clone())); }
                            };
                            let on_close = {
                                let f = file_close.clone();
                                let at = active_tab_tb;
                                let sd = session_data_tb;
                                let asi = active_session_tb;
                                move |e: web_sys::MouseEvent| {
                                    e.stop_propagation();
                                    if let Some(id) = asi.get_untracked() {
                                        sd.update(|map| {
                                            if let Some(s) = map.get_mut(&id) {
                                                s.open_files.retain(|x| x != &f);
                                                // If closed tab was active, switch to last remaining
                                                if at.get_untracked().as_deref() == Some(&f) {
                                                    let next = s.open_files.last().cloned();
                                                    at.set(next);
                                                }
                                            }
                                        });
                                    }
                                }
                            };
                            view! {
                                <div
                                    class="tab-chip"
                                    class:tab-active=is_active
                                    class:tab-exec=is_exec_class
                                    title={file.clone()}
                                    on:click=on_click
                                >
                                    <Show when=is_exec_show>
                                        <span class="tab-exec-arrow" title="Debugger paused here">"▶"</span>
                                    </Show>
                                    <Show when=has_bp>
                                        <span class="tab-bp-dot">"●"</span>
                                    </Show>
                                    {file_name}
                                    <span class="tab-close" on:click=on_close>"✕"</span>
                                </div>
                            }
                        }
                    }
                />
                <Show when=move || open_files().is_empty()>
                    <span class="tab-empty">"No files open"</span>
                </Show>
            </div>
            <div class="panel-content source-panel-content">
                <div node_ref=container_ref id="code-view-container"></div>
                <Show when=show_source_onboarding>
                    <div class="onboarding-overlay">
                        <h3>"Start a debug session"</h3>
                        <p>"Launch from terminal, then use the toolbar or F-keys to step."</p>
                        <div class="onboarding-cmd-list">
                            <code>"debugium launch app.py --adapter python --breakpoint /abs/path/app.py:42"</code>
                            <code>"debugium launch app.js --adapter node --breakpoint /abs/path/app.js:15"</code>
                        </div>
                    </div>
                </Show>
            </div>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Stack panel
// ─────────────────────────────────────────────

#[component]
fn StackPanel(
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let layout = use_context::<LayoutState>().expect("no LayoutState");
    let ws = use_context::<WsSenders>().expect("no WsSenders");
    let ws_senders = ws.0;
    let active_tab = layout.active_tab;

    // event_seq changes on every event → drives the panel-updating flash
    let event_seq = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).map(|s| s.event_seq))
            .unwrap_or(0)
    };

    let frames = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.stack_frames)
            .unwrap_or_default()
    };

    let active_frame_id = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .and_then(|s| s.active_frame_id)
    };

    let header_class = move || {
        if event_seq() > 0 { "panel-header panel-updating" } else { "panel-header" }
    };
    let stack_loading = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.status == "paused" && s.stack_frames.is_empty())
            .unwrap_or(false)
    };

    view! {
        <div class="panel" class:panel-loading=stack_loading style="flex:1;overflow:hidden;border-bottom:1px solid var(--border)">
            <div class=header_class>
                <h2>"Threads & Stack"</h2>
                <Show when=stack_loading>
                    <span class="panel-chip">"loading"</span>
                </Show>
                <button
                    class="collapse-btn"
                    title="Collapse right panel"
                    on:click=move |_| layout.right_collapsed.update(|v| *v = !*v)
                >{move || if layout.right_collapsed.get() { "◀" } else { "▶" }}</button>
            </div>
            <div class="panel-content scrollable">
                <ul class="list-view">
                    <For
                        each=frames
                        key=|f| f.id
                        children={
                            let ws_s = ws_senders.clone();
                            move |f: StackFrame| {
                                let top_id = frames().first().map(|f| f.id).unwrap_or(0);
                                let is_active = move || active_frame_id().unwrap_or(top_id) == f.id;
                                let is_top = f.id == top_id;
                                let frame_file = f.file.clone();
                                let frame_id = f.id;
                                let frame_line = f.line;
                                let ws_click = ws_s.clone();
                                let active_click = active_session;
                                let data_click = session_data;
                                let at_click = active_tab;
                                view! {
                                    <li
                                        class:frame-active=is_active
                                        class:frame-subtle=move || !is_active()
                                        style="cursor:pointer"
                                        on:click=move |_| {
                                            let Some(sid) = active_click.get_untracked() else { return; };
                                            // Update active frame, source line, open tab
                                            data_click.update(|map| {
                                                if let Some(s) = map.get_mut(&sid) {
                                                    s.active_frame_id = Some(frame_id);
                                                    // Scroll editor to this frame's line
                                                    s.source_line = Some(frame_line);
                                                    if !frame_file.is_empty() {
                                                        s.source_path = Some(frame_file.clone());
                                                        if !s.open_files.contains(&frame_file) {
                                                            s.open_files.push(frame_file.clone());
                                                        }
                                                    }
                                                }
                                            });
                                            if !frame_file.is_empty() {
                                                at_click.set(Some(frame_file.clone()));
                                            }
                                            // Request scopes for this frame → updates Variables panel
                                            send_cmd(&ws_click, &sid, "scopes",
                                                serde_json::json!({ "frameId": frame_id }));
                                        }
                                    >
                                        <span class="frame-icon">{if is_top { "→" } else { " " }}</span>
                                        {" "}{f.name.clone()}
                                        <span class="frame-location">{basename(&f.file)}":"{f.line}</span>
                                    </li>
                                }
                            }
                        }
                    />
                    <Show when=move || frames().is_empty()>
                        <li class="empty-state onboarding-empty-state">
                            {move || {
                                if active_session.get().is_none() {
                                    "No session yet — launch one to load threads and stack frames.".to_string()
                                } else {
                                    "No stack yet — pause execution or hit a breakpoint.".to_string()
                                }
                            }}
                        </li>
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
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let layout = use_context::<LayoutState>().expect("no LayoutState");
    let ws = use_context::<WsSenders>().expect("no WsSenders");
    let ws_senders = ws.0;

    let vars = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.variables)
            .unwrap_or_default()
    };

    let changed_vars = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.changed_vars)
            .unwrap_or_default()
    };

    let expanded = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.expanded_vars)
            .unwrap_or_default()
    };

    let var_filter = layout.var_filter;
    let var_pending_ref = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .and_then(|s| s.pending_var_ref)
    };
    let vars_loading = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.pending_var_ref.is_some() || s.pending_scope_var_ref.is_some())
            .unwrap_or(false)
    };
    let vars_count = move || vars().len();

    view! {
        <div class="panel" class:panel-loading=vars_loading style=move || if layout.vars_collapsed.get() { "flex: 0 0 32px; overflow: hidden;" } else { "flex: 1; min-height: 0; overflow: hidden;" }>
            <div class="panel-header">
                <h2>"Variables"</h2>
                <span class="panel-chip">{move || vars_count().to_string()}</span>
                <button
                    class="collapse-btn"
                    title="Toggle Variables"
                    on:click=move |_| layout.vars_collapsed.update(|v| *v = !*v)
                >{move || if layout.vars_collapsed.get() { "▸" } else { "▾" }}</button>
            </div>
            <Show when=move || !layout.vars_collapsed.get()>
            <div style="padding: 2px 6px;">
                <input
                    type="text"
                    placeholder="Filter variables…"
                    style="width:100%; box-sizing:border-box; font-size:11px; padding:2px 4px; background:var(--bg-secondary); border:1px solid var(--border); color:var(--text); border-radius:3px;"
                    prop:value=move || var_filter.get()
                    on:input=move |e| {
                        use wasm_bindgen::JsCast;
                        let val = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                            .map(|i| i.value()).unwrap_or_default();
                        var_filter.set(val);
                    }
                />
            </div>
            <div class="panel-content scrollable">
                <ul class="list-view">
                    <For
                        each=move || {
                            let filter = var_filter.get();
                            let filter = filter.to_lowercase();
                            vars().into_iter().filter(move |v| {
                                filter.is_empty() || v.name.to_lowercase().contains(&filter)
                            }).collect::<Vec<_>>()
                        }
                        key=|v| format!("{}={}@{}", v.name, v.value, v.variables_reference)
                        children={
                            let ws_senders2 = ws_senders.clone();
                            move |v| {
                                let type_class = match v.kind.as_deref() {
                                    Some("int") | Some("float") => "var-number",
                                    Some("str")                 => "var-string",
                                    Some("bool")                => "var-bool",
                                    _                           => "var-value",
                                }.to_string();
                                let vref = v.variables_reference;
                                let has_children = vref > 0;
                                let is_expanded = move || expanded().contains_key(&vref);
                                let chevron = move || if is_expanded() { "▼" } else { "▶" };

                                let ws_click = ws_senders2.clone();
                                let data_click = session_data;
                                let active_click = active_session;
                                let on_expand = move |_| {
                                    if !has_children { return; }
                                    let Some(sid) = active_click.get_untracked() else { return; };
                                    let already = data_click.get_untracked()
                                        .get(&sid).map(|s| s.expanded_vars.contains_key(&vref))
                                        .unwrap_or(false);
                                    if already {
                                        // collapse
                                        data_click.update(|map| {
                                            if let Some(s) = map.get_mut(&sid) {
                                                s.expanded_vars.remove(&vref);
                                            }
                                        });
                                    } else {
                                        // expand — mark pending ref then request
                                        data_click.update(|map| {
                                            if let Some(s) = map.get_mut(&sid) {
                                                s.pending_var_ref = Some(vref);
                                            }
                                        });
                                        send_cmd(&ws_click, &sid, "variables",
                                            serde_json::json!({ "variablesReference": vref }));
                                    }
                                };

                                let children_signal = move || {
                                    expanded().get(&vref).cloned().unwrap_or_default()
                                };

                                let editing: RwSignal<bool> = RwSignal::new(false);
                                let edit_val: RwSignal<String> = RwSignal::new(v.value.clone());
                                let v_name = v.name.clone();
                                let ws_edit = ws_senders2.clone();
                                let active_edit = active_session;
                                let v_init_val = v.value.clone();
                                let v_name_chk = v.name.clone();

                                view! {
                                    <li
                                        class="var-item"
                                        class:var-changed=move || changed_vars().contains(&v_name_chk)
                                        class:var-inspecting=move || var_pending_ref() == Some(vref)
                                    >
                                        <Show when=move || has_children>
                                            <span class="var-chevron" on:click=on_expand.clone()>
                                                {chevron}
                                            </span>
                                        </Show>
                                        <Show when=move || !has_children>
                                            <span class="var-chevron var-leaf">"·"</span>
                                        </Show>
                                        <span class="var-name">{v.name.clone()}</span>
                                        <span class="var-sep">" = "</span>
                                        <Show when=move || !editing.get()>
                                            <span
                                                class={type_class.clone()}
                                                title="Double-click to edit"
                                                on:dblclick={
                                                    let init = v_init_val.clone();
                                                    move |_| {
                                                        edit_val.set(init.clone());
                                                        editing.set(true);
                                                    }
                                                }
                                            >{v.value.clone()}</span>
                                        </Show>
                                        <Show when=move || editing.get()>
                                            <input
                                                type="text"
                                                class="var-edit-input"
                                                prop:value=move || edit_val.get()
                                                on:input=move |e| {
                                                    use wasm_bindgen::JsCast;
                                                    let val = e.target().unwrap()
                                                        .unchecked_into::<web_sys::HtmlInputElement>()
                                                        .value();
                                                    edit_val.set(val);
                                                }
                                                on:keydown={
                                                    let ws_kd = ws_edit.clone();
                                                    let nm_kd = v_name.clone();
                                                    let act_kd = active_edit;
                                                    move |e| {
                                                        use wasm_bindgen::JsCast;
                                                        let ke = e.unchecked_ref::<web_sys::KeyboardEvent>();
                                                        match ke.key().as_str() {
                                                            "Enter" => {
                                                                let new_val = edit_val.get_untracked();
                                                                if let Some(sid) = act_kd.get_untracked() {
                                                                    send_cmd(&ws_kd, &sid, "setVariable", serde_json::json!({
                                                                        "variablesReference": vref,
                                                                        "name": nm_kd.clone(),
                                                                        "value": new_val
                                                                    }));
                                                                }
                                                                editing.set(false);
                                                            }
                                                            "Escape" => editing.set(false),
                                                            _ => {}
                                                        }
                                                    }
                                                }
                                                on:blur={
                                                    let ws_bl = ws_edit.clone();
                                                    let nm_bl = v_name.clone();
                                                    let act_bl = active_edit;
                                                    move |_| {
                                                        let new_val = edit_val.get_untracked();
                                                        if let Some(sid) = act_bl.get_untracked() {
                                                            send_cmd(&ws_bl, &sid, "setVariable", serde_json::json!({
                                                                "variablesReference": vref,
                                                                "name": nm_bl.clone(),
                                                                "value": new_val
                                                            }));
                                                        }
                                                        editing.set(false);
                                                    }
                                                }
                                            />
                                        </Show>
                                    </li>
                                    <Show when=move || is_expanded()>
                                        <For
                                            each=children_signal
                                            key=|c| format!("{}={}", c.name, c.value)
                                            children=move |c| {
                                                let child_class = match c.kind.as_deref() {
                                                    Some("int") | Some("float") => "var-number",
                                                    Some("str")                 => "var-string",
                                                    Some("bool")                => "var-bool",
                                                    _                           => "var-value",
                                                }.to_string();
                                                view! {
                                                    <li class="var-item var-child">
                                                        <span class="var-indent">"  "</span>
                                                        <span class="var-name">{c.name}</span>
                                                        <span class="var-sep">" = "</span>
                                                        <span class={child_class}>{c.value}</span>
                                                    </li>
                                                }
                                            }
                                        />
                                    </Show>
                                }
                            }
                        }
                    />
                    <Show when=move || vars().is_empty()>
                        <li class="empty-state onboarding-empty-state">
                            {move || {
                                if active_session.get().is_none() {
                                    "No session yet — variables appear after launch and pause.".to_string()
                                } else {
                                    "No variables for current frame — click a stack frame or run get_debug_context.".to_string()
                                }
                            }}
                        </li>
                    </Show>
                </ul>
            </div>
            </Show>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Completions dropdown
// ─────────────────────────────────────────────

#[component]
fn CompletionsDropdown(
    active_session: ReadSignal<Option<String>>,
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    eval_expr: RwSignal<String>,
    selected: RwSignal<usize>,
    show: RwSignal<bool>,
) -> impl IntoView {
    let comps_signal = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.completions)
            .unwrap_or_default()
    };

    view! {
        <div class="completions-dropdown" style="position:absolute;bottom:100%;left:0;right:0">
            <For
                each=comps_signal
                key=|label| label.clone()
                children={
                    move |label: String| {
                        let label2 = label.clone();
                        view! {
                            <div
                                class="completions-item"
                                on:mousedown=move |_| {
                                    eval_expr.set(label2.clone());
                                    show.set(false);
                                }
                            >{label.clone()}</div>
                        }
                    }
                }
            />
        </div>
    }
}

// ─────────────────────────────────────────────
//  Console panel
// ─────────────────────────────────────────────

#[component]
fn ConsolePanel(
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let layout = use_context::<LayoutState>().expect("no LayoutState");
    let ws = use_context::<WsSenders>().expect("no WsSenders");
    let ws_senders = ws.0;

    let eval_expr: RwSignal<String> = RwSignal::new(String::new());
    let command_history: RwSignal<Vec<String>> = RwSignal::new(vec![]);
    let history_index: RwSignal<Option<usize>> = RwSignal::new(None);
    let exc_uncaught: RwSignal<bool> = RwSignal::new(true);  // default on
    let show_completions: RwSignal<bool> = RwSignal::new(false);
    let selected_completion: RwSignal<usize> = RwSignal::new(0);

    let logs = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.console_logs)
            .unwrap_or_default()
    };

    // Auto-scroll console to bottom when new logs arrive
    Effect::new(move |_| {
        let _ = logs(); // subscribe to changes
        if let Some(el) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("console-logs"))
        {
            el.set_scroll_top(el.scroll_height());
        }
    });

    let do_eval = {
        let ws_eval = ws_senders.clone();
        move |_| {
            let expr = eval_expr.get_untracked();
            if expr.trim().is_empty() { return; }
            command_history.update(|h| {
                if h.last().map(|v| v != &expr).unwrap_or(true) {
                    h.push(expr.clone());
                    if h.len() > 50 { h.remove(0); }
                }
            });
            history_index.set(None);
            let Some(sid) = active_session.get_untracked() else { return; };
            let frame_id = session_data.get_untracked()
                .get(&sid).and_then(|s| s.stack_frames.first().cloned())
                .map(|f| f.id).unwrap_or(0);
            send_cmd(&ws_eval, &sid, "evaluate", serde_json::json!({
                "expression": expr,
                "frameId": frame_id,
                "context": "repl"
            }));
            eval_expr.set(String::new());
            show_completions.set(false);
        }
    };

    let do_completions = {
        let ws_comp = ws_senders.clone();
        move |text: String| {
            if text.trim().is_empty() { show_completions.set(false); return; }
            let Some(sid) = active_session.get_untracked() else { return; };
            let frame_id = session_data.get_untracked()
                .get(&sid).and_then(|s| s.stack_frames.first().cloned())
                .map(|f| f.id).unwrap_or(0);
            let col = text.len() as u32;
            send_cmd(&ws_comp, &sid, "completions", serde_json::json!({
                "text": text,
                "column": col,
                "frameId": frame_id
            }));
            show_completions.set(true);
        }
    };

    let do_exc_toggle = {
        let ws_exc = ws_senders.clone();
        move |_| {
            let new_val = !exc_uncaught.get_untracked();
            exc_uncaught.set(new_val);
            let Some(sid) = active_session.get_untracked() else { return; };
            let filters: Vec<&str> = if new_val { vec!["uncaught"] } else { vec![] };
            send_cmd(&ws_exc, &sid, "setExceptionBreakpoints", serde_json::json!({ "filters": filters }));
        }
    };

    view! {
        <div class="panel console-panel" style=move || if layout.console_collapsed.get() { "height:32px;flex-shrink:0;flex-grow:0" } else { "" }>
            <div class="panel-header">
                <h2>"Debug Console"</h2>
                <button
                    class="collapse-btn"
                    title="Toggle console body"
                    on:click=move |_| layout.console_collapsed.update(|v| *v = !*v)
                >{move || if layout.console_collapsed.get() { "▸" } else { "▾" }}</button>
                <button
                    class="collapse-btn"
                    title="Clear console"
                    on:click=move |_| {
                        if let Some(sid) = active_session.get_untracked() {
                            session_data.update(|map| {
                                if let Some(s) = map.get_mut(&sid) {
                                    s.console_logs.clear();
                                }
                            });
                        }
                    }
                >"🗑"</button>
                <button
                    class=move || if exc_uncaught.get() { "exc-toggle exc-on" } else { "exc-toggle" }
                    title="Toggle break on uncaught exceptions"
                    on:click=do_exc_toggle
                >
                    {move || if exc_uncaught.get() { "✕ uncaught" } else { "✕ off" }}
                </button>
            </div>
            <Show when=move || !layout.console_collapsed.get()>
            <div class="panel-content scrollable" id="console-logs">
                <Show when=move || logs().is_empty()>
                    <div class="console-empty">"No output yet — run the program to see logs here."</div>
                </Show>
                <For
                    each=logs
                    key=|l| l.seq
                    children=move |l| {
                        view! {
                            <div class={format!("log-entry {}", l.class)}>
                                <span class="log-tag">{l.tag}</span>
                                {" "}{l.message}
                            </div>
                        }
                    }
                />
            </div>
            <div class="console-input-row" style="position:relative">
                <span class="console-prompt">"›"</span>
                <div style="flex:1;position:relative">
                    <input
                        type="text"
                        class="console-input"
                        style="width:100%"
                        placeholder="Evaluate expression…"
                        prop:value=move || eval_expr.get()
                        on:input={
                            let do_comp = do_completions.clone();
                            move |e| {
                                use wasm_bindgen::JsCast;
                                let val = e.target().unwrap()
                                    .unchecked_into::<web_sys::HtmlInputElement>()
                                    .value();
                                eval_expr.set(val.clone());
                                history_index.set(None);
                                do_comp(val);
                            }
                        }
                        on:keydown={
                            let do_ev = do_eval.clone();
                            let hist = command_history;
                            let hist_idx = history_index;
                            move |e| {
                                use wasm_bindgen::JsCast;
                                let ke = e.unchecked_ref::<web_sys::KeyboardEvent>();
                                match ke.key().as_str() {
                                    "Enter" => { do_ev(()); show_completions.set(false); }
                                    "Escape" => show_completions.set(false),
                                    "ArrowDown" => {
                                        if show_completions.get_untracked() {
                                            selected_completion.update(|n| *n = n.saturating_add(1));
                                        } else {
                                            e.prevent_default();
                                            let len = hist.get_untracked().len();
                                            match hist_idx.get_untracked() {
                                                Some(i) if i + 1 < len => {
                                                    let ni = i + 1;
                                                    hist_idx.set(Some(ni));
                                                    if let Some(cmd) = hist.get_untracked().get(ni).cloned() {
                                                        eval_expr.set(cmd);
                                                    }
                                                }
                                                _ => {
                                                    hist_idx.set(None);
                                                    eval_expr.set(String::new());
                                                }
                                            }
                                        }
                                    }
                                    "ArrowUp" => {
                                        if show_completions.get_untracked() {
                                            selected_completion.update(|n| *n = n.saturating_sub(1));
                                        } else {
                                            e.prevent_default();
                                            let len = hist.get_untracked().len();
                                            if len == 0 { return; }
                                            let next = match hist_idx.get_untracked() {
                                                Some(i) if i > 0 => i - 1,
                                                Some(_) => 0,
                                                None => len - 1,
                                            };
                                            hist_idx.set(Some(next));
                                            if let Some(cmd) = hist.get_untracked().get(next).cloned() {
                                                eval_expr.set(cmd);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        on:blur=move |_| {
                            // Delay so click on dropdown item registers first
                            leptos::task::spawn_local(async move {
                                gloo_timers::future::sleep(std::time::Duration::from_millis(150)).await;
                                show_completions.set(false);
                            });
                        }
                    />
                    <Show when=move || show_completions.get()>
                        <CompletionsDropdown
                            active_session=active_session
                            session_data=session_data
                            eval_expr=eval_expr
                            selected=selected_completion
                            show=show_completions
                        />
                    </Show>
                </div>
                <button class="eval-btn" on:click=move |_| do_eval(())>"▶"</button>
            </div>
            </Show>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Getting started helper
// ─────────────────────────────────────────────

#[component]
fn GettingStartedCard(
    sessions: ReadSignal<Vec<String>>,
    onboarding_dismissed: RwSignal<bool>,
    show_launch_modal: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <Show when=move || sessions.get().is_empty() && !onboarding_dismissed.get()>
            <div class="onboarding-card">
                <div class="onboarding-card-header">
                    <h3>"Getting Started"</h3>
                    <button
                        class="collapse-btn"
                        title="Dismiss onboarding tips"
                        on:click=move |_| onboarding_dismissed.set(true)
                    >"✕"</button>
                </div>
                <ol class="onboarding-steps">
                    <li>"Launch your target with a breakpoint."</li>
                    <li><code>"debugium launch app.py --adapter python --breakpoint /abs/path/app.py:42"</code></li>
                    <li>"Use toolbar or shortcuts: F5 continue, F10 over, F11 into."</li>
                    <li>"For AI debugging, start from " <code>"get_debug_context"</code> " before deeper tools."</li>
                </ol>
                <div class="onboarding-actions">
                    <button class="launch-primary-btn" on:click=move |_| show_launch_modal.set(true)>
                        "Launch from UI"
                    </button>
                </div>
            </div>
        </Show>
    }
}

#[component]
fn LaunchSessionModal(
    show: RwSignal<bool>,
    sessions: RwSignal<Vec<String>>,
    active_session: RwSignal<Option<String>>,
    session_metas: RwSignal<std::collections::HashMap<String, Value>>,
) -> impl IntoView {
    let program = RwSignal::new(String::new());
    let adapter = RwSignal::new(String::from("python"));
    let breakpoints = RwSignal::new(String::new());
    let session_name = RwSignal::new(String::new());
    let submitting = RwSignal::new(false);
    let error_msg: RwSignal<Option<String>> = RwSignal::new(None);

    let close_modal = move |_| {
        if submitting.get() {
            return;
        }
        show.set(false);
        error_msg.set(None);
    };

    let submit_launch = move |_| {
        if submitting.get() {
            return;
        }
        let program_val = program.get().trim().to_string();
        if program_val.is_empty() {
            error_msg.set(Some("Program path is required.".to_string()));
            return;
        }
        submitting.set(true);
        error_msg.set(None);

        let adapter_val = adapter.get();
        let breakpoints_val = parse_breakpoint_input(&breakpoints.get());
        let session_name_val = session_name.get().trim().to_string();
        let sessions_u = sessions;
        let active_session_u = active_session;
        let session_metas_u = session_metas;
        let show_u = show;
        let program_reset = program;
        let breakpoints_reset = breakpoints;
        let session_name_reset = session_name;
        let submitting_u = submitting;
        let error_msg_u = error_msg;

        leptos::task::spawn_local(async move {
            let mut payload = serde_json::Map::new();
            payload.insert("program".to_string(), Value::String(program_val.clone()));
            payload.insert("adapter".to_string(), Value::String(adapter_val.clone()));
            if !breakpoints_val.is_empty() {
                payload.insert(
                    "breakpoints".to_string(),
                    Value::Array(breakpoints_val.into_iter().map(Value::String).collect()),
                );
            }
            if !session_name_val.is_empty() {
                payload.insert("session_id".to_string(), Value::String(session_name_val.clone()));
            }

            let response = match gloo_net::http::Request::post("/sessions").json(&payload) {
                Ok(req) => req.send().await,
                Err(e) => {
                    error_msg_u.set(Some(format!("Failed to encode request: {e}")));
                    submitting_u.set(false);
                    return;
                }
            };

            match response {
                Ok(resp) if resp.ok() => {
                    match resp.json::<Value>().await {
                        Ok(body) => {
                            let sid = body
                                .get("session_id")
                                .and_then(Value::as_str)
                                .unwrap_or("default")
                                .to_string();
                            sessions_u.update(|s| {
                                if !s.contains(&sid) {
                                    s.push(sid.clone());
                                }
                            });
                            session_metas_u.update(|m| {
                                m.insert(
                                    sid.clone(),
                                    serde_json::json!({ "id": sid, "program": program_val, "adapter": adapter_val }),
                                );
                            });
                            active_session_u.set(Some(sid));
                            program_reset.set(String::new());
                            breakpoints_reset.set(String::new());
                            session_name_reset.set(String::new());
                            show_u.set(false);
                        }
                        Err(e) => error_msg_u.set(Some(format!("Invalid launch response: {e}"))),
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_else(|_| "Unknown launch error".to_string());
                    let parsed_msg = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or(text);
                    error_msg_u.set(Some(format!("Launch failed ({status}): {parsed_msg}")));
                }
                Err(e) => error_msg_u.set(Some(format!("Network error while launching: {e}"))),
            }
            submitting_u.set(false);
        });
    };

    view! {
        <Show when=move || show.get()>
            <div class="modal-backdrop" on:click=close_modal>
                <div class="launch-modal" on:click=move |ev| ev.stop_propagation()>
                    <div class="launch-modal-header">
                        <h3>"Launch Debug Session"</h3>
                        <button class="collapse-btn" title="Close" on:click=close_modal>"✕"</button>
                    </div>
                    <div class="launch-modal-body">
                        <label class="launch-field">
                            <span>"Program path"</span>
                            <input
                                class="launch-input"
                                placeholder="/abs/path/to/app.py"
                                prop:value=move || program.get()
                                on:input=move |ev| program.set(event_target_value(&ev))
                            />
                        </label>

                        <label class="launch-field">
                            <span>"Adapter"</span>
                            <select
                                class="launch-input"
                                prop:value=move || adapter.get()
                                on:change=move |ev| adapter.set(event_target_value(&ev))
                            >
                                <option value="python">"python"</option>
                                <option value="node">"node"</option>
                                <option value="typescript">"typescript"</option>
                                <option value="lldb">"lldb"</option>
                            </select>
                        </label>

                        <label class="launch-field">
                            <span>"Breakpoints (optional, file:line; comma or newline separated)"</span>
                            <textarea
                                class="launch-input launch-breakpoints"
                                placeholder="/abs/path/to/app.py:42"
                                prop:value=move || breakpoints.get()
                                on:input=move |ev| breakpoints.set(event_target_value(&ev))
                            ></textarea>
                        </label>

                        <label class="launch-field">
                            <span>"Session name (optional)"</span>
                            <input
                                class="launch-input"
                                placeholder="session-custom"
                                prop:value=move || session_name.get()
                                on:input=move |ev| session_name.set(event_target_value(&ev))
                            />
                        </label>

                        <Show when=move || error_msg.get().is_some()>
                            <div class="launch-error">{move || error_msg.get().unwrap_or_default()}</div>
                        </Show>
                    </div>
                    <div class="launch-modal-actions">
                        <button class="debug-btn btn-theme" on:click=close_modal disabled=move || submitting.get()>
                            "Cancel"
                        </button>
                        <button
                            class=move || if submitting.get() { "debug-btn btn-continue btn-inflight" } else { "debug-btn btn-continue" }
                            on:click=submit_launch
                            disabled=move || submitting.get()
                        >
                            <Show when=move || submitting.get()>
                                <span class="cmd-spinner"></span>
                            </Show>
                            {move || if submitting.get() { "Launching..." } else { "Launch" }}
                        </button>
                    </div>
                </div>
            </div>
        </Show>
    }
}

#[component]
fn GlobalErrorToast() -> impl IntoView {
    let ge = use_context::<GlobalError>().expect("no GlobalError").0;
    view! {
        <Show when=move || ge.get().is_some()>
            <div class="global-error-toast">
                <span>{move || ge.get().unwrap_or_default()}</span>
                <button class="collapse-btn" on:click=move |_| ge.set(None)>"✕"</button>
            </div>
        </Show>
    }
}

// ─────────────────────────────────────────────
//  Process identity bar
// ─────────────────────────────────────────────

#[component]
fn ProcessInfoBar(
    active_session: ReadSignal<Option<String>>,
    session_metas: ReadSignal<std::collections::HashMap<String, Value>>,
) -> impl IntoView {
    let program = move || {
        active_session.get()
            .and_then(|id| session_metas.get().get(&id).and_then(|m| m.get("program")).and_then(Value::as_str).map(str::to_string))
    };
    let adapter = move || {
        active_session.get()
            .and_then(|id| session_metas.get().get(&id).and_then(|m| m.get("adapter")).and_then(Value::as_str).map(str::to_string))
    };
    let pid = move || {
        active_session.get()
            .and_then(|id| session_metas.get().get(&id).and_then(|m| m.get("adapter_pid")).and_then(Value::as_u64))
    };

    view! {
        <Show when=move || program().is_some()>
            <div class="process-info-bar">
                <span class="process-info-program">{move || program().unwrap_or_default()}</span>
                <Show when=move || adapter().is_some()>
                    <span class="session-adapter-pill">{move || adapter().unwrap_or_default()}</span>
                </Show>
                <Show when=move || pid().is_some()>
                    <span class="process-info-pid">"PID: "{move || pid().unwrap_or(0).to_string()}</span>
                </Show>
            </div>
        </Show>
    }
}

// ─────────────────────────────────────────────
//  Breakpoints panel
// ─────────────────────────────────────────────

#[component]
fn BreakpointsPanel(
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let layout = use_context::<LayoutState>().expect("no LayoutState");
    let ws = use_context::<WsSenders>().expect("no WsSenders");
    let ws_senders = ws.0;
    let active_tab = layout.active_tab;
    let bps_collapsed = layout.bps_collapsed;

    let breakpoints = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| {
                let mut entries: Vec<(String, Vec<BreakpointSpec>)> = s.breakpoints.iter()
                    .map(|(f, specs)| (f.clone(), specs.clone()))
                    .collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                entries
            })
            .unwrap_or_default()
    };
    let recent_bp = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .and_then(|s| s.recent_verified_breakpoint)
    };
    let bp_count = move || {
        breakpoints().iter().map(|(_, specs)| specs.len()).sum::<usize>()
    };

    let clear_all = {
        let ws_c = ws_senders.clone();
        let sd = session_data;
        let asi = active_session;
        move |_| {
            let Some(sid) = asi.get_untracked() else { return; };
            let files: Vec<String> = sd.get_untracked()
                .get(&sid).map(|s| s.breakpoints.keys().cloned().collect()).unwrap_or_default();
            for file in files {
                let bp_args = serde_json::json!({
                    "source": { "path": file },
                    "breakpoints": []
                });
                send_cmd(&ws_c, &sid, "setBreakpoints", bp_args);
            }
        }
    };

    view! {
        <div class="panel bp-panel" style=move || if bps_collapsed.get() { "flex: 0 0 32px; overflow: hidden;" } else { "flex: 0 0 auto; max-height: 200px; overflow: hidden; border-top: 1px solid var(--border);" }>
            <div class="panel-header">
                <h2>"Breakpoints"</h2>
                <span class="panel-chip">{move || bp_count().to_string()}</span>
                <button
                    class="collapse-btn"
                    title="Clear all breakpoints"
                    on:click=clear_all
                >"🗑"</button>
                <button
                    class="collapse-btn"
                    title="Toggle breakpoints panel"
                    on:click=move |_| bps_collapsed.update(|v| *v = !*v)
                >{move || if bps_collapsed.get() { "▸" } else { "▾" }}</button>
            </div>
            <Show when=move || !bps_collapsed.get()>
            <div class="panel-content scrollable">
                <ul class="list-view">
                    <For
                        each=breakpoints
                        key=|(f, _)| f.clone()
                        children={
                            let at = active_tab;
                            let sd2 = session_data;
                            let asi2 = active_session;
                            move |(file, specs): (String, Vec<BreakpointSpec>)| {
                                let file_name = basename(&file);
                                view! {
                                    <li class="bp-file-header">
                                        <span class="bp-file-name" title={file.clone()}>{file_name}</span>
                                    </li>
                                    <For
                                        each=move || specs.clone()
                                        key=|s| s.line
                                        children={
                                            let f = file.clone();
                                            let at2 = at;
                                            let sd3 = sd2;
                                            let asi3 = asi2;
                                            move |spec: BreakpointSpec| {
                                                let f2 = f.clone();
                                                let f_for_class = f2.clone();
                                                let f_for_click = f2.clone();
                                                let at3 = at2;
                                                let sd4 = sd3;
                                                let asi4 = asi3;
                                                let line = spec.line;
                                                let cond = spec.condition.clone();
                                                view! {
                                                    <li
                                                        class="bp-line-item"
                                                        class:bp-just-verified=move || recent_bp() == Some((f_for_class.clone(), line))
                                                        style="cursor:pointer"
                                                        on:click=move |_| {
                                                            // Open that file in the editor
                                                            if let Some(id) = asi4.get_untracked() {
                                                                sd4.update(|map| {
                                                                    if let Some(s) = map.get_mut(&id) {
                                                                        if !s.open_files.contains(&f_for_click) {
                                                                            s.open_files.push(f_for_click.clone());
                                                                        }
                                                                    }
                                                                });
                                                            }
                                                            at3.set(Some(f_for_click.clone()));
                                                            // Scroll editor to line
                                                            editor::set_exec_line(line);
                                                        }
                                                    >
                                                        <span class="bp-dot">"●"</span>
                                                        <span class="bp-line-num">"line "{line}</span>
                                                        {
                                                            let c = cond.clone();
                                                            if let Some(condition) = c {
                                                                view! { <span class="bp-condition" title="Condition">"⚑ "{condition}</span> }.into_any()
                                                            } else {
                                                                view! { <span></span> }.into_any()
                                                            }
                                                        }
                                                    </li>
                                                }
                                            }
                                        }
                                    />
                                }
                            }
                        }
                    />
                    <Show when=move || breakpoints().is_empty()>
                        <li class="empty-state">"No breakpoints"</li>
                    </Show>
                </ul>
            </div>
            </Show>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Findings panel (LLM-authored observations)
// ─────────────────────────────────────────────

#[component]
fn FindingsPanel(
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let collapsed: RwSignal<bool> = RwSignal::new(false);

    let findings = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.findings)
            .unwrap_or_default()
    };
    let findings_latest_first = move || {
        let mut items = findings();
        items.reverse();
        items
    };

    view! {
        <div class="panel findings-panel" style=move || {
            if collapsed.get() {
                "flex: 0 0 28px; overflow: hidden; border-top: 1px solid var(--border);".to_string()
            } else if findings().is_empty() {
                "display: none;".to_string()
            } else {
                format!("flex: 0 0 auto; max-height: 220px; overflow: auto; border-top: 1px solid var(--border);")
            }
        }>
            <div class="panel-header">
                <h2>"Findings"</h2>
                <span class="panel-chip">{move || findings().len().to_string()}</span>
                <button
                    class="collapse-btn"
                    title="Toggle findings panel"
                    on:click=move |_| collapsed.update(|v| *v = !*v)
                >{move || if collapsed.get() { "▸" } else { "▾" }}</button>
            </div>
            <Show when=move || !collapsed.get()>
            <div class="panel-content scrollable">
                <ul class="list-view">
                    <For
                        each=findings_latest_first
                        key=|f| f.id
                        children=|f: FindingEntry| {
                            let icon = match f.level.as_str() { "error" => "🔴", "warning" => "🟡", _ => "🔵" };
                            let is_error = f.level == "error";
                            let is_warning = f.level == "warning";
                            let msg_full = f.message.clone();
                            let msg_short = truncate_text(&msg_full, 140);
                            let is_long = msg_full.chars().count() > 140;
                            let ts = f.timestamp.clone();
                            let expanded = RwSignal::new(false);
                            view! {
                                <li class="finding-item" class:finding-error=move || is_error class:finding-warning=move || is_warning>
                                    <span class="finding-icon">{icon}</span>
                                    <div class="finding-body">
                                        <span class="finding-msg">
                                            {move || {
                                                if is_long && !expanded.get() { msg_short.clone() } else { msg_full.clone() }
                                            }}
                                        </span>
                                        <div class="finding-meta-row">
                                            <span class="finding-ts">{ts.clone()}</span>
                                            <Show when=move || is_long>
                                                <button
                                                    class="finding-toggle"
                                                    on:click=move |_| expanded.update(|v| *v = !*v)
                                                >
                                                    {move || if expanded.get() { "show less" } else { "show more" }}
                                                </button>
                                            </Show>
                                        </div>
                                    </div>
                                </li>
                            }
                        }
                    />
                    <Show when=move || findings().is_empty()>
                        <li class="empty-state onboarding-empty-state">
                            "No findings yet — add notes with annotate/add_finding so teammates and AI keep shared context."
                        </li>
                    </Show>
                </ul>
            </div>
            </Show>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Watch panel
// ─────────────────────────────────────────────

#[component]
fn WatchPanel(
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let layout = use_context::<LayoutState>().expect("no LayoutState");
    let ws = use_context::<WsSenders>().expect("no WsSenders");
    let ws_senders = ws.0;

    let new_watch: RwSignal<String> = RwSignal::new(String::new());
    let watches = layout.watches;
    let watch_collapsed: RwSignal<bool> = RwSignal::new(false);

    let watch_results = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.watch_results)
            .unwrap_or_default()
    };
    let watch_loading = move || watch_results().iter().any(|(_, r)| r == "…");

    let add_watch = {
        let ws_add = ws_senders.clone();
        move |_| {
            let expr = new_watch.get_untracked();
            if expr.trim().is_empty() { return; }
            watches.update(|w| {
                if !w.contains(&expr) { w.push(expr.clone()); }
            });
            // Evaluate immediately if paused
            if let Some(sid) = active_session.get_untracked() {
                let frame_id = session_data.get_untracked()
                    .get(&sid).and_then(|s| s.stack_frames.first().cloned())
                    .map(|f| f.id).unwrap_or(0);
                send_cmd(&ws_add, &sid, "evaluate", serde_json::json!({
                    "expression": expr,
                    "frameId": frame_id,
                    "context": "watch"
                }));
            }
            new_watch.set(String::new());
        }
    };

    view! {
        <div class="panel watch-panel" class:panel-loading=watch_loading style=move || if watch_collapsed.get() { "flex: 0 0 32px; overflow: hidden; border-top: 1px solid var(--border);" } else { "flex: 0 0 auto; max-height: 200px; border-top: 1px solid var(--border); overflow: hidden;" }>
            <div class="panel-header">
                <h2>"Watch"</h2>
                <span class="panel-chip">{move || watches.get().len().to_string()}</span>
                <Show when=watch_loading>
                    <span class="panel-chip">"evaluating"</span>
                </Show>
                <button
                    class="collapse-btn"
                    title="Toggle Watch"
                    on:click=move |_| watch_collapsed.update(|v| *v = !*v)
                >{move || if watch_collapsed.get() { "▸" } else { "▾" }}</button>
            </div>
            <Show when=move || !watch_collapsed.get()>
            <div class="panel-content scrollable">
                <ul class="list-view">
                    <For
                        each=move || watches.get()
                        key=|e: &String| e.clone()
                        children={
                            let ws_del = ws_senders.clone();
                            move |expr: String| {
                                let expr2 = expr.clone();
                                let result = move || {
                                    watch_results().into_iter()
                                        .find(|(e, _)| e == &expr2)
                                        .map(|(_, r)| r)
                                };
                                let result_for_class = result.clone();
                                let expr_del = expr.clone();
                                view! {
                                    <li class="var-item" class:watch-pending=move || result_for_class().as_deref() == Some("…")>
                                        <span class="var-name">{expr.clone()}</span>
                                        <span class="var-sep">" = "</span>
                                        <span class="var-value">{move || result().unwrap_or_else(|| "…".into())}</span>
                                        <span
                                            style="margin-left:auto;cursor:pointer;color:var(--text-dim);font-size:10px"
                                            on:click={
                                                move |_| {
                                                    let _ = &ws_del;
                                                    watches.update(|w| w.retain(|e| e != &expr_del));
                                                }
                                            }
                                        >"✕"</span>
                                    </li>
                                }
                            }
                        }
                    />
                </ul>
                <div class="console-input-row">
                    <input
                        type="text"
                        class="console-input"
                        placeholder="Add watch expression…"
                        prop:value=move || new_watch.get()
                        on:input=move |e| {
                            use wasm_bindgen::JsCast;
                            let val = e.target().unwrap()
                                .unchecked_into::<web_sys::HtmlInputElement>()
                                .value();
                            new_watch.set(val);
                        }
                        on:keydown={
                            let add = add_watch.clone();
                            move |e| {
                                use wasm_bindgen::JsCast;
                                let ke = e.unchecked_ref::<web_sys::KeyboardEvent>();
                                if ke.key() == "Enter" { add(()); }
                            }
                        }
                    />
                    <button class="eval-btn" on:click=move |_| add_watch(())>"+"</button>
                </div>
            </div>
            </Show>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Timeline panel
// ─────────────────────────────────────────────

#[component]
fn TimelinePanel(
    session_data: RwSignal<std::collections::HashMap<String, SessionState>>,
    active_session: ReadSignal<Option<String>>,
) -> impl IntoView {
    let layout = use_context::<LayoutState>().expect("no LayoutState");
    let timeline_collapsed: RwSignal<bool> = RwSignal::new(true);
    let active_tab = layout.active_tab;

    let timeline = move || {
        active_session.get()
            .and_then(|id| session_data.get().get(&id).cloned())
            .map(|s| s.timeline)
            .unwrap_or_default()
    };

    view! {
        <div class="panel timeline-panel" style="flex: 0 0 auto; max-height: 220px; border-top: 1px solid var(--border); overflow: hidden;">
            <div class="panel-header" style="cursor:pointer" on:click=move |_| timeline_collapsed.update(|v| *v = !*v)>
                <h2>
                    {move || if timeline_collapsed.get() { "▸" } else { "▾" }}
                    " Timeline"
                </h2>
                <span class="panel-chip" style="margin-left:auto;">
                    {move || timeline().len()}
                </span>
            </div>
            <Show when=move || !timeline_collapsed.get()>
                <div class="panel-content scrollable" style="max-height:180px">
                    <ul class="list-view timeline-list">
                        <For
                            each=move || {
                                let mut v = timeline();
                                v.reverse();
                                v.into_iter().enumerate().collect::<Vec<_>>()
                            }
                            key=|(_, e): &(usize, TimelineEntryUi)| e.id
                            children=move |(_, entry): (usize, TimelineEntryUi)| {
                                let file_short = entry.file.split('/').last().unwrap_or("?").to_string();
                                let has_changes = !entry.changed_vars.is_empty();
                                let changed_label = if has_changes {
                                    format!(" — {}", entry.changed_vars.join(", "))
                                } else { String::new() };
                                let file_for_click = entry.file.clone();
                                view! {
                                    <li
                                        class="var-item timeline-item"
                                        class:timeline-changed=has_changes
                                        style="cursor:pointer;align-items:flex-start;padding:3px 6px"
                                        on:click={
                                            let file_click = file_for_click.clone();
                                            move |_| {
                                                if !file_click.is_empty() {
                                                    active_tab.set(Some(file_click.clone()));
                                                }
                                            }
                                        }
                                    >
                                        <span class="var-name" style="min-width:80px;flex-shrink:0">
                                            {format!("{}:{}", file_short, entry.line)}
                                        </span>
                                        <span
                                            class="var-value"
                                            class:var-changed=has_changes
                                            style="white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
                                        >
                                            {changed_label}
                                        </span>
                                    </li>
                                }
                            }
                        />
                    </ul>
                    {move || if timeline().is_empty() {
                        view! {
                            <div class="empty-msg onboarding-empty-state">
                                "No timeline stops yet — continue to a breakpoint or step once to start history."
                            </div>
                        }.into_any()
                    } else {
                        view! { <div></div> }.into_any()
                    }}
                </div>
            </Show>
        </div>
    }
}

// ─────────────────────────────────────────────
//  Utils
// ─────────────────────────────────────────────

fn read_window_storage(key: &str) -> Option<String> {
    let win = web_sys::window()?;
    let storage = Reflect::get(win.as_ref(), &JsValue::from_str("localStorage")).ok()?;
    if storage.is_null() || storage.is_undefined() {
        return None;
    }
    let get_item = Reflect::get(&storage, &JsValue::from_str("getItem"))
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?;
    let value = get_item
        .call1(&storage, &JsValue::from_str(key))
        .ok()?;
    value.as_string()
}

fn write_window_storage(key: &str, value: &str) {
    let Some(win) = web_sys::window() else { return; };
    let Ok(storage) = Reflect::get(win.as_ref(), &JsValue::from_str("localStorage")) else { return; };
    if storage.is_null() || storage.is_undefined() {
        return;
    }
    let Ok(set_item) = Reflect::get(&storage, &JsValue::from_str("setItem")) else { return; };
    let Ok(set_item_fn) = set_item.dyn_into::<js_sys::Function>() else { return; };
    let _ = set_item_fn.call2(&storage, &JsValue::from_str(key), &JsValue::from_str(value));
}

fn basename(path: &str) -> String {
    path.split('/').last().unwrap_or(path).to_string()
}

fn parse_breakpoint_input(input: &str) -> Vec<String> {
    input
        .split(&[',', '\n', '\r'][..])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

// ─────────────────────────────────────────────
//  WASM entry point
// ─────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
