mod bridge;
mod gadget;
mod mapping;
mod models;
mod priority;
mod switch_proto;
mod web;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clap::Parser;
use log::{error, info, warn};

use crate::gadget::GadgetManager;
use crate::models::{
    lock_mutex, AppState, Command, SwitchInput, WebState, DEFAULT_SLOTS, MAX_SLOTS,
};

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

    /// Web UI listen address (e.g. 0.0.0.0:8080).
    #[arg(long, default_value = "0.0.0.0:8080")]
    web_addr: String,

    /// Disable the web UI.
    #[arg(long)]
    no_web: bool,

    /// Skip USB gadget creation/teardown (for UI development without hardware).
    #[arg(long)]
    no_gadget: bool,
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

    let web_addr: SocketAddr = match cli.web_addr.parse() {
        Ok(addr) => addr,
        Err(e) => {
            error!("Invalid --web-addr '{}': {}", cli.web_addr, e);
            std::process::exit(1);
        }
    };

    let state = Arc::new(Mutex::new(AppState::Connected));

    // Create the composite USB gadget with N HID functions.
    let manager = if cli.no_gadget {
        info!("--no-gadget set: skipping USB gadget creation");
        None
    } else {
        let manager = GadgetManager::new();
        if let Err(e) = manager.create(num_slots) {
            error!("Unable to create USB gadget (run as root?): {}", e);
            std::process::exit(1);
        }
        info!(
            "USB gadget created with {} Pro Controller slot(s) ({} Hz)",
            num_slots, polling_rate
        );
        Some(manager)
    };

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
    if let Some(manager) = &manager {
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
    } else {
        // No gadget: drop the receivers so bridge sends fail fast (non-blocking).
        input_rx.clear();
    }

    // Create the command channel (web UI -> bridge) and shared web state.
    let (command_tx, command_rx) = flume::unbounded::<Command>();
    let web_state = Arc::new(Mutex::new(WebState::default()));

    // Spawn the gilrs bridge with realtime priority.
    let bridge_state = state.clone();
    let bridge_web_state = web_state.clone();
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
            command_rx,
            bridge_web_state,
        );
    });

    // Spawn the web UI.
    let web_thread = if cli.no_web {
        info!("--no-web set: web UI disabled");
        None
    } else {
        let app = Arc::new(web::WebApp {
            state: state.clone(),
            web: web_state.clone(),
            command_tx: command_tx.clone(),
            num_slots,
        });
        Some(thread::spawn(move || {
            if let Err(e) = web::serve(web_addr, app) {
                error!("Web UI exited with error: {}", e);
            }
        }))
    };

    // Wait for Ctrl-C.
    let (shutdown_tx, shutdown_rx) = flume::unbounded::<()>();
    if let Err(e) = ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(());
    }) {
        error!("Unable to set Ctrl-C handler: {}", e);
    }

    let _ = shutdown_rx.recv();
    info!("Shutting down...");
    *lock_mutex(&state) = AppState::Exiting;

    let _ = bridge_thread.join();
    if let Some(web_thread) = web_thread {
        let _ = web_thread.join();
    }
    for handle in slot_threads {
        let _ = handle.join();
    }

    if let Some(manager) = &manager {
        manager.destroy();
    }
    info!("Gadget torn down. Goodbye.");
}
