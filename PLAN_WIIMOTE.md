# Plan: Wiimote (Wii Remote Plus) Motion for Xbox → Switch

## Overview

Add Splatoon-style motion aiming to the Xbox → Switch bridge by strapping a
Wii Remote Plus (which has a 3-axis gyroscope + accelerometer) to the Xbox
controller. The Wiimote's motion data is read over Bluetooth, transformed into
the Switch Pro Controller frame, and injected into the per-slot `0x30` input
report so Switch titles that use gyro aiming (Splatoon 2/3, etc.) respond to
physical controller rotation.

## Key facts

- The Switch Pro Controller `0x30` input report is already **64 bytes**
  (`REPORT_LEN = 64`, `src/gadget.rs:28`), but `encode_input()` only fills the
  11-byte button/stick buffer (`src/switch_proto.rs:83`) and `input_report()`
  pads the rest with zeros (`src/switch_proto.rs:146`). The gyro/accel bytes are
  currently emitted as 0, so nothing changes until we write them.
- Pro controller `0x30` report layout (relevant part):
  - `[0]=0x30`, `[1]=counter`, `[2..12]` = 11-byte input buffer (already built),
    `[12]` = battery
  - `[13..18]` = **gyro** (3 × int16 LE)
  - `[19..24]` = **accel** (3 × int16 LE)
  - rest = padding
- Wii Remote Plus has a MotionPlus **gyro** (3-axis) + accelerometer (3-axis) +
  buttons at **100 Hz** over classic Bluetooth (not BLE).
- The `wiimote-rs` crate (v0.2.0, MIT, `cesmec/wiimote-rs`) handles the bulk of
  acquisition: BT connect, MotionPlus enable, report decoding, and calibration.

## What the crate gives us vs. what we build

| Concern | Who |
|---|---|
| BT connect, MotionPlus enable, report decoding | `wiimote-rs` |
| MotionPlus + accel calibration → real units | `wiimote-rs` (`get_angular_velocity`, `get_acceleration`) |
| Mount rotation (Wiimote → Pro Controller frame) | **us** (new `src/motion.rs`) |
| Scale deg/s + g → Switch int16 | **us** |
| Emit gyro/accel in `0x30` report + SPI gyro cal | **us** (`src/switch_proto.rs`) |
| Data model + wiring | **us** |

## Phase 1 — Protocol support (self-contained, testable first)

### 1a. `src/models.rs` — extend the data model
- Add a `Motion { gyro: [i16; 3], accel: [i16; 3] }` struct and a `motion: Motion`
  field on `SwitchInput`.
- Zero it in `NEUTRAL_INPUT` so idle slots stay neutral (preserves the "always N
  connected" design).

### 1b. `src/switch_proto.rs` — emit the sensor bytes
- Extend `encode_input()` / `input_report()` to append gyro bytes `[13..18]` and
  accel bytes `[19..24]` from the motion field.
- Report stays 64 bytes → **no HID descriptor change** (`REPORT_DESC_HEX` in
  `src/gadget.rs:26` untouched).

### 1c. SPI gyro calibration
- The Switch applies the Pro controller's gyro calibration read from SPI ROM
  (currently `SPI_ROM_DATA` at `src/switch_proto.rs:14` only serves stick/accel
  calibration; gyro calibration at `0x8026` is absent/misleading).
- Add/repair the gyro calibration block so the Switch maps our raw int16 gyro to
  real angular rate (offset ≈ 0, scale chosen for the `0x30` scale factor,
  commonly ~0.0025 rad/s per LSB — confirm against `nscon.go` / `HIDtoVPAD`).

**Verification:** with motion zeroed it's a no-op; with a test constant it should
make Splatoon-style aiming drift if the sensor is fed raw.

## Phase 2 — Wiimote acquisition (new `src/wiimote.rs`)

Thin wrapper over `wiimote-rs`:
- `WiimoteDevice` connect over classic BT (L2CAP PSM `0x11` / `0x13`),
  auto-reconnect, using the crate's `simple_io` module for setup.
- Enable MotionPlus + accel reporting (mode `0x35`, per the crate's own
  `examples/motion_plus.rs`).
- Parse `MotionPlusData` + `AccelerometerData`; run through
  `get_angular_velocity` / `get_acceleration`.
- Push a calibrated `(gyro[deg/s], accel[g])` sample into shared state
  (Arc/Mutex or channel) at ~100 Hz.

## Phase 3 — Mount transform & scaling (new `src/motion.rs`)

- Apply a fixed 3×3 mount rotation: Wiimote axes → Pro Controller frame
  (constant, since it's strapped on; determined once at calibration time).
- Convert deg/s → Switch int16 gyro scale and g → int16 accel scale, matching
  Phase 1c's SPI calibration.
- Hold the latest sample; resample to the report rate (default 250 Hz).

## Phase 4 — Wiring

### `src/bridge.rs`
- Share a motion source (e.g. `Arc<Mutex<Option<Motion>>>` per slot, fed by the
  Wiimote thread).
- In the per-slot poll loop (`src/bridge.rs:192`), merge the bound slot's motion
  into the `SwitchInput` produced by `mapping.poll()`; other slots keep zero
  motion.

### `src/main.rs`
- Add CLI flags (e.g. `--wiimote-slot <N>` and `--wiimote-debug`).
- Spawn the Wiimote reader thread alongside the gilrs bridge thread
  (`src/main.rs:105`).

### `Cargo.toml`
- Add `wiimote-rs = "0.2"`.

## Phase 5 — Calibration & verification (hardware)

- `--wiimote-debug` dumps raw gyro/accel to verify decode + transform.
- A mount-calibration helper to determine the rotation matrix (hold flat / roll
  left).
- Verify on the Pi in a Splatoon 2/3-style title: motion aiming should track
  physical controller rotation.
- No automated tests exist (hardware-dependent), consistent with the project.

## Caveats

1. **`wiimote-rs` uses `bindgen` on Linux** (build-time only) to generate FFI
   bindings for **libbluetooth** (BlueZ classic-BT socket API). Cross-compiling
   to aarch64 needs a C toolchain + bluetooth headers on the host, and
   `libbluetooth-dev` on the target / in the cross image (`Cross.toml` pre-build
   deps may need updating).
2. **`get_angular_velocity` returns degrees** — confirm the exact unit/scale in
   the crate source so the int16 mapping matches the SPI calibration.
3. Remaining choices: **slot binding** (`--wiimote-slot <N>` vs. auto-assign)
   and **mount calibration** (fixed matrix vs. runtime routine).

## Open questions

1. **Slot binding**: bind the Wiimote to a specific slot via CLI, or auto-assign
   to the slot whose Xbox controller it's strapped to?
2. **Mount calibration**: fixed compile-time rotation matrix, or an interactive
   runtime calibration routine?