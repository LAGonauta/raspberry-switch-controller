//! USB gadget transport.
//!
//! `GadgetManager` creates and tears down a composite USB gadget with N HID
//! functions via configfs. Each slot is driven by an independent task that
//! owns its `/dev/hidgN` file, runs the Switch handshake, reads host reports
//! (decoding rumble) and writes input reports on a ticker.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use flume::{Receiver, Sender};

use log::{warn, error};

use crate::models::{AppState, Rumble, SwitchInput, NEUTRAL_INPUT};
use crate::switch_proto;

const GADGET_BASE: &str = "/sys/kernel/config/usb_gadget";
const GADGET_NAME: &str = "switchbridge";
const REPORT_DESC_HEX: &str = "050115000904A1018530050105091901290A150025017501950A5500650081020509190B290E150025017501950481027501950281030B01000100A1000B300001000B310001000B320001000B35000100150027FFFF0000751095048102C00B39000100150025073500463B0165147504950181020509190F2912150025017501950481027508953481030600FF852109017508953F8103858109027508953F8103850109037508953F9183851009047508953F9183858009057508953F9183858209067508953F9183C0";

const REPORT_LEN: usize = 64;
const READ_BUF_LEN: usize = 128;

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    fs::write(path, content)
}

pub struct GadgetManager {
    root: PathBuf,
}

impl GadgetManager {
    pub fn new() -> Self {
        Self {
            root: Path::new(GADGET_BASE).join(GADGET_NAME),
        }
    }

    /// Create the gadget with `n` HID functions and bring it online.
    pub fn create(&self, n: usize) -> std::io::Result<()> {
        let base = &self.root;
        fs::create_dir_all(base)?;

        write_file(&base.join("idVendor"), "0x057e")?;
        write_file(&base.join("idProduct"), "0x2009")?;
        write_file(&base.join("bcdUSB"), "0x0200")?;
        write_file(&base.join("bDeviceClass"), "0")?;

        let strings = base.join("strings/0x409");
        fs::create_dir_all(&strings)?;
        write_file(&strings.join("serialnumber"), "000000000001")?;
        write_file(&strings.join("manufacturer"), "Nintendo Co., Ltd.")?;
        write_file(&strings.join("product"), "Pro Controller")?;

        let config = base.join("configs/c.1");
        fs::create_dir_all(&config)?;
        write_file(&config.join("MaxPower"), "500")?;
        write_file(&config.join("bmAttributes"), "0xa0")?;

        let report_desc = hex_to_bytes(REPORT_DESC_HEX);

        for i in 0..n {
            let func = base.join(format!("functions/hid.usb{}", i));
            fs::create_dir_all(&func)?;
            write_file(&func.join("protocol"), "0")?;
            write_file(&func.join("subclass"), "0")?;
            write_file(&func.join("report_length"), &format!("{}", REPORT_LEN))?;
            fs::write(func.join("report_desc"), &report_desc)?;
            std::os::unix::fs::symlink(&func, config.join(format!("hid.usb{}", i)))?;
        }

        let udc = self.find_udc()?;
        write_file(&base.join("UDC"), &udc)?;

        // Make gadget endpoints world-accessible.
        for i in 0..n {
            let dev = PathBuf::from(format!("/dev/hidg{}", i));
            if dev.exists() {
                if let Err(e) = fs::set_permissions(&dev, fs::Permissions::from_mode(0o666)) {
                    warn!(
                        "Unable to set permissions on {}: {}",
                        dev.display(),
                        e
                    );
                }
            }
        }

        Ok(())
    }

    fn find_udc(&self) -> std::io::Result<String> {
        let udc_dir = Path::new("/sys/class/udc");
        let mut udcs = fs::read_dir(udc_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        udcs.sort();
        udcs.into_iter()
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no UDC available"))
    }

    /// Tear the gadget down.
    pub fn destroy(&self) {
        let base = &self.root;
        if !base.exists() {
            return;
        }
        // Unbind from the UDC first, then remove everything we created:
        // config symlinks, the config's strings dir (auto-created by
        // configfs), each HID function, and finally the gadget root.
        let _ = write_file(&base.join("UDC"), "");
        remove_configfs_dir(&base.join("configs/c.1"));
        remove_configfs_dir(&base.join("functions"));
        remove_configfs_dir(&base.join("strings"));
        let _ = fs::remove_dir(base);
    }

    /// Path to a slot's HID device node.
    pub fn hidg_path(&self, slot: usize) -> String {
        format!("/dev/hidg{}", slot)
    }
}

/// Remove a configfs directory tree: user-created subdirectories (recursively)
/// and symlinks first; auto-generated attribute files cannot be deleted and
/// are dropped by the kernel when their parent is removed.
fn remove_configfs_dir(path: &Path) {
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            let is_dir = entry
                .file_type()
                .map(|t| t.is_dir() && !t.is_symlink())
                .unwrap_or(false);
            if is_dir {
                remove_configfs_dir(&child);
            } else {
                let _ = fs::remove_file(&child);
            }
        }
    }
    let _ = fs::remove_dir(path);
}

