//! A terminal drawn natively: libghostty-vt holds the screen, and frames of
//! cells come out for a GPU renderer.

#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    unsafe_op_in_unsafe_fn,
    clippy::all
)]
mod ffi {
    include!("ffi_generated.rs");
}

pub mod atlas;
pub mod font;
pub mod input;
pub mod osc;
pub mod pointer;
pub mod preedit;
pub mod protocol;
pub mod renderer;
pub mod stream;
pub mod surface;
pub mod theme;
pub mod vt;

#[cfg(target_os = "macos")]
pub mod macos;

// Links the pinned libghostty-vt archive the bindings call into.
use pm_ghostty as _;
