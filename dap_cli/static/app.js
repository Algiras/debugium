const wsUrl = `ws://${window.location.host}/ws`;
let socket = null;

let sessions = {};
let activeSessionId = null;

let editor;
let currentHighlightLine = null;
let currentArrowMarker = null;
let breakpoints = new Set();

function connectWebSocket() {
    socket = new WebSocket(wsUrl);

    socket.onopen = () => {
        logToConsole("System", "Connected to debug server", "log-success");
    };

    socket.onmessage = (event) => {
        try {
            const payload = JSON.parse(event.data);
            const sessionId = payload.session_id || "Root";
            const msg = payload.msg;

            if (!sessions[sessionId]) {
                sessions[sessionId] = {
                    id: sessionId,
                    threads: [],
                    activeThreadId: 1,
                    variables: [],
                    stackFrames: [],
                    sourcePath: null,
                    sourceLine: null,
                    sourceCode: null,
                    status: "running"
                };
                if (!activeSessionId) activeSessionId = sessionId;
                renderSessionsList();
            }

            handleDapMessage(msg, sessionId);
        } catch (e) {
            console.error("Failed to parse message", e);
        }
    };

    socket.onclose = () => {
        logToConsole("System", "Disconnected. Reconnecting in 3s...", "log-error");
        setTimeout(connectWebSocket, 3000);
    };
}

function sendCommand(cmd, args = {}) {
    if (socket && socket.readyState === WebSocket.OPEN && activeSessionId) {
        socket.send(JSON.stringify({
            cmd: "request",
            session_id: activeSessionId,
            args: { command: cmd, arguments: args }
        }));
        logToConsole("→", `${cmd}`, "log-request");
    }
}

function selectSession(sessionId) {
    activeSessionId = sessionId;
    renderSessionsList();
    renderThreads();
    renderVariables();
    const s = sessions[sessionId];
    if (s && s.sourcePath) {
        document.getElementById('current-file').textContent = `${s.sourcePath}:${s.sourceLine}`;
        if (s.sourceCode) {
            renderSourceCodeFromState(s.sourceCode, s.sourceLine, s.sourcePath);
        } else {
            fetchSourceCode(s.sourcePath, s.sourceLine, sessionId);
        }
    } else {
        document.getElementById('current-file').textContent = "No file mapped";
        clearExecMarkers();
        if (editor) editor.setValue("// Waiting for debugger to stop...");
    }
}

function handleDapMessage(msg, sessionId) {
    const type = msg.type;
    const isActive = (sessionId === activeSessionId);
    const state = sessions[sessionId];

    if (type === "event") {
        if (msg.event === "stopped") {
            state.activeThreadId = msg.body.threadId || 1;
            state.status = "paused";
            renderSessionsList();
            logToConsole("⏸", `Paused (${msg.body.reason || 'breakpoint'}) thread ${state.activeThreadId}`, "log-event");

            if (isActive) {
                sendCommand('threads');
                sendCommand('stackTrace', { threadId: state.activeThreadId });
            }
        } else if (msg.event === "continued") {
            state.status = "running";
            renderSessionsList();
            logToConsole("▶", "Resumed", "log-event");
        } else if (msg.event === "terminated") {
            state.status = "ended";
            renderSessionsList();
            logToConsole("⏹", "Program terminated", "log-event");
        } else if (msg.event === "output") {
            const cat = msg.body.category || "console";
            if (cat !== "telemetry") {
                logToConsole("out", msg.body.output.trim(), "log-text");
            }
        } else if (msg.event === "thread") {
            logToConsole("⚡", `Thread ${msg.body.threadId} ${msg.body.reason}`, "log-event");
        }
    } else if (type === "response") {
        if (!msg.success) return;

        if (msg.command === "threads") {
            state.threads = msg.body.threads || [];
            if (isActive) renderThreads();
        }

        if (msg.command === "stackTrace") {
            const frames = msg.body.stackFrames || [];
            state.stackFrames = frames;
            if (frames.length > 0) {
                const topFrame = frames[0];
                state.sourcePath = topFrame.source ? topFrame.source.path : null;
                state.sourceLine = topFrame.line;

                if (isActive) {
                    if (topFrame.source && topFrame.source.path) {
                        document.getElementById('current-file').textContent = basename(topFrame.source.path) + ':' + topFrame.line;
                        fetchSourceCode(topFrame.source.path, topFrame.line, sessionId);
                    } else {
                        document.getElementById('current-file').textContent = topFrame.name;
                        if (editor) editor.setValue(`// ${topFrame.name}\n// Line: ${topFrame.line}`);
                    }
                    renderStackFrames(frames);
                }

                sendCommand('scopes', { frameId: topFrame.id });
            }
        }

        if (msg.command === "scopes") {
            const scopes = msg.body.scopes || [];
            if (scopes.length > 0) {
                sendCommand('variables', { variablesReference: scopes[0].variablesReference });
            }
        }

        if (msg.command === "variables") {
            state.variables = msg.body.variables || [];
            if (isActive) renderVariables();
        }
    }
}

