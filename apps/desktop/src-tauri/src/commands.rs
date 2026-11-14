/// The app's own commands, shared with the build script. Each gets an
/// `allow-` permission, which the connect page's and the controller's
/// capabilities grant.
pub const COMMANDS: &[&str] = &[
    "connect",
    "saved_controller",
    "native_notify",
    "native_badge",
    "open_window",
    "native_terminal_supported",
    "native_terminal_open",
    "native_terminal_place",
    "native_terminal_focus",
    "native_terminal_close",
    "native_terminal_regions",
    "native_terminal_close_all",
    "native_terminal_theme",
];
