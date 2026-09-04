//! Gilrs loop: enumerates Xbox pads, assigns them to slots, polls -> maps ->
//! sends `SwitchInput` to each slot, and applies incoming rumble.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use governor::{
    clock::{self, Clock},
    Quota, RateLimiter,
};

use flume::{Receiver, Sender};
use gilrs::{
    ff::Effect,
    ff::{BaseEffect, BaseEffectType, EffectBuilder},
    GamepadId, Gilrs, PowerInfo,
};

use log::{error, info, warn};

use crate::mapping::Mapping;
use crate::models::{
    AppState, Command, Controller, Rumble, SwitchInput, WebController, WebState, XboxInput,
    NEUTRAL_INPUT,
};

/// Convert battery percentage (0-100) to Switch 5-level scale (0-4).
fn percentage_to_level(pct: u8) -> u8 {
    if pct >= 70 {
        4 // full
    } else if pct >= 50 {
        3 // medium
    } else if pct >= 30 {
        2 // low
    } else if pct >= 10 {
        1 // critical
    } else {
        0 // empty
    }
}

const RUMBLE_MAX_MAGNITUDE: u16 = u16::MAX;

/// Find a Controller by its numeric (usize) gilrs id as used by the web UI.
fn controller_by_raw_id(controllers: &[Controller], raw: usize) -> Option<&Controller> {
    controllers.iter().find(|c| usize::from(c.id) == raw)
}

/// Copy slot assignments from the bridge's source of truth into the web state.
fn sync_web_slots(web_state: &Arc<Mutex<WebState>>, controllers: &[Controller]) {
    let mut ws = web_state.lock().unwrap();
    for wc in &mut ws.controllers {
        wc.slot = controllers
            .iter()
            .find(|c| usize::from(c.id) == wc.id)
            .map(|c| c.slot);
    }
}

struct RumbleEffects {
    strong: Option<Effect>,
    weak: Option<Effect>,
    strong_mag: u16,
    weak_mag: u16,
}

fn effect_for_gamepad(id: GamepadId, gilrs: &mut Gilrs, kind: BaseEffectType) -> Option<Effect> {
    if !gilrs.gamepad(id).is_ff_supported() {
        return None;
    }
    match EffectBuilder::new()
        .add_effect(BaseEffect {
            kind,
            ..Default::default()
        })
        .add_gamepad(&gilrs.gamepad(id))
        .finish(gilrs)
    {
        Ok(effect) => Some(effect),
        Err(e) => {
            warn!(
                "Unable to create rumble effect for {}: {}",
                gilrs.gamepad(id).name(),
                e
            );
            None
        }
    }
}

