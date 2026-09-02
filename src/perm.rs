//! macOS TCC: Screen Recording and Accessibility. Tests never open System Settings.

pub fn privacy_pane_url(which: &str) -> Option<&'static str> {
    match which.trim().to_ascii_lowercase().as_str() {
        "screen" | "screencapture" | "capture" => Some(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        ),
        "accessibility" | "ax" => Some(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        ),
        "input" | "listen" | "hid" | "monitoring" => Some(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
        ),
        _ => None,
    }
}

pub fn screen_ok() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::screen_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

pub fn accessibility_ok() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::accessibility_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

pub fn input_ok() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::input_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

pub fn open_privacy_pane(which: &str) -> Result<(), &'static str> {
    let url = privacy_pane_url(which).ok_or("unknown pane")?;
    #[cfg(target_os = "macos")]
    {
        macos::open_url(url)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err("macos only")
    }
}

#[cfg(target_os = "macos")]
mod macos {
    #[link(name = "CoreGraphics", kind = "framework")]
    #[link(name = "ApplicationServices", kind = "framework")]
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn AXIsProcessTrusted() -> bool;
        fn IOHIDCheckAccess(request_type: u32) -> u32;
    }

    const HID_LISTEN: u32 = 1;
    const HID_GRANTED: u32 = 0;

    pub fn screen_ok() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn accessibility_ok() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    pub fn input_ok() -> bool {
        unsafe { IOHIDCheckAccess(HID_LISTEN) == HID_GRANTED }
    }

    pub fn open_url(url: &str) -> Result<(), &'static str> {
        let st = std::process::Command::new("open")
            .arg(url)
            .status()
            .map_err(|_| "open failed")?;
        if st.success() {
            Ok(())
        } else {
            Err("open failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_urls_for_screen_and_accessibility() {
        assert!(privacy_pane_url("screen")
            .unwrap()
            .contains("Privacy_ScreenCapture"));
        assert!(privacy_pane_url("CAPTURE")
            .unwrap()
            .contains("Privacy_ScreenCapture"));
        assert!(privacy_pane_url("ax")
            .unwrap()
            .contains("Privacy_Accessibility"));
        assert!(privacy_pane_url("input")
            .unwrap()
            .contains("Privacy_ListenEvent"));
        assert!(privacy_pane_url("hid")
            .unwrap()
            .contains("Privacy_ListenEvent"));
        assert!(privacy_pane_url("nope").is_none());
        assert!(privacy_pane_url("").is_none());
    }

    #[test]
    fn unknown_pane_does_not_open() {
        assert_eq!(open_privacy_pane("nope"), Err("unknown pane"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_mac_permissions_are_ok() {
        assert!(screen_ok());
        assert!(accessibility_ok());
        assert!(input_ok());
        assert_eq!(open_privacy_pane("screen"), Err("macos only"));
    }
}
