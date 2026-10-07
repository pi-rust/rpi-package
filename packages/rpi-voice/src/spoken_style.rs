//! Spoken-style system prompt — injected only while replies are being read
//! aloud.
//!
//! ## Why this exists
//!
//! Text replies are read; spoken replies are *heard*. The two want different
//! writing: a reply that scans fine on screen is often unbearable when
//! synthesised — nested clauses, tables, file paths and tool names all become
//! noise a listener cannot re-read or skim.
//!
//! So when Auto-TTS is on, the plugin appends a short style section to the
//! system prompt for that turn, asking for a spoken register: acknowledge the
//! request first, say what is about to be done before doing it, and use a
//! human tone.
//!
//! ## Why the system prompt and not a file
//!
//! A global `APPEND_SYSTEM.md` would apply to every session from the moment it
//! is written until the file is deleted — including purely typed conversation,
//! where the style is unnecessary. This hook is asked **once per turn**, so the
//! section is present exactly while `/voice on` is in effect and disappears the
//! moment it is turned off, with nothing to clean up and no file to remember.
//!
//! ## Scope
//!
//! The hook is `before_agent_start`, whose contract is **replacement**: the
//! returned text becomes the prompt. This module therefore reads the prompt the
//! host hands over and returns prompt + section, and returns "no change" (an
//! empty object) when speech is off — so a session without voice is untouched.

use serde_json::{json, Value};
use std::sync::atomic::Ordering;

use rpi_plugin_sdk::{StbString, StbStringRef};

/// Heading the section is introduced by. Distinctive so a reader who sees it in
/// a transcript can tell where it came from.
const SECTION_HEADING: &str = "## How to speak your replies";

/// The spoken-style guidance.
///
/// Kept short on purpose: every line here is paid for on every turn of every
/// spoken session, and a long prompt section gets skimmed rather than followed.
/// The rules are ordered by how much they change the listening experience —
/// immediate acknowledgement first, since that is what makes a spoken
/// conversation feel responsive.
pub const SPOKEN_STYLE: &str = "\
The user is listening to your reply through text-to-speech, not reading it.
Write for the ear:

1. **Acknowledge first.** Open with a short reaction before anything else —
   \"Got it.\" / \"Okay, let me look.\" — so the user hears a response
   immediately instead of waiting through silence.
2. **Announce before you act.** Before running a tool, say in one sentence what
   you are about to do and why. After it, say in one sentence what you found.
   Never let a long silence pass unexplained.
3. **Sound like a person.** Contractions, everyday words, the occasional
   \"hmm\", \"oh\", \"actually\". If something is genuinely funny, laugh — \"ha,
   that's great.\" If you get something wrong, just say so. Do not overdo it: a
   laugh in every sentence is worse than none.
4. **Short sentences.** Lead with the answer, then expand only if it is
   actually needed. Speak the conclusion, not the reasoning.
5. **Nothing unpronounceable.** No tool names, file paths, JSON, code or URLs
   read literally — describe them instead (\"I read that file\", not \"I called
   read on D:\\...\\foo.rs\"). No tables, no bullet-point lists read verbatim,
   no markdown syntax.";

/// The section as it is appended, with the heading.
fn section() -> String {
    format!("\n\n{SECTION_HEADING}\n\n{SPOKEN_STYLE}\n")
}

/// Build the prompt to install for this turn, or `None` to leave it untouched.
///
/// `base` is the prompt the run would otherwise use; `enabled` is whether
/// replies are currently being spoken. Returns `Some(base + section)` when
/// enabled, `None` when not — the "no change" signal, which leaves a session
/// without voice byte-identical to one that never loaded this plugin.
///
/// Idempotent: a base that already carries the section is returned unchanged,
/// so two registrations (or a chained handler that already appended it) cannot
/// stack the text.
pub fn prompt_for_turn(base: &str, enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    if has_spoken_style(base) {
        return Some(base.to_string());
    }
    Some(format!("{base}{}", section()))
}

/// Whether a system prompt already carries the spoken-style section.
///
/// The idempotence guard in [`prompt_for_turn`], and the predicate the tests
/// assert against.
pub fn has_spoken_style(prompt: &str) -> bool {
    prompt.contains(SECTION_HEADING)
}

/// `before_agent_start` handler: append the spoken-style section while speech is
/// on.
///
/// Writes `{"systemPrompt": "..."}` to `out` when it changed the prompt, and an
/// empty object (⇒ no change) when it did not. Returns `0` on success; the host
/// treats a nonzero return as a handled error and leaves the prompt alone.
///
/// Reads the current prompt from the event envelope (`systemPrompt`), because
/// the contract is replacement — a handler that wants to append must do it
/// itself. A malformed or missing envelope is treated as an empty base rather
/// than a failure: losing the host's base prompt is worse than not appending, so
/// in that case this returns "no change".
extern "C" fn on_before_agent_start(
    event_json: StbStringRef,
    out: *mut StbString,
    _user_data: *mut std::ffi::c_void,
) -> i32 {
    let raw = unsafe { event_json.as_str() };
    let enabled = super::AUTO_TTS_ENABLED.load(Ordering::Relaxed);

    // Nothing to say, and the cheapest exit — most turns in a typed session.
    if !enabled && !super::pet::enabled() {
        write_out(out, json!({}));
        return 0;
    }

    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            // Without a usable envelope there is no base to append to. Decline
            // rather than replacing the host's prompt with our section alone.
            super::debug_log("spoken style: unparseable before_agent_start envelope");
            write_out(out, json!({}));
            return 0;
        }
    };
    let base = parsed
        .get("systemPrompt")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let spoken = prompt_for_turn(base, enabled);
    let next = super::pet::prompt(spoken.as_deref().unwrap_or(base)).or(spoken);
    match next {
        Some(next) => {
            super::debug_log(&format!(
                "spoken style: appended {} chars (prompt {} -> {})",
                next.len().saturating_sub(base.len()),
                base.len(),
                next.len()
            ));
            write_out(out, json!({ "systemPrompt": next }));
        }
        None => write_out(out, json!({})),
    }
    0
}

