//! Mount transform and scaling for Wiimote motion.
//!
//! The Wiimote is strapped to the Xbox controller at a fixed, arbitrary
//! orientation. This module rotates its raw gyro/accel into the Switch Pro
//! Controller frame (via a constant mount matrix) and resamples to the report
//! rate. It also converts real units (deg/s, g) into the Switch's raw int16
//! units, matching the gyro/accel calibration served from SPI ROM.

use crate::models::Motion;
use crate::wiimote::RawMotion;

/// Radians per second per raw LSB, for a gyro calibration scale of 0x0100.
/// Standard Pro Controller convention: raw * 0.0025 = rad/s.
const GYRO_RAD_PER_LSB: f64 = 0.0025;
/// g per raw LSB, for an accel calibration scale of 0x0100.
/// Standard Pro Controller convention: raw * 0.0005 = g.
const ACCEL_G_PER_LSB: f64 = 0.0005;

/// Convert deg/s to the raw int16 gyro value expected by the Switch.
fn gyro_raw(deg_s: f64) -> i16 {
    let rad_s = deg_s.to_radians();
    (rad_s / GYRO_RAD_PER_LSB).round() as i16
}

/// Convert g to the raw int16 accel value expected by the Switch.
fn accel_raw(g: f64) -> i16 {
    (g / ACCEL_G_PER_LSB).round() as i16
}

/// Convert a calibrated Wiimote sample into Switch int16 units.
///
/// The current mount matrix is the identity (the Wiimote's yaw/roll/pitch are
/// mapped directly onto the Switch's gyro axes). Replace this with a fixed
/// 3x3 rotation once the physical mounting orientation is known.
pub fn to_switch_motion(raw: &RawMotion) -> Motion {
    Motion {
        gyro: [
            gyro_raw(raw.gyro_deg_s[0]),
            gyro_raw(raw.gyro_deg_s[1]),
            gyro_raw(raw.gyro_deg_s[2]),
        ],
        accel: [
            accel_raw(raw.accel_g[0]),
            accel_raw(raw.accel_g[1]),
            accel_raw(raw.accel_g[2]),
        ],
    }
}
