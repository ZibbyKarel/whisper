//! Pasteboard text insertion (`ARCHITECTURE.md` §5, D-0007): save clipboard → set text (NFC) →
//! synthesize Cmd-V → restore after a bounded delay; NFC read-back protects Czech diacritics;
//! Secure Input is detected and never bypassed (clipboard-only fallback).
//!
//! The delivery logic is generic over three seams — [`Clipboard`], [`KeySynth`], [`SecureInput`]
//! — so it is fully unit-tested with mocks (no hardware, no synthesized keys, no TCC). The macOS
//! implementations (arboard clipboard, CGEvent Cmd-V, `IsSecureEventInputEnabled`) live behind
//! `#[cfg(target_os = "macos")]` and are manual-only (D-0012).

use std::time::Duration;

use unicode_normalization::UnicodeNormalization;
use whisper_core::error::DeliverError;
use whisper_core::ports::{DeliverOutcome, TextSink};

/// Bounded delay before restoring the previous clipboard after a synthesized paste (D-0007).
///
/// macOS exposes no reliable paste-completion signal: `NSPasteboard` does not notify when a
/// target app has read the pasteboard, so we cannot restore "on completion". We wait a fixed
/// window long enough for the focused app to service the synthesized Cmd-V, then restore. Too
/// short risks restoring before the app reads our text; 300 ms is the convergent choice of
/// comparable tools and is imperceptible for a dictation flow.
pub const DEFAULT_RESTORE_DELAY: Duration = Duration::from_millis(300);

/// Clipboard access seam (save/set/read-back/restore).
pub trait Clipboard {
    /// Current clipboard text, or `None` if empty / non-text.
    fn get_text(&self) -> Option<String>;
    /// Replace the clipboard text.
    fn set_text(&self, text: &str) -> Result<(), DeliverError>;
}

/// Key synthesis seam: perform a paste (Cmd-V) into the focused app.
pub trait KeySynth {
    fn paste(&self) -> Result<(), DeliverError>;
}

/// Secure Input detection seam (`ARCHITECTURE.md` §5, D-0007).
pub trait SecureInput {
    /// Whether Secure Input is currently enabled system-wide (`IsSecureEventInputEnabled`).
    fn is_enabled(&self) -> bool;
    /// The app holding Secure Input, when it can be determined. Default: unknown.
    fn owner(&self) -> Option<String> {
        None
    }
}

/// Pasteboard [`TextSink`] parameterized over the three OS seams (D-0007). Delivery:
/// 1. set the (NFC) text on the clipboard, correcting an NFD read-back;
/// 2. if Secure Input is active, stop here — never synthesize keys — and report
///    [`DeliverOutcome::ClipboardOnly`] (the text stays on the clipboard to paste manually);
/// 3. otherwise synthesize Cmd-V, wait [`DEFAULT_RESTORE_DELAY`], and restore the prior clipboard.
pub struct PasteboardSink<C, K, S> {
    clipboard: C,
    keysynth: K,
    secure: S,
    restore_delay: Duration,
}

impl<C, K, S> PasteboardSink<C, K, S>
where
    C: Clipboard,
    K: KeySynth,
    S: SecureInput,
{
    /// Construct from explicit parts (used by tests with mocks and by the macOS factory). The
    /// delay is a parameter so tests can set it to zero.
    pub fn new(clipboard: C, keysynth: K, secure: S, restore_delay: Duration) -> Self {
        Self {
            clipboard,
            keysynth,
            secure,
            restore_delay,
        }
    }
}

impl<C, K, S> TextSink for PasteboardSink<C, K, S>
where
    C: Clipboard,
    K: KeySynth,
    S: SecureInput,
{
    fn deliver(&self, text: &str) -> Result<DeliverOutcome, DeliverError> {
        // NFC before it ever touches the pasteboard (`ARCHITECTURE.md` §3.3/§5).
        let text_nfc: String = text.nfc().collect();

        // Save the user's current clipboard to restore after paste — NFC-normalized, so a
        // decomposed (NFD) read-back is never written back on restore (D-0007).
        let saved: Option<String> = self.clipboard.get_text().map(|s| s.nfc().collect());

        self.clipboard.set_text(&text_nfc)?;

        // NFC read-back guard (D-0007): macOS can hand back a decomposed (NFD) form; normalize it
        // and, if it differs, put the composed form back so what the app pastes — and what a
        // manual paste yields — is NFC.
        if let Some(back) = self.clipboard.get_text() {
            let back_nfc: String = back.nfc().collect();
            if back_nfc != back {
                self.clipboard.set_text(&back_nfc)?;
            }
        }

        if self.secure.is_enabled() {
            // Secure Input active: never synthesize keys; leave our text on the clipboard for a
            // manual paste and surface it upward (`ARCHITECTURE.md` §5: detect, never bypass).
            // The saved clipboard is intentionally NOT restored — our text must remain available.
            return Ok(DeliverOutcome::ClipboardOnly {
                owner: self.secure.owner(),
            });
        }

        self.keysynth.paste()?;

        // No paste-completion signal exists (see DEFAULT_RESTORE_DELAY); wait then restore.
        std::thread::sleep(self.restore_delay);
        if let Some(prev) = saved {
            let _ = self.clipboard.set_text(&prev);
        }
        Ok(DeliverOutcome::Injected)
    }
}

