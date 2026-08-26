//! Wiimote (Wii Remote Plus) motion acquisition.
//!
//! Wraps `wiimote-rs`: connects to a Wii Remote Plus over classic Bluetooth,
//! activates its MotionPlus gyro + accelerometer, and publishes calibrated
//! motion samples. The raw degrees/g values are consumed by `crate::motion`,
//! which applies the mount transform and rescales them into Switch int16 units.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use log::{error, info, warn};
use wiimote_rs::input::InputReport;
use wiimote_rs::output::{DataReporingMode, OutputReport, PlayerLedFlags};
use wiimote_rs::prelude::*;

use crate::models::AppState;

/// A calibrated motion sample in real units.
/// Gyro in deg/s (yaw, roll, pitch), accel in g (x, y, z).
#[derive(Clone, Copy, Debug, Default)]
pub struct RawMotion {
    pub gyro_deg_s: [f64; 3],
    pub accel_g: [f64; 3],
}

/// Shared handle to the latest Wiimote motion sample.
pub type MotionHandle = Arc<Mutex<Option<RawMotion>>>;

/// Spawn the Wiimote reader thread. It blocks waiting for a Wii Remote Plus to
/// connect, then continuously publishes motion samples to `handle` until the
/// app is shutting down.
pub fn run(handle: MotionHandle, state: Arc<Mutex<AppState>>) {
    thread::spawn(move || loop {
        if state.lock().unwrap().is_exiting() {
            break;
        }

        let manager = WiimoteManager::get_instance();
        let new_devices = {
            let manager = manager.lock().unwrap();
            manager.new_devices_receiver()
        };

        match new_devices.recv_timeout(Duration::from_millis(500)) {
            Ok(device) => {
                info!("Wiimote connected");
                run_device(device, &handle, &state);
            }
            Err(_) => continue,
        }
    });
}

/// Drive a single connected Wiimote: initialize MotionPlus, set reporting mode,
/// and stream motion samples until disconnect or shutdown.
fn run_device(
    device: Arc<Mutex<WiimoteDevice>>,
    handle: &MotionHandle,
    state: &Arc<Mutex<AppState>>,
) {
    let led = OutputReport::PlayerLed(PlayerLedFlags::LED_1);
    if device.lock().unwrap().write(&led).is_err() {
        warn!("Wiimote disconnected during setup");
        return;
    }

    let (accel_cal, motion_cal) = {
        let wiimote = device.lock().unwrap();
        if let Some(motion_plus) = wiimote.motion_plus() {
            if let Err(e) = motion_plus.initialize(&wiimote) {
                warn!("MotionPlus init failed: {:?}", e);
            }
            if let Err(e) = motion_plus.change_mode(&wiimote, MotionPlusMode::Active) {
                warn!("MotionPlus activate failed: {:?}", e);
            }
        }
        (
            wiimote
                .accelerometer_calibration()
                .expect("Wiimote should have accelerometer calibration")
                .clone(),
            wiimote.motion_plus().map(MotionPlus::calibration),
        )
    };

    let reporting_mode = OutputReport::DataReportingMode(DataReporingMode {
        continuous: false,
        mode: 0x35, // Core Buttons + Accelerometer + 16 Extension Bytes
    });
    if device.lock().unwrap().write(&reporting_mode).is_err() {
        warn!("Wiimote disconnected during reporting-mode setup");
        return;
    }

    loop {
        if state.lock().unwrap().is_exiting() {
            break;
        }

        let report = match device.lock().unwrap().read_timeout(50) {
            Ok(r) => r,
            Err(_) => {
                warn!("Wiimote disconnected");
                break;
            }
        };

        match report {
            InputReport::StatusInformation(_) => {
                // Host must re-assert the reporting mode after a status report.
                let _ = device.lock().unwrap().write(&reporting_mode);
            }
            InputReport::DataReport(0x35, wiimote_data) => {
                let accel = AccelerometerData::from_normal_reporting(&wiimote_data.data);
                let (ax, ay, az) = accel_cal.get_acceleration(&accel);

                let mut mp_buf = [0u8; 6];
                mp_buf.copy_from_slice(&wiimote_data.data[5..11]);
                if let Ok(mp_data) = MotionPlusData::try_from(mp_buf) {
                    if let Some(cal) = &motion_cal {
                        let (yaw, roll, pitch) = cal.get_angular_velocity(&mp_data);
                        let sample = RawMotion {
                            gyro_deg_s: [yaw, roll, pitch],
                            accel_g: [ax, ay, az],
                        };
                        if let Ok(mut h) = handle.lock() {
                            *h = Some(sample);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    error!("Wiimote stream ended");
}
