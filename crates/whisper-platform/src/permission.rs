//! macOS TCC permission preflight (`ARCHITECTURE.md` §5, D-0016): query — and, for an explicit
//! user action, request — the two grants the hotkey/paste path needs, so a caller can detect a
//! missing grant *proactively* instead of relying on a downstream API returning an error.
//!
//! This closes a real silent-failure gap (D-0016): after an ad-hoc-signed reinstall, macOS can
//! drop the Input Monitoring grant for the new bundle identity, but rdev's `CGEventTapCreate`
//! still succeeds (it just then delivers zero key events) — `rdev::listen` returns `Ok`, not
//! `Err`, so nothing downstream ever finds out. Checking the grant with `IOHIDCheckAccess`
//! *before* starting the listener finds this case directly.
//!
//! Every FFI call here is either a parameterless query (no side effects, safe to call any time)
//! or one of the two `request_*` functions, which are the documented "may show the OS grant
//! dialog" entry points and must only be called from a deliberate user action, never in a poll
//! loop. Non-macOS builds get a stub returning [`Access::Unknown`]/`false` so the crate compiles
//! everywhere and the pure logic (the `Access` mapping, callers built against it) is
//! unit-testable without a macOS host.

/// Grant status of a macOS privacy-sensitive capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    Unknown,
}

#[cfg(target_os = "macos")]
pub use mac::{
    accessibility_access, input_monitoring_access, request_accessibility, request_input_monitoring,
};

#[cfg(target_os = "macos")]
mod mac {
    use super::Access;

    // -- Input Monitoring (IOKit `IOHIDCheckAccess`/`IOHIDRequestAccess`) -------------------

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOHIDCheckAccess(request_type: u32) -> u32;
        fn IOHIDRequestAccess(request_type: u32) -> u8;
    }

    /// `kIOHIDRequestTypeListenEvent` — the request type covering a global input tap (rdev's
    /// listen-only `CGEventTapCreate`).
    const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
    /// `kIOHIDAccessTypeGranted`.
    const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;
    /// `kIOHIDAccessTypeDenied`.
    const K_IOHID_ACCESS_TYPE_DENIED: u32 = 1;

    /// Current Input Monitoring grant for this process (`IOHIDCheckAccess`). A pure query — no
    /// side effects, no prompt; safe to poll.
    #[must_use]
    pub fn input_monitoring_access() -> Access {
        // SAFETY: `IOHIDCheckAccess` is a parameterless (beyond the request-type enum) IOKit
        // query with no side effects; safe to call from any thread at any time.
        match unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) } {
            K_IOHID_ACCESS_TYPE_GRANTED => Access::Granted,
            K_IOHID_ACCESS_TYPE_DENIED => Access::Denied,
            // `kIOHIDAccessTypeUnknown` (2) and any future/undocumented value both mean "we
            // cannot say" — treat identically rather than assume either grant state.
            _ => Access::Unknown,
        }
    }

    /// Explicitly request Input Monitoring access (`IOHIDRequestAccess`) — this is the
    /// documented entry point that can show the OS grant dialog on first call. Call only from a
    /// deliberate user action (e.g. a button press), never from a poll loop.
    pub fn request_input_monitoring() -> bool {
        // SAFETY: same parameterless-beyond-the-enum shape as `IOHIDCheckAccess`; the only
        // documented side effect is the OS prompt, which is exactly what this entry point is
        // for.
        unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) != 0 }
    }

    // -- Accessibility (ApplicationServices `AXIsProcessTrusted*`) ---------------------------

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(
            options: core_foundation::dictionary::CFDictionaryRef,
        ) -> u8;
        /// The `CFStringRef` key ApplicationServices exports for the "may prompt" option dict.
        static kAXTrustedCheckOptionPrompt: core_foundation::string::CFStringRef;
    }

    /// Current Accessibility ("trusted") grant for this process (`AXIsProcessTrusted`). A pure
    /// query — no side effects, no prompt. `AX*` only distinguishes trusted/untrusted, so this
    /// never returns [`Access::Unknown`].
    #[must_use]
    pub fn accessibility_access() -> Access {
        // SAFETY: parameterless ApplicationServices query, no side effects, no prompt.
        if unsafe { AXIsProcessTrusted() != 0 } {
            Access::Granted
        } else {
            Access::Denied
        }
    }

    /// Explicitly request Accessibility access (`AXIsProcessTrustedWithOptions` with
    /// `kAXTrustedCheckOptionPrompt` set) — can show the OS grant dialog. Call only from a
    /// deliberate user action, never from a poll loop.
    pub fn request_accessibility() -> bool {
        use core_foundation::base::TCFType;
        use core_foundation::boolean::CFBoolean;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::string::CFString;

        // SAFETY: `kAXTrustedCheckOptionPrompt` is a process-lifetime CFStringRef constant
        // exported by ApplicationServices; wrapping it under the "get" rule (we don't own an
        // extra retain) is the documented way to consume a CF constant from Rust.
        let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
        let value = CFBoolean::true_value();
        let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
        // SAFETY: `options` is a valid CFDictionaryRef for the duration of this synchronous
        // call. This is the explicit "may prompt" variant — the OS grant dialog appearing is
        // the documented, intended side effect of calling it.
        unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0 }
    }
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn input_monitoring_access() -> Access {
    Access::Unknown
}

#[cfg(not(target_os = "macos"))]
pub fn request_input_monitoring() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn accessibility_access() -> Access {
    Access::Unknown
}

#[cfg(not(target_os = "macos"))]
pub fn request_accessibility() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_variants_are_distinct() {
        assert_ne!(Access::Granted, Access::Denied);
        assert_ne!(Access::Granted, Access::Unknown);
        assert_ne!(Access::Denied, Access::Unknown);
    }

    #[test]
    fn access_is_copy_and_debuggable() {
        let a = Access::Granted;
        let b = a; // Copy, not a move.
        assert_eq!(a, b);
        assert_eq!(format!("{a:?}"), "Granted");
        assert_eq!(format!("{:?}", Access::Denied), "Denied");
        assert_eq!(format!("{:?}", Access::Unknown), "Unknown");
    }

    // The real grant state needs a macOS TCC prompt/host and cannot be asserted in CI — the
    // hardware test in `tests/hardware.rs` prints the live values for a manual pass instead
    // (mirrors the existing `#[ignore]` hardware-test pattern, D-0012).
}
