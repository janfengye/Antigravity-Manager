//! Linux Display Guard: Intercepts fatal GDK Wayland display disconnections and
//! transitions the application to headless background mode rather than crashing
//! the core port 8045 proxy service.

use std::sync::atomic::{AtomicBool, Ordering};

/// Global flag set when GDK reports a broken display connection (e.g. Broken pipe on Wayland).
static WAYLAND_DISPLAY_DISCONNECTED: AtomicBool = AtomicBool::new(false);

/// Check if Wayland / GDK display connection was lost
pub fn is_display_disconnected() -> bool {
    WAYLAND_DISPLAY_DISCONNECTED.load(Ordering::SeqCst)
}

/// Set display disconnected state
pub fn set_display_disconnected(val: bool) {
    WAYLAND_DISPLAY_DISCONNECTED.store(val, Ordering::SeqCst);
}

/// Matcher for GDK / Wayland display disconnection messages.
/// Pure function testable on any platform without GTK dependencies.
pub fn is_display_disconnect_message(domain: &str, msg: &str) -> bool {
    let lower = msg.to_lowercase();
    let is_gdk = domain.is_empty()
        || domain.eq_ignore_ascii_case("gdk")
        || domain.eq_ignore_ascii_case("gtk");

    if !is_gdk && !lower.contains("wayland") && !lower.contains("display") {
        return false;
    }

    lower.contains("error reading events from display")
        || lower.contains("lost connection to wayland")
        || lower.contains("error 32 (broken pipe) dispatching to wayland display")
        || lower.contains("error flushing display")
        || lower.contains("lost its connection to the display")
        || (lower.contains("display")
            && (lower.contains("broken pipe")
                || lower.contains("断开的管道")
                || lower.contains("connection reset by peer")
                || lower.contains("连接被对方重置")
                || lower.contains("protocol error")))
}

#[cfg(target_os = "linux")]
type GLogLevelFlags = i32;
#[cfg(target_os = "linux")]
type GLogFunc = Option<
    unsafe extern "C" fn(
        log_domain: *const std::os::raw::c_char,
        log_level: GLogLevelFlags,
        message: *const std::os::raw::c_char,
        user_data: *mut std::os::raw::c_void,
    ),
>;

#[cfg(target_os = "linux")]
static PREVIOUS_GLOG_HANDLER: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(target_os = "linux")]
extern "C" {
    fn g_log_set_default_handler(
        log_func: GLogFunc,
        user_data: *mut std::os::raw::c_void,
    ) -> GLogFunc;
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn custom_gdk_log_handler(
    log_domain: *const std::os::raw::c_char,
    log_level: GLogLevelFlags,
    message: *const std::os::raw::c_char,
    user_data: *mut std::os::raw::c_void,
) {
    if !message.is_null() {
        let msg_cstr = std::ffi::CStr::from_ptr(message);
        let msg_str = msg_cstr.to_string_lossy();
        let domain_str = if !log_domain.is_null() {
            std::ffi::CStr::from_ptr(log_domain).to_string_lossy()
        } else {
            std::borrow::Cow::Borrowed("")
        };

        if is_display_disconnect_message(&domain_str, &msg_str) {
            eprintln!(
                "\x1b[33m[Antigravity Manager]\x1b[0m Caught GDK Wayland display disconnect: {}",
                msg_str
            );
            set_display_disconnected(true);
        }
    }

    let prev_ptr = PREVIOUS_GLOG_HANDLER.load(Ordering::SeqCst);
    if !prev_ptr.is_null() {
        let prev_fn: unsafe extern "C" fn(
            *const std::os::raw::c_char,
            GLogLevelFlags,
            *const std::os::raw::c_char,
            *mut std::os::raw::c_void,
        ) = std::mem::transmute(prev_ptr);
        prev_fn(log_domain, log_level, message, user_data);
    } else {
        libc::printf(b"%s\n\0".as_ptr() as *const _, message);
    }
}

/// Initialize the Linux GDK display disconnection guard.
#[cfg(target_os = "linux")]
pub fn init_display_guard() {
    unsafe {
        let old = g_log_set_default_handler(Some(custom_gdk_log_handler), std::ptr::null_mut());
        if let Some(old_fn) = old {
            PREVIOUS_GLOG_HANDLER.store(old_fn as *mut (), Ordering::SeqCst);
        }
    }
    tracing::info!("[Linux Display Guard] Wayland display disconnect guard initialized.");
}

#[cfg(not(target_os = "linux"))]
pub fn init_display_guard() {}

/// Intercept `_exit` on Linux so display loss does not kill Tokio runtime and port 8045.
#[cfg(target_os = "linux")]
#[no_mangle]
pub unsafe extern "C" fn _exit(status: std::os::raw::c_int) -> ! {
    if is_display_disconnected() {
        eprintln!(
            "\x1b[32m[Antigravity Manager]\x1b[0m Wayland display disconnected. \
             Main GUI thread terminating; port 8045 proxy service remains ACTIVE in headless fallback."
        );
        libc::pthread_exit(std::ptr::null_mut());
    }
    libc::syscall(libc::SYS_exit_group, status);
    loop {}
}

/// Intercept `_Exit` on Linux as well.
#[cfg(target_os = "linux")]
#[no_mangle]
pub unsafe extern "C" fn _Exit(status: std::os::raw::c_int) -> ! {
    _exit(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_display_disconnect_message_recognition() {
        assert!(is_display_disconnect_message(
            "Gdk",
            "Error reading events from display: Broken pipe"
        ));
        assert!(is_display_disconnect_message(
            "Gdk",
            "Error reading events from display: 断开的管道"
        ));
        assert!(is_display_disconnect_message(
            "Gdk",
            "Error reading events from display: 连接被对方重置"
        ));
        assert!(is_display_disconnect_message(
            "",
            "Lost connection to Wayland compositor"
        ));
        assert!(is_display_disconnect_message(
            "Gtk",
            "Error 32 (Broken pipe) dispatching to Wayland display"
        ));
        assert!(is_display_disconnect_message(
            "Gdk",
            "The application 'antigravity-tools' lost its connection to the display :0"
        ));

        // Normal logs must NOT trigger display disconnect
        assert!(!is_display_disconnect_message("Gdk", "Normal gdk warning"));
        assert!(!is_display_disconnect_message(
            "App",
            "Proxy request failed: connection reset"
        ));
        assert!(!is_display_disconnect_message(
            "",
            "Some unrelated log message"
        ));
    }

    #[test]
    fn test_display_disconnect_flag() {
        set_display_disconnected(false);
        assert!(!is_display_disconnected());
        set_display_disconnected(true);
        assert!(is_display_disconnected());
        set_display_disconnected(false);
    }
}
