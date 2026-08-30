//! Xbox -> Switch Pro Controller input mapping.
//!
//! Ported from `xboxjoystick.go` and the HIDtoVPAD `controller_manager.rs`.
//! Converts a `gilrs::Gamepad` snapshot into a `SwitchInput`.

use gilrs::{Axis, Button, Gamepad};

use crate::models::{Stick, SwitchInput};

/// Trigger axis threshold above which ZL/ZR is considered pressed. From
/// `xboxjoystick.go:7` (-0.8): gilrs normalizes trigger axes to [-1, 1] with
/// the resting value at -1.0 (axis min = 0), so -0.8 fires at ~10% pull,
/// same as the reference.
const TRIGGER_THRESHOLD: f32 = -0.8;

pub struct Mapping;

impl Mapping {
    pub fn new() -> Self {
        Mapping
    }

    /// Poll a gamepad and produce a `SwitchInput`.
    pub fn poll(&self, gamepad: &Gamepad) -> SwitchInput {
        SwitchInput {
            // Buttons (A/B/X/Y swapped, following xboxjoystick.go).
            b: gamepad.is_pressed(Button::South), // Xbox A -> Switch B
            a: gamepad.is_pressed(Button::East),  // Xbox B -> Switch A
            y: gamepad.is_pressed(Button::West),  // Xbox X -> Switch Y
            x: gamepad.is_pressed(Button::North), // Xbox Y -> Switch X
            l: gamepad.is_pressed(Button::LeftTrigger),
            r: gamepad.is_pressed(Button::RightTrigger),
            minus: gamepad.is_pressed(Button::Select),
            plus: gamepad.is_pressed(Button::Start),
            home: gamepad.is_pressed(Button::Mode),
            left_stick_press: gamepad.is_pressed(Button::LeftThumb),
            right_stick_press: gamepad.is_pressed(Button::RightThumb),
            capture: false,
            // D-pad (hat axes exposed as buttons by gilrs).
            dpad_up: gamepad.is_pressed(Button::DPadUp),
            dpad_down: gamepad.is_pressed(Button::DPadDown),
            dpad_left: gamepad.is_pressed(Button::DPadLeft),
            dpad_right: gamepad.is_pressed(Button::DPadRight),
            // Triggers: axis value above threshold OR trigger button pressed.
            zl: self.trigger_pressed(gamepad, Axis::LeftZ, Button::LeftTrigger2),
            zr: self.trigger_pressed(gamepad, Axis::RightZ, Button::RightTrigger2),
            // Sticks. Y is inverted so up (negative raw) maps to +1 (top).
            left_stick: Stick::new(
                gamepad.value(Axis::LeftStickX),
                -gamepad.value(Axis::LeftStickY),
            ),
            right_stick: Stick::new(
                gamepad.value(Axis::RightStickX),
                -gamepad.value(Axis::RightStickY),
            ),
            // Battery will be set by bridge.rs after this call
            battery: 0,
        }
    }

    fn trigger_pressed(&self, gamepad: &Gamepad, axis: Axis, button: Button) -> bool {
        gamepad.is_pressed(button) || gamepad.value(axis) >= TRIGGER_THRESHOLD
    }
}
