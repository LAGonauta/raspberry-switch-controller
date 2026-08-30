//! Switch Pro Controller HID protocol helpers.
//!
//! Ported from `nscontroller/nscon.go`. Provides report encoding
//! (`encode_input` / `pack_shorts`), the SPI ROM data map, and packet
//! construction for input reports, handshake and subcommand responses.

use log::warn;

use crate::models::{Stick, SwitchInput};

/// SPI ROM data served in response to subcommand `0x10` reads.
/// Byte-identical to `nscon.go` (`SPI_ROM_DATA`); the calibration magic must
/// sit at 0x8026 and the factory stick params at 0x6080.
pub const SPI_ROM_DATA: &[(&[u8], &[u8])] = &[
    (
        &[0x60],
        &[
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0x03, 0xa0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02,
            0xff, 0xff, 0xff, 0xff, 0xf0, 0xff, 0x89, 0x00, 0xf0, 0x01, 0x00, 0x40, 0x00, 0x40,
            0x00, 0x40, 0xf9, 0xff, 0x06, 0x00, 0x09, 0x00, 0xe7, 0x3b, 0xe7, 0x3b, 0xe7, 0x3b,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xba, 0x15, 0x62, 0x11, 0xb8, 0x7f, 0x29, 0x06, 0x5b,
            0xff, 0xe7, 0x7e, 0x0e, 0x36, 0x56, 0x9e, 0x85, 0x60, 0xff, 0x32, 0x32, 0x32, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0x50, 0xfd, 0x00, 0x00, 0xc6, 0x0f, 0x0f, 0x30, 0x61, 0x96, 0x30, 0xf3,
            0xd4, 0x14, 0x54, 0x41, 0x15, 0x54, 0xc7, 0x79, 0x9c, 0x33, 0x36, 0x63, 0x0f, 0x30,
            0x61, 0x96, 0x30, 0xf3, 0xd4, 0x14, 0x54, 0x41, 0x15, 0x54, 0xc7, 0x79, 0x9c, 0x33,
            0x36, 0x63, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ],
    ),
    (
        &[0x80],
        &[
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xb2, 0xa1, 0xbe, 0xff,
            0x3e, 0x00, 0xf0, 0x01, 0x00, 0x40, 0x00, 0x40, 0x00, 0x40, 0xfe, 0xff, 0xfe, 0xff,
            0x08, 0x00, 0xe7, 0x3b, 0xe7, 0x3b, 0xe7, 0x3b,
        ],
    ),
];

fn spi_data(addr: u8) -> Option<&'static [u8]> {
    for (key, data) in SPI_ROM_DATA {
        if key[0] == addr {
            return Some(data);
        }
    }
    None
}

fn bit_input(input: bool, offset: u8) -> u8 {
    if input {
        1 << offset
    } else {
        0
    }
}

/// Pack two 12-bit sticks into 3 bytes (little-endian nibbles).
pub fn pack_shorts(short1: u16, short2: u16) -> [u8; 3] {
    [
        (short1 & 0xff) as u8,
        (((short2 << 4) & 0xf0) | ((short1 >> 8) & 0x0f)) as u8,
        ((short2 >> 4) & 0xff) as u8,
    ]
}

/// Clamp a float in [-1.0 .. 1.0] into a 12-bit value (0..4095).
/// Fixes the overflow from BUGS.md #17.
fn clamp_stick(v: f32) -> u16 {
    let v = v.clamp(-1.0, 1.0);
    ((1.0 + v) * 2047.5).round() as u16
}

fn stick_12bit(s: &Stick) -> (u16, u16) {
    (clamp_stick(s.x), clamp_stick(s.y))
}

/// Encode a `SwitchInput` into the 11-byte `0x81` input buffer.
pub fn encode_input(input: &SwitchInput) -> [u8; 11] {
    let left = bit_input(input.y, 0)
        | bit_input(input.x, 1)
        | bit_input(input.b, 2)
        | bit_input(input.a, 3)
        | bit_input(input.r, 6)
        | bit_input(input.zr, 7);

    let center = bit_input(input.minus, 0)
        | bit_input(input.plus, 1)
        | bit_input(input.right_stick_press, 2)
        | bit_input(input.left_stick_press, 3)
        | bit_input(input.home, 4)
        | bit_input(input.capture, 5);

    let right = bit_input(input.dpad_down, 0)
        | bit_input(input.dpad_up, 1)
        | bit_input(input.dpad_right, 2)
        | bit_input(input.dpad_left, 3)
        | bit_input(input.l, 6)
        | bit_input(input.zl, 7);

    let (lx, ly) = stick_12bit(&input.left_stick);
    let (rx, ry) = stick_12bit(&input.right_stick);
    let left_stick = pack_shorts(lx, ly);
    let right_stick = pack_shorts(rx, ry);

    [
        0x81,
        left,
        center,
        right,
        left_stick[0],
        left_stick[1],
        left_stick[2],
        right_stick[0],
        right_stick[1],
        right_stick[2],
        0x00,
    ]
}

