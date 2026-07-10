//! Hardware smoke tests for the macOS platform adapters (`ARCHITECTURE.md` §5, D-0012). Every
//! test here touches real CoreAudio / CGEvent / Input-Monitoring APIs, so each requires a macOS
//! TCC grant that CI cannot provide — they are `#[ignore]` and must be run by hand, one at a time,
//! after granting the relevant permission. See `docs/phase5-manual-verification.md` for the full
//! checklist.
//!
//! Run one with, e.g.:
//!   cargo test -p whisper-platform --test hardware -- --ignored mic_capture_opens_and_streams

#![cfg(target_os = "macos")]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use whisper_core::ports::{AudioSource, TextSink};
use whisper_platform::{secure_input_enabled, CpalAudioSource};

/// Grant: System Settings → Privacy & Security → Microphone → (your terminal / the test binary).
/// Expect: at least one non-empty 16 kHz mono frame within a couple of seconds, then a graceful
/// end after `stop` is set.
#[test]
#[ignore = "needs Microphone permission (TCC); run manually with --ignored"]
fn mic_capture_opens_and_streams() {
    let stop = Arc::new(AtomicBool::new(false));
    let source = CpalAudioSource::open(
        stop.clone(),
        Box::new(|reason| eprintln!("device lost: {reason}")),
    )
    .expect("open default input device (grant Microphone permission)");
    let mut stream = source.start(Default::default()).expect("start capture");

    let mut got_audio = false;
    for _ in 0..200 {
        match stream.next_frame() {
            Ok(Some(frame)) if !frame.is_empty() => {
                got_audio = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => panic!("capture error: {e}"),
        }
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    // Drain to end-of-stream.
    while let Ok(Some(_)) = stream.next_frame() {}
    assert!(
        got_audio,
        "no audio frames captured — is the mic muted / permission granted?"
    );
}

/// Grant: Accessibility (for synthesized Cmd-V). Steps: focus a text field first, run this, then
/// check the field received "žluťoučký kůň" with diacritics intact. The previous clipboard is
/// restored after ~300 ms.
#[test]
#[ignore = "synthesizes Cmd-V into the focused app; needs Accessibility permission; run manually"]
fn pasteboard_sink_injects_czech() {
    let sink = whisper_platform::macos_pasteboard_sink().expect("open clipboard");
    let outcome = sink.deliver("žluťoučký kůň").expect("deliver");
    eprintln!("delivery outcome: {outcome:?}");
    // With Secure Input off this is `Injected`; in a password field it is `ClipboardOnly`.
}

/// No permission needed — a pure query. Focus a normal field → expect `false`; focus a macOS
/// password field (e.g. the login/lock screen or a Keychain prompt) → expect `true`.
#[test]
#[ignore = "observe the value while toggling focus between a normal and a secure field"]
fn secure_input_query_runs() {
    let enabled = secure_input_enabled();
    eprintln!("IsSecureEventInputEnabled = {enabled}");
}

/// Grant: Input Monitoring. Press the configured combo (default Ctrl+Alt+Space) and watch the
/// printed events. Ctrl-C to end. This blocks (the rdev listener never returns on its own).
#[test]
#[ignore = "needs Input Monitoring permission; blocks on the rdev listener; run manually"]
fn hotkey_listener_reports_events() {
    let (tx, rx) = std::sync::mpsc::channel();
    let combo = whisper_platform::HotkeyCombo::parse("Ctrl+Alt+Space").unwrap();
    let _listener = whisper_platform::spawn_listener(combo, tx);
    eprintln!("press Ctrl+Alt+Space a few times (grant Input Monitoring)…");
    for _ in 0..4 {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(ev) => eprintln!("hotkey event: {ev:?}"),
            Err(_) => panic!("no hotkey events within 30s — is Input Monitoring granted?"),
        }
    }
}
