//! A desktop window onto a Puppet Master controller. The window loads the
//! controller's own web UI, so sign-in, the API and updates work exactly as
//! in a browser, and a small bridge adds what a browser tab cannot do.

mod controller;

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
/// `window.__pmNative` and uses it in place of browser-only behavior.
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
            open_window
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

fn main_builder<'a>(
    app: &'a AppHandle,
    url: WebviewUrl,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    WebviewWindowBuilder::new(app, MAIN_WINDOW, url)
        .title("Puppet Master")
        .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
}

fn replace_main(app: &AppHandle, url: WebviewUrl) -> tauri::Result<WebviewWindow> {
    if let Some(existing) = app.get_webview_window(MAIN_WINDOW) {
        existing.destroy()?;
    }
    main_builder(app, url)
        .initialization_script(BRIDGE_SCRIPT)
        .build()
}

fn show_connect(app: &AppHandle) -> tauri::Result<()> {
    replace_main(app, WebviewUrl::App(CONNECT_PAGE.into()))?;
    Ok(())
}

/// Lets pages from the controller's origin call the app's commands. Only
/// that origin is granted, so a page it links to cannot reach them.
fn grant_controller(app: &AppHandle, url: &Url) -> tauri::Result<()> {
    let origin = url.origin().ascii_serialization();
    app.add_capability(
        CapabilityBuilder::new("controller")
            .remote(format!("{origin}/*"))
            .window("*")
            .permission("core:default"),
    )
}

fn show_controller(app: &AppHandle, url: &Url) -> tauri::Result<()> {
    grant_controller(app, url)?;
    replace_main(app, WebviewUrl::External(url.clone()))?;
    Ok(())
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
        let built = WebviewWindowBuilder::new(&handle, label, WebviewUrl::External(url))
            .title("Puppet Master")
            .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
            .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
            .initialization_script(BRIDGE_SCRIPT)
            .build();
        if let Err(error) = built {
            eprintln!("could not open a window: {error}");
        }
    })
    .map_err(|error| error.to_string())
}
