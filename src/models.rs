use gilrs::GamepadId;

/// Lock a mutex, recovering from poisoning: a thread that panicked while
/// holding the lock must not take the whole daemon (or web UI) down with it.
pub fn lock_mutex<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stick {
    /// X in range [-1.0 .. 1.0]. -1.0 is Left, 1.0 is Right.
    pub x: f32,
    /// Y in range [-1.0 .. 1.0]. 1.0 is Up, -1.0 is Down.
    pub y: f32,
}

impl Stick {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// The state to be written to a single Switch Pro Controller slot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SwitchInput {
    pub dpad_up: bool,
    pub dpad_down: bool,
    pub dpad_left: bool,
    pub dpad_right: bool,

    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,

    pub r: bool,
    pub zr: bool,
    pub l: bool,
    pub zl: bool,

    pub home: bool,
    pub plus: bool,
    pub minus: bool,
    pub capture: bool,

    pub left_stick_press: bool,
    pub right_stick_press: bool,

    pub left_stick: Stick,
    pub right_stick: Stick,

    /// Battery level: bits [7:5] = capacity (0=empty .. 4=full),
    /// bit 4 = charging, bit 0 = host_powered (USB).
    pub battery: u8,
}

/// Neutral input report: centered sticks, no buttons pressed.
pub const NEUTRAL_INPUT: SwitchInput = SwitchInput {
    dpad_up: false,
    dpad_down: false,
    dpad_left: false,
    dpad_right: false,
    a: false,
    b: false,
    x: false,
    y: false,
    r: false,
    zr: false,
    l: false,
    zl: false,
    home: false,
    plus: false,
    minus: false,
    capture: false,
    left_stick_press: false,
    right_stick_press: false,
    left_stick: Stick::new(0.0, 0.0),
    right_stick: Stick::new(0.0, 0.0),
    battery: 0x81,
};

/// Rumble event raised by a gadget slot (from a Switch output report).
#[derive(Clone, Copy, Debug)]
pub struct Rumble {
    pub slot: usize,
    /// Left motor amplitude 0..255 (peak of HF/LF envelope).
    pub left: u8,
    /// Right motor amplitude 0..255 (peak of HF/LF envelope).
    pub right: u8,
}

/// A live controller. Single source of truth: maintained by the bridge thread,
/// rendered by the web thread (which never mutates it).
#[derive(Clone, Debug)]
pub struct ControllerState {
    pub id: GamepadId,
    /// Assigned Switch slot (None = not assigned).
    pub slot: Option<usize>,
    pub name: String,
    /// Raw Switch battery byte (see `SwitchInput::battery`).
    pub battery: u8,
    pub is_vibrating: bool,
    /// Latest raw Xbox input snapshot (web tester page).
    pub input: Option<XboxInput>,
}

/// Raw Xbox input snapshot used by the web tester page (Xbox-native labels).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XboxInput {
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
    pub lb: bool,
    pub rb: bool,
    /// Left trigger button pressed.
    pub lt: bool,
    /// Right trigger button pressed.
    pub rt: bool,
    /// Left trigger analog value 0.0 (rest) .. 1.0 (full pull).
    pub lt_value: f32,
    /// Right trigger analog value 0.0 (rest) .. 1.0 (full pull).
    pub rt_value: f32,
    pub view: bool,
    pub menu: bool,
    pub xbox: bool,
    pub left_stick_press: bool,
    pub right_stick_press: bool,
    pub dpad_up: bool,
    pub dpad_down: bool,
    pub dpad_left: bool,
    pub dpad_right: bool,
    pub left_stick: Stick,
    pub right_stick: Stick,
}

/// Read-only snapshot the web layer renders from. Maintained by the bridge
/// thread; the web thread never mutates it.
#[derive(Clone, Debug, Default)]
pub struct WebState {
    pub controllers: Vec<ControllerState>,
    pub status: String,
}

/// Result of a slot-remap attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemapOutcome {
    NotFound,
    InvalidSlot,
    SameSlot,
    Moved,
    Swapped { old_slot: usize, new_slot: usize },
}

/// Minimal (id, slot) assignment entry used by the remap core; also lets unit
/// tests exercise the swap/move logic without constructing gilrs ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotEntry {
    pub id: usize,
    pub slot: Option<usize>,
}

