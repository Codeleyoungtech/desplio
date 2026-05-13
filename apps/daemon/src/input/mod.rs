use std::io;
use std::sync::{Arc, Mutex};

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode,
    UinputAbsSetup,
};
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use crate::display::LiveVideoSource;

pub type SharedInputInjector = Arc<Mutex<Option<InputInjector>>>;

#[derive(Debug, Clone, Copy)]
pub struct InputTarget {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientInputMessage {
    Pointer {
        phase: PointerPhase,
        x: f32,
        y: f32,
        button: Option<PointerButton>,
    },
    Wheel {
        dx: f32,
        dy: f32,
    },
    Key {
        code: String,
        pressed: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerPhase {
    Move,
    Down,
    Up,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Error)]
pub enum InputError {
    #[error("failed to create uinput device: {0}")]
    Create(#[from] io::Error),
}

pub fn shared_input_for_live_source(source: Option<&LiveVideoSource>) -> SharedInputInjector {
    let injector = source
        .and_then(input_target_from_live_source)
        .and_then(|target| match InputInjector::new(target) {
            Ok(injector) => {
                debug!(?target, "M5 input injector is ready");
                Some(injector)
            }
            Err(err) => {
                warn!(
                    error = %err,
                    "M5 input injector is disabled; check /dev/uinput permissions if remote input is needed"
                );
                None
            }
        });

    Arc::new(Mutex::new(injector))
}

fn input_target_from_live_source(source: &LiveVideoSource) -> Option<InputTarget> {
    match source {
        LiveVideoSource::X11Grab {
            x,
            y,
            width,
            height,
            ..
        } => Some(InputTarget {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        }),
    }
}

pub struct InputInjector {
    pointer: VirtualDevice,
    keyboard: VirtualDevice,
    target: InputTarget,
}

impl InputInjector {
    pub fn new(target: InputTarget) -> Result<Self, InputError> {
        let max_x = target.x.saturating_add(target.width).saturating_sub(1) as i32;
        let max_y = target.y.saturating_add(target.height).saturating_sub(1) as i32;

        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::BTN_LEFT);
        keys.insert(KeyCode::BTN_MIDDLE);
        keys.insert(KeyCode::BTN_RIGHT);

        let mut rel_axes = AttributeSet::<RelativeAxisCode>::new();
        rel_axes.insert(RelativeAxisCode::REL_WHEEL);
        rel_axes.insert(RelativeAxisCode::REL_HWHEEL);

        let pointer = VirtualDevice::builder()?
            .name("Desplio Virtual Pointer")
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_X,
                AbsInfo::new(0, 0, max_x.max(1), 0, 0, 1),
            ))?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_Y,
                AbsInfo::new(0, 0, max_y.max(1), 0, 0, 1),
            ))?
            .with_keys(&keys)?
            .with_relative_axes(&rel_axes)?
            .build()?;

        let keyboard_keys = keyboard_keys();
        let keyboard = VirtualDevice::builder()?
            .name("Desplio Virtual Keyboard")
            .with_keys(&keyboard_keys)?
            .build()?;

        Ok(Self {
            pointer,
            keyboard,
            target,
        })
    }

    pub fn handle_client_message(&mut self, message: ClientInputMessage) -> io::Result<()> {
        match message {
            ClientInputMessage::Pointer {
                phase,
                x,
                y,
                button,
            } => self.pointer_event(phase, x, y, button),
            ClientInputMessage::Wheel { dx, dy } => self.wheel_event(dx, dy),
            ClientInputMessage::Key { code, pressed } => self.key_event(&code, pressed),
        }
    }

    fn pointer_event(
        &mut self,
        phase: PointerPhase,
        x: f32,
        y: f32,
        button: Option<PointerButton>,
    ) -> io::Result<()> {
        let (x, y) = self.map_coords(x, y);
        let mut events = vec![
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
        ];

        if let Some(button) = button.map(button_key) {
            let value = match phase {
                PointerPhase::Down => Some(1),
                PointerPhase::Up => Some(0),
                PointerPhase::Move => None,
            };
            if let Some(value) = value {
                events.push(InputEvent::new(EventType::KEY.0, button.0, value));
            }
        }

        self.pointer.emit(&events)
    }

    fn wheel_event(&mut self, dx: f32, dy: f32) -> io::Result<()> {
        let wheel_y = wheel_units(dy);
        let wheel_x = wheel_units(dx);
        if wheel_x == 0 && wheel_y == 0 {
            return Ok(());
        }

        let mut events = Vec::new();
        if wheel_y != 0 {
            events.push(InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_WHEEL.0,
                -wheel_y,
            ));
        }
        if wheel_x != 0 {
            events.push(InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_HWHEEL.0,
                wheel_x,
            ));
        }

        self.pointer.emit(&events)
    }

    fn key_event(&mut self, code: &str, pressed: bool) -> io::Result<()> {
        let Some(key) = web_code_to_key(code) else {
            return Ok(());
        };

        self.keyboard
            .emit(&[InputEvent::new(EventType::KEY.0, key.0, i32::from(pressed))])
    }

    fn map_coords(&self, nx: f32, ny: f32) -> (i32, i32) {
        let nx = nx.clamp(0.0, 1.0);
        let ny = ny.clamp(0.0, 1.0);
        let x = self.target.x as f32 + nx * self.target.width.saturating_sub(1) as f32;
        let y = self.target.y as f32 + ny * self.target.height.saturating_sub(1) as f32;
        (x.round() as i32, y.round() as i32)
    }
}