function basename(path) {
    return path.split('/').pop();
}

function renderSessionsList() {
    const list = document.getElementById("sessions-list");
    if (!list) return;
    list.innerHTML = "";

    const keys = Object.keys(sessions);
    if (keys.length === 0) {
        list.innerHTML = `<li class="empty-state">No sessions</li>`;
        return;
    }

    keys.forEach(id => {
        const s = sessions[id];
        const li = document.createElement("li");
        const icon = s.status === "paused" ? "⏸" : s.status === "ended" ? "⏹" : "▶";
        li.innerHTML = `<span class="session-icon">${icon}</span> ${s.id}`;
        if (s.id === activeSessionId) li.classList.add("active-item");
        li.onclick = () => selectSession(s.id);
        list.appendChild(li);
    });
}

function renderStackFrames(frames) {
    const list = document.getElementById("threads-list");
    if (!list) return;
    list.innerHTML = "";

    // Show active thread header
    const state = sessions[activeSessionId];
    if (state && state.threads.length > 0) {
        const threadHeader = document.createElement("li");
        threadHeader.className = "thread-header";
        const t = state.threads.find(t => t.id === state.activeThreadId) || state.threads[0];
        threadHeader.innerHTML = `<span class="thread-icon">🧵</span> ${t.name || 'Thread ' + t.id}`;
        list.appendChild(threadHeader);
    }

    frames.forEach((f, i) => {
        const li = document.createElement("li");
        li.className = i === 0 ? "frame-active" : "frame-subtle";
        const name = f.name || "???";
        const file = f.source ? basename(f.source.path) : "unknown";
        li.innerHTML = `<span class="frame-icon">${i === 0 ? '→' : ' '}</span> ${name} <span class="frame-location">${file}:${f.line}</span>`;
        list.appendChild(li);
    });
}

function renderThreads() {
    const state = sessions[activeSessionId];
    if (!state) return;
    // Don't clear if we already have stack frames rendered
    if (state.stackFrames && state.stackFrames.length > 0) return;

    const list = document.getElementById("threads-list");
    if (!list) return;
    list.innerHTML = "";

    if (state.threads.length === 0) {
        list.innerHTML = `<li class="empty-state">No active threads</li>`;
        return;
    }

    state.threads.forEach(t => {
        const li = document.createElement("li");
        li.innerHTML = `<span class="thread-icon">🧵</span> [${t.id}] ${t.name || "Thread"}`;
        if (t.id === state.activeThreadId) li.classList.add("active-item");
        list.appendChild(li);
    });
}

function renderVariables() {
    if (!activeSessionId) return;
    const state = sessions[activeSessionId];
    const list = document.getElementById("variables-list");
    if (!list) return;
    list.innerHTML = "";

    if (state.variables.length === 0) {
        list.innerHTML = `<li class="empty-state">No variables in scope</li>`;
        return;
    }

    state.variables.forEach(v => {
        const li = document.createElement("li");
        li.className = "var-item";
        const typeClass = getTypeClass(v.type);
        li.innerHTML = `<span class="var-name">${escapeHtml(v.name)}</span><span class="var-sep">=</span><span class="var-value ${typeClass}">${escapeHtml(v.value)}</span>`;
        list.appendChild(li);
    });
}

function getTypeClass(type) {
    if (!type) return '';
    if (type === 'int' || type === 'float') return 'var-number';
    if (type === 'str') return 'var-string';
    if (type === 'bool') return 'var-bool';
    return '';
}

