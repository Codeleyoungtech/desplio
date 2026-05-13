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

        Ok(Self { pointer, target })
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
