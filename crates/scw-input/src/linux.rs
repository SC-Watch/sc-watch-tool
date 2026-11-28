//! Keys and joysticks on Linux, read through evdev (/dev/input/event*).
//!
//! The same polling as on Windows: the current state of each key and button
//! is asked for (EVIOCGKEY), nothing is grabbed, nothing is swallowed, so
//! the press still reaches the game. Joysticks are numbered in device order
//! and named by vendor and product as on Windows (`vid231d:0126`); their
//! buttons are counted in the kernel's order - the joystick buttons, then
//! the "trigger happy" range, then the misc buttons - which follows the
//! stick's own HID button order, as Windows numbers them.
//!
//! Joysticks are readable by the logged-in user on most systems. Keyboards
//! are not, on purpose (reading one is how a keylogger works): reading keys
//! needs the `input` group (the page sets it up: [`crate::access`]), or bind
//! a joystick button instead.
//!
//! Every device is opened read-only - evdev's own enumerate opens read-write
//! where it may - so sc-watch could not send anything to a keyboard or stick
//! even by mistake: it only asks.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use evdev::KeyCode;

use crate::InputState;
use crate::binding::Device;

/// How long a snapshot of the keys held is reused (the binding form asks
/// about every key at once).
const SNAPSHOT: Duration = Duration::from_millis(5);
/// How often the devices are looked for again.
const RESCAN: Duration = Duration::from_secs(1);

const BTN_MISC: u16 = 0x100;
const BTN_JOYSTICK: u16 = 0x120;
const BTN_GAMEPAD_END: u16 = 0x13f;
const BTN_TRIGGER_HAPPY: u16 = 0x2c0;
const KEY_MAX: u16 = 0x2ff;

/// The Linux key codes for a Windows virtual key (the bindings' own codes).
/// SHIFT, CTRL and ALT are either side.
pub fn keycodes(vk: u32) -> &'static [u16] {
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    const DIGITS: [u16; 10] = [11, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    const NUMPAD: [u16; 10] = [82, 79, 80, 81, 75, 76, 77, 71, 72, 73];
    const F: [u16; 24] = [
        59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88, 183, 184, 185, 186, 187, 188, 189, 190,
        191, 192, 193, 194,
    ];
    macro_rules! one {
        ($t:expr, $i:expr) => {
            std::slice::from_ref(&$t[$i])
        };
    }
    match vk {
        0x41..=0x5A => one!(LETTERS, (vk - 0x41) as usize),
        0x30..=0x39 => one!(DIGITS, (vk - 0x30) as usize),
        0x60..=0x69 => one!(NUMPAD, (vk - 0x60) as usize),
        0x70..=0x87 => one!(F, (vk - 0x70) as usize),
        0x09 => &[15],
        0x20 => &[57],
        0x0D => &[28, 96],
        0x1B => &[1],
        0x10 => &[42, 54],
        0x11 => &[29, 97],
        0x12 => &[56, 100],
        0xA0 => &[42],
        0xA1 => &[54],
        0xA2 => &[29],
        0xA3 => &[97],
        0xA4 => &[56],
        0xA5 => &[100],
        0x14 => &[58],
        0x08 => &[14],
        0x2D => &[110],
        0x2E => &[111],
        0x24 => &[102],
        0x23 => &[107],
        0x21 => &[104],
        0x22 => &[109],
        0x26 => &[103],
        0x28 => &[108],
        0x25 => &[105],
        0x27 => &[106],
        0x90 => &[69],
        0x91 => &[70],
        0x13 => &[119],
        0xBD => &[12],
        0xBB => &[13],
        0xDB => &[26],
        0xDD => &[27],
        0xBA => &[39],
        0xDE => &[40],
        0xBC => &[51],
        0xBE => &[52],
        0xBF => &[53],
        0xDC => &[43],
        0xC0 => &[41],
        _ => &[],
    }
}

/// A joystick's buttons in the order they are numbered: the joystick and
/// gamepad buttons and the "trigger happy" range upward, then the misc
/// buttons (as SDL counts them).
pub fn button_order(codes: &[u16]) -> Vec<u16> {
    let mut high: Vec<u16> = codes
        .iter()
        .copied()
        .filter(|c| (BTN_JOYSTICK..=KEY_MAX).contains(c))
        .collect();
    let mut low: Vec<u16> = codes
        .iter()
        .copied()
        .filter(|c| (BTN_MISC..BTN_JOYSTICK).contains(c))
        .collect();
    high.sort_unstable();
    low.sort_unstable();
    high.extend(low);
    high
}