pub fn run(
    num_slots: usize,
    input_tx: Vec<Sender<SwitchInput>>,
    rumble_rx: Receiver<Rumble>,
    polling_rate: u32,
    state: Arc<Mutex<AppState>>,
    command_rx: Receiver<Command>,
    web_state: Arc<Mutex<WebState>>,
) {
    let mut gilrs = match Gilrs::new() {
        Ok(g) => g,
        Err(e) => {
            error!("Unable to initialise gilrs: {}", e);
            return;
        }
    };

    let mapping = Mapping::new();
    let mut slot_occupied = vec![false; num_slots];
    let mut controllers: Vec<Controller> = Vec::new();
    let mut rumble_effects: HashMap<GamepadId, RumbleEffects> = HashMap::new();

    // Attach any already-connected pads.
    for gamepad_id in gilrs.gamepads().map(|(id, _)| id).collect::<Vec<_>>() {
        if let Some(slot) = lowest_free_slot(&slot_occupied) {
            info!(
                "{} attached to slot {}",
                gilrs.gamepad(gamepad_id).name(),
                slot
            );
            slot_occupied[slot] = true;
            controllers.push(Controller {
                id: gamepad_id,
                slot,
            });
            // Mirror into web state.
            {
                let mut ws = web_state.lock().unwrap();
                ws.controllers.push(WebController {
                    id: usize::from(gamepad_id),
                    name: gilrs.gamepad(gamepad_id).name().to_string(),
                    slot: Some(slot),
                    battery: 0,
                    is_vibrating: false,
                });
                ws.inputs.push(None);
            }
            // Create both strong and weak effects for this gamepad.
            let strong = effect_for_gamepad(
                gamepad_id,
                &mut gilrs,
                BaseEffectType::Strong { magnitude: 0 },
            );
            let weak = effect_for_gamepad(
                gamepad_id,
                &mut gilrs,
                BaseEffectType::Weak { magnitude: 0 },
            );
            rumble_effects.insert(
                gamepad_id,
                RumbleEffects {
                    strong,
                    weak,
                    strong_mag: 0,
                    weak_mag: 0,
                },
            );
        } else {
            warn!(
                "{} not attached: no free slot",
                gilrs.gamepad(gamepad_id).name()
            );
        }
    }

    let clock = clock::DefaultClock::default();
    let limiter = RateLimiter::direct_with_clock(
        Quota::per_second(NonZeroU32::new(polling_rate).unwrap())
            .allow_burst(NonZeroU32::new(1u32).unwrap()),
        clock.clone(),
    );

    loop {
        if state.lock().unwrap().is_exiting() {
            break;
        }

        // Apply rumble from gadget slots.
        while let Ok(rumble) = rumble_rx.try_recv() {
            if let Some(controller) = controllers.iter().find(|c| c.slot == rumble.slot) {
                if let Some(effects) = rumble_effects.get_mut(&controller.id) {
                    // Map Switch left motor -> Xbox strong motor (low frequency, heavy)
                    // Map Switch right motor -> Xbox weak motor (high frequency, light)
                    let strong_mag =
                        (rumble.left as u32 * RUMBLE_MAX_MAGNITUDE as u32 / 255) as u16;
                    let weak_mag = (rumble.right as u32 * RUMBLE_MAX_MAGNITUDE as u32 / 255) as u16;

                    // Update strong effect if changed
                    if effects.strong_mag != strong_mag {
                        if let Some(old) = effects.strong.take() {
                            let _ = old.stop();
                        }
                        match effect_for_gamepad(
                            controller.id,
                            &mut gilrs,
                            BaseEffectType::Strong {
                                magnitude: strong_mag,
                            },
                        ) {
                            Some(effect) => {
                                effects.strong = Some(effect);
                                effects.strong_mag = strong_mag;
                            }
                            None => {
                                effects.strong_mag = 0;
                                continue;
                            }
                        }
                    }

                    // Update weak effect if changed
                    if effects.weak_mag != weak_mag {
                        if let Some(old) = effects.weak.take() {
                            let _ = old.stop();
                        }
                        match effect_for_gamepad(
                            controller.id,
                            &mut gilrs,
                            BaseEffectType::Weak {
                                magnitude: weak_mag,
                            },
                        ) {
                            Some(effect) => {
                                effects.weak = Some(effect);
                                effects.weak_mag = weak_mag;
                            }
                            None => {
                                effects.weak_mag = 0;
                                continue;
                            }
                        }
                    }

                    // Play or stop effects based on magnitude
                    if let Some(ref effect) = effects.strong {
                        if strong_mag > 0 {
                            let _ = effect.play();
                        } else {
                            let _ = effect.stop();
                        }
                    }
                    if let Some(ref effect) = effects.weak {
                        if weak_mag > 0 {
                            let _ = effect.play();
                        } else {
                            let _ = effect.stop();
                        }
                    }
                }
            }
        }

        // Handle commands from the web UI.
        while let Ok(cmd) = command_rx.try_recv() {
            match cmd {
                Command::Remap {
                    controller_id,
                    new_slot,
                } => {
                    if new_slot >= num_slots {
                        warn!("Invalid slot {} for remap", new_slot);
                        continue;
                    }
                    let Some(old_slot) = controllers
                        .iter()
                        .find(|c| usize::from(c.id) == controller_id)
                        .map(|c| c.slot)
                    else {
                        continue;
                    };
                    if old_slot == new_slot {
                        continue;
                    }
                    // Check if new slot is occupied
                    if slot_occupied[new_slot] {
                        // Find what's in the new slot and swap
                        if let Some(other) = controllers.iter_mut().find(|c| c.slot == new_slot) {
                            let other_id = usize::from(other.id);
                            other.slot = old_slot;
                            slot_occupied[old_slot] = true;
                            info!(
                                "Swapped slots: controller {} (slot {}) <-> controller {} (slot {})",
                                controller_id, new_slot, other_id, old_slot
                            );
                            let mut ws = web_state.lock().unwrap();
                            ws.status =
                                format!("Swapped slot {} <-> slot {}", new_slot + 1, old_slot + 1);
                        }
                    } else {
                        slot_occupied[old_slot] = false;
                        slot_occupied[new_slot] = true;
                        info!(
                            "Remapped controller {} from slot {} to slot {}",
                            controller_id, old_slot, new_slot
                        );
                        let mut ws = web_state.lock().unwrap();
                        ws.status = format!(
                            "Remapped controller {} to slot {}",
                            controller_id,
                            new_slot + 1
                        );
                    }
                    // Update the controller's slot
                    if let Some(controller) = controllers
                        .iter_mut()
                        .find(|c| usize::from(c.id) == controller_id)
                    {
                        controller.slot = new_slot;
                    }
                    sync_web_slots(&web_state, &controllers);
                }
                Command::Identify { controller_id } => {
                    if let Some(controller) = controller_by_raw_id(&controllers, controller_id) {
                        if let Some(effects) = rumble_effects.get_mut(&controller.id) {
                            // Mark as vibrating for the web UI, then spawn a
                            // separate thread so input polling isn't blocked.
                            {
                                let mut ws = web_state.lock().unwrap();
                                if let Some(wc) =
                                    ws.controllers.iter_mut().find(|c| c.id == controller_id)
                                {
                                    wc.is_vibrating = true;
                                }
                            }
                            let strong_effect = effects.strong.take();
                            let web_state = web_state.clone();

                            thread::spawn(move || {
                                // Send a quick vibration pattern: two short pulses
                                if let Some(effect) = strong_effect {
                                    let _ = effect.play();
                                    thread::sleep(Duration::from_millis(100));
                                    let _ = effect.stop();
                                    thread::sleep(Duration::from_millis(100));
                                    let _ = effect.play();
                                    thread::sleep(Duration::from_millis(100));
                                    let _ = effect.stop();
                                    // Note: effect is dropped here, which stops it
                                }
                                let mut ws = web_state.lock().unwrap();
                                if let Some(wc) =
                                    ws.controllers.iter_mut().find(|c| c.id == controller_id)
                                {
                                    wc.is_vibrating = false;
                                }
                            });
                        }
                    }
                }
                Command::Vibrate {
                    controller_id,
                    duration_ms,
                } => {
                    if let Some(controller) = controller_by_raw_id(&controllers, controller_id) {
                        if let Some(effects) = rumble_effects.get_mut(&controller.id) {
                            // Mark as vibrating for the web UI, then spawn a
                            // separate thread so input polling isn't blocked.
                            {
                                let mut ws = web_state.lock().unwrap();
                                if let Some(wc) =
                                    ws.controllers.iter_mut().find(|c| c.id == controller_id)
                                {
                                    wc.is_vibrating = true;
                                }
                            }
                            let strong_effect = effects.strong.take();
                            let web_state = web_state.clone();

                            thread::spawn(move || {
                                if let Some(effect) = strong_effect {
                                    let _ = effect.play();
                                    thread::sleep(Duration::from_millis(duration_ms));
                                    let _ = effect.stop();
                                    // Note: effect is dropped here, which stops it
                                }
                                let mut ws = web_state.lock().unwrap();
                                if let Some(wc) =
                                    ws.controllers.iter_mut().find(|c| c.id == controller_id)
                                {
                                    wc.is_vibrating = false;
                                }
                            });
                        }
                    }
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
                            let name = gilrs.gamepad(event.id).name().to_string();
                            info!("{} connected, assigned slot {}", name, slot);
                            slot_occupied[slot] = true;
                            controllers.push(Controller { id: event.id, slot });
                            {
                                let mut ws = web_state.lock().unwrap();
                                ws.controllers.push(WebController {
                                    id: usize::from(event.id),
                                    name: name.clone(),
                                    slot: Some(slot),
                                    battery: 0,
                                    is_vibrating: false,
                                });
                                ws.inputs.push(None);
                                ws.status = format!("{} connected (slot {})", name, slot + 1);
                            }
                            // Create both strong and weak effects for this gamepad.
                            let strong = effect_for_gamepad(
                                event.id,
                                &mut gilrs,
                                BaseEffectType::Strong { magnitude: 0 },
                            );
                            let weak = effect_for_gamepad(
                                event.id,
                                &mut gilrs,
                                BaseEffectType::Weak { magnitude: 0 },
                            );
                            rumble_effects.insert(
                                event.id,
                                RumbleEffects {
                                    strong,
                                    weak,
                                    strong_mag: 0,
                                    weak_mag: 0,
                                },
                            );
                        }
                        None => warn!(
                            "{} connected but no free slot",
                            gilrs.gamepad(event.id).name()
                        ),
                    }
                }
                gilrs::EventType::Disconnected => {
                    if let Some(pos) = controllers.iter().position(|c| c.id == event.id) {
                        let controller = controllers.remove(pos);
                        let name = gilrs.gamepad(event.id).name().to_string();
                        info!("{} disconnected, slot {} freed", name, controller.slot);
                        slot_occupied[controller.slot] = false;
                        if let Some(mut effects) = rumble_effects.remove(&event.id) {
                            if let Some(effect) = effects.strong.take() {
                                let _ = effect.stop();
                            }
                            if let Some(effect) = effects.weak.take() {
                                let _ = effect.stop();
                            }
                        }
                        {
                            let mut ws = web_state.lock().unwrap();
                            ws.controllers.remove(pos);
                            ws.inputs.remove(pos);
                            ws.status = format!("{} disconnected", name);
                        }
                    }
                }
                _ => {}
            }
        }

        // Poll and send per slot (idle slots send neutral reports).
        let mut ws_guard = web_state.lock().unwrap();
        for (slot, tx) in input_tx.iter().enumerate().take(num_slots) {
            let input = match controllers.iter().position(|c| c.slot == slot) {
                Some(idx) => {
                    let controller = &controllers[idx];
                    let gamepad = gilrs.gamepad(controller.id);
                    let mut inp = mapping.poll(&gamepad);
                    // Set battery level from controller's power info
                    let battery = match gamepad.power_info() {
                        PowerInfo::Discharging(pct) => (percentage_to_level(pct) << 5) | 0x01,
                        PowerInfo::Charging(pct) => (percentage_to_level(pct) << 5) | 0x10 | 0x01,
                        PowerInfo::Charged => (4 << 5) | 0x10 | 0x01, // full + charging
                        PowerInfo::Wired => 0x81,                    // full + USB powered
                        _ => 0x81,                                   // unknown/empty
                    };
                    inp.battery = battery;
                    // Mirror the latest raw Xbox state into the web UI.
                    if let Some(wc) = ws_guard.controllers.get_mut(idx) {
                        wc.battery = battery;
                        wc.name = gamepad.name().to_string();
                    }
                    if let Some(slot_inputs) = ws_guard.inputs.get_mut(idx) {
                        *slot_inputs = Some(XboxInput::from_gamepad(&gamepad));
                    }
                    inp
                }
                None => NEUTRAL_INPUT,
            };
            let _ = tx.send(input);
        }
        drop(ws_guard);

        if let Err(e) = limiter.check() {
            thread::sleep(e.wait_time_from(clock.now()));
        }
    }
}

fn lowest_free_slot(slot_occupied: &[bool]) -> Option<usize> {
    slot_occupied.iter().position(|&occupied| !occupied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentage_to_level() {
        assert_eq!(percentage_to_level(0), 0);
        assert_eq!(percentage_to_level(9), 0);
        assert_eq!(percentage_to_level(10), 1);
        assert_eq!(percentage_to_level(29), 1);
        assert_eq!(percentage_to_level(30), 2);
        assert_eq!(percentage_to_level(49), 2);
        assert_eq!(percentage_to_level(50), 3);
        assert_eq!(percentage_to_level(69), 3);
        assert_eq!(percentage_to_level(70), 4);
        assert_eq!(percentage_to_level(100), 4);
    }
}
