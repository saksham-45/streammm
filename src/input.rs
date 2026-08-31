//! Remote input injection. Tests use FakeInjector; macOS uses CGEvent.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Action {
    Move { x: f64, y: f64 },
    Click { x: f64, y: f64 },
    Scroll {
        x: f64,
        y: f64,
        #[serde(default)]
        dy: f64,
    },
    Type { text: String },
    Key { key: String },
    Wait {
        #[serde(default)]
        ms: u64,
    },
    Done,
}

impl Action {
    pub fn clamp_coords(self) -> Self {
        match self {
            Action::Move { x, y } => Action::Move {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
            },
            Action::Click { x, y } => Action::Click {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
            },
            Action::Scroll { x, y, dy } => Action::Scroll {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
                dy,
            },
            other => other,
        }
    }
}

pub fn parse_control_json(v: &serde_json::Value) -> Option<Action> {
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    let x = v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let y = v.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let a = match action {
        "move" => Action::Move { x, y },
        "click" => Action::Click { x, y },
        "scroll" => Action::Scroll {
            x,
            y,
            dy: v.get("dy").and_then(|x| x.as_f64()).unwrap_or(0.0),
        },
        "type" | "text" => Action::Type {
            text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").into(),
        },
        "key" | "keydown" => Action::Key {
            key: v
                .get("key")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .into(),
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

pub trait Injector: Send + Sync {
    fn apply(&self, action: &Action);
    fn set_screen_size(&self, _w: u32, _h: u32) {}
    fn screen_size(&self) -> (u32, u32) {
        (0, 0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Injected {
    Move { x: f64, y: f64 },
    Click { x: f64, y: f64 },
    Scroll { x: f64, y: f64, dy: f64 },
    Type { text: String },
    Key { key: String },
}

#[derive(Clone, Default)]
pub struct FakeInjector {
    pub events: Arc<Mutex<Vec<Injected>>>,
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
            Action::Move { x, y } => Injected::Move { x: *x, y: *y },
            Action::Click { x, y } => Injected::Click { x: *x, y: *y },
            Action::Scroll { x, y, dy } => Injected::Scroll {
                x: *x,
                y: *y,
                dy: *dy,
            },
            Action::Type { text } => Injected::Type { text: text.clone() },
            Action::Key { key } => Injected::Key { key: key.clone() },
            Action::Wait { .. } | Action::Done => return,
        };
        self.events.lock().push(ev);
    }
}

pub struct NullInjector;

impl Injector for NullInjector {
    fn apply(&self, _action: &Action) {}
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{Action, Injector};

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
    const MOVED: CGEventType = 5;
    const SCROLL: CGEventType = 22;
    const KEY_DOWN: CGEventType = 10;
    const KEY_UP: CGEventType = 11;
    const LEFT_BUTTON: CGMouseButton = 0;

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

    unsafe fn post_mouse(ty: CGEventType, pos: CGPoint, button: CGMouseButton) {
        let ev = CGEventCreateMouseEvent(std::ptr::null_mut(), ty, pos, button);
        if !ev.is_null() {
            CGEventPost(HID, ev);
            CFRelease(ev);
        }
    }

    pub struct MacInjector {
        pub width: std::sync::atomic::AtomicU32,
        pub height: std::sync::atomic::AtomicU32,
    }

    impl MacInjector {
        pub fn new() -> Self {
            let s = Self {
                width: std::sync::atomic::AtomicU32::new(0),
                height: std::sync::atomic::AtomicU32::new(0),
            };
            s.refresh_display_size();
            s
        }
        pub fn refresh_display_size(&self) {
            use std::sync::atomic::Ordering::Relaxed;
            if let Some((w, h)) = display_size() {
                self.width.store(w, Relaxed);
                self.height.store(h, Relaxed);
            }
        }
        /// CGEvent mapping size: live display bounds. Encoder/hub size is ignored.
        pub fn mapping_size(&self) -> (u32, u32) {
            if let Some(d) = display_size() {
                return d;
            }
            use std::sync::atomic::Ordering::Relaxed;
            (
                self.width.load(Relaxed).max(1),
                self.height.load(Relaxed).max(1),
            )
        }
        fn size(&self) -> (f64, f64) {
            let (w, h) = self.mapping_size();
            (w as f64, h as f64)
        }
    }

    impl Injector for MacInjector {
        fn set_screen_size(&self, _w: u32, _h: u32) {
            // Encoder/hub size (capped 1920×1080) must not replace CGDisplayBounds.
        }
        fn screen_size(&self) -> (u32, u32) {
            self.mapping_size()
        }
        fn apply(&self, action: &Action) {
            let (w, h) = self.size();
            unsafe {
                match action {
                    Action::Move { x, y } => {
                        post_mouse(MOVED, screen_px(*x, *y, w, h), LEFT_BUTTON);
                    }
                    Action::Click { x, y } => {
                        let pos = screen_px(*x, *y, w, h);
                        post_mouse(MOVED, pos, LEFT_BUTTON);
                        post_mouse(LEFT_DOWN, pos, LEFT_BUTTON);
                        post_mouse(LEFT_UP, pos, LEFT_BUTTON);
                    }
                    Action::Scroll { x, y, dy } => {
                        post_mouse(MOVED, screen_px(*x, *y, w, h), LEFT_BUTTON);
                        let ev = CGEventCreateScrollWheelEvent(
                            std::ptr::null_mut(),
                            0,
                            1,
                            (*dy * 5.0) as i32,
                        );
                        if !ev.is_null() {
                            CGEventPost(HID, ev);
                            CFRelease(ev);
                        }
                    }
                    Action::Type { text } => {
                        for ch in text.encode_utf16() {
                            let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, true);
                            if ev.is_null() {
                                continue;
                            }
                            let u = [ch];
                            CGEventKeyboardSetUnicodeString(ev, 1, u.as_ptr());
                            CGEventPost(HID, ev);
                            CFRelease(ev);
                            let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, false);
                            if !ev.is_null() {
                                CGEventKeyboardSetUnicodeString(ev, 1, u.as_ptr());
                                CGEventPost(HID, ev);
                                CFRelease(ev);
                            }
                            let _ = KEY_DOWN;
                            let _ = KEY_UP;
                            let _ = SCROLL;
                            let _ = CGEventSetIntegerValueField;
                        }
                    }
                    Action::Key { key } => {
                        let code = keycode(key);
                        let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, true);
                        if !down.is_null() {
                            CGEventPost(HID, down);
                            CFRelease(down);
                        }
                        let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, false);
                        if !up.is_null() {
                            CGEventPost(HID, up);
                            CFRelease(up);
                        }
                    }
                    Action::Wait { .. } | Action::Done => {}
                }
            }
        }
    }

    fn keycode(key: &str) -> CGKeyCode {
        match key {
            "Enter" | "Return" => 36,
            "Escape" => 53,
            "Tab" => 48,
            "Backspace" => 51,
            " " | "Space" => 49,
            "ArrowLeft" => 123,
            "ArrowRight" => 124,
            "ArrowDown" => 125,
            "ArrowUp" => 126,
            _ => 0,
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
        inj.apply(&Action::Click { x: 1.5, y: -0.2 }.clamp_coords());
        inj.apply(&Action::Type {
            text: "hello".into(),
        });
        assert_eq!(
            inj.recorded(),
            vec![
                Injected::Click { x: 1.0, y: 0.0 },
                Injected::Type {
                    text: "hello".into()
                }
            ]
        );
    }

    #[test]
    fn parse_control_json_click() {
        let v = serde_json::json!({"type":"control","action":"click","x":0.5,"y":0.25});
        assert_eq!(
            parse_control_json(&v),
            Some(Action::Click { x: 0.5, y: 0.25 })
        );
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
}
