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
    /// The page's viewport width in CSS pixels, which gives the page zoom.
    #[serde(default)]
    viewport_width: f64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct ColorArg {
    r: u8,
    g: u8,
    b: u8,
    /// Opacity from 0 to 1, for the selection highlight.
    #[serde(default)]
    a: Option<f32>,
}

/// The page's terminal theme.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct ThemeArg {
    foreground: ColorArg,
    background: ColorArg,
    cursor: ColorArg,
    #[serde(default)]
    palette: Vec<ColorArg>,
    #[serde(default)]
    selection: Option<ColorArg>,
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
    #[serde(default)]
    read_only: bool,
    placement: PlacementArg,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use pm_native_terminal::macos::{NativeTerminal, Placement, TerminalEvent};
    use pm_native_terminal::renderer::Tint;
    use pm_native_terminal::stream::Attach;
    use pm_native_terminal::surface::Colors;
    use pm_native_terminal::theme::Theme;
    use pm_native_terminal::vt::Rgb;
    use serde_json::json;
    use tauri::Manager;

    use super::*;

    thread_local! {
        static TERMINALS: RefCell<HashMap<(String, String), NativeTerminal>> = RefCell::new(HashMap::new());
        /// Each window's terminal theme, for terminals it opens later.
        static THEMES: RefCell<HashMap<String, ThemeArg>> = RefCell::new(HashMap::new());
    }

    /// Web links open from here; `pm:` links name places in the page, which opens them.
    fn is_web_link(href: &str) -> bool {
        href.starts_with("https://") || href.starts_with("http://")
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
                viewport_width: p.viewport_width,
            }
        }
    }

    fn rgb(color: ColorArg) -> Rgb {
        Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }

    fn theme_of(theme: &ThemeArg) -> (Theme, Colors) {
        let background = rgb(theme.background);
        (
            Theme {
                foreground: rgb(theme.foreground),
                background,
                cursor: rgb(theme.cursor),
                palette: theme.palette.iter().copied().map(rgb).collect(),
            },
            Colors {
                background,
                selection: theme.selection.map(|color| Tint {
                    color: rgb(color),
                    alpha: color.a.unwrap_or(1.0).clamp(0.0, 1.0),
                }),
            },
        )
    }

    pub fn theme(window: WebviewWindow, theme: ThemeArg) -> Result<(), String> {
        let label = window.label().to_string();
        window
            .run_on_main_thread(move || {
                THEMES.with(|themes| themes.borrow_mut().insert(label.clone(), theme.clone()));
                let (theme, colors) = theme_of(&theme);
                TERMINALS.with(|terminals| {
                    for ((owner, _), terminal) in terminals.borrow().iter() {
                        if *owner == label {
                            terminal.set_theme(theme.clone(), colors);
                        }
                    }
                });
            })
            .map_err(|error| error.to_string())
    }

    pub fn open(
        window: WebviewWindow,
        request: OpenRequest,
        events: Channel<serde_json::Value>,
    ) -> Result<(), String> {
        let label = window.label().to_string();
        let app = window.app_handle().clone();
        // A raw pointer is not Send, so it crosses to the main thread as an address.
        let parent = window.ns_view().map_err(|error| error.to_string())? as usize;
        let runtime = tauri::async_runtime::handle().inner().clone();
        window
            .run_on_main_thread(move || {
                let key = (label, request.id.clone());
                TERMINALS.with(|terminals| terminals.borrow_mut().remove(&key));
                let id = request.id.clone();
                let sink = events.clone();
                let app = app.clone();
                let zoom_label = key.0.clone();
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
                        request.read_only,
                        move |event| {
                            let message = match event {
                                TerminalEvent::Closed { reason, code } => {
                                    json!({ "type": "closed", "id": id, "reason": reason, "code": code })
                                }
                                TerminalEvent::Online => json!({ "type": "online", "id": id }),
                                TerminalEvent::Link(href) if is_web_link(&href) => {
                                    match crate::open_link(&app, &href) {
                                        Ok(()) => return,
                                        Err(error) => {
                                            eprintln!("could not open {href}: {error}");
                                            json!({
                                                "type": "linkfailed",
                                                "id": id,
                                                "reason": format!("Could not open {href}: {error}"),
                                            })
                                        }
                                    }
                                }
                                TerminalEvent::Link(href) => {
                                    json!({ "type": "link", "id": id, "href": href })
                                }
                                TerminalEvent::Zoom(step) => {
                                    if let Some(window) = app.get_webview_window(&zoom_label) {
                                        if let Err(error) = crate::zoom_window(&window, step) {
                                            eprintln!("could not zoom: {error}");
                                        }
                                    }
                                    return;
                                }
                                TerminalEvent::LinkHover(link) => {
                                    json!({ "type": "hover", "id": id, "link": link })
                                }
                                TerminalEvent::ContextMenu { x, y } => {
                                    json!({ "type": "contextmenu", "id": id, "x": x, "y": y })
                                }
                                TerminalEvent::Command(command) => {
                                    json!({ "type": "command", "id": id, "command": command })
                                }
                            };
                            let _ = sink.send(message);
                        },
                    )
                };
                match opened {
                    Ok(terminal) => {
                        if let Some(theme) = THEMES.with(|themes| themes.borrow().get(&key.0).cloned()) {
                            let (theme, colors) = theme_of(&theme);
                            terminal.set_theme(theme, colors);
                        }
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

    pub fn close_all(window: WebviewWindow) -> Result<(), String> {
        let label = window.label().to_string();
        window
            .run_on_main_thread(move || {
                TERMINALS.with(|terminals| {
                    terminals
                        .borrow_mut()
                        .retain(|(owner, _), _| *owner != label)
                });
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

    pub fn close_all(_: WebviewWindow) -> Result<(), String> {
        Ok(())
    }

    pub fn theme(_: WebviewWindow, _: ThemeArg) -> Result<(), String> {
        Ok(())
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

/// Closes every native terminal in the window, for a page that just loaded
/// and has opened none of them.
#[tauri::command]
pub fn native_terminal_close_all(window: WebviewWindow) -> Result<(), String> {
    platform::close_all(window)
}

/// Sets the terminal theme for every native terminal in the window, now and
/// for ones it opens later.
#[tauri::command]
pub fn native_terminal_theme(window: WebviewWindow, theme: ThemeArg) -> Result<(), String> {
    platform::theme(window, theme)
}