fn is_keyboard(keys: &HashSet<u16>) -> bool {
    [30u16, 44, 28].iter().all(|k| keys.contains(k))
}

fn is_joystick(keys: &HashSet<u16>, axes: usize) -> bool {
    axes > 0
        && keys
            .iter()
            .any(|&k| (BTN_JOYSTICK..=BTN_GAMEPAD_END).contains(&k) || k >= BTN_TRIGGER_HAPPY)
}

struct Joy {
    dev: evdev::Device,
    info: Device,
    buttons: Vec<u16>,
}

#[derive(Default)]
struct Inner {
    keyboards: Vec<evdev::Device>,
    joysticks: Vec<Joy>,
    scanned: Option<Instant>,
    /// Event nodes that could not be opened (permissions).
    unreadable: usize,
    held: Option<(Instant, HashSet<u16>)>,
}

fn event_number(p: &std::path::Path) -> u32 {
    p.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("event"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX)
}

/// The event nodes, each opened read-only, and how many there are.
fn open_read_only() -> (Vec<(PathBuf, evdev::Device)>, usize) {
    let mut found = Vec::new();
    let mut nodes = 0;
    for e in std::fs::read_dir("/dev/input")
        .into_iter()
        .flatten()
        .flatten()
    {
        if !e.file_name().to_string_lossy().starts_with("event") {
            continue;
        }
        nodes += 1;
        let path = e.path();
        if let Ok(f) = std::fs::File::open(&path)
            && let Ok(dev) = evdev::Device::from_fd(f.into())
        {
            found.push((path, dev));
        }
    }
    (found, nodes)
}

/// Was this descriptor opened for writing too? (/proc/self/fdinfo's flags,
/// the access mode in their low two bits.) Unknown counts as yes.
fn writable(fd: std::os::fd::RawFd) -> bool {
    std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("flags:"))
                .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
        })
        .is_none_or(|flags| flags & 0o3 != 0)
}

impl Inner {
    fn scan(&mut self) {
        let (mut found, nodes) = open_read_only();
        found.sort_by_key(|(p, _)| event_number(p));
        self.unreadable = nodes.saturating_sub(found.len());
        self.keyboards.clear();
        self.joysticks.clear();
        for (_, dev) in found {
            let keys: HashSet<u16> = dev
                .supported_keys()
                .map(|k| k.iter().map(|c| c.code()).collect())
                .unwrap_or_default();
            let axes = dev
                .supported_absolute_axes()
                .map_or(0, |a| a.iter().count());
            if is_joystick(&keys, axes) {
                let codes: Vec<u16> = keys.iter().copied().collect();
                let buttons = button_order(&codes);
                let id = dev.input_id();
                let index = self.joysticks.len() as u32;
                let info = Device {
                    index,
                    vid: id.vendor(),
                    pid: id.product(),
                    name: dev.name().unwrap_or("joystick").to_string(),
                    buttons: buttons.len().min(32) as u32,
                    axes: axes as u32,
                    truncated: buttons.len() > 32,
                };
                self.joysticks.push(Joy { dev, info, buttons });
            } else if is_keyboard(&keys) {
                self.keyboards.push(dev);
            }
        }
        self.scanned = Some(Instant::now());
        self.held = None;
    }

    fn scan_if_due(&mut self) {
        if self.scanned.is_none_or(|t| t.elapsed() >= RESCAN) {
            self.scan();
        }
    }

    fn held_keys(&mut self) -> &HashSet<u16> {
        if self.scanned.is_none() {
            self.scan();
        }
        let fresh = self
            .held
            .as_ref()
            .is_some_and(|(t, _)| t.elapsed() < SNAPSHOT);
        if !fresh {
            let mut held = HashSet::new();
            for k in &self.keyboards {
                if let Ok(state) = k.get_key_state() {
                    held.extend(state.iter().map(|c: KeyCode| c.code()));
                }
            }
            self.held = Some((Instant::now(), held));
        }
        &self.held.as_ref().unwrap().1
    }
}

