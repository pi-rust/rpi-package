# rpi-run-stats

Live, non-focusing top-right runtime monitor for rpi. Enabled by default.
The panel hides automatically when the terminal is narrower than 50 columns.

- `/stats on` opens the panel; `/stats off` hides it while counting continues.
- `/stats` or `/stats toggle` toggles visibility.
- `/stats position top-left` changes the panel position; the default is `top-right`.
  All nine anchors are supported: `top-left`, `top-center`, `top-right`,
  `left-center`, `center`, `right-center`, `bottom-left`, `bottom-center`, `bottom-right`.
  Visibility and position preferences apply to the current process.
- Rounds count agent runs; steps count provider requests in the current session.
- TTFT measures request dispatch to the first nonempty text, thinking or tool-call delta.
- tok/s measures latest response output tokens divided by total request time, including TTFT.
- TPM is reported token usage from requests completed within the last 60 seconds, including input, output and cache tokens. It is not a provider rate-limit quota or an extrapolated speed.
- Input, output, cache read/write, errors/aborts and USD cost accumulate in the current session. Missing timing/cost is displayed as `—`.

The plugin uses the host's generic declarative `SetStatus` panel interface;
the host does not interpret monitor metrics or know this plugin's identity.
It requires a host with generic panel support. Older hosts do not display the
panel. Headless runs collect data without displaying a panel. Counts start on
load and reset at session start, rather than replaying history.

Build: `cargo build --release -p rpi-run-stats`; install the platform cdylib in `~/.rpi/agent/extensions` and restart rpi.
