//! Native terminals the page asks for, drawn over its own terminal area.
//! They live on the main thread, keyed by window and the page's id.

use serde::Deserialize;
use tauri::ipc::Channel;
use tauri::WebviewWindow;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct PlacementArg {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    cell_width: f64,
    cell_height: f64,
    font_size: f64,
    padding: f64,
    visible: bool,
}

/// A rectangle in page coordinates, in CSS pixels.
#[derive(Clone, Copy, Debug, Deserialize)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct RectArg {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct OpenRequest {
    id: String,
    /// The socket URL with its one-use ticket, minted by the page.
    url: String,
    generation: u64,
    viewer: u64,
    placement: PlacementArg,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use pm_native_terminal::macos::{NativeTerminal, Placement, TerminalEvent};
    use pm_native_terminal::stream::Attach;
    use serde_json::json;

    use super::*;

    thread_local! {
        static TERMINALS: RefCell<HashMap<(String, String), NativeTerminal>> = RefCell::new(HashMap::new());
    }

    impl From<PlacementArg> for Placement {
        fn from(p: PlacementArg) -> Self {
            Placement {
                x: p.x,
                y: p.y,
                width: p.width,
                height: p.height,
                cell_width: p.cell_width,
                cell_height: p.cell_height,
                font_size: p.font_size,
                padding: p.padding,
                visible: p.visible,
            }
        }
    }

    pub fn open(
        window: WebviewWindow,
        request: OpenRequest,
        events: Channel<serde_json::Value>,
    ) -> Result<(), String> {
        let label = window.label().to_string();
        // A raw pointer is not Send, so it crosses to the main thread as an address.
        let parent = window.ns_view().map_err(|error| error.to_string())? as usize;
        let runtime = tauri::async_runtime::handle().inner().clone();
        window
            .run_on_main_thread(move || {
                let key = (label, request.id.clone());
                TERMINALS.with(|terminals| terminals.borrow_mut().remove(&key));
                let id = request.id.clone();
                let sink = events.clone();
                let opened = unsafe {
                    NativeTerminal::open(
                        parent as *mut std::ffi::c_void,
                        &runtime,
                        Attach {
                            url: request.url,
                            generation: request.generation,
                            viewer: request.viewer,
                            claim: None,
                        },
                        request.placement.into(),
                        move |event| {
                            let message = match event {
                                TerminalEvent::Closed(reason) => {
                                    json!({ "type": "closed", "id": id, "reason": reason })
                                }
                                TerminalEvent::Link(href) => {
                                    json!({ "type": "link", "id": id, "href": href })
                                }
                                TerminalEvent::ContextMenu { x, y } => {
                                    json!({ "type": "contextmenu", "id": id, "x": x, "y": y })
                                }
                            };
                            let _ = sink.send(message);
                        },
                    )
                };
                match opened {
                    Ok(terminal) => {
                        TERMINALS.with(|terminals| terminals.borrow_mut().insert(key, terminal));
                    }
                    Err(reason) => {
                        let _ = events
                            .send(json!({ "type": "failed", "id": request.id, "reason": reason }));
                    }
                }
            })
            .map_err(|error| error.to_string())
    }

    fn with_terminal(
        window: &WebviewWindow,
        id: String,
        act: impl FnOnce(&NativeTerminal) + Send + 'static,
    ) -> Result<(), String> {
        let key = (window.label().to_string(), id);
        window
            .run_on_main_thread(move || {
                TERMINALS.with(|terminals| {
                    if let Some(terminal) = terminals.borrow().get(&key) {
                        act(terminal);
                    }
                });
            })
            .map_err(|error| error.to_string())
    }

    pub fn place(window: WebviewWindow, id: String, placement: PlacementArg) -> Result<(), String> {
        with_terminal(&window, id, move |terminal| {
            terminal.place(placement.into())
        })
    }

    pub fn focus(window: WebviewWindow, id: String) -> Result<(), String> {
        with_terminal(&window, id, |terminal| terminal.focus())
    }

    fn rects(rects: &[RectArg]) -> Vec<NSRect> {
        rects
            .iter()
            .map(|r| NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.width, r.height)))
            .collect()
    }

    pub fn regions(
        window: WebviewWindow,
        id: String,
        holes: Vec<RectArg>,
        passthrough: Vec<RectArg>,
    ) -> Result<(), String> {
        with_terminal(&window, id, move |terminal| {
            terminal.set_regions(&rects(&holes), &rects(&passthrough))
        })
    }

    pub fn close(window: WebviewWindow, id: String) -> Result<(), String> {
        let key = (window.label().to_string(), id);
        window
            .run_on_main_thread(move || {
                TERMINALS.with(|terminals| terminals.borrow_mut().remove(&key));
            })
            .map_err(|error| error.to_string())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    const UNSUPPORTED: &str = "native terminals are drawn on macOS only";

    pub fn open(
        _: WebviewWindow,
        _: OpenRequest,
        _: Channel<serde_json::Value>,
    ) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn place(_: WebviewWindow, _: String, _: PlacementArg) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn focus(_: WebviewWindow, _: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn regions(
        _: WebviewWindow,
        _: String,
        _: Vec<RectArg>,
        _: Vec<RectArg>,
    ) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn close(_: WebviewWindow, _: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

/// Whether this build draws terminals natively, so the page knows to ask.
#[tauri::command]
pub fn native_terminal_supported() -> bool {
    cfg!(target_os = "macos")
}

#[tauri::command]
pub fn native_terminal_open(
    window: WebviewWindow,
    request: OpenRequest,
    on_event: Channel<serde_json::Value>,
) -> Result<(), String> {
    platform::open(window, request, on_event)
}

#[tauri::command]
pub fn native_terminal_place(
    window: WebviewWindow,
    id: String,
    placement: PlacementArg,
) -> Result<(), String> {
    platform::place(window, id, placement)
}

#[tauri::command]
pub fn native_terminal_focus(window: WebviewWindow, id: String) -> Result<(), String> {
    platform::focus(window, id)
}

#[tauri::command]
pub fn native_terminal_close(window: WebviewWindow, id: String) -> Result<(), String> {
    platform::close(window, id)
}

/// Page content over the terminal: `holes` show through and take the mouse,
/// `passthrough` only takes the mouse.
#[tauri::command]
pub fn native_terminal_regions(
    window: WebviewWindow,
    id: String,
    holes: Vec<RectArg>,
    passthrough: Vec<RectArg>,
) -> Result<(), String> {
    platform::regions(window, id, holes, passthrough)
}
