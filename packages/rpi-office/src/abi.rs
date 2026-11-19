//! One documented fallback for the only raw ABI step the published SDK does not
//! wrap yet: reading the `out` slot of a runtime action.
//!
//! The host repository already ships a `call_action!` macro, but it is
//! unreleased: `rpi-plugin-sdk` 0.26.0 (the version this extension pins) exposes
//! `execute_handler!`, `OwnedString` and `string_handler!` only. When a release
//! carries `call_action!`, delete this module and call the macro directly — the
//! rest of the crate is already `#![forbid(unsafe_code)]`.
#![allow(unsafe_code)]

use rpi_plugin_sdk::callbacks::OwnedString;
use rpi_plugin_sdk::{PluginApi, RuntimeActionId, StbString, StbStringRef};

/// Copy a borrowed ABI string (host-owned for the current callback).
#[cfg(test)]
pub(crate) fn text(value: &StbString) -> String {
    // SAFETY: the caller only passes a string the host lends us for the current
    // callback; reading copies it and retains no buffer.
    unsafe { value.to_string_lossy() }
}

/// Invoke a runtime action and copy its response body.
pub(crate) fn action_text(api: PluginApi, action: RuntimeActionId, args: &str) -> (i32, String) {
    let mut out = StbString::empty();
    let rc = (api.runtime_action)(
        u32::from(action),
        StbStringRef::from_str(args),
        &mut out,
        api.user_data,
    );
    // SAFETY: `out` is an owning string the host produced for this call.
    // `OwnedString` adopts it with the producer-side deallocator and frees it
    // exactly once when this local is dropped.
    let text = unsafe { OwnedString::from_raw(out, Some(api.free_string)) };
    (rc, text.to_string_lossy())
}
