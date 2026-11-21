//! Reading keys and joysticks on Windows, as inputs.py: GetAsyncKeyState
//! reads a flag Windows already keeps, joyGetPosEx asks the driver for a
//! device's state. Neither hooks input or registers a hotkey, so the press
//! still reaches the game.

use windows::Win32::Media::Multimedia::{JOYCAPSW, JOYINFOEX, joyGetDevCapsW, joyGetPosEx};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::InputState;
use crate::binding::Device;

/// JOYERR_NOERROR.
const NO_ERROR: u32 = 0;
/// JOY_RETURNBUTTONS and JOY_RETURNALL, inputs.py's values.
const JOY_RETURNBUTTONS: u32 = 0x80;
const JOY_RETURNALL: u32 = 0xFF;

/// The Windows readers.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsInput;

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

fn pos(index: u32, flags: u32) -> Option<JOYINFOEX> {
    let mut info = JOYINFOEX {
        dwSize: size_of::<JOYINFOEX>() as u32,
        dwFlags: flags,
        ..Default::default()
    };
    (unsafe { joyGetPosEx(index, &mut info) } == NO_ERROR).then_some(info)
}

impl InputState for WindowsInput {
    fn key_down(&self, vk: u32) -> bool {
        (unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000 != 0
    }

    fn devices(&self) -> Vec<Device> {
        let mut out = Vec::new();
        for i in 0..16u32 {
            let mut caps = JOYCAPSW::default();
            if unsafe { joyGetDevCapsW(i as usize, &mut caps, size_of::<JOYCAPSW>() as u32) }
                != NO_ERROR
            {
                continue;
            }
            // A device in the table is not necessarily attached: poll it.
            if pos(i, JOY_RETURNALL).is_none() {
                continue;
            }
            out.push(Device {
                index: i,
                vid: caps.wMid,
                pid: caps.wPid,
                name: wide(&{ caps.szPname }),
                buttons: caps.wNumButtons,
                axes: caps.wNumAxes,
                truncated: caps.wMaxButtons > 32,
            });
        }
        out
    }

    fn buttons_down(&self, index: u32) -> u32 {
        pos(index, JOY_RETURNBUTTONS).map_or(0, |i| i.dwButtons)
    }
}
