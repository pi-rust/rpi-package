//! One optional provider package, one native plugin entry, independent identities.
pub mod antigravity;
pub mod deepseek;
pub mod llama_cpp;
pub mod openrouter;
pub mod proxy;
mod registration;
mod transport;

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
