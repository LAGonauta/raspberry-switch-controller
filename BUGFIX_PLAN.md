# Plan: Bug Review & Fixes (protocol, concurrency, web, refactor)

Results of a full code review comparing the implementation byte-for-byte against
the two bundled working references:

- `raspberry-switch-control/nscontroller/nscon.go` — proven Switch 1 USB protocol
- `openpuck/OpenPuck/mode_switch_pro.cpp` — proven Switch 2 protocol

**Locked decisions:**

1. Scope = bug fixes **plus** the state-consolidation refactor.
2. Connection gating = **unified Switch 1 + 2 model** (`0x80 0x04` OR
   host-selected report mode `0x30`, gates cleared on USB reset).
3. Idle slots report battery byte `0x81` (full + USB powered), matching the
   current visible behavior on the Switch.

---

## 1. Findings

### 1.1 Critical

#### C1. Slot writer threads die permanently on the first failed write

`gadget.rs:312-329` (`run_slot` writer loop):

- `input_active` is initialized to `true`, and the loop does
  `if guard.write_all(&report).is_err() { break; }`.
- Writes to `/dev/hidgN` fail with `ESHUTDOWN` until the host enumerates the
  gadget (~50ms+ after UDC bind), but the writer starts ~4ms after bind.
  **Every slot's writer breaks on its first write, at every startup, before
  the Switch ever enumerates it.** Same for Switch sleep/wake and cable
  replug (USB reset → write error → dead slot, no recovery).
- The reader thread also dies on any read error (`gadget.rs:225
  Err(_) => break`), which kills the handshake responder as well.

Evidence:

- `nscon.go` ignores all write errors (`c.fp.Write(data)` unchecked) and
  starts input reports **only** on host report `0x80 0x04`
  (`startInputReport()`), stopping on `0x80 0x05`.
- `PLAN.md §5` states the same intent ("`0x04` start input reports"), which
  the code deviates from.

#### C2. Battery byte written to the wrong report offset (feature is dead code)

`switch_proto.rs:149` (`input_report`): `report[12] = input.battery`.

- `report[12]` is the vibrator-echo byte. Genuine pads emit `0x09..0x0C`
  there; openpuck notes "some Switch firmware expects this nonzero".
- The battery/connection byte is **`report[2]`** (confirmed by openpuck
  `jcInputPrefix` → `out[1]` after the report id, and by nscon's constant
  `0x81` sitting there = full battery + USB).
- Net effect: the Switch always sees "full battery" (the hard-coded `0x81`),
  and the actual battery data lands in a byte the Switch doesn't read.

### 1.2 Medium

- **M1. Reader panic on truncated `0x01` report** — `gadget.rs:286` reads
  `report[11]` for subcommand `0x21` after only checking `len >= 11`
  (index out of bounds when `len == 11`; kills the reader thread).
  BUGS.md #13 redux, one field short.
- **M2. Identify/Vibrate break rumble** — `bridge.rs:328,369` `take()` the
  strong effect but leave `strong_mag` stale; afterwards rumble with an
  unchanged magnitude never recreates the effect (rumble dead for that motor
  until the magnitude changes). Also: `duration_ms: u64` unbounded (a thread
  can sleep ~forever with `is_vibrating` stuck true) and no re-entrancy guard.
- **M3. Stale report-mode gate** — `report_mode`/`input_active` are never
  cleared on USB re-enumeration; a host selecting any mode ≠ `0x30` stops
  output forever. Folded into the C1 fix.
- **M4. Bridge can stall on a full slot channel** — `bridge.rs:499` uses a
  blocking `tx.send()` into a `bounded(1)` channel; one stalled slot thread
  freezes all slots, rumble, and commands, *while holding the `web_state`
  lock* (web UI hangs too).
- **M5. SSE task leaks on client disconnect** — `web.rs:221` `break` exits
  the inner `for` loop, not the outer `loop`; when nothing changed no send is
  attempted, so a closed client spins at ~30 Hz rendering `pads_full` until
  process exit.