/// Run a single slot task. Owns the HID node for `slot`, runs the handshake,
/// reads host reports (rumble -> `rumble_tx`), and writes input reports at
/// `tick` from `input_rx`.
pub fn run_slot(
    slot: usize,
    path: &str,
    input_rx: Receiver<SwitchInput>,
    rumble_tx: Sender<Rumble>,
    tick: Duration,
    state: Arc<Mutex<AppState>>,
) {
    let file = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) => {
            error!("[slot {}] Unable to open {}: {}", slot, path, e);
            return;
        }
    };

    let write_lock: Arc<Mutex<fs::File>> = Arc::new(Mutex::new(file));
    let mut read_file = match write_lock.lock().unwrap().try_clone() {
        Ok(f) => f,
        Err(e) => {
            error!("[slot {}] Unable to clone device file: {}", slot, e);
            return;
        }
    };

    // Handshake. Writes to /dev/hidgN fail with ESHUTDOWN until the host has
    // enumerated the gadget, so ignore errors here (like `Connect()` in
    // nscon.go): the host-initiated 0x80 handshake in the reader loop below
    // is what actually completes the connection.
    {
        let mut guard = write_lock.lock().unwrap();
        for packet in switch_proto::handshake_packets() {
            let _ = guard.write_all(&packet);
        }
    }

    let count = Arc::new(AtomicU8::new(0));
    let input_active = Arc::new(AtomicBool::new(true));

    // Reader thread: host output reports + rumble. Detached on purpose: it
    // blocks in read() while the Switch is idle, so joining it would hang
    // shutdown forever; the OS reaps it when the process exits.
    {
        let write_lock = write_lock.clone();
        let rumble_tx = rumble_tx.clone();
        let state = state.clone();
        let count = count.clone();
        let input_active = input_active.clone();
        let mut buf = [0u8; READ_BUF_LEN];
        thread::spawn(move || {
            loop {
                if state.lock().unwrap().is_exiting() {
                    break;
                }
                let n = match read_file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n < 2 {
                    continue;
                }
                let report = &buf[..n];
                match report[0] {
                    0x80 => {
                        let mut guard = write_lock.lock().unwrap();
                        match report[1] {
                            0x01 => {
                                let resp = switch_proto::device_info_response(report[1]);
                                let _ = guard.write_all(&resp);
                            }
                            0x02 | 0x03 => {
                                let resp = switch_proto::simple_ack_response(report[1]);
                                let _ = guard.write_all(&resp);
                            }
                            0x04 => {
                                input_active.store(true, Ordering::Relaxed);
                            }
                            0x05 => {
                                input_active.store(false, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                    0x01 => {
                        if report.len() < 11 {
                            continue;
                        }
                        let c = count.load(Ordering::Relaxed);
                        let response = match report[10] {
                            0x01 => switch_proto::pairing_response(c, &NEUTRAL_INPUT, report[10]),
                            0x02 => switch_proto::device_info_subcommand_response(
                                c,
                                &NEUTRAL_INPUT,
                                report[10],
                            ),
                            0x03 | 0x04 | 0x08 | 0x30 | 0x38 | 0x40 | 0x41 | 0x48 => {
                                switch_proto::subcommand_response(
                                    c,
                                    &NEUTRAL_INPUT,
                                    true,
                                    report[10],
                                    &[],
                                )
                            }
                            0x10 => {
                                match switch_proto::spi_read_response(c, &NEUTRAL_INPUT, report) {
                                    Some(resp) => resp,
                                    None => switch_proto::subcommand_response(
                                        c,
                                        &NEUTRAL_INPUT,
                                        false,
                                        report[10],
                                        &[],
                                    ),
                                }
                            }
                            0x21 => switch_proto::set_report_mode_response(
                                c,
                                &NEUTRAL_INPUT,
                                report[10],
                            ),
                            _ => continue,
                        };
                        let mut guard = write_lock.lock().unwrap();
                        let _ = guard.write_all(&response);
                    }
                    // Rumble-only output report. (0x11 is MCU/NFC data, not rumble.)
                    0x10 => {
                        if let Some(mag) = switch_proto::decode_rumble(report) {
                            let _ = rumble_tx.try_send(Rumble {
                                slot,
                                magnitude: mag,
                            });
                        }
                    }
                    _ => {}
                }
            }
        })
    };

    // Writer loop: input reports on ticker.
    let mut latest = NEUTRAL_INPUT;
    loop {
        if state.lock().unwrap().is_exiting() {
            break;
        }

        if let Ok(input) = input_rx.recv_timeout(tick) {
            latest = input;
        }

        if input_active.load(Ordering::Relaxed) {
            let c = count.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            let report = switch_proto::input_report(c, &latest);
            let mut guard = write_lock.lock().unwrap();
            if guard.write_all(&report).is_err() {
                break;
            }
        }
    }
}
