//! A DLL can be registered again without resetting its statics. Keep the host
//! function table refreshable, but bind each callback to its supplied owner.
//! Registration is not proof that a native reload candidate was committed.
use rpi_plugin_sdk::PluginApi;
use std::{
    cell::RefCell,
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{OnceLock, RwLock},
};

static API: OnceLock<RwLock<Option<PluginApi>>> = OnceLock::new();
thread_local! {
    static SCOPED: RefCell<Option<PluginApi>> = const { RefCell::new(None) };
}
fn api() -> &'static RwLock<Option<PluginApi>> {
    API.get_or_init(|| RwLock::new(None))
}
pub(super) fn install(value: PluginApi) {
    *api().write().unwrap_or_else(|p| p.into_inner()) = Some(value);
}
pub(super) fn snapshot() -> Option<PluginApi> {
    SCOPED
        .with(|scope| *scope.borrow())
        .or_else(|| *api().read().unwrap_or_else(|p| p.into_inner()))
}
/// Resolve explicit callback identity independently of an enclosing callback's
/// TLS. Host functions belong to this native process; only user_data selects
/// its owner. Null preserves compatibility with hosts without command context.
pub(super) fn callback(owner: *mut c_void) -> Option<PluginApi> {
    let mut runtime = (*api().read().unwrap_or_else(|p| p.into_inner()))?;
    if !owner.is_null() {
        runtime.user_data = owner;
    }
    Some(runtime)
}

/// Thread-bound and nestable; do not hold the API lock across foreign calls.
pub(super) struct Scope {
    previous: Option<PluginApi>,
    _thread_bound: PhantomData<Rc<()>>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        SCOPED.with(|scope| *scope.borrow_mut() = self.previous.take());
    }
}
pub(super) fn enter(runtime: Option<PluginApi>) -> Scope {
    Scope {
        previous: SCOPED.with(|scope| scope.replace(runtime)),
        _thread_bound: PhantomData,
    }
}
