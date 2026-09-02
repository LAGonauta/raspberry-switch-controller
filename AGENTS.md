# AGENTS.md

## Repository Overview

Rust project bridging Xbox wireless controllers to Nintendo Switch over USB OTG gadget mode on Raspberry Pi 4.

## Key Commands

### Build
```bash
cargo build
cargo build --release  # With LTO (configured in Cargo.toml)
```

### Run (requires root)
```bash
sudo ./target/release/raspberry-switch-controller
sudo ./target/release/raspberry-switch-controller --controllers 4 --polling-rate 250
```

**CLI Arguments:**
- `--controllers <N>`: 1-8 slots (default 4, max 8)
- `--polling-rate <Hz>`: 20-1000 Hz (default 250)
- `--web-addr <host:port>`: web UI listen address (default `0.0.0.0:8080`)
- `--no-web`: disable the web UI
- `--no-gadget`: skip USB gadget creation (UI development without root/hardware)

### Cross-compilation
Root project has `Cross.toml` with pre-build dependencies for aarch64 (RPi4).
```bash
CROSS_CONTAINER_ENGINE=podman cross build --target aarch64-unknown-linux-gnu --release
```

## Hardware Requirements

- **Raspberry Pi 4** (Pi 3 cannot do gadget mode)
- **USB-C port** = USB OTG gadget (to Switch)
- **USB-A port** = Official Xbox wireless dongle (via xone driver)
- **Powered USB hub** recommended between Pi and Switch

## Architecture

### Module Responsibilities

| Module | Responsibility |
|--------|----------------|
| `src/main.rs` | CLI parsing, thread spawning, Ctrl-C handling |
| `src/bridge.rs` | gilrs loop, slot assignment, Xbox pad enumeration, WebState maintenance |
| `src/gadget.rs` | USB gadget creation via configfs, per-slot HID tasks |
| `src/mapping.rs` | Xbox → Switch button/stick mapping + `XboxInput` tester snapshot |
| `src/models.rs` | Data structures (SwitchInput, Rumble, AppState, WebState, Command) |
| `src/switch_proto.rs` | Switch Pro HID protocol (handshake, reports, SPI ROM, rumble) |
| `src/web.rs` | HARM-stack web UI (Axum + Maud + HTMX SSE + Alpine), reads WebState, sends Commands |

### Web UI (HARM stack)
- HTMX 2.x + Axum/Alpine.js + Rust + Maud, all server-rendered HTML fragments
- Live controller tester streams raw Xbox button/stick state over SSE (~30Hz)
- `#overview` polls `/fragments/overview` every 1s for list/slots/status
- Vendored JS in `web/static/` (self-contained binary, no internet on Pi)

### Key Design Decisions

1. **Fixed slots**: Creates N HID functions at startup, never re-enumerates
2. **Idle slots**: Send neutral reports so Switch always sees N connected controllers
3. **Per-slot ownership**: Each slot owns its `/dev/hidgN` file (prevents races)
4. **Button mapping**: Xbox A/B/X/Y → Switch B/A/Y/X (swapped)

## Development Environment

### Dev Container
- Requires `libudev-dev` and `pkg-config` (already in Dockerfile)
- Install ripgrep: `cargo install ripgrep`

### Code Quality
- `cargo fmt` needed: formatting issues exist in several files
- `cargo clippy` passes with no warnings

### No Test Suite
No tests found in the codebase. Verification requires hardware.

## Critical Constraints

1. **Root required**: USB gadget creation writes to `/sys/kernel/config/usb_gadget`
2. **Hardware dependent**: Cannot test without Pi 4 + Switch + Xbox dongle
3. **xone driver**: Must be installed for Xbox wireless dongle support
4. **Pi setup**: Enable OTG (`dtoverlay=dwc2` in `/boot/firmware/config.txt`, create `/etc/modules-load.d/usb-gadget.conf` with `dwc2` and `libcomposite`)

## Reference Implementations

- `HIDtoVPADNetworkClientCli/` - Rust reference architecture
- `raspberry-switch-control/` - Go backend being ported (especially `nscon.go` for protocol)

## Known Issues from PLAN.md

- Switch 2 negotiation untested
- Idle slots occupy Switch player slots even when no Xbox present (by design)
- Rumble amplitude scaling may need tuning