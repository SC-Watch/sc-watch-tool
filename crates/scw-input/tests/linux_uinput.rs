//! The Linux reader against the kernel itself: a virtual keyboard and a
//! virtual joystick (uinput) are pressed, and LinuxInput reads the keys,
//! names the stick by vendor and product, and numbers its buttons.
//!
//! Needs /dev/uinput writable and the new event nodes readable (CI sets
//! both up); skipped elsewhere.
#![cfg(target_os = "linux")]

use std::thread::sleep;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    UinputAbsSetup,
};
use scw_input::InputState;
use scw_input::linux::LinuxInput;

fn press(dev: &mut VirtualDevice, code: KeyCode, down: bool) {
    dev.emit(&[InputEvent::new(
        EventType::KEY.0,
        code.code(),
        i32::from(down),
    )])
    .unwrap();
    sleep(Duration::from_millis(50));
}

#[test]
fn reads_a_virtual_keyboard_and_joystick() {
    if std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_err()
    {
        println!("::warning title=linux input::skipped - /dev/uinput is not writable");
        return;
    }
    let keys: AttributeSet<KeyCode> = [
        KeyCode::KEY_A,
        KeyCode::KEY_Z,
        KeyCode::KEY_ENTER,
        KeyCode::KEY_0,
        KeyCode::KEY_RIGHTCTRL,
        KeyCode::KEY_F9,
    ]
    .into_iter()
    .collect();
    let mut kb = VirtualDevice::builder()
        .unwrap()
        .name("scw test keyboard")
        .with_keys(&keys)
        .unwrap()
        .build()
        .unwrap();

    let buttons: AttributeSet<KeyCode> = [
        KeyCode::BTN_TRIGGER,
        KeyCode::BTN_THUMB,
        KeyCode::BTN_THUMB2,
        KeyCode::BTN_TOP,
        KeyCode::BTN_TRIGGER_HAPPY1,
    ]
    .into_iter()
    .collect();
    let axis = |a| UinputAbsSetup::new(a, AbsInfo::new(128, 0, 255, 0, 0, 1));
    let mut stick = VirtualDevice::builder()
        .unwrap()
        .name("scw test stick")
        .input_id(InputId::new(BusType::BUS_USB, 0x231d, 0x0126, 1))
        .with_keys(&buttons)
        .unwrap()
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X))
        .unwrap()
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y))
        .unwrap()
        .build()
        .unwrap();
    sleep(Duration::from_millis(800)); // the event nodes appear

    let input = LinuxInput::new();
    let stick_dev = input
        .devices()
        .into_iter()
        .find(|d| d.vid == 0x231d && d.pid == 0x0126)
        .expect("the virtual stick, by vendor and product");
    assert_eq!(stick_dev.ident(), "vid231d:0126");
    assert_eq!(stick_dev.buttons, 5);
    assert_eq!(stick_dev.axes, 2);
    assert_eq!(
        input.writable_handles(),
        0,
        "every device is held read-only"
    );
    let keyboard = input.problems().is_empty();
    if !keyboard {
        println!(
            "::warning title=linux input::keyboard part skipped - {:?}",
            input.problems()
        );
    } else {
        assert!(!input.key_down(u32::from(b'0')));
        press(&mut kb, KeyCode::KEY_0, true);
        press(&mut kb, KeyCode::KEY_RIGHTCTRL, true);
        assert!(input.key_down(u32::from(b'0')), "0 held");
        assert!(input.key_down(0xA3), "right ctrl held");
        assert!(input.key_down(0x11), "either ctrl held");
        assert!(!input.key_down(0x78), "F9 not held");
        press(&mut kb, KeyCode::KEY_0, false);
        press(&mut kb, KeyCode::KEY_RIGHTCTRL, false);
        assert!(!input.key_down(u32::from(b'0')));
    }

    let i = stick_dev.index;
    assert_eq!(input.buttons_down(i), 0);
    press(&mut stick, KeyCode::BTN_THUMB2, true); // button 3
    press(&mut stick, KeyCode::BTN_TRIGGER_HAPPY1, true); // button 5
    assert_eq!(input.buttons_down(i), (1 << 2) | (1 << 4));
    press(&mut stick, KeyCode::BTN_THUMB2, false);
    press(&mut stick, KeyCode::BTN_TRIGGER_HAPPY1, false);
    assert_eq!(input.buttons_down(i), 0);
    println!(
        "::notice title=linux input::ran - virtual stick read (vid231d:0126, 5 buttons){}",
        if keyboard {
            ", virtual keyboard read"
        } else {
            ""
        }
    );
}
