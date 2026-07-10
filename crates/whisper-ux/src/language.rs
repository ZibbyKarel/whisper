//! Dictation language presets (Task 1, user feedback: "I should be able to set
//! the dictation language, for now just in Settings"). The daemon's `language`
//! config field (`Config::language` in [`crate::api`]) is a free-form ISO code
//! string; this module maps it to a small set of UI presets without ever
//! clobbering a value the daemon returns that isn't one of them — an unknown
//! code (or any future language the daemon adds) round-trips byte-exact
//! through [`LanguagePreset::Other`].
//!
//! Kept `egui`-free so the mapping is unit-tested directly.

/// A dictation language preset shown in the Dictation section's picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguagePreset {
    /// Czech (`"cs"`) — the project's first-class language (constitution).
    Czech,
    English,
    /// Let whisper.cpp auto-detect the spoken language per segment.
    Auto,
    /// Any other ISO code: shows a free-text field instead of clobbering it.
    Other,
}

/// The presets offered as one-click buttons, in display order. [`LanguagePreset::Other`]
/// is handled separately (it reveals a text field rather than setting a fixed code).
pub const PRESETS: [LanguagePreset; 3] = [
    LanguagePreset::Czech,
    LanguagePreset::English,
    LanguagePreset::Auto,
];

impl LanguagePreset {
    /// Human-readable label for the picker button.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            LanguagePreset::Czech => "Čeština (cs)",
            LanguagePreset::English => "English (en)",
            LanguagePreset::Auto => "Auto-detect",
            LanguagePreset::Other => "Other…",
        }
    }

    /// The `Config::language` value this preset writes, or `None` for
    /// [`LanguagePreset::Other`] (its value is whatever the user typed/the
    /// daemon returned — never overwritten by clicking the affordance).
    #[must_use]
    pub fn code(self) -> Option<&'static str> {
        match self {
            LanguagePreset::Czech => Some("cs"),
            LanguagePreset::English => Some("en"),
            LanguagePreset::Auto => Some("auto"),
            LanguagePreset::Other => None,
        }
    }
}

/// Map a `Config::language` value to the preset it belongs to. Any value other
/// than the three known codes maps to [`LanguagePreset::Other`] — this is the
/// "unknown code stays intact" contract: the caller must keep displaying/
/// sending `value` unchanged rather than snapping it to a preset's code.
#[must_use]
pub fn preset_for(value: &str) -> LanguagePreset {
    match value {
        "cs" => LanguagePreset::Czech,
        "en" => LanguagePreset::English,
        "auto" => LanguagePreset::Auto,
        _ => LanguagePreset::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_map_to_their_preset() {
        assert_eq!(preset_for("cs"), LanguagePreset::Czech);
        assert_eq!(preset_for("en"), LanguagePreset::English);
        assert_eq!(preset_for("auto"), LanguagePreset::Auto);
    }

    #[test]
    fn unknown_code_stays_intact_as_other() {
        // A daemon can support (or a user can type) any ISO code; it must never
        // be clobbered to one of the three presets.
        for code in ["de", "sk", "pl", "", "cs-CZ", "CS"] {
            assert_eq!(preset_for(code), LanguagePreset::Other, "code: {code}");
        }
    }

    #[test]
    fn preset_codes_round_trip_through_preset_for() {
        for preset in PRESETS {
            let code = preset.code().expect("known presets have a fixed code");
            assert_eq!(preset_for(code), preset);
        }
    }

    #[test]
    fn other_has_no_fixed_code() {
        assert_eq!(LanguagePreset::Other.code(), None);
    }

    #[test]
    fn labels_are_stable_and_include_czech_diacritics() {
        assert_eq!(LanguagePreset::Czech.label(), "Čeština (cs)");
        assert_eq!(LanguagePreset::English.label(), "English (en)");
        assert_eq!(LanguagePreset::Auto.label(), "Auto-detect");
        assert_eq!(LanguagePreset::Other.label(), "Other…");
    }
}