fn button_key(button: PointerButton) -> KeyCode {
    match button {
        PointerButton::Left => KeyCode::BTN_LEFT,
        PointerButton::Middle => KeyCode::BTN_MIDDLE,
        PointerButton::Right => KeyCode::BTN_RIGHT,
    }
}

fn wheel_units(delta: f32) -> i32 {
    (delta / 80.0).round().clamp(-10.0, 10.0) as i32
}

fn keyboard_keys() -> AttributeSet<KeyCode> {
    [
        KeyCode::KEY_ESC,
        KeyCode::KEY_1,
        KeyCode::KEY_2,
        KeyCode::KEY_3,
        KeyCode::KEY_4,
        KeyCode::KEY_5,
        KeyCode::KEY_6,
        KeyCode::KEY_7,
        KeyCode::KEY_8,
        KeyCode::KEY_9,
        KeyCode::KEY_0,
        KeyCode::KEY_MINUS,
        KeyCode::KEY_EQUAL,
        KeyCode::KEY_BACKSPACE,
        KeyCode::KEY_TAB,
        KeyCode::KEY_Q,
        KeyCode::KEY_W,
        KeyCode::KEY_E,
        KeyCode::KEY_R,
        KeyCode::KEY_T,
        KeyCode::KEY_Y,
        KeyCode::KEY_U,
        KeyCode::KEY_I,
        KeyCode::KEY_O,
        KeyCode::KEY_P,
        KeyCode::KEY_LEFTBRACE,
        KeyCode::KEY_RIGHTBRACE,
        KeyCode::KEY_ENTER,
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_A,
        KeyCode::KEY_S,
        KeyCode::KEY_D,
        KeyCode::KEY_F,
        KeyCode::KEY_G,
        KeyCode::KEY_H,
        KeyCode::KEY_J,
        KeyCode::KEY_K,
        KeyCode::KEY_L,
        KeyCode::KEY_SEMICOLON,
        KeyCode::KEY_APOSTROPHE,
        KeyCode::KEY_GRAVE,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_BACKSLASH,
        KeyCode::KEY_Z,
        KeyCode::KEY_X,
        KeyCode::KEY_C,
        KeyCode::KEY_V,
        KeyCode::KEY_B,
        KeyCode::KEY_N,
        KeyCode::KEY_M,
        KeyCode::KEY_COMMA,
        KeyCode::KEY_DOT,
        KeyCode::KEY_SLASH,
        KeyCode::KEY_RIGHTSHIFT,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_SPACE,
        KeyCode::KEY_CAPSLOCK,
        KeyCode::KEY_F1,
        KeyCode::KEY_F2,
        KeyCode::KEY_F3,
        KeyCode::KEY_F4,
        KeyCode::KEY_F5,
        KeyCode::KEY_F6,
        KeyCode::KEY_F7,
        KeyCode::KEY_F8,
        KeyCode::KEY_F9,
        KeyCode::KEY_F10,
        KeyCode::KEY_F11,
        KeyCode::KEY_F12,
        KeyCode::KEY_RIGHTCTRL,
        KeyCode::KEY_RIGHTALT,
        KeyCode::KEY_HOME,
        KeyCode::KEY_UP,
        KeyCode::KEY_PAGEUP,
        KeyCode::KEY_LEFT,
        KeyCode::KEY_RIGHT,
        KeyCode::KEY_END,
        KeyCode::KEY_DOWN,
        KeyCode::KEY_PAGEDOWN,
        KeyCode::KEY_INSERT,
        KeyCode::KEY_DELETE,
        KeyCode::KEY_LEFTMETA,
        KeyCode::KEY_RIGHTMETA,
    ]
    .into_iter()
    .collect()
}

