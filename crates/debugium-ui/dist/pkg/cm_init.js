// CodeMirror 6 JS module — provides the editor that Rust/WASM calls into.
// This runs in the browser as a regular ES module.
// Leptos SourcePanel calls `window.cm_init(container, initialCode)` and
// `window.cm_set_exec_line(lineNum)` via js_sys::Function.

import { EditorState, Compartment } from "https://esm.sh/@codemirror/state@6";
import { EditorView, gutter, GutterMarker, lineNumbers, keymap } from "https://esm.sh/@codemirror/view@6";
import { javascript } from "https://esm.sh/@codemirror/lang-javascript@6";
import { python } from "https://esm.sh/@codemirror/lang-python@6";
import { rust } from "https://esm.sh/@codemirror/lang-rust@6";
import { oneDark } from "https://esm.sh/@codemirror/theme-one-dark@6";
import { syntaxHighlighting, HighlightStyle } from "https://esm.sh/@codemirror/language@6";
import { tags } from "https://esm.sh/@lezer/highlight@1";
import { RangeSetBuilder } from "https://esm.sh/@codemirror/state@6";
import { Decoration, ViewPlugin } from "https://esm.sh/@codemirror/view@6";

// ─── Light theme (VS Code Light style) ──────────────────────────────────────

const lightHighlight = HighlightStyle.define([
    { tag: tags.keyword,        color: "#0000ff" },
    { tag: tags.controlKeyword, color: "#af00db" },
    { tag: tags.operatorKeyword,color: "#0000ff" },
    { tag: tags.definitionKeyword, color: "#0000ff" },
    { tag: tags.typeName,       color: "#267f99" },
    { tag: tags.className,      color: "#267f99" },
    { tag: tags.function(tags.variableName), color: "#795e26" },
    { tag: tags.definition(tags.variableName), color: "#001080" },
    { tag: tags.variableName,   color: "#001080" },
    { tag: tags.propertyName,   color: "#001080" },
    { tag: tags.comment,        color: "#008000", fontStyle: "italic" },
    { tag: tags.lineComment,    color: "#008000", fontStyle: "italic" },
    { tag: tags.blockComment,   color: "#008000", fontStyle: "italic" },
    { tag: tags.string,         color: "#a31515" },
    { tag: tags.number,         color: "#098658" },
    { tag: tags.bool,           color: "#0000ff" },
    { tag: tags.null,           color: "#0000ff" },
    { tag: tags.self,           color: "#0000ff" },
    { tag: tags.atom,           color: "#0000ff" },
    { tag: tags.operator,       color: "#000000" },
    { tag: tags.punctuation,    color: "#000000" },
    { tag: tags.meta,           color: "#267f99" },
    { tag: tags.regexp,         color: "#811f3f" },
    { tag: tags.tagName,        color: "#800000" },
    { tag: tags.attributeName,  color: "#ff0000" },
    { tag: tags.attributeValue, color: "#0000ff" },
    { tag: tags.heading,        color: "#0000ff", fontWeight: "bold" },
]);

const lightEditorTheme = EditorView.theme({
    "&": { backgroundColor: "#ffffff", color: "#1e1e1e" },
    ".cm-gutters": { backgroundColor: "#f0f0f0", color: "#999", borderRight: "1px solid #ddd" },
    ".cm-activeLineGutter": { backgroundColor: "#e8e8e8" },
    ".cm-activeLine": { backgroundColor: "rgba(0,0,0,.04)" },
    ".cm-selectionBackground": { backgroundColor: "#add6ff !important" },
    ".cm-cursor": { borderLeftColor: "#000" },
    ".cm-exec-arrow": { color: "#c89800" },
    ".cm-exec-line": { background: "rgba(255,204,0,0.18) !important" },
}, { dark: false });

const lightTheme = [lightEditorTheme, syntaxHighlighting(lightHighlight)];

// ─── Theme compartment ──────────────────────────────────────────────────────

const themeCompartment = new Compartment();

function isLightMode() {
    return document.documentElement.classList.contains("light-mode");
}

function currentTheme() {
    return isLightMode() ? lightTheme : oneDark;
}

// Watch for light-mode class toggle on <html>
const observer = new MutationObserver(() => {
    if (activeView) {
        activeView.dispatch({ effects: themeCompartment.reconfigure(currentTheme()) });
    }
});
observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });

// ─── Execution marker (yellow arrow) ──────────────────────────────────────

class ExecArrow extends GutterMarker {
    toDOM() {
        const el = document.createElement("span");
        el.textContent = "▶";
        el.className = "cm-exec-arrow";
        return el;
    }
}

const arrowMarker = new ExecArrow();

// Shared mutable state (simple; single editor instance)
let execLineState = { line: -1 }; // 1-indexed, -1 = none

const execArrowGutter = gutter({
    lineMarker(view, line) {
        const lineNo = view.state.doc.lineAt(line.from).number;
        return lineNo === execLineState.line ? arrowMarker : null;
    },
    class: "cm-exec-gutter",
});

