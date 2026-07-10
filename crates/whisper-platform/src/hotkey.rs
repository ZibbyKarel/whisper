//! Global hotkey listener (`ARCHITECTURE.md` §5, D-0007/D-0012): a configurable combination in
//! push-to-talk (hold) or toggle mode, driving mic sessions. rdev is the listener backend (its
//! maintenance status is a watched risk, D-0007); Fn/globe capture is a separate work item
//! (needs `flagsChanged`/IOKit, `ARCHITECTURE.md` §8) and is intentionally not attempted here.
//!
//! Everything except the rdev event pump is pure and unit-tested: combo parsing, the
//! [`ComboTracker`] that turns key up/down into [`HotkeyEvent`]s, and the [`HotkeySession`] that
//! applies the mode. The rdev listen loop needs macOS Input Monitoring (TCC), so it lives behind
//! `#[cfg(target_os = "macos")]` and is manual-only (D-0012).

use std::collections::HashSet;

/// One transition of the configured combination. The mode ([`HotkeySession`]) turns these into
/// [`SessionIntent`]s; the raw event is deliberately mode-agnostic so the same listener serves
/// both push-to-talk and toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// The full combination just became satisfied (all keys down).
    Pressed,
    /// The combination is no longer satisfied (a key was released).
    Released,
}

/// How a [`HotkeyEvent`] stream maps to capture start/stop (`ARCHITECTURE.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HotkeyMode {
    /// Hold to talk: capture while the combo is held, stop on release.
    #[default]
    PushToTalk,
    /// Press to start, press again to stop.
    Toggle,
}

/// A mic-session intent derived from hotkey activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIntent {
    Start,
    Stop,
}

/// A parsed hotkey combination, e.g. `Ctrl+Alt+Space`. Stored as the set of normalized key
/// tokens that must all be held simultaneously.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyCombo {
    tokens: Vec<String>,
}

impl HotkeyCombo {
    /// Parse a `+`-separated combo. Modifier aliases are normalized (`cmd`/`command`/`super`/
    /// `win` → `meta`, `option`/`opt` → `alt`, `control` → `ctrl`); the main key is
    /// lower-cased. Empty / all-blank input is rejected.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut tokens = Vec::new();
        for part in spec.split('+') {
            let t = part.trim();
            if t.is_empty() {
                continue;
            }
            let norm = normalize_token(t);
            if !tokens.contains(&norm) {
                tokens.push(norm);
            }
        }
        if tokens.is_empty() {
            return Err(format!("empty hotkey combo: {spec:?}"));
        }
        Ok(Self { tokens })
    }

    /// Whether every key of the combo is currently held.
    pub fn is_satisfied(&self, down: &HashSet<String>) -> bool {
        self.tokens.iter().all(|t| down.contains(t))
    }

    /// The normalized tokens (test/inspection helper).
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }
}

impl Default for HotkeyCombo {
    /// The default combination (`ARCHITECTURE.md` §5, D-0012): `Ctrl+Alt+Space` — three keys, no
    /// clash with common single-modifier shortcuts, and reachable on any Mac keyboard (unlike
    /// Fn/globe, which is out of rdev's scope, §8).
    fn default() -> Self {
        Self::parse("Ctrl+Alt+Space").expect("valid default combo")
    }
}

fn normalize_token(t: &str) -> String {
    match t.to_ascii_lowercase().as_str() {
        "control" | "ctrl" => "ctrl".to_string(),
        "option" | "opt" | "alt" | "altgr" => "alt".to_string(),
        "command" | "cmd" | "super" | "win" | "meta" => "meta".to_string(),
        "shift" => "shift".to_string(),
        "spacebar" | "space" => "space".to_string(),
        other => other.to_string(),
    }
}

/// Tracks pressed keys and emits a [`HotkeyEvent`] each time the combo becomes satisfied or stops
/// being satisfied. Pure: fed normalized key tokens, no OS dependency.
#[derive(Debug)]
pub struct ComboTracker {
    combo: HotkeyCombo,
    down: HashSet<String>,
    active: bool,
}

