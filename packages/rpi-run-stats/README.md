# rpi-run-stats

Live runtime monitor for rpi. Disabled by default; enable with `/stats on`.
The compact 24-column monitor uses a reserved sidebar (26 columns including
the separator), rather than covering the chat transcript. The prompt editor
and footer retain their full width. Closing it restores the full-width chat.
The panel hides automatically when the terminal is narrower than 80 columns
or insufficient space remains for chat.

- `/stats on` opens the panel; `/stats off` hides it while counting continues.
- **F8** toggles the monitor without submitting a command or altering the draft.
  Holding it does not repeatedly toggle; modified F8 keys are left unclaimed.
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

Labels: **TPS** (output tokens per second), **TTFT** (time to first token),
**Latency** (request duration), **TPM** (rolling tokens per minute), **In/Out**
(input/output tokens), **Cache R/W** (cache read/write tokens), **Errors**,
**Cost USD**, **Elapsed** (current request elapsed time). Counts use `k` and `M`
abbreviations. **Turns** counts agent runs; **Steps** counts provider requests.

The plugin uses the host's generic declarative `SetStatus` panel interface;
the host does not interpret monitor metrics or know this plugin's identity.
It requires a host with generic panel support. Older hosts do not display the
panel. Headless runs collect data without displaying a panel. Counts start on
load and reset at session start, rather than replaying history.

Build: `cargo build --release -p rpi-run-stats`; install the platform cdylib in `~/.rpi/agent/extensions` and restart rpi.
