//! Input injection.
//!
//! The wire format uses W3C `KeyboardEvent.code` names and normalised
//! coordinates, so the platform layer is the only place that knows about pixels
//! and native key codes. Everything below is the desktop (`enigo`) mapping; the
//! mobile clients implement [`Injector`] with `AccessibilityService` (Android)
//! or `CGEvent`/`IOHIDEvent` (iOS, via a ReplayKit broadcast extension).

use std::collections::HashSet;

use anyhow::{Context, Result};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use vr_proto::{denormalized, InputEvent, MouseButton, TouchPhase};

/// A rectangle an input event can address.
#[derive(Debug, Clone, Copy)]
pub struct DisplayRect {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

/// Platform input sink.
pub trait Injector: Send {
    fn apply(&mut self, event: &InputEvent) -> Result<()>;
    fn release_all(&mut self) -> Result<()>;
}

/// Desktop injector backed by `enigo`.
pub struct DesktopInjector {
    enigo: Enigo,
    displays: Vec<DisplayRect>,
    pressed_buttons: HashSet<MouseButton>,
}

impl DesktopInjector {
    pub fn new(displays: Vec<DisplayRect>) -> Result<Self> {
        let enigo = Enigo::new(&Settings::default())
            .context("cannot create input sink; on macOS grant Accessibility permission")?;
        Ok(Self {
            enigo,
            displays,
            pressed_buttons: HashSet::new(),
        })
    }

    pub fn set_displays(&mut self, displays: Vec<DisplayRect>) {
        self.displays = displays;
    }

    fn rect(&self, display: u32) -> DisplayRect {
        self.displays
            .iter()
            .find(|d| d.id == display)
            .or_else(|| self.displays.first())
            .copied()
            .unwrap_or(DisplayRect {
                id: 0,
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale: 1.0,
            })
    }

