# Plan: Replace TUI with a HARM-stack Web UI

## Goal
Remove the `ratatui`/`crossterm` terminal UI and serve a mobile-friendly web page from the same binary. The page shows everything the TUI had (controller list, slot table, status, vibrate/identify/remap) plus a **live controller tester**: pressing a physical Xbox button lights up that button on the page over SSE.

Stack per the HARM article (https://nguyenhuythanh.com/posts/the-harm-stack-considered-unharmful/): **H**TMX 2.x + **A**xum/**A**lpine.js + **R**ust + **M**aud, all in the one Rust binary (no build step, offline-safe for a LAN Pi).

## Dependency policy
Use the **latest version of every direct dependency**, unless it breaks the build (i.e. changes an API our code relies on and no compatible migration is reasonable). If the latest breaks the build, keep the last working version and note the pinned version + reason.

Versions verified at planning time (2026-09-02, `cargo search`):
- Existing deps: `gilrs 0.11.2`, `flume 0.12.0`, `ctrlc 3.5.2`, `clap 4.6.6`, `libc 0.2` (latest stable; 1.0 line is pre-release), `log 0.4.34`, `pretty_env_logger 0.5.0`
- `governor`: upgraded from `0.6` to latest `0.10.4` — the rate-limiter API changed (`Clock` no longer implemented for references, `direct_with_clock` takes the clock by value, `Quota::per_second` arg semantics unchanged); migration was trivial: pass `clock.clone()` to `direct_with_clock` and keep the original for `wait_time_from(clock.now())`
- Removed: `ratatui`, `crossterm`
- New: `axum 0.8.9`, `tokio 1.53.1` (`rt-multi-thread`, `sync`, `time`, `net`), `tokio-stream 0.1.19` (`sync`), `maud 0.27.0` (`axum` feature)
- Vendored JS (checked into `web/static/` so the Pi needs no internet): `htmx.min.js` (2.x), `htmx-ext-sse.min.js` (2.x), `alpine.min.js` (3.x)

## Architecture: single source of truth
- The **bridge thread stays the only gilrs owner**. It also now maintains a shared `Arc<Mutex<WebState>>` (connected controllers incl. name/slot/battery/is_vibrating, latest **raw Xbox input snapshot**, slot table, status message).
- The **web thread** runs a Tokio runtime + Axum on its own std thread. It *only reads* `WebState` to render HTML and *writes commands* to the bridge via the existing flume channel. No gilrs access in the web thread.
- Live data flows: htmx polls a cheap `#overview` fragment every 1s; an **SSE stream** pushes per-controller HTML fragments at ~30Hz for instant button lights (htmx `sse-connect`/`sse-swap` extension). Server pushes HTML fragments, keeping the client a pure rendering of server state — exactly the HARM philosophy.

## Changes by file

### `Cargo.toml`
- Remove: `ratatui`, `crossterm`.
- Add: `axum` 0.8, `tokio` (`rt-multi-thread`, `sync`, `time`, `net`), `tokio-stream` (`sync`), `maud` (`axum`). All pure Rust → no new cross-compilation issues.

### `src/models.rs`
- Add `XboxInput` (A/B/X/Y, LB/RB, LT/RT button + analog 0..1, View/Menu/Xbox, LS/RS press, D-pad, left/right sticks ±1.0), `WebController`, `WebState`.
- Move the command enum out of `ui.rs` into `models.rs` as `Command`: `Remap { controller_id: usize, new_slot }`, `Identify { controller_id }`, `Vibrate { controller_id, duration_ms }`. **Reason:** `GamepadId` is opaque outside the bridge (`pub(crate) usize`, no public constructor); the web layer sends `usize::from(GamepadId)` as a form value, and the bridge resolves it back by scanning `controllers`.

### `src/mapping.rs`
- Add `XboxInput::from_gamepad(&Gamepad)` to snapshot the raw Xbox state for the tester page (mirrors the Switch mapping poll, Xbox-native labels).

### `src/bridge.rs`
- Accept `Arc<Mutex<WebState>>`; replace the `UiEvent` sends with direct `WebState` updates on connect/disconnect/remap.
- In the poll loop, build `XboxInput` from the same `gamepad` snapshot already taken for mapping and store it in `WebState` (cheap short lock; SSE reads at 30Hz so 250Hz writes are fine).
- Resolve `usize` controller id → `GamepadId` for commands; mark/clear `is_vibrating` in `WebState` around the existing Identify/Vibrate threads (clone the `Arc` into the spawned threads).

### `src/web.rs` (new)
- **Maud templates:** `page`, `overview_partial`, `pads_full`, `pad_card`, `pad_readout`. Note: quote Maud attributes that start with `@` (Alpine `@click` → `"@click"`).
- **Routes:**
  - `GET /` — full page from current `WebState`
  - `GET /fragments/overview` — controller list + slot table + status (htmx `hx-trigger="every 1s"` → `#overview`)
  - `POST /actions/identify`, `/actions/vibrate` (with duration), `/actions/remap` — parse form, `command_tx.try_send(Command)`, return updated overview fragment
  - `GET /events` — SSE: named events `pads` (full card set on connect/disconnect/remap + on stream open for reconnect resync) and `pad-<id>` (card readout HTML at ~30Hz, diffed to skip unchanged). Per-client ticker task over an mpsc → `Sse<ReceiverStream<…>>`.
  - `GET /static/*` — vendored `htmx.min.js`, `htmx-ext-sse.min.js`, `alpine.min.js`, `style.css` embedded via `include_str!` (self-contained binary, no internet needed on the Pi).
- **Alpine.js:** kept minimal (client-only state): dark-mode toggle, vibrate-duration slider display. Everything else is server-rendered HTML.

### Page layout (mobile-first)
1. Header + status line
2. `#overview` (polled): connected controller names/battery/slot badges + slot table (Slot 1..N: name or "idle")
3. `#pads-wrap` (SSE container `hx-ext="sse" sse-connect="/events"`): one card per controller. Each card keeps its **controls** (vibrate slider/buttons, remap select) static outside the SSE-swapped **readout** div (`sse-swap="pad-<id>"`) so Alpine state survives the 30Hz updates:
   - Xbox-native button grid that lights up when pressed (CSS `.lit`)
   - Two stick crosshairs (dot positioned from inline style) + two trigger bars
   - Vibrate/Identify buttons, remap `<select>` + Move button
   - Empty-state card when no controllers are connected
4. Footer with hint text

### `src/main.rs`
- Delete `mod ui;` and the UI thread; spawn the web thread (`tokio::runtime → block_on(web::serve(...))`).
- CLI: add `--web-addr <host:port>` (default `0.0.0.0:8080`, reachable from mobile on the LAN), `--no-web`, and `--no-gadget` (skips configfs so the UI can be verified on a dev machine without root/hardware).
- Nothing else changes: gadget slots, polling, rumble pass-through untouched.

### Delete `src/ui.rs`
Remove `ratatui`/`crossterm` imports and the `UiCommand`/`UiEvent` pairs (superseded by `Command` + `WebState`).

## Real-time flow
1. `GET /` renders the full page from current state (no flicker).
2. Browser opens `/events`; server immediately pushes `pads` + current `pad-<id>` snapshots → resync after any reconnect.
3. Bridge poll updates `WebState.input` (~250Hz); SSE ticker diffs and pushes `pad-<id>` at ~30Hz → buttons light up within ~30ms.
4. Connect/disconnect/remap push `pads` to add/remove cards; the 1s overview poll keeps the list/slot/status in sync.

## Verification
1. `cargo build` + `cargo clippy` (AGENTS.md notes clippy currently clean) + `cargo fmt`.
2. Dev-machine check without hardware: `cargo run -- --no-gadget --web-addr 127.0.0.1:8080`, exercise the page + `curl` the action endpoints.
3. On-Pi manual test (hardware): button lights, stick crosshairs, remap, vibrate/identify, connect/disconnect, mobile browser on LAN.
4. `CROSS_CONTAINER_ENGINE=podman cross build --target aarch64-unknown-linux-gnu --release`.

## Risks / notes
- `GamepadId` opacity handled via `usize` ids resolved in the bridge.
- htmx SSE requires two vendored files — `htmx.min.js` **plus** `htmx-ext-sse.min.js` (it's a separate core extension in htmx 2.x).
- Removing the TUI removes the terminal dependency → the service can now run under systemd without a TTY.
- No auth on the page (LAN-trusted device); a simple token can be added later if needed.
- README/AGENTS.md feature/architecture tables should be updated to reflect the web UI.