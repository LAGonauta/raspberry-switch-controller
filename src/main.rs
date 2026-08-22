mod bridge;
mod gadget;
mod mapping;
mod models;
mod switch_proto;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clap::Parser;

use crate::gadget::GadgetManager;
use crate::models::{AppState, SwitchInput, DEFAULT_SLOTS, MAX_SLOTS};

const MIN_SLOTS: usize = 1;
const MIN_POLLING_RATE: u32 = 20;
const MAX_POLLING_RATE: u32 = 1000;
const DEFAULT_POLLING_RATE: u32 = 250;

#[derive(Parser, Debug)]
#[command(name = "raspberry-switch-controller", version, about = "Bridge Xbox wireless controllers to a Nintendo Switch over USB OTG gadget mode")]
struct Cli {
    /// Number of Switch Pro Controller slots to expose (1..8).
    #[arg(long, default_value_t = DEFAULT_SLOTS)]
    controllers: usize,

    /// Polling rate in Hz (20..1000).
    #[arg(long, short = 'p', default_value_t = DEFAULT_POLLING_RATE)]
    polling_rate: u32,
}

fn main() {
    let cli = Cli::parse();

    let num_slots = cli.controllers.clamp(MIN_SLOTS, MAX_SLOTS);
    if cli.controllers < MIN_SLOTS || cli.controllers > MAX_SLOTS {
        eprintln!(
            "Warning: --controllers must be between {} and {}; using {}",
            MIN_SLOTS, MAX_SLOTS, num_slots
        );
    }
    let polling_rate = cli.polling_rate.clamp(MIN_POLLING_RATE, MAX_POLLING_RATE);
    if cli.polling_rate < MIN_POLLING_RATE || cli.polling_rate > MAX_POLLING_RATE {
        eprintln!(
            "Warning: --polling-rate must be between {} and {} Hz; using {}",
            MIN_POLLING_RATE, MAX_POLLING_RATE, polling_rate
        );
    }

    let state = Arc::new(Mutex::new(AppState::Connected));

    // Create the composite USB gadget with N HID functions.
    let manager = GadgetManager::new();
    if let Err(e) = manager.create(num_slots) {
        eprintln!("Unable to create USB gadget (run as root?): {}", e);
        std::process::exit(1);
    }
    println!(
        "USB gadget created with {} Pro Controller slot(s) ({} Hz)",
        num_slots, polling_rate
    );

    // Per-slot input channels (bridge -> gadget) and a shared rumble channel.
    let mut input_tx = Vec::with_capacity(num_slots);
    let mut input_rx = Vec::with_capacity(num_slots);
    for _ in 0..num_slots {
        let (tx, rx) = flume::unbounded::<SwitchInput>();
        input_tx.push(tx);
        input_rx.push(rx);
    }
    let (rumble_tx, rumble_rx) = flume::unbounded();

    let tick = Duration::from_micros(1_000_000 / polling_rate.max(1) as u64);

    // Spawn one task per slot.
    let mut slot_threads = Vec::with_capacity(num_slots);
    for slot in 0..num_slots {
        let rx = input_rx.remove(0);
        let rumble_tx = rumble_tx.clone();
        let state = state.clone();
        let path = manager.hidg_path(slot);
        slot_threads.push(thread::spawn(move || {
            gadget::run_slot(slot, &path, rx, rumble_tx, tick, state);
        }));
    }

    // Spawn the gilrs bridge.
    let bridge_state = state.clone();
    let bridge_thread = thread::spawn(move || {
        bridge::run(num_slots, input_tx, rumble_rx, polling_rate, bridge_state);
    });

    // Wait for Ctrl-C.
    let (shutdown_tx, shutdown_rx) = flume::unbounded::<()>();
    if let Err(e) = ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(());
    }) {
        eprintln!("Unable to set Ctrl-C handler: {}", e);
    }

    let _ = shutdown_rx.recv();
    println!("Shutting down...");
    *state.lock().unwrap() = AppState::Exiting;

    let _ = bridge_thread.join();
    for handle in slot_threads {
        let _ = handle.join();
    }

    manager.destroy();
    println!("Gadget torn down. Goodbye.");
}
