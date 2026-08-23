# ALPHA SOFTWARE WIP

Do not use!

# Raspberry Switch Controller

Bridge Xbox wireless controllers to a Nintendo Switch over USB OTG gadget mode on Raspberry Pi 4.

## Overview

This project turns a Raspberry Pi 4 into a USB adapter that allows multiple Xbox wireless controllers (connected via Microsoft's official wireless dongle) to be used as Nintendo Switch Pro Controllers. The Pi 4 acts as a USB gadget, emulating multiple Switch Pro Controllers over USB-C, enabling local multiplayer on the Switch with Xbox controllers.

## Features

- **Multi-controller support**: 1-8 virtual Switch Pro Controllers (default 4)
- **Xbox wireless dongle support**: Uses the xone driver for official Microsoft wireless adapters
- **Plug-and-play**: Xbox controllers automatically map to available slots
- **Rumble support**: Switch rumble/vibration is forwarded to Xbox controller haptics
- **Fixed slots**: Stable connections - no disconnection alerts when Xbox pads connect/disconnect
- **Low latency**: Configurable polling rate (20-1000 Hz, default 250 Hz)
- **Realtime priority**: Optimized for smooth input handling

## Hardware Requirements

- **Raspberry Pi 4** (Pi 3 cannot do USB gadget mode)
- **USB-C cable**: Connects Pi to Switch (through a powered USB hub recommended)
- **USB-A port**: For official Xbox wireless dongle (or direct Xbox controller connection via USB)
- **Xbox wireless controllers**: Official Microsoft controllers paired to the dongle
- **Nintendo Switch**: Original or Switch 2 (untested)
- **Powered USB hub** (recommended): Between Pi and Switch to ensure stable power delivery

## Prerequisites

### Raspberry Pi Setup

1. **Enable USB OTG**:
   - Add `dtoverlay=dwc2` to `/boot/firmware/config.txt`
   - Add `dwc2` and `libcomposite` to `/etc/modules`
   - Reboot the Pi

2. **Install xone driver**:
   - Install the xone kernel driver for Xbox wireless dongle support
   - Pair your Xbox controllers with the dongle
   - Verify controllers appear as gamepads (e.g., `ls /dev/input/js*`)

3. **Run as root**: The program requires root access to create USB gadgets

### Software Requirements

- Rust toolchain (for building)
- `cross` (for cross-compilation) or native Rust on Pi 4
- Podman or Docker (if using cross-compilation)

## Installation

### Cross-compilation for Raspberry Pi 4

```bash
# Install cross
cargo install cross --git https://github.com/cross-rs/cross

# Build for aarch64 (RPi4)
CROSS_CONTAINER_ENGINE=podman cross build --target aarch64-unknown-linux-gnu --release
```

The binary will be at:
```
target/aarch64-unknown-linux-gnu/release/raspberry-switch-controller
```

### Native compilation on Raspberry Pi 4

```bash
cargo build --release
```

## Usage

### Basic usage (4 controllers, 250 Hz polling)

```bash
sudo ./target/release/raspberry-switch-controller
```

### Command-line arguments

```
raspberry-switch-controller [OPTIONS]

Options:
  --controllers <N>     Number of Switch Pro Controller slots to expose (1-8) [default: 4]
  -p, --polling-rate <HZ>  Polling rate in Hz (20-1000) [default: 250]
  -h, --help            Print help
  -V, --version         Print version
```

### Examples

```bash
# 2 controllers at 500 Hz
sudo ./target/release/raspberry-switch-controller --controllers 2 --polling-rate 500

# Maximum 8 controllers
sudo ./target/release/raspberry-switch-controller --controllers 8

# High polling rate for competitive gaming
sudo ./target/release/raspberry-switch-controller -p 1000
```

## How It Works

1. **USB Gadget Creation**: At startup, the program creates a USB composite gadget with multiple HID functions, each emulating a Switch Pro Controller
2. **Xbox Controller Input**: The bridge thread polls Xbox controllers via gilrs and maps inputs to Switch Pro Controller format
3. **Slot Assignment**: Xbox controllers are assigned to available slots on a first-come-first-served basis
4. **Input Reports**: Each slot thread sends input reports at the configured polling rate
5. **Rumble Feedback**: Switch rumble data is decoded and forwarded to Xbox controller haptics

## Troubleshooting

### "Unable to create USB gadget (run as root?)"
- Run the program with `sudo`

### "Warning: --controllers must be between 1 and 8"
- Adjust the `--controllers` argument to be within the valid range

### Xbox controllers not detected
- Ensure xone driver is installed and loaded: `lsmod | grep xone`
- Verify controllers are paired and appear in `/dev/input/`

### Switch doesn't recognize controllers
- Check USB connections (try different cables/hub)
- Ensure OTG is enabled (`ls /sys/class/udc/` should show a UDC device)
- Verify gadget creation: `ls /sys/kernel/config/usb_gadget/`

### High CPU usage
- Reduce polling rate: `--polling-rate 250` or lower
- Check if other USB devices are causing conflicts

## Architecture

| Module | Responsibility |
|--------|----------------|
| `main.rs` | CLI parsing, thread spawning, Ctrl-C handling |
| `bridge.rs` | Xbox controller polling, slot assignment, input mapping |
| `gadget.rs` | USB gadget creation via configfs, per-slot HID I/O |
| `mapping.rs` | Xbox → Switch button/stick mapping |
| `models.rs` | Data structures (SwitchInput, Rumble, AppState) |
| `switch_proto.rs` | Switch Pro HID protocol implementation |
| `priority.rs` | Realtime priority setting for performance |

## Known Limitations

- **No motion/IMU support**: Motion controls are not implemented (sends zeros)
- **Player LED ordering**: Not implemented (LEDs may not match player numbers)
- **Switch 2 support**: Untested (expected to work with same protocol)
- **Idle slots occupy player slots**: Unused controllers still appear as connected on Switch

## Credits

- Based on [nscon](https://github.com/mzyy94/nscon) - Original Switch Pro Controller emulation
- Reference architecture from [HIDtoVPADNetworkClientCli](https://github.com/HIDtoVPAD/HIDtoVPADNetworkClientCli)
- Switch Pro Controller protocol ported from [raspberry-switch-control](https://github.com/aspect-build/raspberry-switch-control)

## License

This project is open source. See repository for license details.