/// Apply a remap of `controller_id` (numeric gilrs id) to `new_slot`.
/// Pure function over the controllers vec (single source of truth).
pub fn apply_remap(
    controllers: &mut [ControllerState],
    controller_id: usize,
    new_slot: usize,
    num_slots: usize,
) -> RemapOutcome {
    let mut entries: Vec<SlotEntry> = controllers
        .iter()
        .map(|c| SlotEntry {
            id: usize::from(c.id),
            slot: c.slot,
        })
        .collect();
    let outcome = apply_remap_entries(&mut entries, controller_id, new_slot, num_slots);
    for (controller, entry) in controllers.iter_mut().zip(entries) {
        controller.slot = entry.slot;
    }
    outcome
}

/// Core remap logic over `SlotEntry` pairs. Pure and unit-testable.
pub fn apply_remap_entries(
    entries: &mut [SlotEntry],
    controller_id: usize,
    new_slot: usize,
    num_slots: usize,
) -> RemapOutcome {
    if new_slot >= num_slots {
        return RemapOutcome::InvalidSlot;
    }
    let Some(idx) = entries.iter().position(|e| e.id == controller_id) else {
        return RemapOutcome::NotFound;
    };
    let Some(old_slot) = entries[idx].slot else {
        return RemapOutcome::NotFound; // not assigned to a slot
    };
    if old_slot == new_slot {
        return RemapOutcome::SameSlot;
    }
    if let Some(other_idx) = entries.iter().position(|e| e.slot == Some(new_slot)) {
        entries[other_idx].slot = Some(old_slot);
        entries[idx].slot = Some(new_slot);
        return RemapOutcome::Swapped { old_slot, new_slot };
    }
    entries[idx].slot = Some(new_slot);
    RemapOutcome::Moved
}

/// Commands sent from the web UI to the bridge thread.
#[derive(Debug, Clone, Copy)]
pub enum Command {
    /// Remap a controller (numeric gilrs id) to a new slot.
    Remap {
        controller_id: usize,
        new_slot: usize,
    },
    /// Send a quick vibration to identify a controller.
    Identify { controller_id: usize },
    /// Send vibration for a specific duration.
    Vibrate {
        controller_id: usize,
        duration_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppState {
    Connected,
    Exiting,
}

impl AppState {
    pub const fn is_exiting(&self) -> bool {
        matches!(*self, AppState::Exiting)
    }
}

pub const MAX_SLOTS: usize = 8;
pub const DEFAULT_SLOTS: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(
        id_a: usize,
        slot_a: Option<usize>,
        id_b: usize,
        slot_b: Option<usize>,
    ) -> Vec<SlotEntry> {
        vec![
            SlotEntry {
                id: id_a,
                slot: slot_a,
            },
            SlotEntry {
                id: id_b,
                slot: slot_b,
            },
        ]
    }

    #[test]
    fn test_apply_remap_entries_move() {
        let mut e = entries(10, Some(0), 20, Some(1));
        assert_eq!(apply_remap_entries(&mut e, 10, 2, 4), RemapOutcome::Moved);
        assert_eq!(e[0].slot, Some(2));
        assert_eq!(e[1].slot, Some(1));
    }

    #[test]
    fn test_apply_remap_entries_swap() {
        let mut e = entries(10, Some(0), 20, Some(1));
        assert_eq!(
            apply_remap_entries(&mut e, 10, 1, 4),
            RemapOutcome::Swapped {
                old_slot: 0,
                new_slot: 1
            }
        );
        assert_eq!(e[0].slot, Some(1));
        assert_eq!(e[1].slot, Some(0));
    }

    #[test]
    fn test_apply_remap_entries_invalid_slot() {
        let mut e = entries(10, Some(0), 20, Some(1));
        assert_eq!(
            apply_remap_entries(&mut e, 10, 4, 4),
            RemapOutcome::InvalidSlot
        );
    }

    #[test]
    fn test_apply_remap_entries_not_found() {
        let mut e = entries(10, Some(0), 20, Some(1));
        assert_eq!(
            apply_remap_entries(&mut e, 999, 1, 4),
            RemapOutcome::NotFound
        );
    }

    #[test]
    fn test_apply_remap_entries_same_slot() {
        let mut e = entries(10, Some(0), 20, Some(1));
        assert_eq!(
            apply_remap_entries(&mut e, 10, 0, 4),
            RemapOutcome::SameSlot
        );
        assert_eq!(e[0].slot, Some(0));
        assert_eq!(e[1].slot, Some(1));
    }
}
