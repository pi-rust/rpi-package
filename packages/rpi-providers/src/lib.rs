//! One optional provider package, one native plugin entry, independent identities.
pub mod antigravity;
pub mod deepseek;
pub mod llama_cpp;
pub mod openrouter;
pub mod proxy;
mod registration;
mod transport;

/// The plugin entry. The allow covers the lint that `rpi_plugin_sdk::export_plugin!` expands
/// into, which this crate cannot change. The module keeps the allow off the rest of the crate.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
mod entry {
    use crate::{antigravity, deepseek, llama_cpp, openrouter, proxy};

    rpi_plugin_sdk::export_plugin!(|api| {
        for register in [
            deepseek::register,
            openrouter::register,
            llama_cpp::register,
            proxy::register,
            antigravity::register,
        ] {
            let result = register(api);
            if result != 0 {
                return result;
            }
        }
        0
    });
}