/// Build a 64-byte output packet: `[ack, cmd, payload..., pad]`.
///
/// Mirrors `write()` in `nscon.go`, including capping the payload at 62
/// bytes so the report can never exceed `report_length` (an oversized
/// payload would otherwise spill into a second, malformed HID report).
pub fn packet(ack: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(64);
    data.push(ack);
    data.push(cmd);
    if payload.len() > 62 {
        warn!(
            "Packet payload truncated from {} to 62 bytes",
            payload.len()
        );
    }
    data.extend_from_slice(&payload[..payload.len().min(62)]);
    data.resize(64, 0);
    data
}

/// Build a full `0x30` input report: `[0x30, count, input_buffer, pad]`.
pub fn input_report(count: u8, input: &SwitchInput) -> Vec<u8> {
    let input_buffer = encode_input(input);
    let mut report = packet(0x30, count, &input_buffer);
    report[12] = input.battery; // battery/connection byte
    report
}

/// The two handshake packets sent on connect (from `Connect()`).
pub fn handshake_packets() -> Vec<Vec<u8>> {
    vec![packet(0x81, 0x03, &[]), packet(0x81, 0x01, &[0x00, 0x03])]
}

/// Build a `0x21` subcommand response: input buffer + ack + subcommand + data.
pub fn subcommand_response(
    count: u8,
    input: &SwitchInput,
    ack: bool,
    sub_cmd: u8,
    data: &[u8],
) -> Vec<u8> {
    let mut buf = encode_input(input).to_vec();
    let ack_byte = if ack {
        if data.is_empty() {
            0x80
        } else {
            0x80 | sub_cmd
        }
    } else {
        0x00
    };
    buf.push(ack_byte);
    buf.push(sub_cmd);
    buf.extend_from_slice(data);
    packet(0x21, count, &buf)
}

/// Per-slot MAC address. Each slot gets a distinct identity, derived from the
/// slot index so no two slots (or the legacy fixed MAC) collide.
fn slot_mac(slot: usize) -> [u8; 6] {
    let mut mac: [u8; 6] = [0x00, 0x00, 0x5e, 0x00, 0x53, 0x5e];
    mac[5] = mac[5].wrapping_add(slot as u8 + 1);
    mac
}

/// Device-info response for `0x80/0x01` (host output report type 0x01).
pub fn device_info_response(slot: usize, cmd: u8) -> Vec<u8> {
    let mut payload = vec![0x00, 0x03];
    payload.extend_from_slice(&slot_mac(slot));
    packet(0x81, cmd, &payload)
}

/// Simple-ack response for `0x80/0x02` and `0x80/0x03`.
pub fn simple_ack_response(cmd: u8) -> Vec<u8> {
    packet(0x81, cmd, &[])
}

/// Maximum decoded rumble amplitude (hid-nintendo `joycon_max_rumble_amp`).
const RUMBLE_AMP_MAX: u16 = 1003;

/// Minimum log2 amplitude unit (-8.0 * 32 = -256).
const HDR_AMP_MIN: i16 = -256;
/// Off state (silence).
const HDR_AMP_OFF: i16 = HDR_AMP_MIN;

/// Per-motor running band amplitudes (low and high).
#[derive(Clone, Copy)]
struct HdrBands {
    lo: i16, // low-band amplitude, 1/32 log2 units
    hi: i16, // high-band amplitude
}

/// Per-slot, per-motor (0=left, 1=right) state.
/// Static mutable because each slot is handled by a single thread.
static mut HDR_STATE: [[HdrBands; 2]; 8] = [[HdrBands {
    lo: HDR_AMP_OFF,
    hi: HDR_AMP_OFF,
}; 2]; 8];

/// Amplitude lookup table: log2 units (-256..0) -> linear amplitude (0..1003).
/// Built once at startup.
static HDR_LEVEL: std::sync::OnceLock<[u16; 257]> = std::sync::OnceLock::new();

fn hdr_build_levels() -> &'static [u16; 257] {
    HDR_LEVEL.get_or_init(|| {
        let mut table = [0u16; 257];
        for (i, entry) in table.iter_mut().enumerate() {
            let units = (i as i16) + HDR_AMP_MIN; // -256..0
            let lin = (units as f32) / 32.0;
            let amp = if lin >= -7.9375 { lin.exp2() } else { 0.0 };
            let amp = amp.clamp(0.0, 1.0);
            *entry = (amp * RUMBLE_AMP_MAX as f32).round() as u16;
        }
        table
    })
}

