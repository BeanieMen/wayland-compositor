use std::process::{Command, Stdio};

use smithay::{
    input::keyboard::{Keysym, LedState, ModifiersState, XkbConfig, keysyms},
    reexports::input::{Device, Led},
};

pub fn xkb_config() -> XkbConfig<'static> {
    XkbConfig {
        rules: "evdev",
        model: "pc105",
        layout: "us",
        variant: "",
        options: None,
    }
}

pub fn is_killswitch(mods: &ModifiersState, sym: Keysym) -> bool {
    mods.ctrl && mods.alt && sym == Keysym::from(keysyms::KEY_BackSpace)
}

pub fn is_terminal_shortcut(mods: &ModifiersState, sym: Keysym) -> bool {
    mods.logo
        && (sym == Keysym::from(keysyms::KEY_Return) || sym == Keysym::from(keysyms::KEY_KP_Enter))
}

pub fn is_close_shortcut(mods: &ModifiersState, sym: Keysym) -> bool {
    mods.logo
        && (sym == Keysym::from(keysyms::KEY_q)
            || sym == Keysym::from(keysyms::KEY_Q)
            || sym == Keysym::from(keysyms::KEY_w)
            || sym == Keysym::from(keysyms::KEY_W))
}

/// Move the focused tile by swapping it with the tile in this direction.
pub fn tile_move_direction(mods: &ModifiersState, sym: Keysym) -> Option<(i32, i32)> {
    if !(mods.logo && mods.shift) {
        return None;
    }

    match sym {
        sym if sym == Keysym::from(keysyms::KEY_Left) => Some((-1, 0)),
        sym if sym == Keysym::from(keysyms::KEY_Right) => Some((1, 0)),
        sym if sym == Keysym::from(keysyms::KEY_Up) => Some((0, -1)),
        sym if sym == Keysym::from(keysyms::KEY_Down) => Some((0, 1)),
        _ => None,
    }
}

/// Ctrl+Alt+F1 through Ctrl+Alt+F12 request a VT change via libseat.
pub fn tty_switch_destination(mods: &ModifiersState, sym: Keysym) -> Option<i32> {
    if !(mods.ctrl && mods.alt) {
        return None;
    }

    [
        keysyms::KEY_F1,
        keysyms::KEY_F2,
        keysyms::KEY_F3,
        keysyms::KEY_F4,
        keysyms::KEY_F5,
        keysyms::KEY_F6,
        keysyms::KEY_F7,
        keysyms::KEY_F8,
        keysyms::KEY_F9,
        keysyms::KEY_F10,
        keysyms::KEY_F11,
        keysyms::KEY_F12,
    ]
    .iter()
    .position(|keysym| sym == Keysym::from(*keysym))
    .map(|index| index as i32 + 1)
}

/// Fallback VT binding for keyboards or firmware that reserve Ctrl+Alt+Fn.
pub fn tty_switch_fallback_destination(mods: &ModifiersState, sym: Keysym) -> Option<i32> {
    if !(mods.ctrl && mods.logo) {
        return None;
    }

    [
        keysyms::KEY_1,
        keysyms::KEY_2,
        keysyms::KEY_3,
        keysyms::KEY_4,
        keysyms::KEY_5,
        keysyms::KEY_6,
        keysyms::KEY_7,
        keysyms::KEY_8,
        keysyms::KEY_9,
    ]
    .iter()
    .position(|keysym| sym == Keysym::from(*keysym))
    .map(|index| index as i32 + 1)
}

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

pub fn leds_for_state(state: LedState) -> Led {
    Led::from(state)
}

pub fn forward_leds(device: &mut Device, leds: LedState) {
    device.led_update(leds_for_state(leds));
}
