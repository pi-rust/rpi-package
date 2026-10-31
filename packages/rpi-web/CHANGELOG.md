# Changelog

## [Unreleased]

### Added

- Add a tiny left-side conversation rail for loaded user messages: hold and drag vertically with a mouse or finger to switch questions in real time, scroll with a wheel to browse without navigating, hover/focus for a single localized preview, and click/tap or use keyboard arrows plus Enter to navigate. Held drags scroll long rails at their edges; release, Escape, pointer cancellation and session changes stop cleanly without a synthetic-click jump back. Keep active reading markers, mobile containment, image/attachment grouping and upward history paging without a full-session scan or extra timeline RPCs. Show even a single loaded question, and retain an ↑ history button when a tool-only initial page has no questions; explicit clicks reuse the existing bounded page loader.
- Add Settings → Tool names to switch between exact native names (the default) and mapped/localized action labels across all tool cards. Save the choice locally, preserve expansion, targets, details and timings, and leave native calls and transcript data unchanged.
- Show compact translated interruption/resume hints in desktop and mobile session lists from existing error and settlement metadata, with accessible explanations and no per-session RPC reads. Active retries suppress old errors; normal stops, declined operations, and offline/closed sessions without evidence are not mislabeled. This does not add persistent crash detection for registry-only offline sessions.
- Render fenced `mermaid` flowcharts (`flowchart`/`graph`) and sequence diagrams locally as zoomable SVG images, with source/copy controls, theme updates, mobile containment and debounced streaming recovery. Lazy-load the pinned Mermaid dependency; reject authored configuration, shape metadata, HTML/images and custom CSS before rendering, retaining exact source for invalid, unsupported or oversized diagrams.
- Show Codex-style total elapsed time after completed conversation runs, including stopped/failed runs, in both live and historical views. Use matching native operation start/finish records through a bounded, resumable core/HTTP query; never estimate from individual tool times or browser clocks. Requires the matching backend update; unavailable or incomplete timing metadata is omitted.
- Open transcript and Markdown images in an accessible, keyboard-operated viewer with fit-to-window and scrollable original-size modes.
- Load earlier transcript messages automatically when scrolling near the top, using smaller 48-entry pages with request deduplication and stable reading position, including late-loading images.
- Present tool calls as compact Codex-style activity summaries with paths, commands, change counts and timings; reveal code, outputs, images, diffs and structured plans with one click or keyboard activation. Place tool/status icons on the left and the expand/collapse chevron at the far right. Preserve expansion through live updates without jumping to the bottom.
- Preview long shell commands as four rendered lines inside tool details, with a separate localized expand/collapse control. Keep short commands uncollapsed and copy the complete original command even from the preview.
- Display uploaded documents as compact filename/type cards instead of UUID-prefixed internal paths; expand to inspect or copy the unchanged upload path. Preserve inline user commentary and show known file sizes and image previews before sending.

### Fixed

- Show an icon-only composer Continue action with a localized tooltip and accessible name for native pending recovery or explicit failed/interrupted/manually stopped run settlements. Acknowledging Stop alone does not expose Continue before native settlement. Use native resume when available, otherwise send an explicit continuation instruction only on user action; preserve draft/attachment Send and running Stop, reject duplicate/stale submissions, and keep recovery available after notice dismissal or a rejected request without inventing work from ordinary network errors.
- Start the right-side project files/changes panel collapsed at every viewport width and after reload; retain manual toolbar toggling and the panel's close button.
- Keep timeline ticks clearly visible without activation and show up to ten at once, scrolling excess nodes. Load one bounded 48-message page when the reader scrolls to the earlier edge, releases an upper-edge scrub, or presses Up/Home again on the first node. Preserve both reading anchors, manual ↑ retry and legacy/loading visibility; guard wheel bursts, failures, stale sessions and tool-only pages against repeated reads or full-history scans.
- Move the conversation timeline to the left on desktop and phones, reserving left-side message space and opening previews inward to the right. Preserve narrow sizing, keyboard/mouse/touch navigation, hover-preview access and bounded history paging.
- Prioritize the bounded conversation snapshot on session switch: restore history, native lifecycle state and SSE before loading runtime metadata and catalogs. Defer and serialize diagnostic reads, abort superseded browser requests, ignore stale session/event callbacks, avoid a duplicate state read from old settlement ticks and leave active-session re-selection unchanged. Preserve legacy status fallback, read-only local slash commands, authentication and snapshot/replay boundaries; slow or failed auxiliary metadata no longer blocks an authoritative snapshot.
- Distinguish summary-only todo results from empty task lists: retain progress such as 4/4, explain that task details were not returned, and omit misleading “0/N listed” text and empty checklists. Actual empty lists keep their own message; old lists are not inferred or backfilled as current task state.
- Render `delegate_task` history and live results as read-only task/report cards, with Markdown reports, roles, child status, turns used and explicit turn-limit warnings. Fold execution metadata and original JSON away, retain readable progress/errors and actual call timings, and preserve live expansion without changing native child lifecycle, task execution or history.
- Collapse consecutive todo updates into the latest card, with previous calls available in a closed earlier-updates disclosure instead of repeated progress cards. Keep errors, active calls and native run footers visible; preserve expansion and reading anchors through live updates and older-page prepends without modifying task state/history or issuing new reads.
- Render ask tool history as read-only structured question/answer cards rather than raw JSON, including option descriptions, selections, comments, free text and multi-question ID matching. Keep confirmations, cancellation and errors distinct, preserve native summaries and call timings, and hide raw payloads behind an opt-in disclosure without changing the live answering flow or native history.
- Preserve todo progress from the native result header when completed items are filtered, instead of recounting only listed tasks (for example, keep 6/7 rather than 0/1). Explain partial lists, retain in-progress markers, support header-only/empty and legacy checklist results, and distinguish a completed todo operation from completed tasks without changing native state or call timings.
- Omit generic successful-completion chat notes (Run completed / 运行状态：已完成) while keeping meaningful summaries, error/stopped states, elapsed footers and native settlement/history behavior.
- Omit the redundant visible Queue label above pending composer messages while preserving the count, preview, item actions and localized accessible expand/collapse names.
- Use equal user-message corner radii and show the localized sender label only once instead of repeating it in both avatar and name.
- Render inline Markdown `data:image` sources without allowing data URLs in links or raw HTML.
- Preserve user image blocks in snapshots, reloads and live message events, including image-only messages, without duplicating start/end rows.
- Resolve local raster image references against the active project and serve authenticated image bytes through the project API instead of receiving the Web/Vite SPA HTML fallback. Requires the matching backend update.

