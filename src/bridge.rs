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
use crate::models::{AppState, Controller, Rumble, SwitchInput, NEUTRAL_INPUT};
use crate::motion;
use crate::ui::{UiCommand, UiEvent};
use crate::wiimote::MotionHandle;

/// Slot that receives Wiimote motion, if any.
static MOTION_SLOT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);
/// Shared handle to the latest Wiimote sample.
static MOTION_HANDLE: std::sync::OnceLock<MotionHandle> = std::sync::OnceLock::new();

/// Attach a Wiimote motion source to a slot.
pub fn set_motion_slot(slot: usize, handle: MotionHandle) -> Result<(), &'static str> {
    if MOTION_HANDLE.set(handle).is_err() {
        return Err("motion handle already set");
    }
    MOTION_SLOT.store(slot, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Update which slot receives Wiimote motion (without changing the handle).
pub fn update_motion_slot(slot: usize) {
    MOTION_SLOT.store(slot, std::sync::atomic::Ordering::Relaxed);
}

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

const RUMBLE_MAX_MAGNITUDE: u16 = u16::MAX;

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
    ui_command_rx: Receiver<UiCommand>,
    ui_event_tx: Sender<UiEvent>,
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
        &clock,
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

        // Handle UI commands.
        while let Ok(cmd) = ui_command_rx.try_recv() {
            match cmd {
                UiCommand::Remap {
                    controller_id,
                    new_slot,
                } => {
                    if new_slot >= num_slots {
                        warn!("Invalid slot {} for remap", new_slot);
                        continue;
                    }
                    // Find the controller
                    if let Some(controller) = controllers.iter().find(|c| c.id == controller_id) {
                        let old_slot = controller.slot;
                        if old_slot == new_slot {
                            continue;
                        }
                        // Check if new slot is occupied
                        if slot_occupied[new_slot] {
                            // Find what's in the new slot and swap
                            if let Some(other) = controllers.iter_mut().find(|c| c.slot == new_slot)
                            {
                                other.slot = old_slot;
                                slot_occupied[old_slot] = true;
                                info!(
                                    "Swapped slots: {} (slot {}) <-> {} (slot {})",
                                    gilrs.gamepad(controller_id).name(),
                                    new_slot,
                                    gilrs.gamepad(other.id).name(),
                                    old_slot
                                );
                                // Update UI
                                let _ = ui_event_tx.send(UiEvent::SlotUpdated {
                                    slot: old_slot,
                                    controller_id: Some(other.id),
                                    controller_name: Some(
                                        gilrs.gamepad(other.id).name().to_string(),
                                    ),
                                });
                            }
                        } else {
                            slot_occupied[old_slot] = false;
                            slot_occupied[new_slot] = true;
                            info!(
                                "Remapped {} from slot {} to slot {}",
                                gilrs.gamepad(controller_id).name(),
                                old_slot,
                                new_slot
                            );
                        }
                        // Update the controller's slot
                        if let Some(controller) =
                            controllers.iter_mut().find(|c| c.id == controller_id)
                        {
                            controller.slot = new_slot;
                        }
                        // Update UI
                        let _ = ui_event_tx.send(UiEvent::SlotUpdated {
                            slot: new_slot,
                            controller_id: Some(controller_id),
                            controller_name: Some(gilrs.gamepad(controller_id).name().to_string()),
                        });
                    }
                }
                UiCommand::Identify { controller_id } => {
                    if let Some(effects) = rumble_effects.get_mut(&controller_id) {
                        // Spawn a separate thread for vibration to avoid blocking input polling
                        let strong_effect = effects.strong.take();
                        let ui_event_tx = ui_event_tx.clone();

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
                            let _ =
                                ui_event_tx.send(UiEvent::VibrationComplete { id: controller_id });
                        });
                    }
                }
                UiCommand::Vibrate {
                    controller_id,
                    duration_ms,
                } => {
                    if let Some(effects) = rumble_effects.get_mut(&controller_id) {
                        // Spawn a separate thread for vibration to avoid blocking input polling
                        let strong_effect = effects.strong.take();
                        let ui_event_tx = ui_event_tx.clone();

                        thread::spawn(move || {
                            if let Some(effect) = strong_effect {
                                let _ = effect.play();
                                thread::sleep(Duration::from_millis(duration_ms));
                                let _ = effect.stop();
                                // Note: effect is dropped here, which stops it
                            }
                            let _ =
                                ui_event_tx.send(UiEvent::VibrationComplete { id: controller_id });
                        });
                    }
                }
                UiCommand::SetMotionSlot { slot } => {
                    if slot >= num_slots {
                        warn!("Invalid slot {} for motion slot", slot);
                        continue;
                    }
                    update_motion_slot(slot);
                    info!("Wiimote motion assigned to slot {}", slot);
                    let _ = ui_event_tx.send(UiEvent::MotionSlotUpdated { slot });
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
                            info!(
                                "{} connected, assigned slot {}",
                                gilrs.gamepad(event.id).name(),
                                slot
                            );
                            slot_occupied[slot] = true;
                            controllers.push(Controller { id: event.id, slot });
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
                            // Notify UI
                            let _ = ui_event_tx.send(UiEvent::ControllerConnected {
                                id: event.id,
                                name: gilrs.gamepad(event.id).name().to_string(),
                            });
                            let _ = ui_event_tx.send(UiEvent::SlotUpdated {
                                slot,
                                controller_id: Some(event.id),
                                controller_name: Some(gilrs.gamepad(event.id).name().to_string()),
                            });
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
                        info!(
                            "{} disconnected, slot {} freed",
                            gilrs.gamepad(event.id).name(),
                            controller.slot
                        );
                        slot_occupied[controller.slot] = false;
                        if let Some(mut effects) = rumble_effects.remove(&event.id) {
                            if let Some(effect) = effects.strong.take() {
                                let _ = effect.stop();
                            }
                            if let Some(effect) = effects.weak.take() {
                                let _ = effect.stop();
                            }
                        }
                        // Notify UI
                        let _ = ui_event_tx.send(UiEvent::ControllerDisconnected { id: event.id });
                        let _ = ui_event_tx.send(UiEvent::SlotUpdated {
                            slot: controller.slot,
                            controller_id: None,
                            controller_name: None,
                        });
                    }
                }
                _ => {}
            }
        }

        // Poll and send per slot (idle slots send neutral reports).
        for (slot, tx) in input_tx.iter().enumerate().take(num_slots) {
            let mut input = match controllers.iter().find(|c| c.slot == slot) {
                Some(controller) => {
                    let gamepad = gilrs.gamepad(controller.id);
                    let mut inp = mapping.poll(&gamepad);
                    // Set battery level from controller's power info
                    inp.battery = match gamepad.power_info() {
                        PowerInfo::Discharging(pct) => percentage_to_level(pct) << 5,
                        PowerInfo::Charging(pct) => (percentage_to_level(pct) << 5) | 0x10,
                        PowerInfo::Charged => (4 << 5) | 0x10, // full + charging
                        PowerInfo::Wired => 0x01,              // USB powered
                        _ => 0,                                // unknown/empty
                    };
                    inp
                }
                None => NEUTRAL_INPUT,
            };
            // Inject Wiimote motion into the bound slot.
            if slot == MOTION_SLOT.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(handle) = MOTION_HANDLE.get() {
                    if let Ok(motion_handle) = handle.lock() {
                        if let Some(raw) = motion_handle.as_ref() {
                            input.motion = motion::to_switch_motion(raw);
                        }
                    }
                }
            }
            let _ = tx.send(input);
        }

        if let Err(e) = limiter.check() {
            thread::sleep(e.wait_time_from(clock.now()));
        }
    }
}

fn lowest_free_slot(slot_occupied: &[bool]) -> Option<usize> {
    slot_occupied.iter().position(|&occupied| !occupied)
}
