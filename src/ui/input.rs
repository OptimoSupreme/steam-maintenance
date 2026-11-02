//! SDL events to UI actions: controller, keyboard and mouse, with a deadzone and
//! repeat throttle on the left stick.

use sdl2::controller::{Axis, Button};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use std::time::{Duration, Instant};

const STICK_DEADZONE: i16 = 16000;
const STICK_REPEAT: Duration = Duration::from_millis(280);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Confirm,
    Back,
    Move(Dir),
    Hover(i32, i32),
    Click(i32, i32),
    Quit,
}

/// Left-stick state between events.
#[derive(Debug, Default)]
pub struct Stick {
    x: i16,
    y: i16,
    last_move: Option<Instant>,
}

impl Stick {
    /// Direction the stick points past the deadzone, if any.
    pub fn direction(&self) -> Option<Dir> {
        direction_of(self.x, self.y, STICK_DEADZONE)
    }

    /// Emit a move when the stick is held past the deadzone: at once on the first
    /// poll, then every `STICK_REPEAT`. Call on every loop iteration.
    pub fn poll(&mut self, now: Instant) -> Option<Action> {
        let dir = self.direction()?;
        match self.last_move {
            Some(last) if now.duration_since(last) < STICK_REPEAT => None,
            _ => {
                self.last_move = Some(now);
                Some(Action::Move(dir))
            }
        }
    }

    fn set(&mut self, axis: Axis, value: i16) {
        match axis {
            Axis::LeftX => self.x = value,
            Axis::LeftY => self.y = value,
            _ => {}
        }
        if self.direction().is_none() {
            self.last_move = None;
        }
    }
}

/// Dominant axis beyond the deadzone, or None when centered.
pub fn direction_of(x: i16, y: i16, deadzone: i16) -> Option<Dir> {
    let (ax, ay) = (x.unsigned_abs(), y.unsigned_abs());
    if ax < deadzone as u16 && ay < deadzone as u16 {
        return None;
    }
    Some(if ax >= ay {
        if x < 0 {
            Dir::Left
        } else {
            Dir::Right
        }
    } else if y < 0 {
        Dir::Up
    } else {
        Dir::Down
    })
}

/// Translate one SDL event; axis motion only updates `stick`.
pub fn map_event(event: &Event, stick: &mut Stick) -> Option<Action> {
    match event {
        Event::Quit { .. } => Some(Action::Quit),
        Event::ControllerButtonDown { button, .. } => match button {
            Button::A | Button::Start => Some(Action::Confirm),
            Button::B => Some(Action::Back),
            Button::DPadLeft => Some(Action::Move(Dir::Left)),
            Button::DPadRight => Some(Action::Move(Dir::Right)),
            Button::DPadUp => Some(Action::Move(Dir::Up)),
            Button::DPadDown => Some(Action::Move(Dir::Down)),
            _ => None,
        },
        Event::ControllerAxisMotion { axis, value, .. } => {
            stick.set(*axis, *value);
            None
        }
        Event::KeyDown {
            keycode: Some(key),
            repeat,
            ..
        } => map_key(*key, *repeat),
        Event::MouseMotion { x, y, .. } => Some(Action::Hover(*x, *y)),
        Event::MouseButtonDown {
            mouse_btn: MouseButton::Left,
            x,
            y,
            ..
        } => Some(Action::Click(*x, *y)),
        _ => None,
    }
}

fn map_key(key: Keycode, repeat: bool) -> Option<Action> {
    let action = match key {
        Keycode::Return | Keycode::KpEnter | Keycode::Space => Action::Confirm,
        Keycode::Escape => Action::Back,
        Keycode::Left => Action::Move(Dir::Left),
        Keycode::Right => Action::Move(Dir::Right),
        Keycode::Up => Action::Move(Dir::Up),
        Keycode::Down => Action::Move(Dir::Down),
        _ => return None,
    };
    // Held arrows repeat; a held Enter or Esc must not.
    if repeat && !matches!(action, Action::Move(_)) {
        return None;
    }
    Some(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_direction_respects_the_deadzone_and_dominant_axis() {
        assert_eq!(direction_of(0, 0, 16000), None);
        assert_eq!(direction_of(15999, -15999, 16000), None);
        assert_eq!(direction_of(-20000, 1000, 16000), Some(Dir::Left));
        assert_eq!(direction_of(20000, -19000, 16000), Some(Dir::Right));
        assert_eq!(direction_of(1000, -20000, 16000), Some(Dir::Up));
        assert_eq!(direction_of(-1000, 20000, 16000), Some(Dir::Down));
    }

    #[test]
    fn stick_moves_once_then_repeats_on_the_throttle() {
        let mut stick = Stick::default();
        let t0 = Instant::now();
        stick.set(Axis::LeftX, 30000);
        assert_eq!(stick.poll(t0), Some(Action::Move(Dir::Right)));
        assert_eq!(stick.poll(t0 + Duration::from_millis(100)), None);
        assert_eq!(
            stick.poll(t0 + Duration::from_millis(300)),
            Some(Action::Move(Dir::Right))
        );
        stick.set(Axis::LeftX, 0);
        assert_eq!(stick.poll(t0 + Duration::from_millis(310)), None);
        stick.set(Axis::LeftX, -30000);
        assert_eq!(
            stick.poll(t0 + Duration::from_millis(320)),
            Some(Action::Move(Dir::Left))
        );
    }

    #[test]
    fn held_enter_does_not_repeat_but_arrows_do() {
        assert_eq!(map_key(Keycode::Return, false), Some(Action::Confirm));
        assert_eq!(map_key(Keycode::Return, true), None);
        assert_eq!(map_key(Keycode::Left, true), Some(Action::Move(Dir::Left)));
        assert_eq!(map_key(Keycode::Escape, false), Some(Action::Back));
        assert_eq!(map_key(Keycode::X, false), None);
    }
}