- **M6. `GadgetManager::create()` not idempotent** — a leftover gadget tree
  from a crashed run makes the function symlink creation fail with `EEXIST`
  and startup aborts with a confusing error.

### 1.3 Architectural

- **A1. Parallel-array state sync** — `controllers: Vec<Controller>` (bridge)
  must stay index-aligned with `WebState.controllers` and `WebState.inputs`
  across connect/disconnect/remap/poll. It holds today, but there are five
  hand-maintained sync sites (`sync_web_slots`, position-index mapping in the
  poll loop, mirrored push/remove) — one missed path away from
  wrong-controller bugs.
- **A2. `static mut HDR_STATE` + `unsafe`** (`switch_proto.rs:219`) — global
  mutable static indexed by slot; `hdr_reset_slot` is reachable with any
  index; rejected outright in edition 2024. Replace with a per-slot state
  struct owned by `run_slot`.
- **A3. Mutex-poisoning cascade** — `.unwrap()` on every lock: one panic
  while holding a lock freezes the bridge (poisoned `web_state`) and can
  panic `main` during shutdown, skipping gadget teardown.
- **A4. No tests for protocol code** — `switch_proto` is pure and highly
  testable; only `percentage_to_level` has a test.

### 1.4 Verified correct (no action)

- `0x30` input buffer layout byte-identical to nscon (incl. the `0x81`
  prefix byte, button bit order, `packShorts`).