## [0.1.21](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.20...rpi-web-v0.1.21) - 2026-10-29

### Fixed

- *(web)* keep project archive actions visible without hover

## [0.1.20](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.19...rpi-web-v0.1.20) - 2026-10-29

### Added

- *(web)* add permanent session deletion
- *(web)* add project archiving

### Fixed

- *(web)* restore archived chats and unblock production regression checks
- *(web)* preview image attachments without mime type
- *(web)* withdraw queued message when editing

### Other

- Merge main into feature-1028 and preserve canonical durable recovery
- Merge remote-tracking branch 'origin/feat/project-archive' into HEAD
- *(web)* align e2e expectations with queue UI

## [0.1.19](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.18...rpi-web-v0.1.19) - 2026-10-29

### Other

- updated the following local packages: rpi-extcore, rpi-http

## [0.1.18](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.17...rpi-web-v0.1.18) - 2026-10-28

### Other

- updated the following local packages: rpi-extcore, rpi-http

## [0.1.17](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.16...rpi-web-v0.1.17) - 2026-10-28

### Other

- updated the following local packages: rpi-extcore, rpi-http

## [0.1.16](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.15...rpi-web-v0.1.16) - 2026-10-28

### Other

- *(transcript)* reduce session snapshot and branch polling cost

## [0.1.15](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.14...rpi-web-v0.1.15) - 2026-10-28

### Other

