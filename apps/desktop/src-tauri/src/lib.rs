//! A desktop window onto a Puppet Master controller. The window loads the
//! controller's own web UI, so sign-in, the API and updates work exactly as
//! in a browser, and a small bridge adds what a browser tab cannot do.

mod controller;
mod native;

use std::sync::atomic::{AtomicU64, Ordering};

use tauri::ipc::CapabilityBuilder;
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

const MAIN_WINDOW: &str = "main";
const CONNECT_PAGE: &str = "index.html";
const CHANGE_CONTROLLER: &str = "change-controller";
const DEFAULT_WIDTH: f64 = 1440.0;
const DEFAULT_HEIGHT: f64 = 900.0;
const MIN_WIDTH: f64 = 480.0;
const MIN_HEIGHT: f64 = 360.0;

/// Injected into every page the app loads. The controller's web UI looks for
/// `window.__pmNative` and uses it in place of browser-only behavior. On
/// macOS it also says terminals are drawn natively, so the page knows before
/// showing one and never opens a socket of its own for it.
#[cfg(target_os = "macos")]
const BRIDGE_SCRIPT: &str = concat!(
    "window.__PM_NATIVE_TERMINALS__ = true;\n",
    include_str!("bridge.js")
);
#[cfg(not(target_os = "macos"))]
const BRIDGE_SCRIPT: &str = include_str!("bridge.js");

static EXTRA_WINDOWS: AtomicU64 = AtomicU64::new(0);

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            connect,
            saved_controller,
            native_notify,
            native_badge,
            open_window,
            native::native_terminal_supported,
            native::native_terminal_open,
            native::native_terminal_place,
            native::native_terminal_focus,
            native::native_terminal_close,
            native::native_terminal_regions,
            native::native_terminal_close_all
        ])
        .menu(|app| {
            let menu = Menu::default(app)?;
            let change = MenuItem::with_id(
                app,
                CHANGE_CONTROLLER,
                "Change Controller…",
                true,
                Some("CmdOrCtrl+Shift+K"),
            )?;
            menu.append(&Submenu::with_items(app, "Controller", true, &[&change])?)?;
            Ok(menu)
        })
        .on_menu_event(|app, event| {
            if event.id() == CHANGE_CONTROLLER {
                if let Err(error) = show_connect(app) {
                    eprintln!("could not show the connect page: {error}");
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            match controller::load(&handle) {
                Some(url) => show_controller(&handle, &url)?,
                None => show_connect(&handle)?,
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Puppet Master could not start");
}

/// Every app window: no native title bar on macOS, so the controller UI's own
/// top bar runs to the window's top edge with the traffic lights over it.
fn framed<'a>(
    builder: WebviewWindowBuilder<'a, tauri::Wry, AppHandle>,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    let builder = builder
        .title("Puppet Master")
        .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .initialization_script(BRIDGE_SCRIPT)
        // Tauri's own handler takes dropped files, so the page never sees them.
        .disable_drag_drop_handler();
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    builder
}

/// Builds a framed window and hides its own window buttons, which the page
/// replaces with buttons of its own.
fn build_framed(
    builder: WebviewWindowBuilder<'_, tauri::Wry, AppHandle>,
) -> tauri::Result<WebviewWindow> {
    let window = builder.build()?;
    #[cfg(target_os = "macos")]
    hide_window_buttons(&window);
    Ok(window)
}

#[cfg(target_os = "macos")]
fn hide_window_buttons(window: &WebviewWindow) {
    use objc2_app_kit::{NSWindow, NSWindowButton};
    let Ok(pointer) = window.ns_window() else {
        return;
    };
    // A raw pointer is not Send, so it crosses to the main thread as an address.
    let address = pointer as usize;
    let _ = window.run_on_main_thread(move || {
        let Some(ns_window) = (unsafe { (address as *const NSWindow).as_ref() }) else {
            return;
        };
        for kind in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(button) = ns_window.standardWindowButton(kind) {
                button.setHidden(true);
            }
        }
    });
}

/// The bundled connect page's address, which differs by platform.
fn connect_page_url() -> Url {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let base = "tauri://localhost/";
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let base = "http://tauri.localhost/";
    Url::parse(base)
        .and_then(|base| base.join(CONNECT_PAGE))
        .expect("the connect page address is valid")
}

/// Shows `url` in the main window, navigating the existing window rather
/// than replacing it: a destroyed window lets go of its label only later,
/// so a new one under the same label would be refused.
fn show_in_main(app: &AppHandle, url: Url) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        window.navigate(url)?;
        return Ok(());
    }
    let start = if url == connect_page_url() {
        WebviewUrl::App(CONNECT_PAGE.into())
    } else {
        WebviewUrl::External(url)
    };
    build_framed(framed(WebviewWindowBuilder::new(app, MAIN_WINDOW, start)))?;
    Ok(())
}

