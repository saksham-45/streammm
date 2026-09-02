//! Keep the host awake for remote access, and notice system wake.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Hold an idle-sleep assertion when a session is live, or when the host asked
/// to stay awake for unattended / remote control.
pub fn should_keep_awake(keep: bool, control: bool, unattended: bool, driving: bool) -> bool {
    driving || (keep && (control || unattended))
}

pub trait PowerGuard: Send + Sync {
    fn set_keep_awake(&self, _on: bool) {}
    fn is_awake(&self) -> bool {
        false
    }
    fn take_wake(&self) -> bool {
        false
    }
}

#[derive(Clone, Default)]
pub struct FakePower {
    pub keep_awake: Arc<AtomicBool>,
    pub wake: Arc<AtomicBool>,
}

impl FakePower {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PowerGuard for FakePower {
    fn set_keep_awake(&self, on: bool) {
        self.keep_awake.store(on, Ordering::SeqCst);
    }
    fn is_awake(&self) -> bool {
        self.keep_awake.load(Ordering::SeqCst)
    }
    fn take_wake(&self) -> bool {
        self.wake.swap(false, Ordering::SeqCst)
    }
}

pub struct NullPower;

impl PowerGuard for NullPower {}

pub fn production_power() -> Arc<dyn PowerGuard> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacPower::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(NullPower)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::PowerGuard;
    use parking_lot::Mutex;
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    const UTF8: u32 = 0x0800_0100;
    const ASSERT_ON: u32 = 255;
    const MSG_CAN_SLEEP: u32 = 0xE000_0270;
    const MSG_WILL_SLEEP: u32 = 0xE000_0280;
    const MSG_HAS_POWERED_ON: u32 = 0xE000_0300;

    static WAKE: AtomicBool = AtomicBool::new(false);
    static ROOT_PORT: AtomicU32 = AtomicU32::new(0);

    #[link(name = "IOKit", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            ty: *mut c_void,
            level: u32,
            name: *mut c_void,
            out_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
        fn CFStringCreateWithCString(
            alloc: *mut c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> *mut c_void;
        fn CFRelease(cf: *mut c_void);
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            port_ref: *mut *mut c_void,
            callback: extern "C" fn(*mut c_void, u32, u32, *mut c_void),
            notifier: *mut u32,
        ) -> u32;
        fn IOAllowPowerChange(kernel_port: u32, notification_id: isize) -> i32;
        fn IONotificationPortGetRunLoopSource(port: *mut c_void) -> *mut c_void;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
        fn CFRunLoopRun();
        static kCFRunLoopCommonModes: *const c_void;
    }

    extern "C" fn power_cb(
        _refcon: *mut c_void,
        _service: u32,
        message_type: u32,
        message_arg: *mut c_void,
    ) {
        match message_type {
            MSG_CAN_SLEEP | MSG_WILL_SLEEP => {
                let port = ROOT_PORT.load(Ordering::SeqCst);
                if port != 0 {
                    unsafe {
                        let _ = IOAllowPowerChange(port, message_arg as isize);
                    }
                }
            }
            MSG_HAS_POWERED_ON => {
                WAKE.store(true, Ordering::SeqCst);
            }
            _ => {}
        }
    }

    fn start_wake_watch() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            std::thread::Builder::new()
                .name("streamaid-power".into())
                .spawn(|| unsafe {
                    let mut port: *mut c_void = std::ptr::null_mut();
                    let mut notifier: u32 = 0;
                    let root = IORegisterForSystemPower(
                        std::ptr::null_mut(),
                        &mut port,
                        power_cb,
                        &mut notifier,
                    );
                    if root == 0 || port.is_null() {
                        return;
                    }
                    ROOT_PORT.store(root, Ordering::SeqCst);
                    let src = IONotificationPortGetRunLoopSource(port);
                    if src.is_null() {
                        return;
                    }
                    CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
                    CFRunLoopRun();
                })
                .ok();
        });
    }

    fn cfstr(s: &str) -> *mut c_void {
        let c = CString::new(s).unwrap_or_else(|_| CString::new("streamaid").unwrap());
        unsafe { CFStringCreateWithCString(std::ptr::null_mut(), c.as_ptr(), UTF8) }
    }

    fn create_assertion(kind: &str) -> u32 {
        let ty = cfstr(kind);
        let name = cfstr("streamaid remote access");
        if ty.is_null() || name.is_null() {
            if !ty.is_null() {
                unsafe { CFRelease(ty) };
            }
            if !name.is_null() {
                unsafe { CFRelease(name) };
            }
            return 0;
        }
        let mut id = 0u32;
        let err = unsafe { IOPMAssertionCreateWithName(ty, ASSERT_ON, name, &mut id) };
        unsafe {
            CFRelease(ty);
            CFRelease(name);
        }
        if err == 0 {
            id
        } else {
            0
        }
    }

    fn release_assertion(id: u32) {
        if id != 0 {
            unsafe {
                let _ = IOPMAssertionRelease(id);
            }
        }
    }

    pub struct MacPower {
        ids: Mutex<(u32, u32)>,
        on: AtomicBool,
    }

    impl MacPower {
        pub fn new() -> Self {
            start_wake_watch();
            Self {
                ids: Mutex::new((0, 0)),
                on: AtomicBool::new(false),
            }
        }
    }

    impl PowerGuard for MacPower {
        fn set_keep_awake(&self, on: bool) {
            let mut ids = self.ids.lock();
            if on {
                if ids.0 == 0 {
                    ids.0 = create_assertion("PreventUserIdleSystemSleep");
                }
                if ids.1 == 0 {
                    ids.1 = create_assertion("PreventUserIdleDisplaySleep");
                }
                self.on.store(true, Ordering::SeqCst);
            } else {
                release_assertion(ids.0);
                release_assertion(ids.1);
                *ids = (0, 0);
                self.on.store(false, Ordering::SeqCst);
            }
        }
        fn is_awake(&self) -> bool {
            self.on.load(Ordering::SeqCst)
        }
        fn take_wake(&self) -> bool {
            WAKE.swap(false, Ordering::SeqCst)
        }
    }

    impl Drop for MacPower {
        fn drop(&mut self) {
            self.set_keep_awake(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_awake_during_session_even_without_setting() {
        assert!(should_keep_awake(false, true, false, true));
        assert!(!should_keep_awake(false, true, false, false));
    }

    #[test]
    fn keep_awake_setting_needs_control_or_unattended() {
        assert!(!should_keep_awake(true, false, false, false));
        assert!(should_keep_awake(true, true, false, false));
        assert!(should_keep_awake(true, false, true, false));
        assert!(should_keep_awake(true, true, true, false));
    }

    #[test]
    fn fake_power_flag_and_wake() {
        let p = FakePower::new();
        assert!(!p.is_awake());
        assert!(!p.take_wake());
        p.set_keep_awake(true);
        assert!(p.is_awake());
        p.wake.store(true, Ordering::SeqCst);
        assert!(p.take_wake());
        assert!(!p.take_wake());
        p.set_keep_awake(false);
        assert!(!p.is_awake());
    }
}
