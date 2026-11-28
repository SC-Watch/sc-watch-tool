//! sc-watch's bindings - keys, joystick buttons and combinations - and when
//! they fire (inputs.py). The state is read through [`InputState`], so the
//! logic is the same whatever reads it: on Windows [`windows::WindowsInput`]
//! polls GetAsyncKeyState and joyGetPosEx, which install nothing, intercept
//! nothing and swallow nothing (the press still reaches the game); on Linux
//! [`linux::LinuxInput`] asks evdev for the same state, the same way.

pub mod access;
pub mod binding;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

use std::time::{Duration, Instant};

pub use access::KeyAccess;
pub use binding::{BindError, Device, Token, parse};

/// Reads the current state of keys and joysticks.
pub trait InputState {
    /// Is this virtual key held right now?
    fn key_down(&self, vk: u32) -> bool;
    /// Every joystick the driver can see, in index order.
    fn devices(&self) -> Vec<Device>;
    /// Bitmask of held buttons (bit 0 = button 1); 0 when the device is gone.
    fn buttons_down(&self, index: u32) -> u32;
    /// What this system keeps from being read, in words for the user (on
    /// Linux: keyboards closed to this user). None by default.
    fn problems(&self) -> Vec<String> {
        Vec::new()
    }
    /// Where keyboard keys stand, on a system where they need setting up
    /// (Linux: the `input` group). None: there is nothing to set up.
    fn key_access(&self) -> Option<KeyAccess> {
        None
    }
    /// Sets keyboard keys up (Linux: through the desktop's password
    /// dialog; it takes effect at the next login).
    fn grant_key_access(&self) -> Result<(), String> {
        Err("there is nothing to set up for keyboard keys here".into())
    }
}

/// How often the joystick list is read again (to notice a stick plugged back in).
const DEVICES_EVERY: Duration = Duration::from_secs(1);

/// A binding, polled. Fires once per press: on the change from not all held
/// to all held.
pub struct Binding {
    pub spec: String,
    pub tokens: Vec<Token>,
    was_down: bool,
    devs: Vec<Device>,
    devs_checked: Instant,
}

impl Binding {
    pub fn new(spec: &str, state: &dyn InputState) -> Result<Self, BindError> {
        Ok(Self {
            spec: spec.to_string(),
            tokens: parse(spec)?,
            was_down: false,
            devs: state.devices(),
            devs_checked: Instant::now(),
        })
    }

    /// No inputs: the binding is off.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Joystick parts not plugged in now.
    pub fn missing(&self) -> Vec<String> {
        self.tokens
            .iter()
            .filter(|t| matches!(t, Token::Joy { .. }) && t.resolve(&self.devs).is_none())
            .map(Token::label)
            .collect()
    }

    fn refresh_devices(&mut self, state: &dyn InputState) {
        if self.devs_checked.elapsed() > DEVICES_EVERY {
            self.devs = state.devices();
            self.devs_checked = Instant::now();
        }
    }

    /// Each token held or not, joysticks read once a device.
    fn states(&mut self, state: &dyn InputState, stop_at_first_up: bool) -> Vec<bool> {
        self.refresh_devices(state);
        let mut masks: Vec<(u32, u32)> = Vec::new();
        let mut out = Vec::with_capacity(self.tokens.len());
        for t in &self.tokens {
            let on = match t {
                Token::Key(vk) => state.key_down(*vk),
                Token::Joy { button, .. } => match t.resolve(&self.devs) {
                    None => false,
                    Some(idx) => {
                        let mask = match masks.iter().find(|m| m.0 == idx) {
                            Some(m) => m.1,
                            None => {
                                let m = state.buttons_down(idx);
                                masks.push((idx, m));
                                m
                            }
                        };
                        mask & (1 << (button - 1)) != 0
                    }
                },
            };
            out.push(on);
            if stop_at_first_up && !on {
                break;
            }
        }
        out
    }

    /// Are all parts held right now?
    pub fn held(&mut self, state: &dyn InputState) -> bool {
        !self.tokens.is_empty() && {
            let s = self.states(state, true);
            s.len() == self.tokens.len() && s.iter().all(|&v| v)
        }
    }

    /// How much is held: (held, total, labels of the parts not held) - so a
    /// combination three quarters held looks different from one untouched.
    pub fn partial(&mut self, state: &dyn InputState) -> (usize, usize, Vec<String>) {
        let s = self.states(state, false);
        let missing = self
            .tokens
            .iter()
            .zip(&s)
            .filter(|(_, on)| !**on)
            .map(|(t, _)| t.label())
            .collect();
        (s.iter().filter(|&&v| v).count(), self.tokens.len(), missing)
    }

    /// True once, on the change into all held.
    pub fn pressed(&mut self, state: &dyn InputState) -> bool {
        let now = self.held(state);
        let fired = now && !self.was_down;
        self.was_down = now;
        fired
    }

    /// Forgets the current state, so a press held through a long read does
    /// not fire again the moment polling resumes.
    pub fn drain(&mut self, state: &dyn InputState) {
        self.was_down = self.held(state);
    }

    pub fn label(&self) -> String {
        self.tokens
            .iter()
            .map(Token::label)
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        keys: RefCell<Vec<u32>>,
        buttons: RefCell<u32>,
    }

    impl InputState for Fake {
        fn key_down(&self, vk: u32) -> bool {
            self.keys.borrow().contains(&vk)
        }
        fn devices(&self) -> Vec<Device> {
            vec![Device {
                index: 0,
                vid: 0x231d,
                pid: 0x0126,
                name: "HID-compliant game controller".into(),
                buttons: 32,
                axes: 6,
                truncated: false,
            }]
        }
        fn buttons_down(&self, _: u32) -> u32 {
            *self.buttons.borrow()
        }
    }

    #[test]
    fn fires_once_per_press_of_the_whole_combination() {
        let fake = Fake::default();
        let mut b = Binding::new("rctrl+vid231d:0126.b7", &fake).unwrap();
        assert_eq!(b.label(), "rctrl + vid231d:0126.b7");
        assert!(b.missing().is_empty());
        assert!(!b.pressed(&fake));
        fake.keys.borrow_mut().push(0xA3);
        assert!(!b.pressed(&fake), "half a combination does not fire");
        assert_eq!(b.partial(&fake), (1, 2, vec!["vid231d:0126.b7".into()]));
        *fake.buttons.borrow_mut() = 1 << 6;
        assert!(b.pressed(&fake), "all held: fires");
        assert!(!b.pressed(&fake), "still held: does not fire again");
        *fake.buttons.borrow_mut() = 0;
        assert!(!b.pressed(&fake));
        *fake.buttons.borrow_mut() = 1 << 6;
        assert!(b.pressed(&fake), "pressed again: fires again");
        b.drain(&fake);
        assert!(!b.pressed(&fake), "drained while held: quiet");
    }

    #[test]
    fn a_missing_stick_is_reported_not_fatal() {
        let fake = Fake::default();
        let mut b = Binding::new("joy3.b1", &fake).unwrap();
        assert!(
            b.missing().is_empty(),
            "joy3 by index is not checked against the list"
        );
        let mut c = Binding::new("vid1234:5678.b2", &fake).unwrap();
        assert_eq!(c.missing(), vec!["vid1234:5678.b2".to_string()]);
        assert!(!c.held(&fake));
        assert!(!b.held(&fake), "nothing pressed on joy3");
    }
}
