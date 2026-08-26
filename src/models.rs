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

/// Gyroscope + accelerometer data for a single Switch Pro Controller slot,
/// expressed in the Switch's raw int16 report units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    /// Angular velocity in rad/s scaled to Switch int16 units (3 axes).
    pub gyro: [i16; 3],
    /// Linear acceleration in g scaled to Switch int16 units (3 axes).
    pub accel: [i16; 3],
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

    pub motion: Motion,
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
    motion: Motion {
        gyro: [0; 3],
        accel: [0; 3],
    },
};

/// Rumble event raised by a gadget slot (from a Switch output report).
#[derive(Clone, Copy, Debug)]
pub struct Rumble {
    pub slot: usize,
    /// Amplitude 0..255 (higher of HF/LF envelope).
    pub magnitude: u8,
}

/// A live mapping from a physical Xbox gamepad to one Switch slot.
pub struct Controller {
    pub id: GamepadId,
    pub slot: usize,
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