fn show_connect(app: &AppHandle) -> tauri::Result<()> {
    show_in_main(app, connect_page_url())
}

/// Lets pages from the controller's origin call the app's commands. Only
/// that origin is granted, so a page it links to cannot reach them. Each
/// origin gets its own capability, so switching controllers adds rather than
/// collides.
fn grant_controller(app: &AppHandle, url: &Url) -> tauri::Result<()> {
    let origin = url.origin().ascii_serialization();
    let id: String = origin
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    app.add_capability(
        CapabilityBuilder::new(format!("controller-{id}"))
            .remote(format!("{origin}/*"))
            .window("*")
            .permission("core:default")
            .permission("core:window:allow-start-dragging")
            .permission("core:window:allow-toggle-maximize")
            .permission("core:window:allow-minimize")
            .permission("core:window:allow-close"),
    )
}

fn show_controller(app: &AppHandle, url: &Url) -> tauri::Result<()> {
    if let Err(error) = grant_controller(app, url) {
        eprintln!("could not grant the controller's origin: {error}");
    }
    show_in_main(app, url.clone())
}

#[tauri::command]
fn saved_controller(app: AppHandle) -> Option<String> {
    controller::load(&app).map(|url| url.to_string())
}

#[tauri::command]
fn connect(app: AppHandle, url: String) -> Result<(), String> {
    let url = controller::parse(&url)?;
    controller::save(&app, &url)
        .map_err(|error| format!("could not save the controller: {error}"))?;
    let handle = app.clone();
    // The connect page asked for this, so it must not be torn down while its
    // own call is still being answered.
    app.run_on_main_thread(move || {
        if let Err(error) = show_controller(&handle, &url) {
            eprintln!("could not open the controller: {error}");
        }
    })
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn native_notify(app: AppHandle, title: String, body: Option<String>) -> Result<(), String> {
    let mut builder = app.notification().builder().title(title);
    if let Some(body) = body {
        builder = builder.body(body);
    }
    builder.show().map_err(|error| error.to_string())
}

#[tauri::command]
fn native_badge(app: AppHandle, count: u32) -> Result<(), String> {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return Ok(());
    };
    let shown = (count > 0).then_some(count);
    #[cfg(target_os = "macos")]
    let result = window.set_badge_label(shown.map(|count| count.to_string()));
    #[cfg(not(target_os = "macos"))]
    let result = window.set_badge_count(shown.map(i64::from));
    result.map_err(|error| error.to_string())
}

/// A page the controller opens in a new tab becomes another app window when
/// it is the controller's own, and opens in the browser otherwise.
#[tauri::command]
fn open_window(app: AppHandle, url: String) -> Result<(), String> {
    let url = Url::parse(&url).map_err(|error| error.to_string())?;
    let same_origin =
        controller::load(&app).is_some_and(|controller| controller.origin() == url.origin());
    if !same_origin {
        return app
            .opener()
            .open_url(url.as_str(), None::<&str>)
            .map_err(|error| error.to_string());
    }
    let label = format!(
        "window-{}",
        EXTRA_WINDOWS.fetch_add(1, Ordering::Relaxed) + 1
    );
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let built = build_framed(framed(WebviewWindowBuilder::new(
            &handle,
            label,
            WebviewUrl::External(url),
        )));
        if let Err(error) = built {
            eprintln!("could not open a window: {error}");
        }
    })
    .map_err(|error| error.to_string())
}