impl ComboTracker {
    pub fn new(combo: HotkeyCombo) -> Self {
        Self {
            combo,
            down: HashSet::new(),
            active: false,
        }
    }

    /// Process one key transition; returns an event only on a satisfied↔unsatisfied edge.
    pub fn on_key(&mut self, token: &str, pressed: bool) -> Option<HotkeyEvent> {
        if pressed {
            self.down.insert(token.to_string());
        } else {
            self.down.remove(token);
        }
        let satisfied = self.combo.is_satisfied(&self.down);
        if satisfied && !self.active {
            self.active = true;
            Some(HotkeyEvent::Pressed)
        } else if !satisfied && self.active {
            self.active = false;
            Some(HotkeyEvent::Released)
        } else {
            None
        }
    }
}

/// Applies a [`HotkeyMode`] to a [`HotkeyEvent`] stream, yielding capture start/stop intents and
/// tracking whether a capture is currently in flight. Pure and unit-tested.
#[derive(Debug)]
pub struct HotkeySession {
    mode: HotkeyMode,
    capturing: bool,
}

impl HotkeySession {
    pub fn new(mode: HotkeyMode) -> Self {
        Self {
            mode,
            capturing: false,
        }
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing
    }

    /// Map one hotkey event to a session intent (or nothing).
    pub fn apply(&mut self, ev: HotkeyEvent) -> Option<SessionIntent> {
        match self.mode {
            HotkeyMode::PushToTalk => match ev {
                HotkeyEvent::Pressed => {
                    if self.capturing {
                        None
                    } else {
                        self.capturing = true;
                        Some(SessionIntent::Start)
                    }
                }
                HotkeyEvent::Released => {
                    if self.capturing {
                        self.capturing = false;
                        Some(SessionIntent::Stop)
                    } else {
                        None
                    }
                }
            },
            HotkeyMode::Toggle => match ev {
                HotkeyEvent::Pressed => {
                    if self.capturing {
                        self.capturing = false;
                        Some(SessionIntent::Stop)
                    } else {
                        self.capturing = true;
                        Some(SessionIntent::Start)
                    }
                }
                // A toggle ignores key-up: the combo need not be held.
                HotkeyEvent::Released => None,
            },
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::spawn_listener;

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc::Sender;

    use super::{ComboTracker, HotkeyCombo, HotkeyEvent};

    /// Spawn a background thread running the rdev listener, feeding a [`ComboTracker`] and sending
    /// each [`HotkeyEvent`] to `tx`. Blocking (`rdev::listen` never returns until error); requires
    /// macOS Input Monitoring (TCC) — manual-only (D-0012). Returns the thread handle.
    pub fn spawn_listener(
        combo: HotkeyCombo,
        tx: Sender<HotkeyEvent>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("whisper-hotkey".to_string())
            .spawn(move || {
                let mut tracker = ComboTracker::new(combo);
                let callback = move |event: rdev::Event| {
                    let (token, pressed) = match event.event_type {
                        rdev::EventType::KeyPress(k) => (token_for_rdev(&k), true),
                        rdev::EventType::KeyRelease(k) => (token_for_rdev(&k), false),
                        _ => return,
                    };
                    if let Some(token) = token {
                        if let Some(ev) = tracker.on_key(&token, pressed) {
                            let _ = tx.send(ev);
                        }
                    }
                };
                // Returns Err only if the listen loop cannot be established (e.g. missing Input
                // Monitoring grant); nothing to do but exit the thread.
                let _ = rdev::listen(callback);
            })
            .expect("spawn hotkey listener thread")
    }

    /// Map the rdev key subset we care about to normalized combo tokens. Unknown keys → `None`
    /// (ignored). Fn/globe is deliberately absent (out of rdev's scope, `ARCHITECTURE.md` §8).
    fn token_for_rdev(key: &rdev::Key) -> Option<String> {
        use rdev::Key::*;
        let s = match key {
            ControlLeft | ControlRight => "ctrl",
            Alt | AltGr => "alt",
            ShiftLeft | ShiftRight => "shift",
            MetaLeft | MetaRight => "meta",
            Space => "space",
            Return => "return",
            Escape => "escape",
            Tab => "tab",
            F1 => "f1",
            F2 => "f2",
            F3 => "f3",
            F4 => "f4",
            F5 => "f5",
            F6 => "f6",
            F7 => "f7",
            F8 => "f8",
            F9 => "f9",
            F10 => "f10",
            F11 => "f11",
            F12 => "f12",
            KeyA => "a",
            KeyB => "b",
            KeyC => "c",
            KeyD => "d",
            KeyE => "e",
            KeyF => "f",
            KeyG => "g",
            KeyH => "h",
            KeyI => "i",
            KeyJ => "j",
            KeyK => "k",
            KeyL => "l",
            KeyM => "m",
            KeyN => "n",
            KeyO => "o",
            KeyP => "p",
            KeyQ => "q",
            KeyR => "r",
            KeyS => "s",
            KeyT => "t",
            KeyU => "u",
            KeyV => "v",
            KeyW => "w",
            KeyX => "x",
            KeyY => "y",
            KeyZ => "z",
            _ => return None,
        };
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(tokens: &[&str]) -> HashSet<String> {
        tokens.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_normalizes_modifier_aliases() {
        let a = HotkeyCombo::parse("Cmd+Option+Space").unwrap();
        let b = HotkeyCombo::parse("meta+alt+space").unwrap();
        assert_eq!(a, b);
        assert!(a.tokens().contains(&"meta".to_string()));
        assert!(a.tokens().contains(&"alt".to_string()));
        assert!(a.tokens().contains(&"space".to_string()));
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(HotkeyCombo::parse("").is_err());
        assert!(HotkeyCombo::parse("  + ").is_err());
    }

    #[test]
    fn default_combo_is_ctrl_alt_space() {
        let c = HotkeyCombo::default();
        assert!(c.is_satisfied(&down(&["ctrl", "alt", "space"])));
        assert!(!c.is_satisfied(&down(&["ctrl", "space"])));
    }

    #[test]
    fn tracker_fires_on_edges_only() {
        let mut t = ComboTracker::new(HotkeyCombo::parse("Ctrl+Space").unwrap());
        assert_eq!(t.on_key("ctrl", true), None); // partial
        assert_eq!(t.on_key("space", true), Some(HotkeyEvent::Pressed)); // satisfied edge
        assert_eq!(t.on_key("space", true), None); // no re-fire while held
        assert_eq!(t.on_key("space", false), Some(HotkeyEvent::Released)); // unsatisfied edge
        assert_eq!(t.on_key("ctrl", false), None); // already released
    }

    #[test]
    fn push_to_talk_starts_on_press_stops_on_release() {
        let mut s = HotkeySession::new(HotkeyMode::PushToTalk);
        assert_eq!(s.apply(HotkeyEvent::Pressed), Some(SessionIntent::Start));
        assert!(s.is_capturing());
        assert_eq!(s.apply(HotkeyEvent::Pressed), None); // still held, no double-start
        assert_eq!(s.apply(HotkeyEvent::Released), Some(SessionIntent::Stop));
        assert!(!s.is_capturing());
        assert_eq!(s.apply(HotkeyEvent::Released), None);
    }

    #[test]
    fn toggle_alternates_and_ignores_release() {
        let mut s = HotkeySession::new(HotkeyMode::Toggle);
        assert_eq!(s.apply(HotkeyEvent::Pressed), Some(SessionIntent::Start));
        assert_eq!(s.apply(HotkeyEvent::Released), None); // release is a no-op in toggle
        assert_eq!(s.apply(HotkeyEvent::Pressed), Some(SessionIntent::Stop));
        assert_eq!(s.apply(HotkeyEvent::Pressed), Some(SessionIntent::Start));
    }
}