/// The keys and joysticks of this machine.
#[derive(Default)]
pub struct LinuxInput {
    inner: Mutex<Inner>,
}

impl LinuxInput {
    pub fn new() -> Self {
        Self::default()
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What stands in the way of reading keys, said plainly; empty when
    /// nothing does.
    pub fn problems(&self) -> Vec<String> {
        let mut g = self.inner();
        g.scan_if_due();
        let mut out = Vec::new();
        if g.keyboards.is_empty() {
            out.push(if g.unreadable > 0 {
                format!(
                    "no keyboard can be read: {} input device(s) are closed to this user. Keys \
                     need the 'input' group: on the page, Settings > Maintenance > 'Let \
                     sc-watch read keyboard keys' sets it up - or bind a joystick button instead",
                    g.unreadable
                )
            } else {
                "no keyboard found".to_string()
            });
        }
        out
    }
}

impl LinuxInput {
    /// How many of the devices held open could be written to: none, as
    /// each is opened read-only (for the test that says so).
    pub fn writable_handles(&self) -> usize {
        use std::os::fd::AsRawFd;
        let mut g = self.inner();
        g.scan_if_due();
        g.keyboards
            .iter()
            .map(AsRawFd::as_raw_fd)
            .chain(g.joysticks.iter().map(|j| j.dev.as_raw_fd()))
            .filter(|fd| writable(*fd))
            .count()
    }
}

impl InputState for LinuxInput {
    fn problems(&self) -> Vec<String> {
        LinuxInput::problems(self)
    }

    fn key_access(&self) -> Option<crate::KeyAccess> {
        let readable = {
            let mut g = self.inner();
            g.scan_if_due();
            !g.keyboards.is_empty() || g.unreadable == 0
        };
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
        Some(crate::access::key_access(
            readable,
            crate::access::current_user().as_deref(),
            &group,
            &crate::access::gids_in_status(&status),
        ))
    }

    fn grant_key_access(&self) -> Result<(), String> {
        crate::access::grant()
    }

    fn key_down(&self, vk: u32) -> bool {
        let codes = keycodes(vk);
        if codes.is_empty() {
            return false;
        }
        let mut g = self.inner();
        let held = g.held_keys();
        codes.iter().any(|c| held.contains(c))
    }

    fn devices(&self) -> Vec<Device> {
        let mut g = self.inner();
        g.scan_if_due();
        g.joysticks.iter().map(|j| j.info.clone()).collect()
    }

    fn buttons_down(&self, index: u32) -> u32 {
        let mut g = self.inner();
        if g.scanned.is_none() {
            g.scan();
        }
        let Some(j) = g.joysticks.get(index as usize) else {
            return 0;
        };
        let Ok(state) = j.dev.get_key_state() else {
            return 0;
        };
        let held: HashSet<u16> = state.iter().map(|c: KeyCode| c.code()).collect();
        j.buttons
            .iter()
            .take(32)
            .enumerate()
            .filter(|(_, c)| held.contains(c))
            .fold(0, |m, (i, _)| m | (1 << i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_keys_map_to_linux_codes() {
        assert_eq!(keycodes(u32::from(b'A')), &[30]);
        assert_eq!(keycodes(u32::from(b'Q')), &[16]);
        assert_eq!(keycodes(u32::from(b'0')), &[11]);
        assert_eq!(keycodes(u32::from(b'1')), &[2]);
        assert_eq!(keycodes(0x70), &[59], "F1");
        assert_eq!(keycodes(0x7B), &[88], "F12");
        assert_eq!(keycodes(0x87), &[194], "F24");
        assert_eq!(keycodes(0x11), &[29, 97], "either ctrl");
        assert_eq!(keycodes(0xA3), &[97], "right ctrl");
        assert_eq!(keycodes(0x60), &[82], "numpad 0");
        // every key a binding can name has a code
        for (name, vk) in crate::binding::vk_codes() {
            assert!(!keycodes(vk).is_empty(), "{name}");
        }
    }

    #[test]
    fn buttons_in_the_sticks_order() {
        // BTN_TRIGGER.., then trigger-happy, then misc
        let codes = [0x2c1, 0x101, 0x121, 0x120, 0x2c0, 0x100];
        assert_eq!(
            button_order(&codes),
            vec![0x120, 0x121, 0x2c0, 0x2c1, 0x100, 0x101]
        );
    }
}
