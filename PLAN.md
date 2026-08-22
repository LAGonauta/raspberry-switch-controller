# Plan: Xbox → Switch Pro USB Bridge (Rust, Raspberry Pi 4)

Bridge official Xbox wireless controllers (connected to a Pi 4 via the Microsoft
wireless dongle) to a Nintendo Switch (2) by emulating multiple Switch Pro
Controllers over USB OTG gadget mode.

Architecture mirrors the existing `HIDtoVPADNetworkClientCli` Rust codebase
(`main.rs` + polling loop + mapping module + transport module), but swaps the
network transport for a **USB gadget transport**, and ports the Switch Pro
Controller protocol from the Go `raspberry-switch-control/nscontroller/nscon.go`.

## 1. Locked decisions

- **Hardware:** Raspberry Pi 4. USB-C port = USB OTG gadget (to Switch);
  USB-A port = official Xbox wireless dongle (to controllers).
  *(Note: Pi 3 cannot do gadget mode — its USB goes through an onboard hub.)*
- **Input:** Read Xbox controllers via **gilrs** (same approach as
  HIDtoVPADNetworkClientCli; the `xone` driver exposes the pads as standard
  Linux gamepads that gilrs enumerates).
- **Gadget creation:** The Rust binary **creates the USB gadget itself** via
  configfs (no external shell script at runtime).
- **Features:** multiple controllers + **rumble pass-through** (Switch rumble
  → Xbox haptics). Motion/IMU and player-LED ordering are out of scope
  (send zeros / minimal handling, same as the Go code).
- **Connect model:** **4 fixed Pro Controllers at launch**, with idle slots.
  The gadget is created once with exactly 4 HID functions and never
  re-enumerated. Unused slots send a neutral report (centered sticks, no
  buttons) so the Switch always sees 4 connected controllers; no blips occur
  when Xbox pads connect/disconnect.

## 2. Behavior model

- At startup the binary creates **one USB composite gadget with exactly 4 HID
  functions** (`hid.usb0..3` → `/dev/hidg0..3`). No re-enumeration afterward.
- All 4 gadget tasks **always** run the Switch handshake and continuously send
  input reports. An *idle* slot sends a neutral report — so the Switch sees 4
  always-connected Pro Controllers.
- Xbox pads are mapped **first-come-first-served to the lowest free slot**.
  Connect → take slot; disconnect → slot returns to idle. The Switch never
  blips.
- Number of controllers is CLI-configurable (`--controllers`, default 4, max 8).

## 3. Module map

Mirrors `HIDtoVPADNetworkClientCli`, swapping the network transport for a USB
gadget transport.

| HIDtoVPAD file            | New file             | Responsibility                                                        |
|--------------------------|----------------------|-----------------------------------------------------------------------|
| `main.rs`                | `src/main.rs`        | clap: `--controllers` (default 4, max 8), `--polling-rate` (default 250); channels; ctrl-c/exit state; gadget teardown |
| `go.rs`                  | `src/bridge.rs`      | gilrs loop: enumerate Xbox pads, slot assignment, poll→map→send `SwitchInput` to slot task, apply rumble from slot |
| `controller_manager.rs`  | `src/mapping.rs`     | gilrs Xbox state → `SwitchInput` (buttons + 2 sticks), with A/B/X/Y swap & trigger threshold from `xboxjoystick.go` |
| `network.rs`             | `src/gadget.rs`      | `GadgetManager` (configfs setup/teardown) + per-slot task: open `/dev/hidgN`, read host reports, write input reports on ticker, decode rumble → channel |
| `commands.rs`/`models.rs`| `src/switch_proto.rs`| port of `nscon.go`: report encode (`getInputBuffer`/`packShorts`), handshake/subcommand FSM, `SPI_ROM_DATA`, rumble decode |
| `models.rs`              | `src/models.rs`      | `SwitchInput`, `Rumble`, `AppState`, slot IDs, errors                  |

Key structural difference from HIDtoVPAD: the WiiU transport is one shared UDP
socket for all pads; here **each Switch Pro Controller is a separate USB
function** with its own `/dev/hidgN`, its own handshake state, and its own
read/write task. So `gadget.rs` spawns N independent tasks, each talking to one
slot.

## 4. `gadget.rs` — configfs (port of `scripts/switch-controller-gadget`)

Create once at startup:

- `mkdir -p /sys/kernel/config/usb_gadget/switchbridge`
- `idVendor=0x057e`, `idProduct=0x2009`, `bcdUSB=0x0200`, `bDeviceClass=0`
- strings: manufacturer `Nintendo Co., Ltd.`, product `Pro Controller`
- `configs/c.1` (MaxPower 500, bmAttributes 0xa0)
- For `i` in `0..N`:
  - `mkdir functions/hid.usb{i}`; `protocol=0 subclass=0 report_length=64`
  - write the **existing report descriptor** (the long hex string from
    `raspberry-switch-control/nscontroller/scripts/switch-controller-gadget:45`)
    to `report_desc`
  - `ln -s functions/hid.usb{i} configs/c.1/`