/// Absolute 7-bit amplitude code -> log2 units (1/32).
/// Piecewise linear with slopes 1/4, 1/16, 1/32.
fn hdr_amp7(code: u8) -> i16 {
    match code {
        0 => HDR_AMP_MIN,
        1..=15 => (8 * code as i16) - 248,
        16..=31 => (2 * code as i16) - 158,
        _ => code as i16 - 127,
    }
}

/// Apply a compact 5-bit command to the running amplitude.
/// Returns new amplitude (clamped to [-256, 0]).
fn hdr_amp5(code: u8, cur: i16) -> i16 {
    let v = match code {
        0 => HDR_AMP_OFF,
        1..=11 => -16 * (code as i16 - 1), // presets 0..-5.0
        17..=19 => cur + 4,                // step up +0.125
        20..=22 => cur + 1,                // step up +0.03125
        26..=28 => cur - 1,                // step down -0.03125
        29..=31 => cur - 4,                // step down -0.125
        _ => cur,                          // frequency-only command
    };
    v.clamp(HDR_AMP_MIN, 0)
}

/// Extract a bit-field from a 32-bit word.
fn hdr_field(w: u32, shift: u8, width: u8) -> u8 {
    ((w >> shift) & ((1u32 << width) - 1)) as u8
}

/// Decode one motor's 4 rumble bytes, advancing its state.
/// Returns the peak linear amplitude (0..1003) across all updates in this frame.
fn hdr_decode(slot: usize, motor: usize, bytes: &[u8; 4]) -> u16 {
    let w = u32::from_le_bytes(*bytes);
    let mode = hdr_field(w, 30, 2);
    let state = unsafe { &mut HDR_STATE[slot][motor] };
    let levels = hdr_build_levels();
    let mut peak = 0u16;

    macro_rules! sample {
        ($amp:expr) => {
            // Convert log2 units to linear amplitude.
            let idx = ($amp - HDR_AMP_MIN) as usize;
            let lvl = levels[idx];
            if lvl > peak {
                peak = lvl;
            }
        };
    }

    match mode {
        0 => {
            // Hold - no change.
            sample!(state.lo);
            sample!(state.hi);
        }
        1 => {
            // Single 5-bit or 7-bit update, or 7-bit + two 5-bit burst.
            if (w & 0xFFFFF) == 0 {
                // Single 5-bit update for both bands.
                state.lo = hdr_amp5(hdr_field(w, 25, 5), state.lo);
                state.hi = hdr_amp5(hdr_field(w, 20, 5), state.hi);
                sample!(state.lo);
                sample!(state.hi);
            } else if (w & 0x3) == 0 {
                // Single 7-bit absolute for both bands.
                state.lo = hdr_amp7(hdr_field(w, 23, 7));
                state.hi = hdr_amp7(hdr_field(w, 9, 7));
                sample!(state.lo);
                sample!(state.hi);
            } else {
                // 7-bit for one band + two 5-bit updates.
                let want_hi = (w & 1) != 0;
                let is_freq = ((w >> 2) & 1) != 0;
                if !is_freq {
                    if want_hi {
                        state.hi = hdr_amp7(hdr_field(w, 23, 7));
                    } else {
                        state.lo = hdr_amp7(hdr_field(w, 23, 7));
                    }
                }
                sample!(state.lo);
                sample!(state.hi);
                // First 5-bit update pair.
                state.lo = hdr_amp5(hdr_field(w, 18, 5), state.lo);
                state.hi = hdr_amp5(hdr_field(w, 13, 5), state.hi);
                sample!(state.lo);
                sample!(state.hi);
                // Second 5-bit update pair.
                state.lo = hdr_amp5(hdr_field(w, 8, 5), state.lo);
                state.hi = hdr_amp5(hdr_field(w, 3, 5), state.hi);
                sample!(state.lo);
                sample!(state.hi);
            }
        }
        2 => {
            // Two 5-bit updates, or 7-bit + 5-bit, then a 5-bit update.
            if (w & 0x3FF) == 0 {
                // Two 5-bit updates.
                state.lo = hdr_amp5(hdr_field(w, 25, 5), state.lo);
                state.hi = hdr_amp5(hdr_field(w, 20, 5), state.hi);
                sample!(state.lo);
                sample!(state.hi);
                state.lo = hdr_amp5(hdr_field(w, 15, 5), state.lo);
                state.hi = hdr_amp5(hdr_field(w, 10, 5), state.hi);
                sample!(state.lo);
                sample!(state.hi);
            } else {
                // 7-bit + 5-bit, then a 5-bit update.
                if (w & 1) != 0 {
                    state.hi = hdr_amp7(hdr_field(w, 23, 7));
                    state.lo = hdr_amp5(hdr_field(w, 18, 5), state.lo);
                } else {
                    state.lo = hdr_amp7(hdr_field(w, 23, 7));
                    state.hi = hdr_amp5(hdr_field(w, 18, 5), state.hi);
                }
                sample!(state.lo);
                sample!(state.hi);
                state.lo = hdr_amp5(hdr_field(w, 13, 5), state.lo);
                state.hi = hdr_amp5(hdr_field(w, 8, 5), state.hi);
                sample!(state.lo);
                sample!(state.hi);
            }
        }
        3 => {
            // Three 5-bit updates.
            state.lo = hdr_amp5(hdr_field(w, 25, 5), state.lo);
            state.hi = hdr_amp5(hdr_field(w, 20, 5), state.hi);
            sample!(state.lo);
            sample!(state.hi);
            state.lo = hdr_amp5(hdr_field(w, 15, 5), state.lo);
            state.hi = hdr_amp5(hdr_field(w, 10, 5), state.hi);
            sample!(state.lo);
            sample!(state.hi);
            state.lo = hdr_amp5(hdr_field(w, 5, 5), state.lo);
            state.hi = hdr_amp5(hdr_field(w, 0, 5), state.hi);
            sample!(state.lo);
            sample!(state.hi);
        }
        _ => unreachable!(),
    }
    peak
}

