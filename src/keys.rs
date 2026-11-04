//! Holds a real keyboard key down (for push-to-talk dictation apps such as Wispr Flow,
//! whose default is holding Fn). Posting keys needs macOS Accessibility permission.

use anyhow::{Result, bail};
use std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(source: *const c_void, keycode: u16, keydown: bool) -> *mut c_void;
    fn CGEventSetType(event: *mut c_void, kind: u32);
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFDictionaryCreate(allocator: *const c_void, keys: *const *const c_void, values: *const *const c_void, count: isize, key_cb: *const c_void, value_cb: *const c_void) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

const HID_EVENT_TAP: u32 = 0;
const FLAGS_CHANGED: u32 = 12;
const FLAG_FN: u64 = 0x0080_0000;

/// The keys a dictation app can sensibly use as a hold-to-talk shortcut.
fn keycode(name: &str) -> Option<u16> {
    Some(match name {
        "fn" => 63,
        "f13" => 105,
        "f14" => 107,
        "f15" => 113,
        "f16" => 106,
        "f17" => 64,
        "f18" => 79,
        "f19" => 80,
        "f20" => 90,
        _ => return None,
    })
}

pub fn valid(name: &str) -> bool {
    keycode(name).is_some()
}

pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Asks macOS for Accessibility: shows its dialog and puts herdr-pad in the list in
/// System Settings, so the person only has to flip the switch. Returns whether it's granted.
pub fn request() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let opts = CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), values.as_ptr(), 1, &raw const kCFTypeDictionaryKeyCallBacks, &raw const kCFTypeDictionaryValueCallBacks);
        let ok = AXIsProcessTrustedWithOptions(opts);
        if !opts.is_null() {
            CFRelease(opts);
        }
        ok
    }
}

/// Presses (`down`) or releases a key system-wide.
pub fn hold(name: &str, down: bool) -> Result<()> {
    let Some(code) = keycode(name) else { bail!("unknown key \"{name}\"") };
    if !trusted() {
        bail!("needs Accessibility permission (System Settings → Privacy & Security → Accessibility → herdr-pad)");
    }
    unsafe {
        let ev = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
        if ev.is_null() {
            bail!("couldn't make the key event");
        }
        if name == "fn" {
            // Fn is a modifier: apps see it as a flags change, not a key press.
            CGEventSetType(ev, FLAGS_CHANGED);
            CGEventSetFlags(ev, if down { FLAG_FN } else { 0 });
        }
        CGEventPost(HID_EVENT_TAP, ev);
        CFRelease(ev);
    }
    Ok(())
}
