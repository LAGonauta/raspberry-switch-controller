//! Gilrs loop: enumerates Xbox pads, assigns them to slots, polls -> maps ->
//! sends `SwitchInput` to each slot, and applies incoming rumble.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use atomic::Atomic;
use flume::{Receiver, Sender};
use gilrs::{ff::Effect, ff::{BaseEffect, BaseEffectType, EffectBuilder}, GamepadId, Gilrs};

use crate::mapping::Mapping;
use crate::models::{AppState, Controller, Rumble, SwitchInput, NEUTRAL_INPUT};

const RUMBLE_MAX_MAGNITUDE: u16 = u16::MAX;

fn effect_for_gamepad(id: GamepadId, gilrs: &mut Gilrs, magnitude: u16) -> Option<Effect> {
    if !gilrs.gamepad(id).is_ff_supported() {
        return None;
    }
    match EffectBuilder::new()
        .add_effect(BaseEffect {
            kind: BaseEffectType::Strong { magnitude },
            ..Default::default()
        })
        .add_gamepad(&gilrs.gamepad(id))
        .finish(gilrs)
    {
        Ok(effect) => Some(effect),
        Err(e) => {
            eprintln!("Unable to create rumble effect for {}: {}", gilrs.gamepad(id).name(), e);
            None
        }
    }
}

pub fn run(
    num_slots: usize,
    input_tx: Vec<Sender<SwitchInput>>,
    rumble_rx: Receiver<Rumble>,
    polling_rate: u32,
    state: Arc<Atomic<AppState>>,
) {
    let mut gilrs = match Gilrs::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Unable to initialise gilrs: {}", e);
            return;
        }
    };

    let mapping = Mapping::new();
    let mut slot_occupied = vec![false; num_slots];
    let mut controllers: Vec<Controller> = Vec::new();
    let mut effects: HashMap<GamepadId, Effect> = HashMap::new();
    let mut effect_magnitude: HashMap<GamepadId, u16> = HashMap::new();

    // Attach any already-connected pads.
    for gamepad_id in gilrs.gamepads().map(|(id, _)| id).collect::<Vec<_>>() {
        if let Some(slot) = lowest_free_slot(&slot_occupied) {
            println!("{} attached to slot {}", gilrs.gamepad(gamepad_id).name(), slot);
            slot_occupied[slot] = true;
            controllers.push(Controller { id: gamepad_id, slot });
            if let Some(effect) = effect_for_gamepad(gamepad_id, &mut gilrs, 0) {
                effects.insert(gamepad_id, effect);
                effect_magnitude.insert(gamepad_id, 0);
            }
        } else {
            println!("{} not attached: no free slot", gilrs.gamepad(gamepad_id).name());
        }
    }

    let tick = Duration::from_micros(1_000_000 / polling_rate.max(1) as u64);

    loop {
        if state.load(Ordering::Relaxed).is_exiting() {
            break;
        }

        // Apply rumble from gadget slots.
        while let Ok(rumble) = rumble_rx.try_recv() {
            if let Some(controller) = controllers.iter().find(|c| c.slot == rumble.slot) {
                let magnitude =
                    (rumble.magnitude as u32 * RUMBLE_MAX_MAGNITUDE as u32 / 255) as u16;
                if !effects.contains_key(&controller.id) {
                    continue;
                }
                let needs_update = effect_magnitude.get(&controller.id).copied() != Some(magnitude);
                if needs_update {
                    if let Some(old) = effects.remove(&controller.id) {
                        let _ = old.stop();
                    }
                    if let Some(effect) = effect_for_gamepad(controller.id, &mut gilrs, magnitude) {
                        effects.insert(controller.id, effect);
                        effect_magnitude.insert(controller.id, magnitude);
                    } else {
                        // Creation failed; forget the magnitude so a later
                        // rumble event retries instead of being skipped.
                        effect_magnitude.remove(&controller.id);
                    }
                }
                if magnitude > 0 {
                    if let Some(effect) = effects.get(&controller.id) {
                        let _ = effect.play();
                    }
                } else if let Some(effect) = effects.get(&controller.id) {
                    let _ = effect.stop();
                }
            }
        }

        // Handle gamepad connect/disconnect events.
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::Connected => {
                    if controllers.iter().any(|c| c.id == event.id) {
                        continue;
                    }
                    match lowest_free_slot(&slot_occupied) {
                        Some(slot) => {
                            println!("{} connected, assigned slot {}", gilrs.gamepad(event.id).name(), slot);
                            slot_occupied[slot] = true;
                            controllers.push(Controller { id: event.id, slot });
                            if let Some(effect) = effect_for_gamepad(event.id, &mut gilrs, 0) {
                                effects.insert(event.id, effect);
                                effect_magnitude.insert(event.id, 0);
                            }
                        }
                        None => println!(
                            "{} connected but no free slot",
                            gilrs.gamepad(event.id).name()
                        ),
                    }
                }
                gilrs::EventType::Disconnected => {
                    if let Some(pos) = controllers.iter().position(|c| c.id == event.id) {
                        let controller = controllers.remove(pos);
                        println!(
                            "{} disconnected, slot {} freed",
                            gilrs.gamepad(event.id).name(),
                            controller.slot
                        );
                        slot_occupied[controller.slot] = false;
                        if let Some(effect) = effects.remove(&event.id) {
                            let _ = effect.stop();
                        }
                        effect_magnitude.remove(&event.id);
                    }
                }
                _ => {}
            }
        }

        // Poll and send per slot (idle slots send neutral reports).
        for (slot, tx) in input_tx.iter().enumerate().take(num_slots) {
            let input = match controllers.iter().find(|c| c.slot == slot) {
                Some(controller) => mapping.poll(&gilrs.gamepad(controller.id)),
                None => NEUTRAL_INPUT,
            };
            let _ = tx.send(input);
        }

        thread::sleep(tick);
    }
}

fn lowest_free_slot(slot_occupied: &[bool]) -> Option<usize> {
    slot_occupied.iter().position(|&occupied| !occupied)
}
