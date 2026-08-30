mod bridge;
mod gadget;
mod mapping;
mod models;
mod priority;
mod switch_proto;
mod ui;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clap::Parser;
use log::{error, info, warn};

use crate::gadget::GadgetManager;
use crate::models::{AppState, SwitchInput, DEFAULT_SLOTS, MAX_SLOTS};

const MIN_SLOTS: usize = 1;
const MIN_POLLING_RATE: u32 = 20;
const MAX_POLLING_RATE: u32 = 1000;
const DEFAULT_POLLING_RATE: u32 = 250;

#[derive(Parser, Debug)]
#[command(
    name = "raspberry-switch-controller",
    version,
    about = "Bridge Xbox wireless controllers to a Nintendo Switch over USB OTG gadget mode"
)]
struct Cli {
    /// Number of Switch Pro Controller slots to expose (1..8).
    #[arg(long, default_value_t = DEFAULT_SLOTS)]
    controllers: usize,

    /// Polling rate in Hz (20..1000).
    #[arg(long, short = 'p', default_value_t = DEFAULT_POLLING_RATE)]
    polling_rate: u32,
}

fn main() {
    pretty_env_logger::init();

    let cli = Cli::parse();

    let num_slots = cli.controllers.clamp(MIN_SLOTS, MAX_SLOTS);
    if cli.controllers < MIN_SLOTS || cli.controllers > MAX_SLOTS {
        warn!(
            "--controllers must be between {} and {}; using {}",
            MIN_SLOTS, MAX_SLOTS, num_slots
        );
    }
    let polling_rate = cli.polling_rate.clamp(MIN_POLLING_RATE, MAX_POLLING_RATE);
    if cli.polling_rate < MIN_POLLING_RATE || cli.polling_rate > MAX_POLLING_RATE {
        warn!(
            "--polling-rate must be between {} and {} Hz; using {}",
            MIN_POLLING_RATE, MAX_POLLING_RATE, polling_rate
        );
    }

    let state = Arc::new(Mutex::new(AppState::Connected));

    // Create the composite USB gadget with N HID functions.
    let manager = GadgetManager::new();
    if let Err(e) = manager.create(num_slots) {
        error!("Unable to create USB gadget (run as root?): {}", e);
        std::process::exit(1);
    }
    info!(
        "USB gadget created with {} Pro Controller slot(s) ({} Hz)",
        num_slots, polling_rate
    );

    // Per-slot input channels (bridge -> gadget) and a shared rumble channel.
    let mut input_tx = Vec::with_capacity(num_slots);
    let mut input_rx = Vec::with_capacity(num_slots);
    for _ in 0..num_slots {
        let (tx, rx) = flume::bounded::<SwitchInput>(1);
        input_tx.push(tx);
        input_rx.push(rx);
    }
    let (rumble_tx, rumble_rx) = flume::bounded(8);

    let tick = Duration::from_micros(1_000_000 / polling_rate.max(1) as u64);

    // Spawn one task per slot with realtime priority.
    let mut slot_threads = Vec::with_capacity(num_slots);
    for slot in 0..num_slots {
        let rx = input_rx.remove(0);
        let rumble_tx = rumble_tx.clone();
        let state = state.clone();
        let path = manager.hidg_path(slot);
        slot_threads.push(thread::spawn(move || {
            // Set realtime priority for slot thread (USB gadget I/O).
            if let Err(e) = priority::set_realtime_priority(10) {
                warn!("[slot {}] Unable to set realtime priority: {}", slot, e);
            }
            gadget::run_slot(slot, &path, rx, rumble_tx, tick, state);
        }));
    }

    // Create UI channels.
    let (ui_command_tx, ui_command_rx) = flume::unbounded::<ui::UiCommand>();
    let (ui_event_tx, ui_event_rx) = flume::unbounded::<ui::UiEvent>();

    // Spawn the gilrs bridge with realtime priority.
    let bridge_state = state.clone();
    let bridge_thread = thread::spawn(move || {
        // Set realtime priority for bridge thread (input polling).
        if let Err(e) = priority::set_realtime_priority(10) {
            warn!("Unable to set realtime priority for bridge: {}", e);
        }
        bridge::run(
            num_slots,
            input_tx,
            rumble_rx,
            polling_rate,
            bridge_state,
            ui_command_rx,
            ui_event_tx,
        );
    });

    // Spawn the terminal UI.
    let ui_state = state.clone();
    let ui_thread = thread::spawn(move || {
        ui::run_ui(num_slots, ui_command_tx, ui_event_rx, ui_state);
    });

    // Wait for Ctrl-C.
    let (shutdown_tx, shutdown_rx) = flume::unbounded::<()>();
    if let Err(e) = ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(());
    }) {
        error!("Unable to set Ctrl-C handler: {}", e);
    }

    let _ = shutdown_rx.recv();
    info!("Shutting down...");
    *state.lock().unwrap() = AppState::Exiting;

    let _ = bridge_thread.join();
    let _ = ui_thread.join();
    for handle in slot_threads {
        let _ = handle.join();
    }

    manager.destroy();
    info!("Gadget torn down. Goodbye.");
}