- `ls /sys/class/udc > UDC`; `chmod 666 /dev/hidg*`
- Teardown on exit: `echo "" > UDC`; `rm configs/c.1/hid.usb*`; rmdir gadget tree.
- Each slot task owns its `/dev/hidgN` `File` (Rust ownership prevents the
  BUGS.md #12/#21 nil-pointer races).

## 5. `switch_proto.rs` — port of `nscon.go` (critical for Switch acceptance)

- `getInputBuffer()` → 11-byte `{0x81, left, center, right, lx[3], rx[3], 0x00}`;
  `packShorts` with **clamping 0..4095** (fix BUGS.md #17).
- `write(ack,cmd,buf)` → 64-byte padded packet; **check read byte count**
  (fix BUGS.md #13).
- Host-output reader (port `Connect()`):
  - `0x80` reports → `0x01` device-info reply, `0x02/0x03` ack, `0x04` start
    input reports (guard against duplicate tasks, fix BUGS.md #18), `0x05` stop.
  - `0x01` subcommands `buf[10]`: `0x01` pairing, `0x02` device info,
    `0x10` SPI ROM read (use `SPI_ROM_DATA` map; **bounds checks**, fix
    BUGS.md #14/#22), `0x21` set-report-mode response.
- Input-report ticker emits `0x30` packets at polling rate using latest
  `SwitchInput`.

## 6. `mapping.rs` — Xbox → Switch (from `xboxjoystick.go`)

- Buttons: Xbox `A/B/X/Y` → Switch `B/A/Y/X`; `L/R/L2/R2` → `L/R/ZL/ZR`;
  `Select/Start/Mode` → `Minus/Plus/Home`; stick presses direct; D-pad from
  hat axes.
- Sticks: `Axis::{Left,Right}Stick{X,Y}` → 0..4095, Y inverted
  (`nsbackend.go:244`).
- Triggers: `Axis::LeftZ/RightZ` (+ trigger buttons) → `ZL/ZR` via threshold
  (`xboxjoystick.go:7`).
- Output: `SwitchInput` struct consumed by `switch_proto`.

## 7. Rumble pass-through (new; Go code lacks it)

- Gadget read task detects output reports `0x10`/`0x11`, decodes the 8-byte
  Switch rumble arrays → amplitude (standard HF/LF envelope decode).
- Sends `Rumble{slot, magnitude}` to `bridge.rs`, which maps to the Xbox pad's
  `gilrs::ff::Effect` (`Weak`/`Strong` magnitude, like `go.rs:50-70` but
  amplitude-scaled instead of binary on/off).

## 8. `bridge.rs` flow

1. `gilrs::Gilrs::new()`; on `Connected` → assign lowest free slot, create
   gilrs `Effect`; `Disconnected` → free slot (sends neutral).
2. Poll loop at polling rate: for each mapped Xbox pad, `mapping.rs` →
   `SwitchInput` → send to that slot's gadget channel; idle slots get neutral
   `SwitchInput`.
3. Apply incoming `Rumble` events to the matching pad's effect.
4. `Exiting` → teardown gadget.

## 9. Prerequisites (Pi 4 setup)

- Enable OTG: `dtoverlay=dwc2` in `/boot/firmware/config.txt`, `dwc2` +
  `libcomposite` in `/etc/modules`, reboot.
- Install **xone** kernel driver (medusalix) for the official Xbox wireless
  dongle; pair controllers. Verify they appear as gamepads.
- Rust toolchain; cross-compile via existing `Cross.toml` (ARM64/ARMv7) or
  build natively on the Pi.

## 10. Build / verify

- Cross-compile for Pi 4 (or build natively).
- Run as root on Pi 4; USB-C → Switch (through powered hub). Pair Xbox pads.
- Verify: Switch shows 4 Pro Controllers; inputs map correctly; rumble works;
  disconnecting an Xbox pad leaves its slot idle (Switch keeps the controller
  connected, just no input).

## 11. Risks

- **Switch 2** negotiation untested (same USB HID expected; may need extra
  subcommand responses).
- **xone** kernel module must match Pi 4 kernel; fallback = Xbox-over-Bluetooth.
- Idle slots occupy Switch player slots even when no Xbox present (expected per
  chosen model).

## 12. Implementation phases

1. Scaffold Cargo project; `models.rs` + `switch_proto.rs` (port `nscon.go`,
   single hard-coded `/dev/hidg0`).
2. `gadget.rs` `GadgetManager` (N from arg), open files, teardown.
3. `mapping.rs` + `bridge.rs`: gilrs enumerate + map + drive gadget (single
   controller end-to-end).
4. Fixed **N=4** slots with idle/neutral handling + slot assignment.
5. Rumble decode + gilrs Effect forward.
6. Hardening: bounds checks, clean teardown, CLI flags, debug logging (port
   `LogLevel`).

## 13. References

- `HIDtoVPADNetworkClientCli/` — Rust client architecture to mirror.
- `raspberry-switch-control/nscontroller/nscon.go` — Switch Pro HID protocol to port.
- `raspberry-switch-control/nscontroller/xboxjoystick.go` — Xbox→Switch mapping.
- `raspberry-switch-control/nscontroller/scripts/switch-controller-gadget` — gadget configfs layout / report descriptor.
- Based on: https://github.com/mzyy94/nscon