/// Reset rumble state for a slot (call on disconnect).
pub fn hdr_reset_slot(slot: usize) {
    unsafe {
        HDR_STATE[slot] = [HdrBands {
            lo: HDR_AMP_OFF,
            hi: HDR_AMP_OFF,
        }; 2];
    }
}

/// Decode the 8-byte rumble payload of a `0x10` (rumble-only) output report
/// into left/right amplitudes 0..=255. Layout per
/// hid-nintendo `struct joycon_rumble_output`: [0] = report id,
/// [1] = packet counter, [2..6] = left actuator, [6..10] = right actuator.
pub fn decode_rumble(report: &[u8], slot: usize) -> Option<(u8, u8)> {
    if report.len() < 10 || report[0] != 0x10 {
        return None;
    }
    let left = hdr_decode(slot, 0, report[2..6].try_into().unwrap());
    let right = hdr_decode(slot, 1, report[6..10].try_into().unwrap());
    let left_mag = (left as u32 * 255 / RUMBLE_AMP_MAX as u32) as u8;
    let right_mag = (right as u32 * 255 / RUMBLE_AMP_MAX as u32) as u8;
    Some((left_mag, right_mag))
}

/// Response for subcommand `0x21` (set report mode).
pub fn set_report_mode_response(count: u8, input: &SwitchInput, sub_cmd: u8) -> Vec<u8> {
    subcommand_response(
        count,
        input,
        true,
        sub_cmd,
        &[0x01, 0x00, 0xff, 0x00, 0x03, 0x00, 0x05, 0x01],
    )
}

/// Response for subcommand `0x01` (Bluetooth manual pairing).
pub fn pairing_response(count: u8, input: &SwitchInput, sub_cmd: u8) -> Vec<u8> {
    subcommand_response(count, input, true, sub_cmd, &[0x03, 0x01])
}

/// Response for subcommand `0x02` (request device info).
pub fn device_info_subcommand_response(
    count: u8,
    input: &SwitchInput,
    slot: usize,
    sub_cmd: u8,
) -> Vec<u8> {
    let mut data = vec![0x03, 0x48, 0x03, 0x02];
    data.extend_from_slice(&slot_mac(slot));
    data.extend_from_slice(&[0x03, 0x01]);
    subcommand_response(count, input, true, sub_cmd, &data)
}

/// Build an SPI ROM read response with bounds checking (BUGS.md #14/#22).
/// Returns `None` if the request is malformed or out of range.
pub fn spi_read_response(count: u8, input: &SwitchInput, report: &[u8]) -> Option<Vec<u8>> {
    if report.len() < 16 {
        return None;
    }
    let addr = report[12];
    let start = report[11] as usize;
    let len = report[15] as usize;
    let data = spi_data(addr)?;
    let end = start.checked_add(len)?;
    if end > data.len() || start > data.len() {
        return None;
    }
    let mut resp = report[11..16].to_vec();
    resp.extend_from_slice(&data[start..end]);
    Some(subcommand_response(count, input, true, report[10], &resp))
}