// ─── Exec line highlight ───────────────────────────────────────────────────

const execLineMark = Decoration.line({ class: "cm-exec-line" });

const execLinePlugin = ViewPlugin.fromClass(class {
    constructor(view) { this.decorations = this.build(view); }
    update(update) { if (update.docChanged || update.viewportChanged) this.decorations = this.build(update.view); }
    build(view) {
        if (execLineState.line < 1) return Decoration.none;
        const builder = new RangeSetBuilder();
        for (const { from, to } of view.visibleRanges) {
            for (let pos = from; pos <= to;) {
                const line = view.state.doc.lineAt(pos);
                if (line.number === execLineState.line) {
                    builder.add(line.from, line.from, execLineMark);
                }
                pos = line.to + 1;
            }
        }
        return builder.finish();
    }
}, { decorations: v => v.decorations });

// ─── Breakpoint gutter ─────────────────────────────────────────────────────

// Map from line → { condition?: string, logMessage?: string }
const bpSpecs = new Map();

class BpMarker extends GutterMarker {
    constructor(isLogpoint) { super(); this.isLogpoint = isLogpoint; }
    toDOM() {
        const el = document.createElement("span");
        el.textContent = this.isLogpoint ? "◆" : "●";
        el.className = this.isLogpoint ? "cm-lp-marker" : "cm-bp-marker";
        return el;
    }
    eq(other) { return this.isLogpoint === other.isLogpoint; }
}

const bpGutter = gutter({
    lineMarker(view, line) {
        const no = view.state.doc.lineAt(line.from).number;
        if (!bpSpecs.has(no)) return null;
        const spec = bpSpecs.get(no);
        return new BpMarker(!!spec.logMessage);
    },
    domEventHandlers: {
        mousedown(view, line, event) {
            const no = view.state.doc.lineAt(line.from).number;
            if (event.button === 2) {
                // Right-click → open condition popover
                event.preventDefault();
                showBpPopover(no, event.clientX, event.clientY, view);
                return true;
            }
            // Left-click → toggle
            if (bpSpecs.has(no)) bpSpecs.delete(no);
            else bpSpecs.set(no, {});
            view.dispatch({});
            _notifyBpChange();
            return true;
        },
        contextmenu(view, line, event) {
            event.preventDefault();
            return true;
        }
    },
    class: "cm-bp-gutter",
});

function _notifyBpChange() {
    if (window.__cm_on_bp_change) {
        const lines = [...bpSpecs.keys()];
        window.__cm_on_bp_change(lastPath || "", JSON.stringify(lines));
    }
}

// ─── BP Condition popover ──────────────────────────────────────────────────

function showBpPopover(lineNo, x, y, view) {
    // Remove any existing popover
    document.querySelectorAll('.bp-popover').forEach(el => el.remove());

    const spec = bpSpecs.get(lineNo) || {};
    const div = document.createElement('div');
    div.className = 'bp-popover';
    div.style.cssText = `position:fixed;left:${x}px;top:${y}px;z-index:9999`;
    div.innerHTML = `
        <div class="bp-popover-title">Line ${lineNo}</div>
        <label>Condition:<input type="text" class="bp-cond" placeholder="e.g. x > 10" value="${spec.condition || ''}"></label>
        <label>Log message:<input type="text" class="bp-log" placeholder="e.g. step={step}" value="${spec.logMessage || ''}"></label>
        <div class="bp-popover-btns">
            <button class="bp-save">Save</button>
            <button class="bp-cancel">Cancel</button>
        </div>
    `;

    div.querySelector('.bp-save').onclick = () => {
        const cond = div.querySelector('.bp-cond').value.trim();
        const log = div.querySelector('.bp-log').value.trim();
        const newSpec = {};
        if (cond) newSpec.condition = cond;
        if (log) newSpec.logMessage = log;
        bpSpecs.set(lineNo, newSpec);
        view.dispatch({});
        _notifyBpChange();
        div.remove();
    };
    div.querySelector('.bp-cancel').onclick = () => div.remove();

    document.body.appendChild(div);

    // Close on outside click
    setTimeout(() => {
        const close = (e) => { if (!div.contains(e.target)) { div.remove(); document.removeEventListener('mousedown', close); } };
        document.addEventListener('mousedown', close);
    }, 0);
}

// ─── Annotation gutter ─────────────────────────────────────────────────────

const annSpecs = new Map(); // line → { message, color }

class AnnMarker extends GutterMarker {
    constructor(color) { super(); this.color = color || "blue"; }
    toDOM() {
        const el = document.createElement("span");
        el.textContent = "◉";
        el.className = "cm-ann-marker";
        el.style.color = this.color === "red" ? "#e51400"
            : this.color === "yellow" ? "#ffcc00"
            : this.color === "green" ? "#4ec94e"
            : "#4d9de0";
        return el;
    }
    eq(other) { return this.color === other.color; }
}

