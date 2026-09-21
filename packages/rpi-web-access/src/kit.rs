//! Package-local ABI lifecycle helpers.
#![allow(dead_code)]
//!
//! This file is intentionally vendored so the crate can be published and built
//! independently before any other package in this workspace exists on crates.io.

use std::ffi::c_void;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rpi_plugin_sdk::{FreeStringFn, StbString, StepHandle, StepResult, ToolPartialCb};
use serde_json::Value;
use url::Url;

pub type Builder = fn(&Value) -> Result<String, String>;

struct Drive {
    params: Value,
    builder: Builder,
    cancelled: AtomicBool,
    completed: bool,
}

pub fn execute(
    params: StbString,
    free_params: Option<FreeStringFn>,
    builder: Builder,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free_params);
    let params = serde_json::from_str(&text).unwrap_or(Value::Null);
    Box::into_raw(Box::new(Drive {
        params,
        builder,
        cancelled: AtomicBool::new(false),
        completed: false,
    })) as StepHandle
}

pub unsafe fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null package handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("package tool cancelled".into()));
    }
    if drive.completed {
        return StepResult::err(StbString::from_string(
            "package tool polled after completion".into(),
        ));
    }
    drive.completed = true;
    match (drive.builder)(&drive.params) {
        Ok(text) => StepResult::done(StbString::from_string(tool_result(&text))),
        Err(message) => StepResult::err(StbString::from_string(message)),
    }
}

pub unsafe fn cancel(handle: StepHandle) {
    if !handle.is_null() {
        unsafe { &*(handle as *mut Drive) }
            .cancelled
            .store(true, Ordering::SeqCst);
    }
}

pub unsafe fn destroy(handle: StepHandle) {
    if !handle.is_null() {
        unsafe { drop(Box::from_raw(handle as *mut Drive)) };
    }
}

fn tool_result(text: &str) -> String {
    serde_json::json!({"content": [{"type": "text", "text": text}]}).to_string()
}

pub fn schema(name: &str, description: &str, parameters: &str) -> rpi_plugin_sdk::StableToolSchema {
    rpi_plugin_sdk::StableToolSchema {
        name: StbString::from_string(name.to_string()),
        description: StbString::from_string(description.to_string()),
        parameters: StbString::from_string(parameters.to_string()),
    }
}

pub extern "C" fn plugin_free_string(s: StbString) {
    if s.is_empty() || s.ptr.is_null() {
        return;
    }
    unsafe {
        let slice = std::slice::from_raw_parts(s.ptr as *const u8, s.len);
        let _ = Box::from_raw(slice as *const [u8] as *mut [u8]);
    }
}

pub fn string_param(params: &Value, name: &str) -> Result<String, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string parameter `{name}`"))
}

pub fn optional_string(params: &Value, name: &str, default: &str) -> String {
    params
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

pub fn validate_public_url(input: &str) -> Result<Url, String> {
    let url = Url::parse(input).map_err(|err| format!("invalid URL: {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs are allowed".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("local hosts are not allowed".into());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        let blocked = match ip {
            IpAddr::V4(v4) => {
                v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        };
        if blocked {
            return Err(
                "private, loopback, link-local, and unspecified IPs are not allowed".into(),
            );
        }
    }
    Ok(url)
}

pub fn http_client(timeout_secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs.clamp(1, 30)))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.stop();
            }
            match validate_public_url(attempt.url().as_str()) {
                Ok(_) => attempt.follow(),
                Err(message) => attempt.error(message),
            }
        }))
        .user_agent("rpi-web-access/0.1")
        .build()
        .map_err(|err| format!("failed to create HTTP client: {err}"))
}

#[macro_export]
macro_rules! export_single_tool_plugin {
    ($builder:path, $name:literal, $description:literal, $parameters:literal) => {
        extern "C" fn package_execute(
            _: rpi_plugin_sdk::StbStringRef,
            params: rpi_plugin_sdk::StbString,
            free: Option<rpi_plugin_sdk::FreeStringFn>,
        ) -> rpi_plugin_sdk::StepHandle {
            $crate::kit::execute(params, free, $builder)
        }

        extern "C" fn package_poll(
            handle: rpi_plugin_sdk::StepHandle,
            callback: Option<rpi_plugin_sdk::ToolPartialCb>,
            user_data: *mut std::ffi::c_void,
        ) -> rpi_plugin_sdk::StepResult {
            unsafe { $crate::kit::poll(handle, callback, user_data) }
        }

        extern "C" fn package_cancel(handle: rpi_plugin_sdk::StepHandle) {
            unsafe { $crate::kit::cancel(handle) }
        }

        extern "C" fn package_destroy(handle: rpi_plugin_sdk::StepHandle) {
            unsafe { $crate::kit::destroy(handle) }
        }

        #[no_mangle]
        pub extern "C" fn rpi_plugin_register_v2(
            api: *const rpi_plugin_sdk::PluginApiVt,
            abi: u32,
        ) -> i32 {
            unsafe { rpi_plugin_sdk::register_entrypoint(api, abi, |api| {
                let Some(register) = api.register_tool else {
                    return 1;
                };
                let schema = Box::new($crate::kit::schema($name, $description, $parameters));
                let rc = register(
                    &*schema,
                    package_execute,
                    package_poll,
                    package_cancel,
                    package_destroy,
                    $crate::kit::plugin_free_string,
                );
                drop(schema);
                rc
            }) }
        }
    };
}
