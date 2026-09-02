//! Remote input injection. Tests use FakeInjector; macOS uses CGEvent.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const CLIP_MAX: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

fn one_click() -> u8 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Action {
    Move {
        x: f64,
        y: f64,
        #[serde(default)]
        button: Option<MouseButton>,
    },
    Click {
        x: f64,
        y: f64,
        #[serde(default)]
        button: MouseButton,
        #[serde(default = "one_click")]
        clicks: u8,
    },
    Down {
        x: f64,
        y: f64,
        #[serde(default)]
        button: MouseButton,
        #[serde(default = "one_click")]
        clicks: u8,
    },
    Up {
        x: f64,
        y: f64,
        #[serde(default)]
        button: MouseButton,
        #[serde(default = "one_click")]
        clicks: u8,
    },
    Scroll {
        x: f64,
        y: f64,
        #[serde(default)]
        dy: f64,
        #[serde(default)]
        dx: f64,
    },
    Type {
        text: String,
    },
    Key {
        key: String,
        #[serde(default)]
        down: Option<bool>,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Clipboard {
        text: String,
    },
    Paste {
        text: String,
    },
    Wait {
        #[serde(default)]
        ms: u64,
    },
    Done,
}

impl Action {
    pub fn click(x: f64, y: f64) -> Self {
        Action::Click {
            x,
            y,
            button: MouseButton::Left,
            clicks: 1,
        }
    }

    pub fn clamp_coords(self) -> Self {
        match self {
            Action::Move { x, y, button } => Action::Move {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
                button,
            },
            Action::Click {
                x,
                y,
                button,
                clicks,
            } => Action::Click {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
                button,
                clicks: clicks.max(1),
            },
            Action::Down {
                x,
                y,
                button,
                clicks,
            } => Action::Down {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
                button,
                clicks: clicks.max(1),
            },
            Action::Up {
                x,
                y,
                button,
                clicks,
            } => Action::Up {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
                button,
                clicks: clicks.max(1),
            },
            Action::Scroll { x, y, dy, dx } => Action::Scroll {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
                dy,
                dx,
            },
            Action::Clipboard { text } => Action::Clipboard {
                text: clip_limit(text),
            },
            Action::Paste { text } => Action::Paste {
                text: clip_limit(text),
            },
            other => other,
        }
    }

    pub fn requests_clipboard_read(&self) -> bool {
        match self {
            Action::Key { key, modifiers, .. } => {
                let k = key.to_ascii_lowercase();
                let copy = k == "c" || k == "x";
                copy && modifiers.iter().any(|m| is_accel_mod(m))
            }
            _ => false,
        }
    }
}

fn is_accel_mod(m: &str) -> bool {
    matches!(
        m.to_ascii_lowercase().as_str(),
        "meta" | "command" | "cmd" | "control" | "ctrl" | "os"
    )
}

pub fn clip_limit(s: String) -> String {
    if s.len() <= CLIP_MAX {
        return s;
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > CLIP_MAX {
            break;
        }
        out.push(ch);
    }
    out
}

fn parse_button(v: &serde_json::Value) -> MouseButton {
    if let Some(s) = v.get("button").and_then(|b| b.as_str()) {
        return match s.to_ascii_lowercase().as_str() {
            "right" | "2" | "secondary" => MouseButton::Right,
            "middle" | "1" | "aux" => MouseButton::Middle,
            _ => MouseButton::Left,
        };
    }
    match v.get("button").and_then(|b| b.as_u64()).unwrap_or(0) {
        2 => MouseButton::Right,
        1 => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn parse_clicks(v: &serde_json::Value) -> u8 {
    v.get("clicks")
        .and_then(|c| c.as_u64())
        .or_else(|| v.get("detail").and_then(|c| c.as_u64()))
        .unwrap_or(1)
        .clamp(1, 8) as u8
}

fn parse_modifiers(v: &serde_json::Value) -> Vec<String> {
    let mut m = Vec::new();
    let arr = v
        .get("modifiers")
        .and_then(|x| x.as_array())
        .or_else(|| v.get("mods").and_then(|x| x.as_array()));
    if let Some(arr) = arr {
        for x in arr {
            if let Some(s) = x.as_str() {
                if !s.is_empty() {
                    m.push(s.to_string());
                }
            }
        }
    }
    if v.get("metaKey").and_then(|x| x.as_bool()).unwrap_or(false) {
        m.push("Meta".into());
    }
    if v.get("ctrlKey").and_then(|x| x.as_bool()).unwrap_or(false) {
        m.push("Control".into());
    }
    if v.get("altKey").and_then(|x| x.as_bool()).unwrap_or(false) {
        m.push("Alt".into());
    }
    if v.get("shiftKey").and_then(|x| x.as_bool()).unwrap_or(false) {
        m.push("Shift".into());
    }
    m.sort();
    m.dedup();
    m
}

pub fn parse_control_json(v: &serde_json::Value) -> Option<Action> {
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    let x = v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let y = v.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let button = parse_button(v);
    let clicks = parse_clicks(v);
    let a = match action {
        "move" => Action::Move {
            x,
            y,
            button: v.get("button").map(|_| button),
        },
        "click" => Action::Click {
            x,
            y,
            button,
            clicks,
        },
        "dblclick" | "doubleclick" => Action::Click {
            x,
            y,
            button,
            clicks: clicks.max(2),
        },
        "down" | "mousedown" => Action::Down {
            x,
            y,
            button,
            clicks,
        },
        "up" | "mouseup" => Action::Up {
            x,
            y,
            button,
            clicks,
        },
        "scroll" => Action::Scroll {
            x,
            y,
            dy: v.get("dy").and_then(|x| x.as_f64()).unwrap_or(0.0),
            dx: v.get("dx").and_then(|x| x.as_f64()).unwrap_or(0.0),
        },
        "type" | "text" => Action::Type {
            text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "key" | "keydown" | "keyup" => {
            let down = if action == "keydown" {
                Some(true)
            } else if action == "keyup" {
                Some(false)
            } else {
                v.get("down").and_then(|x| x.as_bool())
            };
            Action::Key {
                key: v.get("key").and_then(|x| x.as_str()).unwrap_or("").into(),
                down,
                modifiers: parse_modifiers(v),
            }
        }
        "clipboard" => Action::Clipboard {
            text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "paste" => Action::Paste {
            text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "wait" => Action::Wait {
            ms: v.get("ms").and_then(|x| x.as_u64()).unwrap_or(0),
        },
        "done" => Action::Done,
        _ => return None,
    };
    Some(a.clamp_coords())
}

pub fn map_norm_to_px(x: f64, y: f64, w: u32, h: u32) -> (f64, f64) {
    (
        x.clamp(0.0, 1.0) * (w.max(1) as f64),
        y.clamp(0.0, 1.0) * (h.max(1) as f64),
    )
}

/// macOS virtual keycodes. `None` for unknown names — never map those to 0 ('a').
pub fn mac_keycode(key: &str) -> Option<u16> {
    let k = key.trim();
    if k.is_empty() {
        return None;
    }
    match k {
        "Enter" | "Return" => Some(36),
        "Escape" | "Esc" => Some(53),
        "Tab" => Some(48),
        "Backspace" => Some(51),
        "Delete" | "Del" => Some(117),
        " " | "Space" | "Spacebar" => Some(49),
        "ArrowLeft" | "Left" => Some(123),
        "ArrowRight" | "Right" => Some(124),
        "ArrowDown" | "Down" => Some(125),
        "ArrowUp" | "Up" => Some(126),
        "Home" => Some(115),
        "End" => Some(119),
        "PageUp" => Some(116),
        "PageDown" => Some(121),
        "Meta" | "MetaLeft" | "OS" | "OSLeft" | "Command" | "Cmd" => Some(55),
        "MetaRight" | "OSRight" | "CommandRight" => Some(54),
        "Shift" | "ShiftLeft" => Some(56),
        "ShiftRight" => Some(60),
        "Alt" | "AltLeft" | "Option" | "OptionLeft" => Some(58),
        "AltRight" | "OptionRight" => Some(61),
        "Control" | "ControlLeft" | "Ctrl" => Some(59),
        "ControlRight" | "CtrlRight" => Some(62),
        "CapsLock" => Some(57),
        "F1" => Some(122),
        "F2" => Some(120),
        "F3" => Some(99),
        "F4" => Some(118),
        "F5" => Some(96),
        "F6" => Some(97),
        "F7" => Some(98),
        "F8" => Some(100),
        "F9" => Some(101),
        "F10" => Some(109),
        "F11" => Some(103),
        "F12" => Some(111),
        _ => {
            let mut s = k.to_ascii_lowercase();
            if let Some(rest) = s.strip_prefix("key") {
                s = rest.to_string();
            } else if let Some(rest) = s.strip_prefix("digit") {
                s = rest.to_string();
            }
            if s.len() != 1 {
                return None;
            }
            Some(match s.as_bytes()[0] {
                b'a' => 0,
                b's' => 1,
                b'd' => 2,
                b'f' => 3,
                b'h' => 4,
                b'g' => 5,
                b'z' => 6,
                b'x' => 7,
                b'c' => 8,
                b'v' => 9,
                b'b' => 11,
                b'q' => 12,
                b'w' => 13,
                b'e' => 14,
                b'r' => 15,
                b'y' => 16,
                b't' => 17,
                b'1' => 18,
                b'2' => 19,
                b'3' => 20,
                b'4' => 21,
                b'6' => 22,
                b'5' => 23,
                b'=' => 24,
                b'9' => 25,
                b'7' => 26,
                b'-' => 27,
                b'8' => 28,
                b'0' => 29,
                b']' => 30,
                b'o' => 31,
                b'u' => 32,
                b'[' => 33,
                b'i' => 34,
                b'p' => 35,
                b'l' => 37,
                b'j' => 38,
                b'\'' => 39,
                b'k' => 40,
                b';' => 41,
                b'\\' => 42,
                b',' => 43,
                b'/' => 44,
                b'n' => 45,
                b'm' => 46,
                b'.' => 47,
                b'`' => 50,
                _ => return None,
            })
        }
    }
}

pub fn modifier_flag(name: &str) -> u64 {
    match name.to_ascii_lowercase().as_str() {
        "shift" => 0x0002_0000,
        "control" | "ctrl" => 0x0004_0000,
        "alt" | "option" | "alternate" => 0x0008_0000,
        "meta" | "command" | "cmd" | "os" => 0x0010_0000,
        _ => 0,
    }
}

pub fn flags_from_mods(mods: &[String], held: u64) -> u64 {
    let mut f = held;
    for m in mods {
        f |= modifier_flag(m);
    }
    f
}

pub trait Injector: Send + Sync {
    fn apply(&self, action: &Action);
    fn set_screen_size(&self, _w: u32, _h: u32) {}
    fn screen_size(&self) -> (u32, u32) {
        (0, 0)
    }
    fn clipboard_set(&self, _text: &str) {}
    fn clipboard_get(&self) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Injected {
    Move {
        x: f64,
        y: f64,
        button: Option<MouseButton>,
    },
    Click {
        x: f64,
        y: f64,
        button: MouseButton,
        clicks: u8,
    },
    Down {
        x: f64,
        y: f64,
        button: MouseButton,
        clicks: u8,
    },
    Up {
        x: f64,
        y: f64,
        button: MouseButton,
        clicks: u8,
    },
    Scroll {
        x: f64,
        y: f64,
        dy: f64,
        dx: f64,
    },
    Type {
        text: String,
    },
    Key {
        key: String,
        down: Option<bool>,
        modifiers: Vec<String>,
    },
    Clipboard {
        text: String,
    },
    Paste {
        text: String,
    },
}

impl Injected {
    pub fn click(x: f64, y: f64) -> Self {
        Injected::Click {
            x,
            y,
            button: MouseButton::Left,
            clicks: 1,
        }
    }
}

#[derive(Clone, Default)]
pub struct FakeInjector {
    pub events: Arc<Mutex<Vec<Injected>>>,
    pub clip: Arc<Mutex<String>>,
}

impl FakeInjector {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn recorded(&self) -> Vec<Injected> {
        self.events.lock().clone()
    }
}

impl Injector for FakeInjector {
    fn apply(&self, action: &Action) {
        let ev = match action {
            Action::Move { x, y, button } => Injected::Move {
                x: *x,
                y: *y,
                button: *button,
            },
            Action::Click {
                x,
                y,
                button,
                clicks,
            } => Injected::Click {
                x: *x,
                y: *y,
                button: *button,
                clicks: *clicks,
            },
            Action::Down {
                x,
                y,
                button,
                clicks,
            } => Injected::Down {
                x: *x,
                y: *y,
                button: *button,
                clicks: *clicks,
            },
            Action::Up {
                x,
                y,
                button,
                clicks,
            } => Injected::Up {
                x: *x,
                y: *y,
                button: *button,
                clicks: *clicks,
            },
            Action::Scroll { x, y, dy, dx } => Injected::Scroll {
                x: *x,
                y: *y,
                dy: *dy,
                dx: *dx,
            },
            Action::Type { text } => Injected::Type { text: text.clone() },
            Action::Key {
                key,
                down,
                modifiers,
            } => Injected::Key {
                key: key.clone(),
                down: *down,
                modifiers: modifiers.clone(),
            },
            Action::Clipboard { text } => {
                self.clipboard_set(text);
                Injected::Clipboard { text: text.clone() }
            }
            Action::Paste { text } => {
                if !text.is_empty() {
                    self.clipboard_set(text);
                }
                Injected::Paste { text: text.clone() }
            }
            Action::Wait { .. } | Action::Done => return,
        };
        self.events.lock().push(ev);
    }

    fn clipboard_set(&self, text: &str) {
        *self.clip.lock() = clip_limit(text.to_string());
    }

    fn clipboard_get(&self) -> Option<String> {
        let s = self.clip.lock().clone();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

pub struct NullInjector;

impl Injector for NullInjector {
    fn apply(&self, _action: &Action) {}
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        flags_from_mods, mac_keycode, modifier_flag, Action, Injector, MouseButton, CLIP_MAX,
    };
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    type CGEventRef = *mut std::ffi::c_void;
    type CGEventSourceRef = *mut std::ffi::c_void;
    type CGEventTapLocation = u32;
    type CGEventType = u32;
    type CGMouseButton = u32;
    type CGKeyCode = u16;

    const HID: CGEventTapLocation = 0;
    const LEFT_DOWN: CGEventType = 1;
    const LEFT_UP: CGEventType = 2;
    const RIGHT_DOWN: CGEventType = 3;
    const RIGHT_UP: CGEventType = 4;
    const MOVED: CGEventType = 5;
    const LEFT_DRAGGED: CGEventType = 6;
    const RIGHT_DRAGGED: CGEventType = 7;
    const KEY_DOWN: CGEventType = 10;
    const KEY_UP: CGEventType = 11;
    const OTHER_DOWN: CGEventType = 25;
    const OTHER_UP: CGEventType = 26;
    const OTHER_DRAGGED: CGEventType = 27;
    const SCROLL: CGEventType = 22;
    const LEFT_BUTTON: CGMouseButton = 0;
    const RIGHT_BUTTON: CGMouseButton = 1;
    const CENTER_BUTTON: CGMouseButton = 2;
    const CLICK_STATE_FIELD: u32 = 1;

    #[link(name = "CoreGraphics", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CGEventCreateMouseEvent(
            source: CGEventSourceRef,
            ty: CGEventType,
            pos: CGPoint,
            button: CGMouseButton,
        ) -> CGEventRef;
        fn CGEventCreateScrollWheelEvent(
            source: CGEventSourceRef,
            units: u32,
            wheel_count: u32,
            wheel1: i32,
        ) -> CGEventRef;
        fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            key: CGKeyCode,
            key_down: bool,
        ) -> CGEventRef;
        fn CGEventKeyboardSetUnicodeString(
            event: CGEventRef,
            len: std::ffi::c_ulong,
            string: *const u16,
        );
        fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
        fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CFRelease(cf: *mut std::ffi::c_void);
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(id: u32) -> CGRect;
    }

    pub fn display_size() -> Option<(u32, u32)> {
        unsafe {
            let id = CGMainDisplayID();
            let r = CGDisplayBounds(id);
            let w = r.size.width.round() as u32;
            let h = r.size.height.round() as u32;
            if w > 0 && h > 0 {
                Some((w, h))
            } else {
                None
            }
        }
    }

    fn screen_px(x: f64, y: f64, w: f64, h: f64) -> CGPoint {
        let (px, py) = super::map_norm_to_px(x, y, w as u32, h as u32);
        CGPoint { x: px, y: py }
    }

    fn cg_button(button: MouseButton) -> CGMouseButton {
        match button {
            MouseButton::Left => LEFT_BUTTON,
            MouseButton::Right => RIGHT_BUTTON,
            MouseButton::Middle => CENTER_BUTTON,
        }
    }

    fn down_ty(button: MouseButton) -> CGEventType {
        match button {
            MouseButton::Left => LEFT_DOWN,
            MouseButton::Right => RIGHT_DOWN,
            MouseButton::Middle => OTHER_DOWN,
        }
    }

    fn up_ty(button: MouseButton) -> CGEventType {
        match button {
            MouseButton::Left => LEFT_UP,
            MouseButton::Right => RIGHT_UP,
            MouseButton::Middle => OTHER_UP,
        }
    }

    fn drag_ty(button: MouseButton) -> CGEventType {
        match button {
            MouseButton::Left => LEFT_DRAGGED,
            MouseButton::Right => RIGHT_DRAGGED,
            MouseButton::Middle => OTHER_DRAGGED,
        }
    }

    fn button_bit(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
        }
    }

    pub fn clipboard_set_os(text: &str) -> bool {
        let mut child = match Command::new("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }

    pub fn clipboard_get_os() -> Option<String> {
        let out = Command::new("pbpaste")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?;
        if s.is_empty() {
            None
        } else {
            Some(if s.len() > CLIP_MAX {
                super::clip_limit(s)
            } else {
                s
            })
        }
    }

    unsafe fn post_mouse(
        ty: CGEventType,
        pos: CGPoint,
        button: CGMouseButton,
        clicks: u8,
        flags: u64,
    ) {
        let ev = CGEventCreateMouseEvent(std::ptr::null_mut(), ty, pos, button);
        if ev.is_null() {
            return;
        }
        if clicks > 1 {
            CGEventSetIntegerValueField(ev, CLICK_STATE_FIELD, clicks as i64);
        }
        if flags != 0 {
            CGEventSetFlags(ev, flags);
        }
        CGEventPost(HID, ev);
        CFRelease(ev);
    }

    unsafe fn post_key(code: CGKeyCode, key_down: bool, flags: u64) {
        let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, key_down);
        if ev.is_null() {
            return;
        }
        if flags != 0 {
            CGEventSetFlags(ev, flags);
        }
        CGEventPost(HID, ev);
        CFRelease(ev);
        let _ = KEY_DOWN;
        let _ = KEY_UP;
        let _ = SCROLL;
    }

    unsafe fn post_unicode(ch: u16, flags: u64) {
        let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, true);
        if ev.is_null() {
            return;
        }
        let u = [ch];
        CGEventKeyboardSetUnicodeString(ev, 1, u.as_ptr());
        if flags != 0 {
            CGEventSetFlags(ev, flags);
        }
        CGEventPost(HID, ev);
        CFRelease(ev);
        let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, false);
        if !ev.is_null() {
            CGEventKeyboardSetUnicodeString(ev, 1, u.as_ptr());
            if flags != 0 {
                CGEventSetFlags(ev, flags);
            }
            CGEventPost(HID, ev);
            CFRelease(ev);
        }
    }

    pub struct MacInjector {
        pub width: std::sync::atomic::AtomicU32,
        pub height: std::sync::atomic::AtomicU32,
        buttons: AtomicU8,
        flags: AtomicU64,
    }

    impl MacInjector {
        pub fn new() -> Self {
            let s = Self {
                width: std::sync::atomic::AtomicU32::new(0),
                height: std::sync::atomic::AtomicU32::new(0),
                buttons: AtomicU8::new(0),
                flags: AtomicU64::new(0),
            };
            s.refresh_display_size();
            s
        }
        pub fn refresh_display_size(&self) {
            if let Some((w, h)) = display_size() {
                self.width.store(w, Ordering::Relaxed);
                self.height.store(h, Ordering::Relaxed);
            }
        }
        /// CGEvent mapping size: live display bounds. Encoder/hub size is ignored.
        pub fn mapping_size(&self) -> (u32, u32) {
            if let Some(d) = display_size() {
                return d;
            }
            (
                self.width.load(Ordering::Relaxed).max(1),
                self.height.load(Ordering::Relaxed).max(1),
            )
        }
        fn size(&self) -> (f64, f64) {
            let (w, h) = self.mapping_size();
            (w as f64, h as f64)
        }

        fn current_flags(&self, extra: &[String]) -> u64 {
            flags_from_mods(extra, self.flags.load(Ordering::SeqCst))
        }

        fn apply_key(&self, key: &str, down: Option<bool>, modifiers: &[String]) {
            let extra_flags = flags_from_mods(modifiers, 0);
            let is_mod = modifier_flag(key) != 0
                || matches!(
                    key,
                    "Meta"
                        | "MetaLeft"
                        | "MetaRight"
                        | "Shift"
                        | "ShiftLeft"
                        | "ShiftRight"
                        | "Alt"
                        | "AltLeft"
                        | "AltRight"
                        | "Control"
                        | "ControlLeft"
                        | "ControlRight"
                        | "Command"
                        | "Cmd"
                        | "Option"
                        | "OS"
                );
            if is_mod {
                let bit = modifier_flag(key);
                match down {
                    Some(true) => {
                        self.flags.fetch_or(bit, Ordering::SeqCst);
                    }
                    Some(false) => {
                        self.flags.fetch_and(!bit, Ordering::SeqCst);
                    }
                    None => {}
                }
            }
            let flags = self.current_flags(modifiers) | extra_flags;
            let code = mac_keycode(key);
            unsafe {
                match down {
                    Some(true) => {
                        if let Some(c) = code {
                            post_key(c, true, flags);
                        }
                    }
                    Some(false) => {
                        if let Some(c) = code {
                            post_key(c, false, flags);
                        }
                    }
                    None => {
                        if let Some(c) = code {
                            post_key(c, true, flags);
                            post_key(c, false, flags);
                        } else if key.chars().count() == 1 {
                            if let Some(ch) = key.encode_utf16().next() {
                                post_unicode(ch, flags);
                            }
                        }
                    }
                }
            }
        }
    }

    impl Injector for MacInjector {
        fn set_screen_size(&self, _w: u32, _h: u32) {
            // Encoder/hub size (capped 1920×1080) must not replace CGDisplayBounds.
        }
        fn screen_size(&self) -> (u32, u32) {
            self.mapping_size()
        }
        fn clipboard_set(&self, text: &str) {
            let _ = clipboard_set_os(text);
        }
        fn clipboard_get(&self) -> Option<String> {
            clipboard_get_os()
        }
        fn apply(&self, action: &Action) {
            let (w, h) = self.size();
            let held = self.flags.load(Ordering::SeqCst);
            unsafe {
                match action {
                    Action::Move { x, y, button } => {
                        let pos = screen_px(*x, *y, w, h);
                        let pressed = self.buttons.load(Ordering::SeqCst);
                        let btn = button.unwrap_or_else(|| {
                            if pressed & 2 != 0 {
                                MouseButton::Right
                            } else if pressed & 4 != 0 {
                                MouseButton::Middle
                            } else {
                                MouseButton::Left
                            }
                        });
                        let ty = if pressed != 0 { drag_ty(btn) } else { MOVED };
                        post_mouse(ty, pos, cg_button(btn), 1, held);
                    }
                    Action::Click {
                        x,
                        y,
                        button,
                        clicks,
                    } => {
                        let pos = screen_px(*x, *y, w, h);
                        post_mouse(MOVED, pos, cg_button(*button), *clicks, held);
                        post_mouse(down_ty(*button), pos, cg_button(*button), *clicks, held);
                        post_mouse(up_ty(*button), pos, cg_button(*button), *clicks, held);
                    }
                    Action::Down {
                        x,
                        y,
                        button,
                        clicks,
                    } => {
                        self.buttons.fetch_or(button_bit(*button), Ordering::SeqCst);
                        let pos = screen_px(*x, *y, w, h);
                        post_mouse(down_ty(*button), pos, cg_button(*button), *clicks, held);
                    }
                    Action::Up {
                        x,
                        y,
                        button,
                        clicks,
                    } => {
                        let pos = screen_px(*x, *y, w, h);
                        post_mouse(up_ty(*button), pos, cg_button(*button), *clicks, held);
                        self.buttons
                            .fetch_and(!button_bit(*button), Ordering::SeqCst);
                    }
                    Action::Scroll { x, y, dy, dx } => {
                        post_mouse(MOVED, screen_px(*x, *y, w, h), LEFT_BUTTON, 1, held);
                        let wheel = if dy.abs() >= dx.abs() {
                            (*dy * 5.0) as i32
                        } else {
                            (*dx * 5.0) as i32
                        };
                        let ev = CGEventCreateScrollWheelEvent(std::ptr::null_mut(), 0, 1, wheel);
                        if !ev.is_null() {
                            if held != 0 {
                                CGEventSetFlags(ev, held);
                            }
                            CGEventPost(HID, ev);
                            CFRelease(ev);
                        }
                    }
                    Action::Type { text } => {
                        for ch in text.encode_utf16() {
                            post_unicode(ch, held);
                        }
                    }
                    Action::Key {
                        key,
                        down,
                        modifiers,
                    } => {
                        self.apply_key(key, *down, modifiers);
                    }
                    Action::Clipboard { text } => {
                        let _ = clipboard_set_os(text);
                    }
                    Action::Paste { text } => {
                        if !text.is_empty() {
                            let _ = clipboard_set_os(text);
                        }
                        self.apply_key("v", None, &["Meta".into()]);
                    }
                    Action::Wait { .. } | Action::Done => {}
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{display_size, MacInjector};

pub fn production_injector() -> Arc<dyn Injector> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(MacInjector::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(NullInjector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_records_click_and_type_with_clamped_coords() {
        let inj = FakeInjector::new();
        inj.apply(&Action::click(1.5, -0.2).clamp_coords());
        inj.apply(&Action::Type {
            text: "hello".into(),
        });
        assert_eq!(
            inj.recorded(),
            vec![
                Injected::click(1.0, 0.0),
                Injected::Type {
                    text: "hello".into()
                }
            ]
        );
    }

    #[test]
    fn parse_control_json_click() {
        let v = serde_json::json!({"type":"control","action":"click","x":0.5,"y":0.25});
        assert_eq!(parse_control_json(&v), Some(Action::click(0.5, 0.25)));
    }

    #[test]
    fn parse_right_click_down_up_and_dblclick() {
        let right = serde_json::json!({"action":"click","x":0.2,"y":0.3,"button":"right"});
        assert_eq!(
            parse_control_json(&right),
            Some(Action::Click {
                x: 0.2,
                y: 0.3,
                button: MouseButton::Right,
                clicks: 1
            })
        );
        let down = serde_json::json!({"action":"down","x":0.1,"y":0.2,"button":2,"clicks":1});
        assert_eq!(
            parse_control_json(&down),
            Some(Action::Down {
                x: 0.1,
                y: 0.2,
                button: MouseButton::Right,
                clicks: 1
            })
        );
        let up = serde_json::json!({"action":"mouseup","x":0.4,"y":0.5,"button":0});
        assert_eq!(
            parse_control_json(&up),
            Some(Action::Up {
                x: 0.4,
                y: 0.5,
                button: MouseButton::Left,
                clicks: 1
            })
        );
        let dbl = serde_json::json!({"action":"dblclick","x":0.5,"y":0.5});
        match parse_control_json(&dbl) {
            Some(Action::Click { clicks, button, .. }) => {
                assert_eq!(clicks, 2);
                assert_eq!(button, MouseButton::Left);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_key_with_modifiers_and_paste() {
        let v = serde_json::json!({
            "action":"keydown",
            "key":"c",
            "modifiers":["Meta"]
        });
        assert_eq!(
            parse_control_json(&v),
            Some(Action::Key {
                key: "c".into(),
                down: Some(true),
                modifiers: vec!["Meta".into()]
            })
        );
        let paste = serde_json::json!({"action":"paste","text":"hello from clip"});
        assert_eq!(
            parse_control_json(&paste),
            Some(Action::Paste {
                text: "hello from clip".into()
            })
        );
        let clip = serde_json::json!({"action":"clipboard","text":"abc"});
        assert_eq!(
            parse_control_json(&clip),
            Some(Action::Clipboard { text: "abc".into() })
        );
    }

    #[test]
    fn fake_clipboard_and_right_click() {
        let inj = FakeInjector::new();
        inj.apply(&Action::Click {
            x: 0.2,
            y: 0.3,
            button: MouseButton::Right,
            clicks: 1,
        });
        inj.apply(&Action::Down {
            x: 0.2,
            y: 0.3,
            button: MouseButton::Left,
            clicks: 1,
        });
        inj.apply(&Action::Move {
            x: 0.8,
            y: 0.9,
            button: Some(MouseButton::Left),
        });
        inj.apply(&Action::Up {
            x: 0.8,
            y: 0.9,
            button: MouseButton::Left,
            clicks: 1,
        });
        inj.apply(&Action::Paste {
            text: "pasted".into(),
        });
        assert_eq!(inj.clipboard_get().as_deref(), Some("pasted"));
        let rec = inj.recorded();
        assert!(matches!(
            rec[0],
            Injected::Click {
                button: MouseButton::Right,
                ..
            }
        ));
        assert!(matches!(rec[1], Injected::Down { .. }));
        assert!(matches!(rec[2], Injected::Move { .. }));
        assert!(matches!(rec[3], Injected::Up { .. }));
        assert_eq!(
            rec[4],
            Injected::Paste {
                text: "pasted".into()
            }
        );
    }

    #[test]
    fn unknown_key_does_not_map_to_letter_a() {
        assert_eq!(mac_keycode("Enter"), Some(36));
        assert_eq!(mac_keycode("c"), Some(8));
        assert_eq!(mac_keycode("KeyC"), Some(8));
        assert_eq!(mac_keycode("Meta"), Some(55));
        assert_eq!(mac_keycode("F1"), Some(122));
        assert_eq!(mac_keycode("Unidentified"), None);
        assert_eq!(mac_keycode("F99"), None);
        assert_eq!(mac_keycode(""), None);
        assert_ne!(mac_keycode("Unidentified"), Some(0));
    }

    #[test]
    fn copy_shortcut_requests_clipboard_read() {
        let copy = Action::Key {
            key: "c".into(),
            down: Some(true),
            modifiers: vec!["Meta".into()],
        };
        assert!(copy.requests_clipboard_read());
        let type_c = Action::Type { text: "c".into() };
        assert!(!type_c.requests_clipboard_read());
    }

    #[test]
    fn map_norm_to_px_uses_capture_or_display_size() {
        assert_eq!(map_norm_to_px(0.5, 0.5, 1440, 900), (720.0, 450.0));
        assert_eq!(map_norm_to_px(0.0, 1.0, 800, 600), (0.0, 600.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_injector_maps_through_display_bounds_not_encoder_hub_size() {
        let expected = display_size().expect("CGDisplayBounds");
        assert!(expected.0 >= 800 && expected.1 >= 600);
        let m = MacInjector::new();
        assert_eq!(m.mapping_size(), expected);
        // Production App used to push hub.size() (ffmpeg cap 1920×1080) here.
        m.set_screen_size(1920, 1080);
        m.set_screen_size(800, 600);
        assert_eq!(
            m.mapping_size(),
            expected,
            "CGEvent mapping must stay on CGDisplayBounds after encoder size overwrite"
        );
        assert_eq!(m.screen_size(), expected);
        let (px, py) = map_norm_to_px(0.5, 0.5, expected.0, expected.1);
        assert_eq!(px, expected.0 as f64 * 0.5);
        assert_eq!(py, expected.1 as f64 * 0.5);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_clipboard_roundtrip_via_pasteboard() {
        let marker = format!("streamaid-clip-{}", std::process::id());
        assert!(macos::clipboard_set_os(&marker));
        let got = macos::clipboard_get_os().unwrap_or_default();
        assert_eq!(got, marker);
    }
}