function logToConsole(tag, message, className) {
    const consoleLogs = document.getElementById("console-logs");
    if (!consoleLogs) return;
    const entry = document.createElement("div");
    entry.className = `log-entry ${className || ""}`;
    entry.innerHTML = `<span class="log-tag">${tag}</span> ${escapeHtml(message)}`;
    consoleLogs.appendChild(entry);
    consoleLogs.scrollTop = consoleLogs.scrollHeight;
}

function escapeHtml(unsafe) {
    return (unsafe || "").toString()
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;");
}

let lastFetchedPath = "";
let lastFetchedLines = [];

async function fetchSourceCode(path, highlightLine, sessionId) {
    if (!sessionId) return;
    const state = sessions[sessionId];

    try {
        if (path === lastFetchedPath && lastFetchedLines.length > 0) {
            state.sourceCode = lastFetchedLines;
            if (sessionId === activeSessionId) renderSourceCodeFromState(lastFetchedLines, highlightLine, path);
            return;
        }

        const response = await fetch(`/source?path=${encodeURIComponent(path)}`);
        if (response.ok) {
            const data = await response.json();
            if (data.lines) {
                lastFetchedPath = path;
                lastFetchedLines = data.lines;
                state.sourceCode = data.lines;
                if (sessionId === activeSessionId) renderSourceCodeFromState(data.lines, highlightLine, path);
                return;
            }
        }
        if (editor && sessionId === activeSessionId) editor.setValue(`// Could not load: ${path}`);
    } catch (e) {
        if (editor && sessionId === activeSessionId) editor.setValue(`// Error loading: ${path}`);
    }
}

function clearExecMarkers() {
    if (!editor) return;
    if (currentHighlightLine !== null) {
        editor.removeLineClass(currentHighlightLine, "background", "exec-line");
        editor.removeLineClass(currentHighlightLine, "gutter", "exec-gutter");
        editor.setGutterMarker(currentHighlightLine, "exec-arrow", null);
        currentHighlightLine = null;
    }
}

function renderSourceCodeFromState(lines, highlightLine, path) {
    let mode = 'python';
    if (path) {
        if (path.endsWith('.js') || path.endsWith('.ts')) mode = 'javascript';
        else if (path.endsWith('.rs')) mode = 'rust';
    }

    const codeContent = lines.join('');

    if (editor && editor.getValue() !== codeContent) {
        editor.setValue(codeContent);
    }
    if (editor) {
        editor.setOption("mode", mode);

        // Remove old markers
        clearExecMarkers();

        // Add new highlight (CodeMirror lines are 0-indexed)
        currentHighlightLine = highlightLine - 1;
        editor.addLineClass(currentHighlightLine, "background", "exec-line");
        editor.addLineClass(currentHighlightLine, "gutter", "exec-gutter");

        // Add arrow marker in gutter
        const marker = document.createElement("div");
        marker.className = "exec-arrow-icon";
        marker.innerHTML = "▶";
        editor.setGutterMarker(currentHighlightLine, "exec-arrow", marker);

        // Scroll to line centered
        const t = editor.charCoords({ line: currentHighlightLine, ch: 0 }, "local").top;
        const middleHeight = editor.getScrollerElement().offsetHeight / 2;
        editor.scrollTo(null, t - middleHeight - 5);
    }
}

// Toggle breakpoint on gutter click
function onGutterClick(cm, line) {
    const info = cm.lineInfo(line);
    if (info.gutterMarkers && info.gutterMarkers["breakpoints"]) {
        cm.setGutterMarker(line, "breakpoints", null);
        breakpoints.delete(line);
    } else {
        const marker = document.createElement("div");
        marker.className = "bp-marker";
        marker.innerHTML = "●";
        cm.setGutterMarker(line, "breakpoints", marker);
        breakpoints.add(line);
    }
}

// Start connection when DOM loads
document.addEventListener("DOMContentLoaded", () => {
    editor = CodeMirror(document.getElementById("code-view-container"), {
        value: "// Waiting for debugger to stop on a breakpoint...",
        mode: "python",
        theme: "darcula",
        lineNumbers: true,
        readOnly: "nocursor",
        gutters: ["breakpoints", "exec-arrow", "CodeMirror-linenumbers"],
        styleActiveLine: false
    });

    editor.setSize("100%", "100%");
    editor.on("gutterClick", onGutterClick);

    connectWebSocket();
});
