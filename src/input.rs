//! Remote input injection. Tests use FakeInjector; macOS uses CGEvent.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const CLIP_MAX: usize = 512 * 1024;
/// 5K / dual-display screenshots as PNG; never truncate, never a 2 GB RAM blob.
pub const CLIP_PNG_MAX: usize = 128 * 1024 * 1024;
pub const PNG_SIG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

pub fn is_png(data: &[u8]) -> bool {
    data.len() >= 8 && data.starts_with(PNG_SIG)
}

pub fn png_fits(len: usize) -> bool {
    len > 0 && len <= CLIP_PNG_MAX
}

pub fn accept_png(png: Vec<u8>) -> Option<Vec<u8>> {
    if is_png(&png) && png_fits(png.len()) {
        Some(png)
    } else {
        None
    }
}

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
    ClipboardPng {
        png: Vec<u8>,
    },
    Display {
        id: String,
    },
    Paste {
        text: String,
    },
    File {
        name: String,
        data: Vec<u8>,
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
            Action::ClipboardPng { png } => Action::ClipboardPng {
                png: accept_png(png).unwrap_or_default(),
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
        "display" => Action::Display {
            id: v
                .get("id")
                .or_else(|| v.get("input"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .into(),
        },
        "clipboard" => {
            let mime = v.get("mime").and_then(|m| m.as_str()).unwrap_or("");
            if mime.contains("png") || v.get("png").is_some() {
                let b64 = v
                    .get("data")
                    .or_else(|| v.get("png"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                match crate::files::decode_b64(b64).ok().and_then(accept_png) {
                    Some(png) => Action::ClipboardPng { png },
                    None => return None,
                }
            } else {
                Action::Clipboard {
                    text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").into(),
                }
            }
        },
        "paste" => Action::Paste {
            text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "file" => {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let data = if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                t.as_bytes().to_vec()
            } else if let Some(b64) = v.get("data").and_then(|d| d.as_str()) {
                match crate::files::decode_b64(b64) {
                    Ok(b) => b,
                    Err(_) => return None,
                }
            } else {
                return None;
            };
            if name.is_empty() || data.is_empty() || data.len() > crate::files::HTTP_PUT_MAX {
                return None;
            }
            Action::File {
                name: name.to_string(),
                data,
            }
        }
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

/// Global CGEvent point for a normalized click on a display rect (origin may not be 0,0).
pub fn map_norm_to_global(nx: f64, ny: f64, x: i32, y: i32, w: u32, h: u32) -> (f64, f64) {
    let (px, py) = map_norm_to_px(nx, ny, w, h);
    (x as f64 + px, y as f64 + py)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub main: bool,
}

/// FFmpeg `Capture screen N` is `CGGetActiveDisplayList()[N]` (N=0 is main).
/// Do not compact/filter the slice before indexing — that desyncs clicks.
pub fn display_for_ffmpeg_screen(
    screen_idx: usize,
    cg: &[DisplayInfo],
) -> Option<&DisplayInfo> {
    if let Some(d) = cg.get(screen_idx) {
        return Some(d);
    }
    if screen_idx == 0 {
        return cg.iter().find(|d| d.main).or_else(|| cg.first());
    }
    cg.iter().filter(|d| !d.main).nth(screen_idx - 1)
}

pub fn pick_display<'a>(input: &str, displays: &'a [DisplayInfo]) -> Option<&'a DisplayInfo> {
    let want = input.trim();
    if !want.is_empty() {
        if let Some(d) = displays.iter().find(|d| d.id == want) {
            return Some(d);
        }
        let stripped = want.trim_end_matches(':');
        if let Some(d) = displays
            .iter()
            .find(|d| d.id.trim_end_matches(':') == stripped)
        {
            return Some(d);
        }
    }
    displays
        .iter()
        .find(|d| d.main)
        .or_else(|| displays.first())
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
    fn set_display_rect(&self, _x: i32, _y: i32, _w: u32, _h: u32) {}
    fn display_rect(&self) -> (i32, i32, u32, u32) {
        (0, 0, 0, 0)
    }
    fn clipboard_set(&self, _text: &str) {}
    fn clipboard_get(&self) -> Option<String> {
        None
    }
    fn clipboard_set_png(&self, _png: &[u8]) {}
    fn clipboard_get_png(&self) -> Option<Vec<u8>> {
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
    ClipboardPng {
        png: Vec<u8>,
    },
    Display {
        id: String,
    },
    Paste {
        text: String,
    },
    File {
        name: String,
        data: Vec<u8>,
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

#[derive(Clone)]
pub struct FakeInjector {
    pub events: Arc<Mutex<Vec<Injected>>>,
    pub clip: Arc<Mutex<String>>,
    pub png: Arc<Mutex<Option<Vec<u8>>>>,
    pub rect: Arc<Mutex<(i32, i32, u32, u32)>>,
}

impl FakeInjector {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn recorded(&self) -> Vec<Injected> {
        self.events.lock().clone()
    }
}

impl Default for FakeInjector {
    fn default() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            clip: Arc::new(Mutex::new(String::new())),
            png: Arc::new(Mutex::new(None)),
            rect: Arc::new(Mutex::new((0, 0, 0, 0))),
        }
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
            Action::ClipboardPng { png } => {
                self.clipboard_set_png(png);
                Injected::ClipboardPng { png: png.clone() }
            }
            Action::Display { id } => Injected::Display { id: id.clone() },
            Action::Paste { text } => {
                if !text.is_empty() {
                    self.clipboard_set(text);
                }
                Injected::Paste { text: text.clone() }
            }
            Action::File { name, data } => Injected::File {
                name: name.clone(),
                data: data.clone(),
            },
            Action::Wait { .. } | Action::Done => return,
        };
        self.events.lock().push(ev);
    }

    fn set_display_rect(&self, x: i32, y: i32, w: u32, h: u32) {
        *self.rect.lock() = (x, y, w, h);
    }
    fn display_rect(&self) -> (i32, i32, u32, u32) {
        *self.rect.lock()
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
    fn clipboard_set_png(&self, png: &[u8]) {
        if let Some(png) = accept_png(png.to_vec()) {
            *self.png.lock() = Some(png);
        }
    }
    fn clipboard_get_png(&self) -> Option<Vec<u8>> {
        self.png.lock().clone()
    }
}

pub struct NullInjector;

impl Injector for NullInjector {
    fn apply(&self, _action: &Action) {}
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        accept_png, flags_from_mods, is_png, mac_keycode, modifier_flag, png_fits, Action, Injector,
        MouseButton, CLIP_MAX,
    };
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};

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
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    }

    pub fn display_size() -> Option<(u32, u32)> {
        list_cg_displays()
            .into_iter()
            .find(|d| d.main)
            .or_else(|| list_cg_displays().into_iter().next())
            .map(|d| (d.width, d.height))
            .filter(|(w, h)| *w > 0 && *h > 0)
    }

    pub fn list_cg_displays() -> Vec<super::DisplayInfo> {
        unsafe {
            let mut ids = [0u32; 32];
            let mut n = 0u32;
            let err = CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n);
            if err != 0 || n == 0 {
                let id = CGMainDisplayID();
                let r = CGDisplayBounds(id);
                let w = r.size.width.round() as u32;
                let h = r.size.height.round() as u32;
                if w == 0 || h == 0 {
                    return Vec::new();
                }
                return vec![super::DisplayInfo {
                    id: "0:".into(),
                    name: "Display 1 (main)".into(),
                    x: r.origin.x.round() as i32,
                    y: r.origin.y.round() as i32,
                    width: w,
                    height: h,
                    main: true,
                }];
            }
            let main_id = CGMainDisplayID();
            (0..n as usize)
                .map(|i| {
                    let id = ids[i];
                    let r = CGDisplayBounds(id);
                    let main = id == main_id;
                    super::DisplayInfo {
                        id: format!("{i}:"),
                        name: if main {
                            format!("Display {} (main)", i + 1)
                        } else {
                            format!("Display {}", i + 1)
                        },
                        x: r.origin.x.round() as i32,
                        y: r.origin.y.round() as i32,
                        width: r.size.width.round() as u32,
                        height: r.size.height.round() as u32,
                        main,
                    }
                })
                .collect()
        }
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

    fn applescript_posix(path: &std::path::Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }

    pub fn clipboard_set_png_os(png: &[u8]) -> bool {
        if !is_png(png) || !png_fits(png.len()) {
            return false;
        }
        let path = std::env::temp_dir().join(format!("streamaid-clip-in-{}.png", std::process::id()));
        if std::fs::write(&path, png).is_err() {
            return false;
        }
        let posix = applescript_posix(&path);
        let ok = Command::new("osascript")
            .arg("-e")
            .arg(format!(
                r#"set the clipboard to (read (POSIX file "{posix}") as «class PNGf»)"#
            ))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(&path);
        ok
    }

    pub fn clipboard_get_png_os() -> Option<Vec<u8>> {
        let path =
            std::env::temp_dir().join(format!("streamaid-clip-out-{}.png", std::process::id()));
        let posix = applescript_posix(&path);
        let script = format!(
            r#"try
  set png_data to (the clipboard as «class PNGf»)
  set f to open for access POSIX file "{posix}" with write permission
  set eof of f to 0
  write png_data to f
  close access f
end try"#
        );
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let data = std::fs::read(&path).ok();
        let _ = std::fs::remove_file(&path);
        let data = data?;
        accept_png(data)
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
        pub width: AtomicU32,
        pub height: AtomicU32,
        origin_x: AtomicI32,
        origin_y: AtomicI32,
        buttons: AtomicU8,
        flags: AtomicU64,
    }

    impl MacInjector {
        pub fn new() -> Self {
            let s = Self {
                width: AtomicU32::new(0),
                height: AtomicU32::new(0),
                origin_x: AtomicI32::new(0),
                origin_y: AtomicI32::new(0),
                buttons: AtomicU8::new(0),
                flags: AtomicU64::new(0),
            };
            s.refresh_display_size();
            s
        }
        pub fn refresh_display_size(&self) {
            if let Some(d) = list_cg_displays().into_iter().find(|d| d.main) {
                self.origin_x.store(d.x, Ordering::Relaxed);
                self.origin_y.store(d.y, Ordering::Relaxed);
                self.width.store(d.width, Ordering::Relaxed);
                self.height.store(d.height, Ordering::Relaxed);
            }
        }
        /// CGEvent mapping size of the selected display. Encoder/hub size is ignored.
        pub fn mapping_size(&self) -> (u32, u32) {
            let w = self.width.load(Ordering::Relaxed);
            let h = self.height.load(Ordering::Relaxed);
            if w > 0 && h > 0 {
                return (w, h);
            }
            display_size().unwrap_or((1, 1))
        }
        pub fn mapping_rect(&self) -> (i32, i32, u32, u32) {
            let (w, h) = self.mapping_size();
            (
                self.origin_x.load(Ordering::Relaxed),
                self.origin_y.load(Ordering::Relaxed),
                w,
                h,
            )
        }
        fn point(&self, nx: f64, ny: f64) -> CGPoint {
            let (ox, oy, w, h) = self.mapping_rect();
            let (x, y) = super::map_norm_to_global(nx, ny, ox, oy, w, h);
            CGPoint { x, y }
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
        fn set_display_rect(&self, x: i32, y: i32, w: u32, h: u32) {
            self.origin_x.store(x, Ordering::Relaxed);
            self.origin_y.store(y, Ordering::Relaxed);
            if w > 0 {
                self.width.store(w, Ordering::Relaxed);
            }
            if h > 0 {
                self.height.store(h, Ordering::Relaxed);
            }
        }
        fn display_rect(&self) -> (i32, i32, u32, u32) {
            self.mapping_rect()
        }
        fn clipboard_set(&self, text: &str) {
            let _ = clipboard_set_os(text);
        }
        fn clipboard_get(&self) -> Option<String> {
            clipboard_get_os()
        }
        fn clipboard_set_png(&self, png: &[u8]) {
            let _ = clipboard_set_png_os(png);
        }
        fn clipboard_get_png(&self) -> Option<Vec<u8>> {
            clipboard_get_png_os()
        }
        fn apply(&self, action: &Action) {
            let held = self.flags.load(Ordering::SeqCst);
            unsafe {
                match action {
                    Action::Move { x, y, button } => {
                        let pos = self.point(*x, *y);
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
                        let pos = self.point(*x, *y);
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
                        let pos = self.point(*x, *y);
                        post_mouse(down_ty(*button), pos, cg_button(*button), *clicks, held);
                    }
                    Action::Up {
                        x,
                        y,
                        button,
                        clicks,
                    } => {
                        let pos = self.point(*x, *y);
                        post_mouse(up_ty(*button), pos, cg_button(*button), *clicks, held);
                        self.buttons
                            .fetch_and(!button_bit(*button), Ordering::SeqCst);
                    }
                    Action::Scroll { x, y, dy, dx } => {
                        post_mouse(MOVED, self.point(*x, *y), LEFT_BUTTON, 1, held);
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
                    Action::ClipboardPng { png } => {
                        let _ = clipboard_set_png_os(png);
                        self.apply_key("v", None, &["Meta".into()]);
                    }
                    Action::Display { .. } => {}
                    Action::Paste { text } => {
                        if !text.is_empty() {
                            let _ = clipboard_set_os(text);
                        }
                        self.apply_key("v", None, &["Meta".into()]);
                    }
                    Action::File { .. } => {}
                    Action::Wait { .. } | Action::Done => {}
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{display_size, list_cg_displays, MacInjector};

#[cfg(not(target_os = "macos"))]
pub fn list_cg_displays() -> Vec<DisplayInfo> {
    Vec::new()
}

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

    /// 1×1 PNG used for clipboard image tests.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xFE, 0xD4, 0xEF, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

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
        let disp = serde_json::json!({"action":"display","id":"4:"});
        assert_eq!(
            parse_control_json(&disp),
            Some(Action::Display { id: "4:".into() })
        );
        let clip = serde_json::json!({"action":"clipboard","text":"abc"});
        assert_eq!(
            parse_control_json(&clip),
            Some(Action::Clipboard { text: "abc".into() })
        );
        let png = crate::files::encode_b64(TINY_PNG);
        let img = serde_json::json!({"action":"clipboard","mime":"image/png","data": png});
        match parse_control_json(&img) {
            Some(Action::ClipboardPng { png }) => assert!(is_png(&png)),
            other => panic!("{other:?}"),
        }
        let file = serde_json::json!({"action":"file","name":"n.txt","text":"abc"});
        match parse_control_json(&file) {
            Some(Action::File { name, data }) => {
                assert_eq!(name, "n.txt");
                assert_eq!(data, b"abc");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clipboard_png_is_kept_whole_or_rejected() {
        let mut padded = TINY_PNG.to_vec();
        padded.extend_from_slice(&[0u8; 2048]);
        let kept = Action::ClipboardPng {
            png: padded.clone(),
        }
        .clamp_coords();
        match kept {
            Action::ClipboardPng { png } => assert_eq!(png, padded),
            other => panic!("{other:?}"),
        }
        let inj = FakeInjector::new();
        inj.clipboard_set_png(&padded);
        assert_eq!(inj.clipboard_get_png().as_deref(), Some(padded.as_slice()));

        assert!(
            CLIP_PNG_MAX >= 128 * 1024 * 1024,
            "clipboard PNG must fit a 5K screenshot"
        );
        assert!(png_fits(CLIP_PNG_MAX));
        assert!(!png_fits(CLIP_PNG_MAX + 1), "must not allocate-and-truncate");
        assert!(!png_fits(0));
        match (Action::ClipboardPng {
            png: TINY_PNG.to_vec(),
        })
        .clamp_coords()
        {
            Action::ClipboardPng { png } => assert_eq!(png, TINY_PNG),
            other => panic!("{other:?}"),
        }
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
        inj.apply(&Action::ClipboardPng {
            png: TINY_PNG.to_vec(),
        });
        assert_eq!(inj.clipboard_get_png().as_deref(), Some(TINY_PNG));
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

    #[test]
    fn map_norm_to_global_offsets_secondary_display() {
        assert_eq!(
            map_norm_to_global(0.0, 0.0, 1920, 0, 1920, 1080),
            (1920.0, 0.0)
        );
        assert_eq!(
            map_norm_to_global(0.5, 0.5, 1920, 0, 1920, 1080),
            (2880.0, 540.0)
        );
    }

    #[test]
    fn ffmpeg_screen_index_uses_unfiltered_cg_list_order() {
        let cg = vec![
            DisplayInfo {
                id: "0:".into(),
                name: "main".into(),
                width: 1440,
                height: 900,
                main: true,
                ..Default::default()
            },
            DisplayInfo {
                id: "1:".into(),
                name: "ext".into(),
                x: 1440,
                width: 1920,
                height: 1080,
                ..Default::default()
            },
        ];
        assert_eq!(display_for_ffmpeg_screen(0, &cg).unwrap().main, true);
        assert_eq!(display_for_ffmpeg_screen(1, &cg).unwrap().x, 1440);
        assert_eq!(display_for_ffmpeg_screen(1, &cg).unwrap().width, 1920);

        // A zero-size slot must not compact the list: Capture screen 1 stays index 1.
        let with_hole = vec![
            DisplayInfo {
                id: "0:".into(),
                width: 0,
                height: 0,
                ..Default::default()
            },
            DisplayInfo {
                id: "1:".into(),
                name: "main".into(),
                width: 1440,
                height: 900,
                main: true,
                ..Default::default()
            },
            DisplayInfo {
                id: "2:".into(),
                name: "ext".into(),
                x: 1920,
                width: 1920,
                height: 1080,
                ..Default::default()
            },
        ];
        assert_eq!(display_for_ffmpeg_screen(0, &with_hole).unwrap().width, 0);
        assert_eq!(display_for_ffmpeg_screen(1, &with_hole).unwrap().main, true);
        assert_eq!(display_for_ffmpeg_screen(2, &with_hole).unwrap().x, 1920);
        let compacted: Vec<_> = with_hole
            .iter()
            .filter(|d| d.width > 0)
            .cloned()
            .collect();
        assert_ne!(
            display_for_ffmpeg_screen(1, &compacted).unwrap().x,
            display_for_ffmpeg_screen(1, &with_hole).unwrap().x,
            "filtering before index would send clicks to the wrong screen"
        );
    }

    #[test]
    fn pick_display_matches_id_or_main() {
        let ds = vec![
            DisplayInfo {
                id: "3:".into(),
                name: "Display 1 (main)".into(),
                width: 1440,
                height: 900,
                main: true,
                ..Default::default()
            },
            DisplayInfo {
                id: "4:".into(),
                name: "Display 2".into(),
                x: 1440,
                width: 1920,
                height: 1080,
                ..Default::default()
            },
        ];
        assert_eq!(pick_display("4:", &ds).unwrap().x, 1440);
        assert_eq!(pick_display("4", &ds).unwrap().id, "4:");
        assert_eq!(pick_display("", &ds).unwrap().main, true);
        assert_eq!(pick_display("missing", &ds).unwrap().main, true);
    }

    #[test]
    fn fake_injector_stores_display_rect() {
        let inj = FakeInjector::new();
        inj.set_display_rect(1920, 0, 1920, 1080);
        assert_eq!(inj.display_rect(), (1920, 0, 1920, 1080));
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
        m.set_display_rect(1920, 0, 1920, 1080);
        assert_eq!(m.mapping_rect(), (1920, 0, 1920, 1080));
        assert_eq!(
            map_norm_to_global(0.0, 0.0, 1920, 0, 1920, 1080),
            (1920.0, 0.0)
        );
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
