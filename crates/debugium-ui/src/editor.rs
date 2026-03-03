use wasm_bindgen::prelude::*;
use web_sys::HtmlElement;

#[wasm_bindgen]
extern "C" {
    /// window.__cm_init(container, code, path)
    #[wasm_bindgen(js_namespace = window, js_name = __cm_init)]
    fn cm_init(container: &HtmlElement, code: &str, path: &str);

    /// window.__cm_set_exec_line(lineNum)
    #[wasm_bindgen(js_namespace = window, js_name = __cm_set_exec_line)]
    fn cm_set_exec_line(line: u32);

    /// window.__cm_set_content(code, path)
    #[wasm_bindgen(js_namespace = window, js_name = __cm_set_content)]
    fn cm_set_content(code: &str, path: &str);

    /// window.__cm_set_breakpoints(linesJson)
    #[wasm_bindgen(js_namespace = window, js_name = __cm_set_breakpoints)]
    fn cm_set_breakpoints(lines_json: &str);

    /// window.__cm_set_annotations(annotationsJson, filePath)
    #[wasm_bindgen(js_namespace = window, js_name = __cm_set_annotations)]
    fn cm_set_annotations(annotations_json: &str, file_path: &str);

    /// window.__cm_set_inline_values(json)
    #[wasm_bindgen(js_namespace = window, js_name = __cm_set_inline_values)]
    fn cm_set_inline_values(json: &str);
}

/// Initialize the CodeMirror editor inside the given DOM element.
pub fn init_editor(container: &HtmlElement, code: &str, path: &str) {
    cm_init(container, code, path);
}

/// Update the execution indicator to point at `line` (1-indexed).
pub fn set_exec_line(line: u32) {
    cm_set_exec_line(line);
}

/// Replace the editor content (e.g. when stepping to a different file).
pub fn set_content(code: &str, path: &str) {
    cm_set_content(code, path);
}

/// Push server-confirmed breakpoint lines into the gutter.
pub fn set_breakpoints(lines_json: &str) {
    cm_set_breakpoints(lines_json);
}

/// Push annotations for the current file into the gutter.
pub fn set_annotations(annotations_json: &str, file_path: &str) {
    cm_set_annotations(annotations_json, file_path);
}

/// Show inline variable values at specific lines in the editor.
pub fn set_inline_values(json: &str) {
    cm_set_inline_values(json);
}
