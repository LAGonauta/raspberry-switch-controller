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
    apply_remap, lock_mutex, AppState, Command, ControllerState, RemapOutcome, Rumble, SwitchInput,
    WebState, XboxInput, NEUTRAL_INPUT,
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

/// Compose the Switch battery byte from an Xbox power state. The gadget is
/// always USB host-powered, so bit 0 (host powered) is always set.
fn battery_byte(power: PowerInfo) -> u8 {
    match power {
        PowerInfo::Discharging(pct) => (percentage_to_level(pct) << 5) | 0x01,
        PowerInfo::Charging(pct) => (percentage_to_level(pct) << 5) | 0x10 | 0x01,
        PowerInfo::Charged => (4 << 5) | 0x10 | 0x01,
        PowerInfo::Wired | PowerInfo::Unknown => 0x81,
    }
}

const RUMBLE_MAX_MAGNITUDE: u16 = u16::MAX;

fn slot_occupied(controllers: &[ControllerState], slot: usize) -> bool {
    controllers.iter().any(|c| c.slot == Some(slot))
}

fn lowest_free_slot(controllers: &[ControllerState], num_slots: usize) -> Option<usize> {
    (0..num_slots).find(|&s| !slot_occupied(controllers, s))
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

/// Start a one-shot manual vibration (Identify or Vibrate) on a controller.
///
/// Uses a dedicated temporary effect so the main rumble loop's `strong`
/// effect is left untouched. Skips if the controller is already vibrating.
fn start_manual_vibration(
    gilrs: &mut Gilrs,
    controller_id: usize, // raw numeric id from the web
    web_state: &Arc<Mutex<WebState>>,
    identify: bool,
    duration_ms: u64,
) {
    // Re-entrancy guard: skip if this controller is already vibrating.
    let gamepad_id = {
        let ws = lock_mutex(web_state);
        let Some(controller) = ws
            .controllers
            .iter()
            .find(|c| usize::from(c.id) == controller_id)
        else {
            return;
        };
        if controller.is_vibrating {
            info!(
                "Skipping manual vibration for {}: already vibrating",
                gilrs.gamepad(controller.id).name()
            );
            return;
        }
        controller.id
    };

    // Dedicated temporary effect; leave the main `strong` effect untouched.
    let Some(effect) = effect_for_gamepad(
        gamepad_id,
        gilrs,
        BaseEffectType::Strong {
            magnitude: u16::MAX,
        },
    ) else {
        return;
    };

    {
        let mut ws = lock_mutex(web_state);
        if let Some(controller) = ws
            .controllers
            .iter_mut()
            .find(|c| usize::from(c.id) == controller_id)
        {
            controller.is_vibrating = true;
        }
    }

    let web_state = web_state.clone();
    thread::spawn(move || {
        if identify {
            // Two short pulses.
            let _ = effect.play();
            thread::sleep(Duration::from_millis(100));
            let _ = effect.stop();
            thread::sleep(Duration::from_millis(100));
            let _ = effect.play();
            thread::sleep(Duration::from_millis(100));
            let _ = effect.stop();
        } else {
            // Single pulse for the requested duration, then stop.
            let _ = effect.play();
            thread::sleep(Duration::from_millis(duration_ms));
            let _ = effect.stop();
        }
        // Effect is dropped here, which stops it.
        let mut ws = lock_mutex(&web_state);
        if let Some(controller) = ws
            .controllers
            .iter_mut()
            .find(|c| usize::from(c.id) == controller_id)
        {
            controller.is_vibrating = false;
        }
    });
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
    let mut rumble_effects: HashMap<GamepadId, RumbleEffects> = HashMap::new();

    // Attach any already-connected pads.
    for gamepad_id in gilrs.gamepads().map(|(id, _)| id).collect::<Vec<_>>() {
        {
            let mut ws = lock_mutex(&web_state);
            if ws.controllers.iter().any(|c| c.id == gamepad_id) {
                continue;
            }
            match lowest_free_slot(&ws.controllers, num_slots) {
                Some(slot) => {
                    info!(
                        "{} attached to slot {}",
                        gilrs.gamepad(gamepad_id).name(),
                        slot
                    );
                    ws.controllers.push(ControllerState {
                        id: gamepad_id,
                        slot: Some(slot),
                        name: gilrs.gamepad(gamepad_id).name().to_string(),
                        battery: 0,
                        is_vibrating: false,
                        input: None,
                    });
                }
                None => {
                    warn!(
                        "{} not attached: no free slot",
                        gilrs.gamepad(gamepad_id).name()
                    );
                }
            }
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
    }

    let clock = clock::DefaultClock::default();
    let limiter = RateLimiter::direct_with_clock(
        Quota::per_second(NonZeroU32::new(polling_rate).unwrap())
            .allow_burst(NonZeroU32::new(1u32).unwrap()),
        clock.clone(),
    );

    loop {
        if lock_mutex(&state).is_exiting() {
            break;
        }

        // Apply rumble from gadget slots.
        while let Ok(rumble) = rumble_rx.try_recv() {
            let controller_id = lock_mutex(&web_state)
                .controllers
                .iter()
                .find(|c| c.slot == Some(rumble.slot))
                .map(|c| c.id);
            if let Some(controller_id) = controller_id {
                if let Some(effects) = rumble_effects.get_mut(&controller_id) {
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
                            controller_id,
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
                            controller_id,
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
                    let mut ws = lock_mutex(&web_state);
                    match apply_remap(&mut ws.controllers, controller_id, new_slot, num_slots) {
                        RemapOutcome::Swapped { old_slot, new_slot } => {
                            info!(
                                "Swapped slots: controller {} (slot {}) <-> (slot {})",
                                controller_id, new_slot, old_slot
                            );
                            ws.status =
                                format!("Swapped slot {} <-> slot {}", new_slot + 1, old_slot + 1);
                        }
                        RemapOutcome::Moved => {
                            info!(
                                "Remapped controller {} to slot {}",
                                controller_id,
                                new_slot + 1
                            );
                            ws.status = format!(
                                "Remapped controller {} to slot {}",
                                controller_id,
                                new_slot + 1
                            );
                        }
                        RemapOutcome::InvalidSlot => {
                            warn!("Invalid slot {} for remap", new_slot);
                        }
                        RemapOutcome::SameSlot => {}
                        RemapOutcome::NotFound => {
                            warn!("Controller {} not found for remap", controller_id);
                        }
                    }
                }
                Command::Identify { controller_id } => {
                    start_manual_vibration(&mut gilrs, controller_id, &web_state, true, 0);
                }
                Command::Vibrate {
                    controller_id,
                    duration_ms,
                } => {
                    start_manual_vibration(
                        &mut gilrs,
                        controller_id,
                        &web_state,
                        false,
                        duration_ms.min(5000),
                    );
                }
            }
        }

        // Handle gamepad connect/disconnect events.
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::Connected => {
                    let name = gilrs.gamepad(event.id).name().to_string();
                    {
                        let mut ws = lock_mutex(&web_state);
                        if ws.controllers.iter().any(|c| c.id == event.id) {
                            continue;
                        }
                        let Some(slot) = lowest_free_slot(&ws.controllers, num_slots) else {
                            warn!(
                                "{} connected but no free slot",
                                gilrs.gamepad(event.id).name()
                            );
                            continue;
                        };
                        info!("{} connected, assigned slot {}", name, slot);
                        ws.controllers.push(ControllerState {
                            id: event.id,
                            slot: Some(slot),
                            name: name.clone(),
                            battery: 0,
                            is_vibrating: false,
                            input: None,
                        });
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
                gilrs::EventType::Disconnected => {
                    let name = gilrs.gamepad(event.id).name().to_string();
                    let freed_slot = {
                        let mut ws = lock_mutex(&web_state);
                        let Some(pos) = ws.controllers.iter().position(|c| c.id == event.id) else {
                            continue;
                        };
                        let controller = ws.controllers.remove(pos);
                        let freed_slot = controller.slot;
                        ws.status = format!("{} disconnected", name);
                        freed_slot
                    };
                    if let Some(freed_slot) = freed_slot {
                        info!("{} disconnected, slot {} freed", name, freed_slot);
                    } else {
                        info!("{} disconnected", name);
                    }
                    if let Some(mut effects) = rumble_effects.remove(&event.id) {
                        if let Some(effect) = effects.strong.take() {
                            let _ = effect.stop();
                        }
                        if let Some(effect) = effects.weak.take() {
                            let _ = effect.stop();
                        }
                    }
                }
                _ => {}
            }
        }

        // Poll and send per slot (idle slots send neutral reports).
        // Collect inputs while holding the web lock (mirroring battery/name/
        // inputs into the web state), then drop the guard before any sends so
        // a stalled slot thread can never block the web UI.
        let mut ws_guard = lock_mutex(&web_state);
        let mut inputs = Vec::with_capacity(num_slots);
        for slot in 0..num_slots {
            let input = match ws_guard
                .controllers
                .iter_mut()
                .find(|c| c.slot == Some(slot))
            {
                Some(controller) => {
                    let gamepad = gilrs.gamepad(controller.id);
                    let mut inp = mapping.poll(&gamepad);
                    // Set battery level from controller's power info
                    let battery = battery_byte(gamepad.power_info());
                    inp.battery = battery;
                    // Mirror the latest raw Xbox state into the web UI.
                    controller.battery = battery;
                    controller.name = gamepad.name().to_string();
                    controller.input = Some(XboxInput::from_gamepad(&gamepad));
                    inp
                }
                None => NEUTRAL_INPUT,
            };
            inputs.push(input);
        }
        drop(ws_guard);

        // Non-blocking latest-wins sends: a full channel just drops the stale
        // input and the slot picks up the next poll's input. Disconnected
        // (no-gadget dev mode drops all receivers) must stay silent.
        for (slot, tx) in input_tx.iter().enumerate().take(num_slots) {
            if let Err(e) = tx.send_timeout(inputs[slot], Duration::from_millis(1)) {
                match e {
                    flume::SendTimeoutError::Timeout(_) => {
                        warn!("Slot {} send timed out; dropping stale input", slot);
                    }
                    flume::SendTimeoutError::Disconnected(_) => {}
                }
            }
        }

        if let Err(e) = limiter.check() {
            thread::sleep(e.wait_time_from(clock.now()));
        }
    }
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

    #[test]
    fn test_battery_byte() {
        assert_eq!(battery_byte(PowerInfo::Discharging(80)), 0x81); // full + host
        assert_eq!(battery_byte(PowerInfo::Discharging(40)), 0x41); // 2/4 + host
        assert_eq!(battery_byte(PowerInfo::Charging(80)), 0x91); // full charging + host
        assert_eq!(battery_byte(PowerInfo::Charged), 0x91);
        assert_eq!(battery_byte(PowerInfo::Wired), 0x81);
        assert_eq!(battery_byte(PowerInfo::Unknown), 0x81);
    }
}
