//! What is plugged in, as `python inputs.py --list` prints it.
//!
//!     cargo run -p scw-input --example inputs
#[cfg(windows)]
fn main() {
    use scw_input::InputState;
    use scw_input::windows::WindowsInput;
    let devs = WindowsInput.devices();
    println!("{} joystick(s):", devs.len());
    for d in &devs {
        println!("  {}", d.describe());
    }
    if devs.is_empty() {
        println!("  none detected");
    }
    let mut names: Vec<String> = scw_input::binding::vk_codes()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    names.sort();
    println!("\nkeyboard names: {}, ...", names[..14].join(", "));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("reading keys and joysticks is Windows-only so far");
}