    fn point(&self, display: u32, x: u16, y: u16) -> (i32, i32) {
        let r = self.rect(display);
        display_point(r, x, y)
    }
}

/// macOS gates synthetic events behind the Accessibility (TCC) grant. Enigo
/// still constructs successfully without it and then drops every event, so the
/// agent must ask explicitly instead of pretending input works.
#[cfg(target_os = "macos")]
pub fn accessibility_granted() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    // Read-only query: does not prompt, does not modify any setting.
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
pub fn accessibility_granted() -> bool {
    true
}

fn display_point(r: DisplayRect, x: u16, y: u16) -> (i32, i32) {
    (
        r.x.saturating_add(denormalized(x, r.width.saturating_sub(1) as f64).round() as i32),
        r.y.saturating_add(denormalized(y, r.height.saturating_sub(1) as f64).round() as i32),
    )
}
impl Injector for DesktopInjector {
    fn apply(&mut self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::MouseMove { x, y, display } => {
                let (px, py) = self.point(*display, *x, *y);
                self.enigo.move_mouse(px, py, Coordinate::Abs)?;
            }
            InputEvent::MouseButton {
                button,
                down,
                x,
                y,
                display,
            } => {
                let (px, py) = self.point(*display, *x, *y);
                self.enigo.move_mouse(px, py, Coordinate::Abs)?;
                let native = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Right => Button::Right,
                    MouseButton::Back => Button::Back,
                    MouseButton::Forward => Button::Forward,
                };
                let dir = if *down {
                    Direction::Press
                } else {
                    Direction::Release
                };
                self.enigo.button(native, dir)?;
                if *down {
                    self.pressed_buttons.insert(*button);
                } else {
                    self.pressed_buttons.remove(button);
                }
            }
            InputEvent::Wheel {
                dx,
                dy,
                x,
                y,
                display,
            } => {
                let (px, py) = self.point(*display, *x, *y);
                self.enigo.move_mouse(px, py, Coordinate::Abs)?;
                if *dy != 0 {
                    self.enigo.scroll(*dy, Axis::Vertical)?;
                }
                if *dx != 0 {
                    self.enigo.scroll(*dx, Axis::Horizontal)?;
                }
            }
            InputEvent::Key { code, down, .. } => {
                if let Some(key) = map_key(code) {
                    let dir = if *down {
                        Direction::Press
                    } else {
                        Direction::Release
                    };
                    self.enigo.key(key, dir)?;
                }
            }
            InputEvent::Text { text } => {
                self.enigo.text(text)?;
            }
            InputEvent::Touch {
                phase,
                points,
                display,
            } => {
                // Desktop has no touch; map the first finger to the mouse so a
                // phone viewer can still drive a desktop.
                if let Some(p) = points.first() {
                    let (px, py) = self.point(*display, p.x, p.y);
                    self.enigo.move_mouse(px, py, Coordinate::Abs)?;
                    match phase {
                        TouchPhase::Start => {
                            self.enigo.button(Button::Left, Direction::Press)?;
                        }
                        TouchPhase::End | TouchPhase::Cancel => {
                            self.enigo.button(Button::Left, Direction::Release)?;
                        }
                        TouchPhase::Move => {}
                    }
                }
            }
            InputEvent::ReleaseAll => {
                self.release_all()?;
            }
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<()> {
        for button in self.pressed_buttons.drain() {
            let native = match button {
                MouseButton::Left => Button::Left,
                MouseButton::Middle => Button::Middle,
                MouseButton::Right => Button::Right,
                MouseButton::Back => Button::Back,
                MouseButton::Forward => Button::Forward,
            };
            self.enigo.button(native, Direction::Release)?;
        }
        // Release only keys held by this injector, not local user modifiers.
        for key in self.enigo.held().0 {
            let _ = self.enigo.key(key, Direction::Release);
        }
        Ok(())
    }
}

/// Maps a W3C `KeyboardEvent.code` to a native key.
///
/// Letters and punctuation use [`Key::Unicode`] because enigo's letter variants
/// are Windows-only, and because it makes the layout follow the *agent's* layout
/// rather than the viewer's — which is what a remote desktop user expects.
pub fn map_key(code: &str) -> Option<Key> {
    let key = match code {
        "Enter" | "NumpadEnter" => Key::Return,
        "Escape" => Key::Escape,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "Space" => Key::Space,
        "ShiftLeft" | "ShiftRight" => Key::Shift,
        "ControlLeft" | "ControlRight" => Key::Control,
        "AltLeft" | "AltRight" => Key::Alt,
        "MetaLeft" | "MetaRight" | "OSLeft" | "OSRight" => Key::Meta,
        "CapsLock" => Key::CapsLock,
        #[cfg(target_os = "windows")]
        "ContextMenu" => Key::Apps,
        // Keys that only exist on some platforms are mapped where enigo exposes
        // them; elsewhere they are ignored rather than breaking compilation.
        #[cfg(target_os = "windows")]
        "Insert" => Key::Insert,
        #[cfg(target_os = "windows")]
        "NumLock" => Key::Numlock,
        #[cfg(target_os = "windows")]
        "PrintScreen" => Key::PrintScr,
        #[cfg(target_os = "windows")]
        "Pause" => Key::Pause,
        "NumpadAdd" => Key::Add,
        "NumpadSubtract" => Key::Subtract,
        "NumpadMultiply" => Key::Multiply,
        "NumpadDivide" => Key::Divide,
        "NumpadDecimal" => Key::Decimal,
        "Numpad0" => Key::Numpad0,
        "Numpad1" => Key::Numpad1,
        "Numpad2" => Key::Numpad2,
        "Numpad3" => Key::Numpad3,
        "Numpad4" => Key::Numpad4,
        "Numpad5" => Key::Numpad5,
        "Numpad6" => Key::Numpad6,
        "Numpad7" => Key::Numpad7,
        "Numpad8" => Key::Numpad8,
        "Numpad9" => Key::Numpad9,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        "Minus" => Key::Unicode('-'),
        "Equal" => Key::Unicode('='),
        "BracketLeft" => Key::Unicode('['),
        "BracketRight" => Key::Unicode(']'),
        "Backslash" => Key::Unicode('\\'),
        "Semicolon" => Key::Unicode(';'),
        "Quote" => Key::Unicode('\''),
        "Comma" => Key::Unicode(','),
        "Period" => Key::Unicode('.'),
        "Slash" => Key::Unicode('/'),
        "Backquote" => Key::Unicode('`'),
        other => {
            if let Some(rest) = other.strip_prefix("Key") {
                let mut chars = rest.chars();
                let c = chars.next()?.to_ascii_lowercase();
                if chars.next().is_none() && c.is_ascii_alphabetic() {
                    return Some(Key::Unicode(c));
                }
                return None;
            }
            if let Some(rest) = other.strip_prefix("Digit") {
                return rest
                    .chars()
                    .next()
                    .filter(|c| c.is_ascii_digit())
                    .map(Key::Unicode);
            }
            return None;
        }
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalised_points_cover_the_whole_display() {
        use super::{display_point, DisplayRect};
        let r = DisplayRect {
            id: 7,
            x: 100,
            y: 50,
            width: 1920,
            height: 1080,
            scale: 1.0,
        };
        assert_eq!(display_point(r, 0, 0), (100, 50));
        assert_eq!(display_point(r, 65535, 65535), (100 + 1919, 50 + 1079));
        let (x, y) = display_point(r, 32768, 32768);
        assert!((x - (100 + 960)).abs() <= 1, "x = {x}");
        assert!((y - (50 + 540)).abs() <= 1, "y = {y}");
    }

    use super::*;

    #[test]
    fn maps_letters_and_digits_to_unicode() {
        assert_eq!(map_key("KeyA"), Some(Key::Unicode('a')));
        assert_eq!(map_key("Digit7"), Some(Key::Unicode('7')));
        assert_eq!(map_key("Enter"), Some(Key::Return));
        assert_eq!(map_key("F11"), Some(Key::F11));
    }

    #[test]
    fn unknown_codes_are_ignored_instead_of_panicking() {
        assert_eq!(map_key("HyperKey"), None);
        assert_eq!(map_key("KeyAB"), None);
        assert_eq!(map_key("DigitX"), None);
    }

    #[test]
    fn coordinates_follow_the_display_origin() {
        let d = DisplayRect {
            id: 2,
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 2.0,
        };
        assert_eq!(display_point(d, 65535, 0), (-1, 0));
        assert_eq!(display_point(d, 0, 65535), (-1920, 1079));
    }
}
