//! Input helpers: explicit XKB keymap, killswitch detect, shortcuts, LED forwarding.

use std::process::{Command, Stdio};

use smithay::{
    input::keyboard::{keysyms, Keysym, LedState, ModifiersState, XkbConfig},
    reexports::input::{Device, Led},
};

/// Explicit XKB keymap config — no environment dependence.
pub fn xkb_config() -> XkbConfig<'static> {
    XkbConfig {
        rules: "evdev",
        model: "pc105",
        layout: "us",
        variant: "",
        options: None,
    }
}

/// True when `mods` + `sym` is the failsafe combo Ctrl+Alt+Backspace.
pub fn is_killswitch(mods: &ModifiersState, sym: Keysym) -> bool {
    mods.ctrl && mods.alt && sym == Keysym::from(keysyms::KEY_BackSpace)
}

/// True when `mods` + `sym` is Super+Enter (launch terminal).
pub fn is_terminal_shortcut(mods: &ModifiersState, sym: Keysym) -> bool {
    mods.logo && (sym == Keysym::from(keysyms::KEY_Return) || sym == Keysym::from(keysyms::KEY_KP_Enter))
}

/// True when `mods` + `sym` is Super+Q or Super+W (close focused window).
pub fn is_close_shortcut(mods: &ModifiersState, sym: Keysym) -> bool {
    mods.logo && (
        sym == Keysym::from(keysyms::KEY_q)
        || sym == Keysym::from(keysyms::KEY_Q)
        || sym == Keysym::from(keysyms::KEY_w)
        || sym == Keysym::from(keysyms::KEY_W)
    )
}

/// Launch default terminal application with detached Stdio.
pub fn spawn_terminal() {
    eprintln!("[EXEC] Super+Enter pressed! Attempting to launch terminal emulator...");
    let result = Command::new("alacritty")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .or_else(|_| {
            Command::new("foot")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        })
        .or_else(|_| {
            Command::new("weston-terminal")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        })
        .or_else(|_| {
            Command::new("kitty")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        })
        .or_else(|_| {
            Command::new("st")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        })
        .or_else(|_| {
            Command::new("xterm")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        });

    match result {
        Ok(child) => eprintln!("[EXEC] Terminal launched successfully (PID {})", child.id()),
        Err(err) => eprintln!("[EXEC ERROR] Could not launch terminal emulator: {:?}", err),
    }
}

/// Convert smithay LED state to a libinput LED mask.
pub fn leds_for_state(state: LedState) -> Led {
    Led::from(state)
}

/// Push `leds` to `device` (caps-lock LED etc.).
pub fn forward_leds(device: &mut Device, leds: LedState) {
    device.led_update(leds_for_state(leds));
}