- *(web)* align e2e expectations with queue UI ([#95](https://github.com/pi-rust/rpi-package/pull/95))

## [0.1.14](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.13...rpi-web-v0.1.14) - 2026-10-28

### Fixed

- *(web)* add send-now queue action and expand projects ([#92](https://github.com/pi-rust/rpi-package/pull/92))

## [0.1.13](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.12...rpi-web-v0.1.13) - 2026-10-28

### Other

- updated the following local packages: rpi-extcore, rpi-http

## [0.1.12](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.11...rpi-web-v0.1.12) - 2026-10-26

### Fixed

- *(web)* deduplicate restored settlement notes

### Other

- Merge pull request #81 from pi-rust/fix/web-settlement-dedupe

## [0.1.11](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.10...rpi-web-v0.1.11) - 2026-10-26

### Added

- *(web)* refine workspace composer and tool output

## [0.1.10](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.9...rpi-web-v0.1.10) - 2026-10-25

### Added

- *(durable)* add branch-backed durable runtime

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.8...rpi-web-v0.1.9) - 2026-10-25

### Other

- *(snapshot)* parallelize messages and history requests
- add architecture boundary guidance

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.7...rpi-web-v0.1.8) - 2026-10-24

### Fixed

- *(web)* improve session loading and workspace UI

### Other

- extract shared extension core and HTTP adapter

### Fixed

- Show the session's project directory and Git branch beside the composer, with linked-worktree detection and compact path truncation on narrow screens.
- Load the latest 80 messages for the initial snapshot and fetch model catalogs after history, while retaining native pagination for earlier messages.
- Return native structured `ask_user` answers, including multiple choices and comments, without changing permission dialog responses.
- Keep the mobile composer at its intended 14px corner radius instead of inheriting the desktop's 17px radius.
- Use square corners for fenced code blocks and remove the nested preformatted border that doubled the frame and heading separator.
- Restore token and cache metrics from the latest historical response when legacy session totals are unavailable, retain them across diagnostic refreshes, and derive input/output counts from native aggregate statistics.
- Keep the composer footer on one line with input, output, cache hit rate and context usage/capacity; move total/cached tokens and secondary metrics into the expandable details.

## [0.1.7] - 2026-10-24

### Changed

- Prefer `rpi --webserver` for starting the Web server, including `=init`, `=help`, explicit configuration paths, and host `--port` / `--bind` overrides. Keep `--web` as a compatibility alias with the same configuration and session storage.
- Reject different values supplied to `--webserver` and `--web` together, and retain service-entry conflict checks and inactive RPC child behavior.
- Document how React/Vite assets are built, embedded in the extension, and served without a separate frontend process.

### Fixed

- Allow the generated, ignored frontend bundle to be included during crate publication, fixing the release failure caused by Cargo's dirty-file check.

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.5...rpi-web-v0.1.6) - 2026-10-23

### Added

- *(e2e)* mock stats data and update shell card assertions
- *(rpi-web)* add MenuSelect component and background process helper

### Fixed

- *(web)* restore desktop chrome after the mobile responsive pass
- *(rpi-web)* prevent console windows in background processes
- fix queued message editing

### Other

- *(web)* note the desktop chrome and toolbar fixes in the changelog
- *(web)* stop tracking generated frontend bundle
- *(release)* bump workspace to 0.1.9
- *(rpi-web)* add session tags and sidebar filters`

### Fixed

- Restore the desktop workspace chrome after the responsive pass: keep the rounded buttons, cards, popovers and composer instead of the global square-corner override, and restore the missing `--radius` token.
- Hide the mobile-only session switcher and its backdrop on desktop, and keep the connection status chip in the toolbar.
- Keep the model and thinking (provider) pickers in the composer only instead of duplicating them in the top-right toolbar.
- Prevent background RPC, Git and authentication refresh processes from opening console windows on Windows while preserving their standard I/O and shutdown behavior.

### Added

- Add durable session tags, server-side session list filters, and sidebar tag/status filtering.
- Add a session tag editor with bounded, normalized tag persistence.

### Notes

- Session search currently indexes project names, session titles, IDs, and tags without opening offline native hosts. Transcript full-text search remains a follow-up feature.

## [0.1.5] - 2026-10-21

### Changed

- Render shell tool calls as terminal cards: the command stays on one summary line with the elapsed time, the full command is shown with a copy button, and output is displayed as cleaned plain text (ANSI colours removed) instead of JSON. Long commands and output stay contained on narrow screens.

### Added

- Accept `rpi --web --port N` and `rpi --web --bind ADDR`: the extension reads the host's parsed CLI flags and applies them after every configuration file, matching the standalone `--port` / `--bind` behavior.
- Read global Web settings from `~/.rpi/agent/web.json`, honoring `RPI_CODING_AGENT_DIR`, so Web configuration lives beside the host's other agent files. A legacy `~/.rpi/web.json` is still read first and overridden by the agent-dir file.

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.3...rpi-web-v0.1.4) - 2026-10-21

### Added

- *(web)* show a terminal server status page after startup ([#49](https://github.com/pi-rust/rpi-package/pull/49))

- Display a terminal Web server status page after binding, with version, listening IP, actual port, browser URL, project, authentication state, Host rules, and Ctrl+C guidance. Report stopping/stopped states during graceful shutdown.
- Support IPv6 listener addresses and use OS-assigned ports consistently in startup output and Host validation. Verify startup and HTTP access on all five release platforms.

## [0.1.3](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.2...rpi-web-v0.1.3) - 2026-10-21

### Fixed

- *(web)* preserve command feedback during initial history load ([#47](https://github.com/pi-rust/rpi-package/pull/47))

- Preserve local command feedback while the initial transcript snapshot loads, and wait for the model catalog before resolving slash model names.

## [0.1.2](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.1...rpi-web-v0.1.2) - 2026-10-21

### Added

- *(web)* display durable tool execution timings and support native extension ([#45](https://github.com/pi-rust/rpi-package/pull/45))

- Show live tool execution durations, freeze timers on completion or interruption, and preserve completed Bash timings across page reloads and service restarts.

- Add a native `rpi --web` extension entry point alongside the standalone executable, including `rpi --web=init`, shared JSON configuration, embedded frontend assets, and bounded shutdown of HTTP streams and native session children.
- Publish a standard `rpi-extension.json` with five-platform extension libraries, plus separate standalone archives built with the same verified frontend.

## [0.1.1](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.0...rpi-web-v0.1.1) - 2026-10-21

### Added

- *(web)* load startup settings from .rpi/web.json ([#43](https://github.com/pi-rust/rpi-package/pull/43))

- Load global `~/.rpi/web.json` and project `.rpi/web.json` settings, with explicit `--config`, CLI overrides and non-overwriting `--init` setup. Keep existing session storage defaults and support `allowedHosts` in JSON.

## [0.1.0] - 2026-10-21

- Initial project and chat workspace with Chinese/English interfaces, password login, Markdown rendering, streaming RPC, tool/permission dialogs, model settings, history recovery and file previews.
- Publish portable binaries with the verified frontend for Windows, Linux and macOS. The native `rpi` host is installed separately.
