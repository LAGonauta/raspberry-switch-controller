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
    packet(0x30, count, &input_buffer)
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

/// Amplitude lookup table ported from hid-nintendo.c `joycon_rumble_amplitudes`
/// (itself from dekuNukem's rumble_data_table.md). Entry = (hf_code, lf_code, amp)
/// where hf_code is data byte 1 with the LSB masked off, lf_code is data byte 3
/// plus bit 7 of data byte 2 shifted to bit 15, and amp is 0..=1003.
const RUMBLE_AMPLITUDES: &[(u8, u16, u16)] = &[
    (0x00, 0x0040, 0),
    (0x02, 0x8040, 10),
    (0x04, 0x0041, 12),
    (0x06, 0x8041, 14),
    (0x08, 0x0042, 17),
    (0x0a, 0x8042, 20),
    (0x0c, 0x0043, 24),
    (0x0e, 0x8043, 28),
    (0x10, 0x0044, 33),
    (0x12, 0x8044, 40),
    (0x14, 0x0045, 47),
    (0x16, 0x8045, 56),
    (0x18, 0x0046, 67),
    (0x1a, 0x8046, 80),
    (0x1c, 0x0047, 95),
    (0x1e, 0x8047, 112),
    (0x20, 0x0048, 117),
    (0x22, 0x8048, 123),
    (0x24, 0x0049, 128),
    (0x26, 0x8049, 134),
    (0x28, 0x004a, 140),
    (0x2a, 0x804a, 146),
    (0x2c, 0x004b, 152),
    (0x2e, 0x804b, 159),
    (0x30, 0x004c, 166),
    (0x32, 0x804c, 173),
    (0x34, 0x004d, 181),
    (0x36, 0x804d, 189),
    (0x38, 0x004e, 198),
    (0x3a, 0x804e, 206),
    (0x3c, 0x004f, 215),
    (0x3e, 0x804f, 225),
    (0x40, 0x0050, 230),
    (0x42, 0x8050, 235),
    (0x44, 0x0051, 240),
    (0x46, 0x8051, 245),
    (0x48, 0x0052, 251),
    (0x4a, 0x8052, 256),
    (0x4c, 0x0053, 262),
    (0x4e, 0x8053, 268),
    (0x50, 0x0054, 273),
    (0x52, 0x8054, 279),
    (0x54, 0x0055, 286),
    (0x56, 0x8055, 292),
    (0x58, 0x0056, 298),
    (0x5a, 0x8056, 305),
    (0x5c, 0x0057, 311),
    (0x5e, 0x8057, 318),
    (0x60, 0x0058, 325),
    (0x62, 0x8058, 332),
    (0x64, 0x0059, 340),
    (0x66, 0x8059, 347),
    (0x68, 0x005a, 355),
    (0x6a, 0x805a, 362),
    (0x6c, 0x005b, 370),
    (0x6e, 0x805b, 378),
    (0x70, 0x005c, 387),
    (0x72, 0x805c, 395),
    (0x74, 0x005d, 404),
    (0x76, 0x805d, 413),
    (0x78, 0x005e, 422),
    (0x7a, 0x805e, 431),
    (0x7c, 0x005f, 440),
    (0x7e, 0x805f, 450),
    (0x80, 0x0060, 460),
    (0x82, 0x8060, 470),
    (0x84, 0x0061, 480),
    (0x86, 0x8061, 491),
    (0x88, 0x0062, 501),
    (0x8a, 0x8062, 512),
    (0x8c, 0x0063, 524),
    (0x8e, 0x8063, 535),
    (0x90, 0x0064, 547),
    (0x92, 0x8064, 559),
    (0x94, 0x0065, 571),
    (0x96, 0x8065, 584),
    (0x98, 0x0066, 596),
    (0x9a, 0x8066, 609),
    (0x9c, 0x0067, 623),
    (0x9e, 0x8067, 636),
    (0xa0, 0x0068, 650),
    (0xa2, 0x8068, 665),
    (0xa4, 0x0069, 679),
    (0xa6, 0x8069, 694),
    (0xa8, 0x006a, 709),
    (0xaa, 0x806a, 725),
    (0xac, 0x006b, 741),
    (0xae, 0x806b, 757),
    (0xb0, 0x006c, 773),
    (0xb2, 0x806c, 790),
    (0xb4, 0x006d, 808),
    (0xb6, 0x806d, 825),
    (0xb8, 0x006e, 843),
    (0xba, 0x806e, 862),
    (0xbc, 0x006f, 881),
    (0xbe, 0x806f, 900),
    (0xc0, 0x0070, 920),
    (0xc2, 0x8070, 940),
    (0xc4, 0x0071, 960),
    (0xc6, 0x8071, 981),
    (0xc8, 0x0072, 1003),
];

/// Look up the amplitude for an HF-band code (even byte).
/// Uses direct index lookup since HF codes are even numbers 0x00..=0xc8.
fn hf_amp(hf_code: u8) -> u16 {
    // HF codes are even numbers from 0x00 to 0xc8 (101 entries)
    // Index = hf_code / 2, clamped to valid range
    let idx = (hf_code / 2) as usize;
    if idx < RUMBLE_AMPLITUDES.len() {
        RUMBLE_AMPLITUDES[idx].2
    } else {
        // Find closest match for out-of-range codes
        RUMBLE_AMPLITUDES
            .iter()
            .min_by_key(|(h, _, _)| (*h as i16 - hf_code as i16).abs())
            .map(|(_, _, amp)| *amp)
            .unwrap_or(0)
    }
}

/// Look up the amplitude for an LF-band code (bit 15 + low byte).
/// Uses HashMap for O(1) lookup instead of linear search.
fn lf_amp(lf_code: u16) -> u16 {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static LF_MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();
    let map = LF_MAP.get_or_init(|| {
        RUMBLE_AMPLITUDES
            .iter()
            .map(|(_, l, amp)| (*l, *amp))
            .collect()
    });

    map.get(&lf_code).copied().unwrap_or_else(|| {
        // Find closest match for codes not in table
        RUMBLE_AMPLITUDES
            .iter()
            .min_by_key(|(_, l, _)| (*l as i32 - lf_code as i32).abs())
            .map(|(_, _, amp)| *amp)
            .unwrap_or(0)
    })
}

/// Decode one 4-byte rumble group into the louder of its HF/LF band
/// amplitudes (0..=1003). Layout per dekuNukem: byte 0 = HF freq high,
/// byte 1 = HF freq LSB (bit 0) + HF amp code (even), byte 2 = LF freq
/// (bits 0-6) + LF amp code bit 15, byte 3 = LF amp code low byte.
fn decode_rumble_group(group: &[u8]) -> u16 {
    let hf_code = group[1] & 0xfe;
    let lf_code = (((group[2] & 0x80) as u16) << 8) | group[3] as u16;
    hf_amp(hf_code).max(lf_amp(lf_code))
}

/// Decode the 8-byte rumble payload of a `0x10` (rumble-only) output report
/// into an amplitude 0..=255 (loudest band of either actuator). Layout per
/// hid-nintendo `struct joycon_rumble_output`: [0] = report id,
/// [1] = packet counter, [2..6] = left actuator, [6..10] = right actuator.
pub fn decode_rumble(report: &[u8]) -> Option<u8> {
    if report.len() < 10 || report[0] != 0x10 {
        return None;
    }
    let left = decode_rumble_group(&report[2..6]);
    let right = decode_rumble_group(&report[6..10]);
    let amp = left.max(right) as u32;
    Some((amp * 255 / RUMBLE_AMP_MAX as u32) as u8)
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