/// Write the handler's payload into the host's out-slot.
fn write_out(out: *mut StbString, value: Value) {
    if !out.is_null() {
        unsafe {
            *out = StbString::from_string(value.to_string());
        }
    }
}

/// Reclaim a [`StbString`] this plugin produced.
///
/// The host calls this to free the `{"systemPrompt": ...}` payload we hand back
/// — it is the plugin's allocation, so only the plugin can release it. The same
/// function [`crate::set_output`] relies on via the SDK's own free slot.
extern "C" fn free_string(s: StbString) {
    if !s.is_empty() && !s.ptr.is_null() {
        // SAFETY: produced by `StbString::from_string` (a `Box<[u8]>`); ptr+len
        // reconstruct that same allocation.
        unsafe {
            let slice = std::slice::from_raw_parts(s.ptr as *const u8, s.len);
            let _ = Box::from_raw(slice as *const [u8] as *mut [u8]);
        }
    }
}

/// Register the `before_agent_start` transformer, if the host offers the slot.
///
/// A host that predates the slot leaves it `None`; skipping the call then means
/// the prompt is simply never modified, which is that host's behaviour anyway.
///
/// Returns whether the handler was registered, so the caller can say so in the
/// debug log — a plugin that silently does nothing on an older host is hard to
/// diagnose from a transcript alone.
pub fn register(api: &rpi_plugin_sdk::PluginApi) -> bool {
    let Some(register) = api.register_before_agent_start else {
        return false;
    };
    register(on_before_agent_start, free_string, api.user_data) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_speech_never_touches_the_prompt() {
        // The whole point of the per-turn hook: a typed session must be
        // byte-identical to one without this plugin.
        assert_eq!(prompt_for_turn("BASE", false), None);
    }

    #[test]
    fn enabled_speech_appends_the_section_to_the_base() {
        let out = prompt_for_turn("BASE", true).expect("changes the prompt");
        assert!(out.starts_with("BASE"), "base must come first: {out}");
        assert!(out.contains(SECTION_HEADING));
        assert!(out.contains("Acknowledge first"));
    }

    #[test]
    fn appending_twice_does_not_stack_the_section() {
        // Guards two registrations, or a chained handler that already appended.
        let once = prompt_for_turn("BASE", true).unwrap();
        let twice = prompt_for_turn(&once, true).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn an_empty_base_still_yields_a_usable_prompt() {
        let out = prompt_for_turn("", true).expect("changes the prompt");
        assert!(has_spoken_style(&out));
    }

    /// Drive the real FFI handler the way the host does.
    fn call(event: &str) -> String {
        let event_ref = StbStringRef::from_str(event);
        let mut out = StbString::empty();
        let rc = on_before_agent_start(event_ref, &mut out, std::ptr::null_mut());
        assert_eq!(rc, 0);
        let text = out.to_string_lossy();
        assert!(
            text.starts_with("{\"systemPrompt\"") || text.starts_with("{}"),
            "unexpected payload: {text}"
        );
        text
    }

    #[test]
    fn handler_declines_when_speech_is_off() {
        // Serialize against other tests touching the process-global flag.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::super::AUTO_TTS_ENABLED.store(false, Ordering::Relaxed);
        let payload = call(r#"{"prompt":"hi","systemPrompt":"BASE"}"#);
        assert_eq!(payload, "{}", "speech off must report no change");
    }

    #[test]
    fn handler_appends_when_speech_is_on() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::super::AUTO_TTS_ENABLED.store(true, Ordering::Relaxed);
        let payload = call(r#"{"prompt":"hi","systemPrompt":"BASE"}"#);
        let v: Value = serde_json::from_str(&payload).unwrap();
        let prompt = v["systemPrompt"].as_str().expect("systemPrompt present");
        assert!(prompt.starts_with("BASE"));
        assert!(has_spoken_style(prompt));
        super::super::AUTO_TTS_ENABLED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn a_malformed_envelope_declines_instead_of_dropping_the_base() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::super::AUTO_TTS_ENABLED.store(true, Ordering::Relaxed);
        // No base to append to: must not replace the host's prompt with ours.
        assert_eq!(call("not json"), "{}");
        super::super::AUTO_TTS_ENABLED.store(false, Ordering::Relaxed);
    }
}