- Handshake packets identical to nscon (`0x81 0x03`, `0x81 0x01 [00 03]`).
- SPI ROM read mirrors nscon keyed on `report[12]` with added bounds checks
  (BUGS.md #14/#22 fixed).
- Stick clamping (BUGS.md #17 fixed); trigger threshold matches
  `xboxjoystick.go` (`-0.8`); A/B/X/Y swap matches.
- `packet()` 62-byte payload cap matches nscon.
- Maud escapes HTML by default (no XSS); form inputs parsed as `usize`.
- Remap/swap slot bookkeeping traced correct for swap/move/no-op cases.
- Disconnect removal keeps `controllers`/`ws.controllers`/`ws.inputs`
  aligned today (fragile — addressed by A1 anyway).

---

## 2. Implementation phases (one commit per phase)

### Phase 1 — Slot connection state machine (C1, M1, M3)

File: `src/gadget.rs`

- Writer loop never exits on write error: on error → log (throttled), reset
  gates, reset rumble state, continue the loop.
- Unified Switch 1 + 2 gating, per-slot:
  - Start: `input_active = false`, report mode *not* selected.
  - Host `0x80 0x04` → `input_active = true`; `0x80 0x05` → `false`
    (Switch 1 flow, nscon semantics).
  - Subcommand `0x21` with mode `0x30` → mode-selected (Switch 2 flow,
    openpuck semantics).
  - Emit `0x30` reports when **either** signal is present.
  - On read EOF/error or USB reset: log, sleep 50–100 ms, continue (keep the
    thread alive), and clear both gates + rumble state so the next
    enumeration's handshake re-opens them.
- Per-subcommand length validation (`0x21` requires `len >= 12`; keep the
  existing `len >= 16` for `0x10`).

### Phase 2 — Battery byte fix (C2)

Files: `src/switch_proto.rs`, `src/models.rs`, `src/bridge.rs`

- `input_report`: `report[2] = input.battery` (overrides the `0x81`);
  `report[12] = 0x09` (rumble-echo constant, per openpuck).
- `NEUTRAL_INPUT.battery = 0x81` so idle slots keep looking like healthy USB
  Pro Controllers.
- `bridge.rs` battery assembly: always set the host-powered bit (`0x01`,
  the gadget *is* USB): `Discharging → level<<5 | 0x01`,
  `Charging → level<<5 | 0x10 | 0x01`, `Charged → 0x91`,
  `Wired`/`Unknown → 0x81`.
- Subcommand responses keep `0x81` at `report[2]` (matches nscon); threading
  real battery through them is an optional follow-up.

### Phase 3 — Identify/Vibrate fixes (M2)

Files: `src/bridge.rs`, `src/web.rs`, `src/models.rs`

- Manual vibration uses a dedicated temporary effect (or, minimally, resets
  `strong_mag = 0` after `take()` so the main loop rebuilds it).
- Skip if the controller is already vibrating (`is_vibrating` guard).
- Cap duration (≤ 5000 ms) in the web handler or bridge.

### Phase 4 — Non-blocking slot sends (M4)

File: `src/bridge.rs`

- Replace the blocking `tx.send(input)` with `send_timeout` (latest-wins;
  the channel is `bounded(1)` by design).
- Collect inputs under the `web_state` guard, drop the guard, then send —
  sends must never happen while holding the web lock.

### Phase 5 — Web fixes (M5)

File: `src/web.rs`

- SSE loop: check `tx.is_closed()` at the top of each iteration → break;
  break the outer loop (not just the `for`) on send error.
- Render `pads_full` only when the controller id list changed (it is only
  sent then anyway).
- Server-side Vibrate duration cap (with Phase 3).

### Phase 6 — Idempotent gadget create (M6)

File: `src/gadget.rs`

- `create()` detects an existing gadget root and runs `destroy()` first
  (logging "removing leftover gadget").

### Phase 7 — Single source of truth for controller state (A1)

Files: `src/models.rs`, `src/bridge.rs`, `src/web.rs`

- Introduce `ControllerState { id, slot: Option<usize>, name, battery,
  is_vibrating, input: Option<XboxInput> }`.
- `WebState.controllers: Vec<ControllerState>`; drop the parallel `inputs`
  vec and the `WebController`/`Controller` split, `sync_web_slots`, and all
  position-index mapping. The web renders straight from the shared state.
- Extract a pure `apply_remap(...)` over the controllers vec so the
  swap/move logic becomes unit-testable.

### Phase 8 — Rumble decoder state (A2)

Files: `src/switch_proto.rs`, `src/gadget.rs`

- Replace `static mut HDR_STATE` + `hdr_reset_slot(slot)` with a
  `pub struct HdrState` owned per slot by `run_slot` (reset on connection
  loss, tying into Phase 1). Removes all `unsafe` and the implicit
  `slot < 8` bound; the amplitude lookup table stays a `OnceLock`.

### Phase 9 — Hardening, tests, housekeeping (A3, A4)

Files: all

- Poison-tolerant lock helper (`unwrap_or_else(PoisonError::into_inner)`)
  used in `main.rs` / `bridge.rs` / `web.rs` where a panic would cascade.
- Unit tests (`cargo test`):
  - `switch_proto`: `encode_input` golden bytes (neutral, all buttons, stick
    extremes 0 / 2048 / 4095 + clamp), `pack_shorts`, `packet` padding/cap,
    `spi_read_response` bounds (in-range, out-of-range, malformed),
    `subcommand_response` ack-byte logic, battery byte at `[2]` and `0x09`
    at `[12]`, `hdr_decode` smoke tests (silence → 0, max → 255).
  - Bridge: `apply_remap` swap/move/invalid-slot cases (via Phase 7).
  - `models`/`bridge`: battery byte composition.
- `cargo fmt` across the repo (AGENTS.md notes it is pending); keep
  `cargo clippy` warning-free.
- Update `AGENTS.md` (models/WebState description, "No Test Suite" section,
  remove the "cargo fmt needed" note) and `README.md` Known Limitations if
  battery behavior is mentioned.

---

## 3. Verification

No hardware:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
# web/UI smoke test without root or hardware:
./target/debug/raspberry-switch-controller --no-gadget --web-addr 127.0.0.1:8080
```

Hardware checklist (Pi 4 + Switch + Xbox dongle):

1. Start daemon with the Switch connected → controllers appear and inputs
   work (C1 regression).
2. Switch sleep/wake or USB replug → controllers reappear without restarting
   the daemon (C1 recovery).
3. Switch controller-settings battery indicator tracks the Xbox pad (C2).
4. Web "Identify"/"Vibrate" → afterwards Switch rumble → Xbox vibration
   still works (M2).
5. Remap/swap from the web UI; open/close browser tabs repeatedly and check
   the SSE tester keeps updating (M5).