#[cfg(target_os = "macos")]
pub use mac::{macos_pasteboard_sink, secure_input_enabled};

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::time::Duration;

    use whisper_core::error::DeliverError;

    use super::{Clipboard, KeySynth, PasteboardSink, SecureInput};

    /// Build the production macOS pasteboard sink (arboard + CGEvent + `IsSecureEventInputEnabled`)
    /// with [`super::DEFAULT_RESTORE_DELAY`]. Fails if the clipboard cannot be opened.
    pub fn macos_pasteboard_sink(
    ) -> Result<PasteboardSink<MacClipboard, MacKeySynth, MacSecureInput>, DeliverError> {
        Ok(PasteboardSink::new(
            MacClipboard::new()?,
            MacKeySynth,
            MacSecureInput,
            super::DEFAULT_RESTORE_DELAY,
        ))
    }

    /// arboard-backed clipboard. Held on the worker thread (arboard is `!Send`); interior
    /// mutability because the trait takes `&self` while arboard needs `&mut`.
    pub struct MacClipboard {
        inner: RefCell<arboard::Clipboard>,
    }

    impl MacClipboard {
        pub fn new() -> Result<Self, DeliverError> {
            let cb = arboard::Clipboard::new()
                .map_err(|e| DeliverError::Failed(format!("open clipboard: {e}")))?;
            Ok(Self {
                inner: RefCell::new(cb),
            })
        }
    }

    impl Clipboard for MacClipboard {
        fn get_text(&self) -> Option<String> {
            self.inner.borrow_mut().get_text().ok()
        }
        fn set_text(&self, text: &str) -> Result<(), DeliverError> {
            self.inner
                .borrow_mut()
                .set_text(text.to_string())
                .map_err(|e| DeliverError::Failed(format!("set clipboard: {e}")))
        }
    }

    /// Synthesizes Cmd-V via CGEvent (D-0007). `keycode 9` is `v` on the ANSI layout; the paste
    /// operates on the clipboard, so the physical key's letter is irrelevant to what is inserted.
    pub struct MacKeySynth;

    impl KeySynth for MacKeySynth {
        fn paste(&self) -> Result<(), DeliverError> {
            use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
            use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

            const V_KEYCODE: core_graphics::event::CGKeyCode = 9;

            let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .map_err(|_| DeliverError::Failed("create CGEventSource".into()))?;
            let key_down = CGEvent::new_keyboard_event(source.clone(), V_KEYCODE, true)
                .map_err(|_| DeliverError::Failed("create key-down event".into()))?;
            key_down.set_flags(CGEventFlags::CGEventFlagCommand);
            key_down.post(CGEventTapLocation::HID);

            let key_up = CGEvent::new_keyboard_event(source, V_KEYCODE, false)
                .map_err(|_| DeliverError::Failed("create key-up event".into()))?;
            key_up.set_flags(CGEventFlags::CGEventFlagCommand);
            key_up.post(CGEventTapLocation::HID);
            Ok(())
        }
    }

    /// Secure Input detection via the Carbon `IsSecureEventInputEnabled` (D-0007). The owning app
    /// would require the private `kCGSSessionSecureInputPID`; we deliberately avoid private APIs,
    /// so `owner` stays `None` (documented, `ARCHITECTURE.md` §5).
    pub struct MacSecureInput;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        // Returns a Carbon `Boolean` (u8); nonzero ⇒ Secure Input is enabled.
        fn IsSecureEventInputEnabled() -> u8;
    }

    impl SecureInput for MacSecureInput {
        fn is_enabled(&self) -> bool {
            // SAFETY: parameterless Carbon query with no side effects; safe to call any time.
            unsafe { IsSecureEventInputEnabled() != 0 }
        }
    }

    /// Whether Secure Input is currently enabled system-wide (`IsSecureEventInputEnabled`,
    /// D-0007). A pure query — no side effects, no prompt.
    pub fn secure_input_enabled() -> bool {
        MacSecureInput.is_enabled()
    }

    // A no-op to keep `Duration` import used when the trait paths above change during refactors.
    #[allow(dead_code)]
    const _RESTORE_DELAY_KIND: Duration = super::DEFAULT_RESTORE_DELAY;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A mock clipboard storing exactly what it is given.
    #[derive(Default)]
    struct MockClipboard {
        text: RefCell<Option<String>>,
        sets: RefCell<usize>,
    }
    impl Clipboard for MockClipboard {
        fn get_text(&self) -> Option<String> {
            self.text.borrow().clone()
        }
        fn set_text(&self, text: &str) -> Result<(), DeliverError> {
            *self.sets.borrow_mut() += 1;
            *self.text.borrow_mut() = Some(text.to_string());
            Ok(())
        }
    }

    /// A mock clipboard modelling macOS's read-side quirk: it stores text faithfully but hands
    /// back a decomposed (NFD) form on `get_text`. `raw` exposes the faithfully-stored bytes.
    struct DecomposingClipboard {
        text: RefCell<Option<String>>,
    }
    impl DecomposingClipboard {
        fn raw(&self) -> Option<String> {
            self.text.borrow().clone()
        }
    }
    impl Clipboard for DecomposingClipboard {
        fn get_text(&self) -> Option<String> {
            self.text.borrow().clone().map(|s| s.nfd().collect())
        }
        fn set_text(&self, text: &str) -> Result<(), DeliverError> {
            *self.text.borrow_mut() = Some(text.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockKeySynth {
        pastes: RefCell<usize>,
    }
    impl KeySynth for MockKeySynth {
        fn paste(&self) -> Result<(), DeliverError> {
            *self.pastes.borrow_mut() += 1;
            Ok(())
        }
    }

    struct FixedSecureInput {
        enabled: bool,
        owner: Option<String>,
    }
    impl SecureInput for FixedSecureInput {
        fn is_enabled(&self) -> bool {
            self.enabled
        }
        fn owner(&self) -> Option<String> {
            self.owner.clone()
        }
    }

    const CZECH: &str = "Příliš žluťoučký kůň úpěl ďábelské ódy.";

    #[test]
    fn normal_delivery_pastes_and_restores_previous_clipboard() {
        let clip = MockClipboard::default();
        clip.set_text("original user clipboard").unwrap();
        let keys = MockKeySynth::default();
        let sink = PasteboardSink::new(
            clip,
            keys,
            FixedSecureInput {
                enabled: false,
                owner: None,
            },
            Duration::from_millis(0),
        );

        let outcome = sink.deliver(CZECH).unwrap();
        assert_eq!(outcome, DeliverOutcome::Injected);
        assert_eq!(*sink.keysynth.pastes.borrow(), 1, "Cmd-V synthesized once");
        // Previous clipboard restored after paste.
        assert_eq!(
            sink.clipboard.get_text().as_deref(),
            Some("original user clipboard")
        );
    }

    #[test]
    fn secure_input_leaves_text_on_clipboard_and_never_pastes() {
        let clip = MockClipboard::default();
        clip.set_text("original").unwrap();
        let keys = MockKeySynth::default();
        let sink = PasteboardSink::new(
            clip,
            keys,
            FixedSecureInput {
                enabled: true,
                owner: Some("Terminal".to_string()),
            },
            Duration::from_millis(0),
        );

        let outcome = sink.deliver(CZECH).unwrap();
        assert_eq!(
            outcome,
            DeliverOutcome::ClipboardOnly {
                owner: Some("Terminal".to_string())
            }
        );
        assert_eq!(
            *sink.keysynth.pastes.borrow(),
            0,
            "must never bypass Secure Input"
        );
        // Our text (NFC) is left on the clipboard for a manual paste, byte-exact.
        assert_eq!(sink.clipboard.get_text().as_deref(), Some(CZECH));
    }

    #[test]
    fn nfd_read_back_is_corrected_to_nfc_on_clipboard() {
        // The clipboard decomposes on store; the sink's read-back guard must restore NFC.
        let clip = DecomposingClipboard {
            text: RefCell::new(None),
        };
        let sink = PasteboardSink::new(
            clip,
            MockKeySynth::default(),
            FixedSecureInput {
                enabled: true, // stop after the clipboard write so we inspect its final state
                owner: None,
            },
            Duration::from_millis(0),
        );
        let _ = sink.deliver(CZECH).unwrap();
        // The faithfully-stored bytes are composed (NFC), byte-exact — the read-back guard
        // corrected the NFD the clipboard reported.
        let on_clipboard = sink.clipboard.raw().unwrap();
        assert_eq!(on_clipboard, CZECH);
        assert_eq!(on_clipboard.as_bytes(), CZECH.as_bytes());
    }

    #[test]
    fn empty_previous_clipboard_is_not_restored() {
        let clip = MockClipboard::default(); // starts empty
        let sink = PasteboardSink::new(
            clip,
            MockKeySynth::default(),
            FixedSecureInput {
                enabled: false,
                owner: None,
            },
            Duration::from_millis(0),
        );
        let outcome = sink.deliver("ahoj").unwrap();
        assert_eq!(outcome, DeliverOutcome::Injected);
        // With no prior clipboard, our text remains after the (no-op) restore.
        assert_eq!(sink.clipboard.get_text().as_deref(), Some("ahoj"));
    }
}