const annGutter = gutter({
    lineMarker(view, line) {
        const no = view.state.doc.lineAt(line.from).number;
        if (!annSpecs.has(no)) return null;
        return new AnnMarker(annSpecs.get(no).color);
    },
    domEventHandlers: {
        mouseenter(view, line, event) {
            const no = view.state.doc.lineAt(line.from).number;
            if (!annSpecs.has(no)) return false;
            _showAnnTooltip(annSpecs.get(no).message, event.clientX, event.clientY);
            return true;
        },
        mouseleave() { _hideAnnTooltip(); return false; }
    },
    class: "cm-ann-gutter",
});

let _annTooltip = null;
function _showAnnTooltip(msg, x, y) {
    _hideAnnTooltip();
    _annTooltip = document.createElement("div");
    _annTooltip.className = "cm-ann-tooltip";
    _annTooltip.textContent = msg;
    _annTooltip.style.cssText = `position:fixed;left:${x+12}px;top:${y}px;z-index:9999`;
    document.body.appendChild(_annTooltip);
}
function _hideAnnTooltip() {
    if (_annTooltip) { _annTooltip.remove(); _annTooltip = null; }
}

// ─── Language detection ────────────────────────────────────────────────────

function langExt(path) {
    if (!path) return python();
    if (path.endsWith(".js") || path.endsWith(".ts")) return javascript();
    if (path.endsWith(".rs")) return rust();
    return python();
}

// ─── Shared editor instance ────────────────────────────────────────────────

let activeView = null;
let lastPath = null;

function buildExtensions(path) {
    return [
        lineNumbers(),
        bpGutter,
        annGutter,
        execArrowGutter,
        execLinePlugin,
        themeCompartment.of(currentTheme()),
        langExt(path),
        EditorView.editable.of(false),
        EditorView.theme({
            "&": { height: "100%", fontSize: "13px" },
            ".cm-scroller": { overflow: "auto", fontFamily: "'JetBrains Mono', monospace" },
            ".cm-exec-gutter": { width: "18px" },
            ".cm-bp-gutter": { width: "18px" },
            ".cm-ann-gutter": { width: "18px" },
            ".cm-exec-arrow": { color: "#ffcc00" },
            ".cm-bp-marker": { color: "#e51400", cursor: "pointer" },
            ".cm-ann-marker": { cursor: "default" },
            ".cm-exec-line": { background: "rgba(255,204,0,0.1) !important" },
        }),
    ];
}

/**
 * Initialize or replace the CodeMirror editor in `container`.
 * Called from Rust via `window.__cm_init(container, code, path)`.
 */
window.__cm_init = function (container, code, path) {
    if (activeView) {
        activeView.destroy();
        activeView = null;
    }
    lastPath = path || null;
    execLineState.line = -1;
    bpSpecs.clear();

    activeView = new EditorView({
        state: EditorState.create({
            doc: code || "// Waiting for debugger...",
            extensions: buildExtensions(lastPath),
        }),
        parent: container,
    });
};

/**
 * Update the execution arrow + highlighted line without recreating the editor.
 * Called from Rust via `window.__cm_set_exec_line(lineNum)`.
 */
window.__cm_set_exec_line = function (lineNum) {
    execLineState.line = lineNum;
    if (!activeView) return;

    activeView.dispatch({}); // force re-render

    // Scroll the exec line into view
    try {
        const line = activeView.state.doc.line(lineNum);
        activeView.dispatch({
            effects: EditorView.scrollIntoView(line.from, { y: "center" })
        });
    } catch (_) { }
};

/**
 * Update editor content (when file changes).
 */
window.__cm_set_content = function (code, path) {
    if (!activeView) return;
    activeView.setState(EditorState.create({
        doc: code,
        extensions: buildExtensions(path),
    }));
    lastPath = path;
};

/**
 * Push server-confirmed breakpoint lines into the gutter.
 * Called from Rust via `window.__cm_set_breakpoints(linesJson)`.
 */
window.__cm_set_breakpoints = function(linesJson) {
    const lines = JSON.parse(linesJson);
    // Remove stale lines; preserve specs for lines still present
    for (const k of bpSpecs.keys()) {
        if (!lines.includes(k)) bpSpecs.delete(k);
    }
    lines.forEach(n => { if (!bpSpecs.has(n)) bpSpecs.set(n, {}); });
    if (activeView) activeView.dispatch({});
};

/**
 * Push annotations for the current file into the gutter.
 * Called from Rust via `window.__cm_set_annotations(annotationsJson, filePath)`.
 */
window.__cm_set_annotations = function(annotationsJson, filePath) {
    annSpecs.clear();
    try {
        const items = JSON.parse(annotationsJson);
        items.forEach(a => annSpecs.set(a.line, { message: a.message, color: a.color || "blue" }));
    } catch (_) {}
    if (activeView) activeView.dispatch({});
};

console.log("[Debugium] CodeMirror 6 interop loaded");
