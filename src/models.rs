use gilrs::GamepadId;

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
    battery: 0,
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

/// A live mapping from a physical Xbox gamepad to one Switch slot.
pub struct Controller {
    pub id: GamepadId,
    pub slot: usize,
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

/// A controller as displayed by the web UI.
#[derive(Clone, Debug)]
pub struct WebController {
    /// Numeric gilrs gamepad id (`usize::from(GamepadId)`).
    pub id: usize,
    pub name: String,
    pub slot: Option<usize>,
    /// Raw Switch battery byte from `SwitchInput` (see `SwitchInput::battery`).
    pub battery: u8,
    pub is_vibrating: bool,
}

/// Read-only snapshot the web layer renders from. Maintained by the bridge
/// thread; the web thread never mutates it.
#[derive(Clone, Debug, Default)]
pub struct WebState {
    pub controllers: Vec<WebController>,
    /// Latest raw Xbox input per controller, aligned by index with `controllers`.
    pub inputs: Vec<Option<XboxInput>>,
    pub status: String,
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
