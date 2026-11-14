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

pub mod vt;

// Links the pinned libghostty-vt archive the bindings call into.
use pm_ghostty as _;
