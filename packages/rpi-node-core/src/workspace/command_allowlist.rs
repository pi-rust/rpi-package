//! Narrow compatibility bridge for native bounded reads missing from extcore's
//! raw command allowlist. Never trust the host's advertised list on its own.
pub(super) const TIMELINE_READS: &[&str] = &["get_message_summaries", "get_message_window"];

pub(super) fn is_timeline_read(command: &str) -> bool {
    TIMELINE_READS.contains(&command)
}

pub(super) fn allowed_commands() -> impl Iterator<Item = &'static str> {
    let native = rpi_extcore::protocol::SUPPORTED_COMMANDS;
    native.iter().copied().chain(
        TIMELINE_READS
            .iter()
            .copied()
            .filter(move |command| !native.contains(command)),
    )
}