fn web_code_to_key(code: &str) -> Option<KeyCode> {
    Some(match code {
        "Escape" => KeyCode::KEY_ESC,
        "Digit1" => KeyCode::KEY_1,
        "Digit2" => KeyCode::KEY_2,
        "Digit3" => KeyCode::KEY_3,
        "Digit4" => KeyCode::KEY_4,
        "Digit5" => KeyCode::KEY_5,
        "Digit6" => KeyCode::KEY_6,
        "Digit7" => KeyCode::KEY_7,
        "Digit8" => KeyCode::KEY_8,
        "Digit9" => KeyCode::KEY_9,
        "Digit0" => KeyCode::KEY_0,
        "Minus" => KeyCode::KEY_MINUS,
        "Equal" => KeyCode::KEY_EQUAL,
        "Backspace" => KeyCode::KEY_BACKSPACE,
        "Tab" => KeyCode::KEY_TAB,
        "KeyQ" => KeyCode::KEY_Q,
        "KeyW" => KeyCode::KEY_W,
        "KeyE" => KeyCode::KEY_E,
        "KeyR" => KeyCode::KEY_R,
        "KeyT" => KeyCode::KEY_T,
        "KeyY" => KeyCode::KEY_Y,
        "KeyU" => KeyCode::KEY_U,
        "KeyI" => KeyCode::KEY_I,
        "KeyO" => KeyCode::KEY_O,
        "KeyP" => KeyCode::KEY_P,
        "BracketLeft" => KeyCode::KEY_LEFTBRACE,
        "BracketRight" => KeyCode::KEY_RIGHTBRACE,
        "Enter" => KeyCode::KEY_ENTER,
        "ControlLeft" => KeyCode::KEY_LEFTCTRL,
        "ControlRight" => KeyCode::KEY_RIGHTCTRL,
        "KeyA" => KeyCode::KEY_A,
        "KeyS" => KeyCode::KEY_S,
        "KeyD" => KeyCode::KEY_D,
        "KeyF" => KeyCode::KEY_F,
        "KeyG" => KeyCode::KEY_G,
        "KeyH" => KeyCode::KEY_H,
        "KeyJ" => KeyCode::KEY_J,
        "KeyK" => KeyCode::KEY_K,
        "KeyL" => KeyCode::KEY_L,
        "Semicolon" => KeyCode::KEY_SEMICOLON,
        "Quote" => KeyCode::KEY_APOSTROPHE,
        "Backquote" => KeyCode::KEY_GRAVE,
        "ShiftLeft" => KeyCode::KEY_LEFTSHIFT,
        "ShiftRight" => KeyCode::KEY_RIGHTSHIFT,
        "Backslash" => KeyCode::KEY_BACKSLASH,
        "KeyZ" => KeyCode::KEY_Z,
        "KeyX" => KeyCode::KEY_X,
        "KeyC" => KeyCode::KEY_C,
        "KeyV" => KeyCode::KEY_V,
        "KeyB" => KeyCode::KEY_B,
        "KeyN" => KeyCode::KEY_N,
        "KeyM" => KeyCode::KEY_M,
        "Comma" => KeyCode::KEY_COMMA,
        "Period" => KeyCode::KEY_DOT,
        "Slash" => KeyCode::KEY_SLASH,
        "AltLeft" => KeyCode::KEY_LEFTALT,
        "AltRight" => KeyCode::KEY_RIGHTALT,
        "Space" => KeyCode::KEY_SPACE,
        "CapsLock" => KeyCode::KEY_CAPSLOCK,
        "F1" => KeyCode::KEY_F1,
        "F2" => KeyCode::KEY_F2,
        "F3" => KeyCode::KEY_F3,
        "F4" => KeyCode::KEY_F4,
        "F5" => KeyCode::KEY_F5,
        "F6" => KeyCode::KEY_F6,
        "F7" => KeyCode::KEY_F7,
        "F8" => KeyCode::KEY_F8,
        "F9" => KeyCode::KEY_F9,
        "F10" => KeyCode::KEY_F10,
        "F11" => KeyCode::KEY_F11,
        "F12" => KeyCode::KEY_F12,
        "Home" => KeyCode::KEY_HOME,
        "ArrowUp" => KeyCode::KEY_UP,
        "PageUp" => KeyCode::KEY_PAGEUP,
        "ArrowLeft" => KeyCode::KEY_LEFT,
        "ArrowRight" => KeyCode::KEY_RIGHT,
        "End" => KeyCode::KEY_END,
        "ArrowDown" => KeyCode::KEY_DOWN,
        "PageDown" => KeyCode::KEY_PAGEDOWN,
        "Insert" => KeyCode::KEY_INSERT,
        "Delete" => KeyCode::KEY_DELETE,
        "MetaLeft" => KeyCode::KEY_LEFTMETA,
        "MetaRight" => KeyCode::KEY_RIGHTMETA,
        _ => return None,
    })
}
