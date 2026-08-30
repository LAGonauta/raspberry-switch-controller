# Plan: Xbox Controller Battery Level Display on Switch

## Overview

Add battery level reporting from Xbox wireless controllers to the Nintendo Switch. The Switch Pro Controller protocol includes a battery field in the `0x30` input report (byte `[12]`), which is currently always zero. This plan implements battery level display by:

1. Reading battery percentage from Xbox controllers via gilrs `power_info()`
2. Encoding the percentage into the Switch's 5-level battery scale
3. Sending it in the standard input report

## Current State

- **Gilrs provides battery data**: `PowerInfo::Discharging(u8)` and `PowerInfo::Charging(u8)` with 0-100% percentage
- **Switch protocol**: Byte `[12]` in `0x30` report contains battery/connection status
- **No battery flow**: Current implementation sets byte `[12]` to 0x00
- **Reference implementation**: `openpuck/OpenPuck/mode_switch_pro.cpp` shows proper encoding

## Implementation Phases

### Phase 1: Data Model (`src/models.rs`)

**Goal**: Add battery field to the input data structure

1. Add `battery: u8` field to `SwitchInput` struct (line 19)
   - Value range: 0-4 (0=empty, 1=critical, 2=low, 3=medium, 4=full)
   - Default: 0 (empty)
2. Update `NEUTRAL_INPUT` constant (line 48) to set `battery: 0`
3. This is backward-compatible; existing code ignores the new field

### Phase 2: Battery Acquisition (`src/bridge.rs`)

**Goal**: Read battery data from Xbox controllers

1. In the poll loop (line 275), after getting gamepad state:
   ```rust
   let battery = match gamepad.power_info() {
       gilrs::PowerInfo::Discharging(pct) => percentage_to_level(pct),
       gilrs::PowerInfo::Charging(pct) => percentage_to_level(pct) | 0x10, // charging flag
       gilrs::PowerInfo::Charged => 0x40 | 4, // charged + full
       gilrs::PowerInfo::Wired => 0x08, // wired/USB powered
       _ => 0, // unknown/empty
   };
   ```
2. Add helper function `percentage_to_level(pct: u8) -> u8`:
   ```rust
   fn percentage_to_level(pct: u8) -> u8 {
       if pct >= 70 { 4 }      // full
       else if pct >= 50 { 3 } // medium
       else if pct >= 30 { 2 } // low
       else if pct >= 10 { 1 } // critical
       else { 0 }              // empty
   }
   ```
3. Pass battery value to `SwitchInput` in the mapping (line 276)
4. For idle slots (no controller), keep `battery: 0` (empty)

### Phase 3: Battery Encoding (`src/switch_proto.rs`)

**Goal**: Encode battery into the Switch protocol format

1. The Switch battery byte format (from `openpuck`):
   - Bits `[7:5]`: capacity (0-4)
   - Bit `4`: charging flag
   - Bit `0`: host_powered (USB)
2. Update `input_report()` function (line 146) to set byte `[12]`:
   ```rust
   pub fn input_report(count: u8, input: &SwitchInput) -> Vec<u8> {
       let input_buffer = encode_input(input);
       let mut report = packet(0x30, count, &input_buffer);
       report[12] = input.battery; // battery/connection byte
       report
   }
   ```
3. Alternatively, modify `encode_input()` to include battery, but the current 11-byte buffer doesn't include it - it's at offset 12 in the full report.

### Phase 4: Integration

**Goal**: Wire the data flow

1. **`src/bridge.rs`**: After line 276, set `input.battery` from the controller's power info
2. **`src/mapping.rs`**: Optionally add battery to the `poll()` return, but simpler to do it in `bridge.rs` since it has direct access to `power_info()`
3. **No changes needed** to `gadget.rs` - it already sends the full report

### Phase 5: Testing & Verification

**Goal**: Hardware validation

1. Connect Xbox wireless controller to Raspberry Pi
2. Run with `sudo ./target/release/raspberry-switch-controller`
3. Check Switch controller settings screen for battery indicator
4. Test scenarios:
   - Controller at various battery levels (if possible)
   - Controller charging via USB
   - Controller disconnect/reconnect
   - Multiple controllers (verify per-slot battery)

## Technical Details

### Switch Battery Byte Format

From `openpuck/OpenPuck/mode_switch_pro.cpp:387-409`:
```
bits[7:5] = battery capacity (0=empty .. 4=full)
bit4 = charging flag
bit0 = host_powered (USB-powered)
```

### Gilrs PowerInfo Mapping

| Gilrs State | Switch Byte | Description |
|-------------|-------------|-------------|
| `Discharging(pct)` | `level << 5` | Battery level (0-4) |
| `Charging(pct)` | `(level << 5) \| 0x10` | Charging flag set |
| `Charged` | `(4 << 5) \| 0x10` | Fully charged |
| `Wired` | `0x01` | USB powered |
| `Unknown` | `0x00` | No info |

### Percentage to Level Conversion

```rust
fn percentage_to_level(pct: u8) -> u8 {
    match pct {
        0..=9 => 0,    // empty
        10..=29 => 1,  // critical
        30..=49 => 2,  // low
        50..=69 => 3,  // medium
        _ => 4,        // full (70-100)
    }
}
```

## Files to Modify

1. **`src/models.rs`** - Add `battery: u8` field to `SwitchInput`
2. **`src/bridge.rs`** - Read `power_info()` and set battery value
3. **`src/switch_proto.rs`** - Set byte `[12]` in `input_report()`

## Optional Enhancements

1. **Charging state detection**: Track if controller is charging (bit 4)
2. **Battery level caching**: Update battery level periodically (not every poll)
3. **CLI flag**: Add `--no-battery` to disable battery reporting
4. **Logging**: Log battery level changes for debugging

## Risks & Mitigations

1. **Xbox controller battery reporting**: Some controllers may not report battery via gilrs
   - Mitigation: Default to 0 (empty) if unavailable
2. **Performance**: Extra `power_info()` call per poll
   - Mitigation: `power_info()` is cached in gilrs, minimal overhead
3. **Switch behavior**: Unknown how Switch handles rapid battery level changes
   - Mitigation: Use the 5-level scale which updates infrequently

## Verification

1. Build with `cargo build --release`
2. Run on Raspberry Pi 4 with Xbox wireless controller
3. Check Switch system settings → Controllers → Battery level
4. Verify battery indicator matches controller's actual state
5. Test multiple controllers show individual battery levels