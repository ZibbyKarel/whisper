//! Client-side mirror of the daemon's `PUT /v1/config` range validation
//! (`validate_config` in `whisper-iface/src/daemon/handlers.rs`), so the
//! Settings window can flag a bad value inline instead of round-tripping to a
//! `400` (feedback item 2: "everything from config must be settable... with
//! appropriate widgets"). Kept free of `egui` so it is unit-tested directly.
//!
//! Ranges are numeric only (no user-authored text), so there is nothing
//! Czech-specific to verify here; the Czech/UTF-8 contract is exercised by
//! [`crate::dict_editor`] and [`crate::api`] instead.

use crate::api::Config;

/// One out-of-range field, mirroring a daemon `400 Bad Request` reason.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfigError {
    /// `vad.threshold` must be in `[0.05, 0.95]`.
    VadThresholdRange(f32),
    /// `vad.max_silence_ms` must be in `[50, 10_000]`.
    VadMaxSilenceRange(u32),
    /// `vad.hangover_ms` must be `<= vad.max_silence_ms`.
    VadHangoverExceedsMaxSilence {
        hangover_ms: u32,
        max_silence_ms: u32,
    },
    /// `audio.agc_target_peak` must be in `(0.0, 1.0]`, checked as `[0.1, 1.0]`
    /// to match the daemon's inclusive range.
    AudioTargetPeakRange(f32),
    /// `audio.agc_max_gain` must be in `[1.0, 64.0]`.
    AudioMaxGainRange(f32),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::VadThresholdRange(v) => {
                write!(f, "vad.threshold {v} out of range [0.05, 0.95]")
            }
            ConfigError::VadMaxSilenceRange(v) => {
                write!(f, "vad.max_silence_ms {v} out of range [50, 10000]")
            }
            ConfigError::VadHangoverExceedsMaxSilence {
                hangover_ms,
                max_silence_ms,
            } => write!(
                f,
                "vad.hangover_ms {hangover_ms} must be <= vad.max_silence_ms {max_silence_ms}"
            ),
            ConfigError::AudioTargetPeakRange(v) => {
                write!(f, "audio.agc_target_peak {v} out of range [0.1, 1.0]")
            }
            ConfigError::AudioMaxGainRange(v) => {
                write!(f, "audio.agc_max_gain {v} out of range [1.0, 64.0]")
            }
        }
    }
}

/// Validate the tunable ranges of `cfg`, mirroring the daemon exactly
/// (`[0.05, 0.95]`, `[50, 10_000]`, `hangover_ms <= max_silence_ms`,
/// `[0.1, 1.0]`, `[1.0, 64.0]`). Returns every violation so the UI can flag
/// them all at once, same pattern as [`crate::dict_editor::DictEditor::validate`].
#[must_use]
pub fn validate(cfg: &Config) -> Vec<ConfigError> {
    let mut errors = Vec::new();

    let v = &cfg.vad;
    if !(0.05..=0.95).contains(&v.threshold) {
        errors.push(ConfigError::VadThresholdRange(v.threshold));
    }
    if !(50..=10_000).contains(&v.max_silence_ms) {
        errors.push(ConfigError::VadMaxSilenceRange(v.max_silence_ms));
    }
    if v.hangover_ms > v.max_silence_ms {
        errors.push(ConfigError::VadHangoverExceedsMaxSilence {
            hangover_ms: v.hangover_ms,
            max_silence_ms: v.max_silence_ms,
        });
    }

    let a = &cfg.audio;
    if !(0.1..=1.0).contains(&a.agc_target_peak) {
        errors.push(ConfigError::AudioTargetPeakRange(a.agc_target_peak));
    }
    if !(1.0..=64.0).contains(&a.agc_max_gain) {
        errors.push(ConfigError::AudioMaxGainRange(a.agc_max_gain));
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(validate(&Config::default()).is_empty());
    }

    #[test]
    fn vad_threshold_out_of_range_is_flagged() {
        let mut cfg = Config::default();
        cfg.vad.threshold = 0.0;
        assert_eq!(validate(&cfg), vec![ConfigError::VadThresholdRange(0.0)]);

        cfg.vad.threshold = 1.0;
        assert_eq!(validate(&cfg), vec![ConfigError::VadThresholdRange(1.0)]);

        cfg.vad.threshold = 0.05;
        assert!(validate(&cfg).is_empty());
        cfg.vad.threshold = 0.95;
        assert!(validate(&cfg).is_empty());
    }

    #[test]
    fn vad_max_silence_out_of_range_is_flagged() {
        let mut cfg = Config::default();
        cfg.vad.max_silence_ms = 10;
        cfg.vad.hangover_ms = 5; // keep the hangover<=max_silence rule satisfied
        assert_eq!(validate(&cfg), vec![ConfigError::VadMaxSilenceRange(10)]);

        cfg.vad.max_silence_ms = 20_000;
        assert_eq!(
            validate(&cfg),
            vec![ConfigError::VadMaxSilenceRange(20_000)]
        );
    }

    #[test]
    fn hangover_exceeding_max_silence_is_flagged() {
        let mut cfg = Config::default();
        cfg.vad.hangover_ms = 2_000;
        cfg.vad.max_silence_ms = 1_000;
        assert_eq!(
            validate(&cfg),
            vec![ConfigError::VadHangoverExceedsMaxSilence {
                hangover_ms: 2_000,
                max_silence_ms: 1_000,
            }]
        );
    }

    #[test]
    fn audio_target_peak_out_of_range_is_flagged() {
        let mut cfg = Config::default();
        cfg.audio.agc_target_peak = 0.0;
        assert_eq!(validate(&cfg), vec![ConfigError::AudioTargetPeakRange(0.0)]);

        cfg.audio.agc_target_peak = 1.5;
        assert_eq!(validate(&cfg), vec![ConfigError::AudioTargetPeakRange(1.5)]);
    }

    #[test]
    fn audio_max_gain_out_of_range_is_flagged() {
        let mut cfg = Config::default();
        cfg.audio.agc_max_gain = 0.5;
        assert_eq!(validate(&cfg), vec![ConfigError::AudioMaxGainRange(0.5)]);

        cfg.audio.agc_max_gain = 100.0;
        assert_eq!(validate(&cfg), vec![ConfigError::AudioMaxGainRange(100.0)]);
    }

    #[test]
    fn multiple_violations_are_all_reported() {
        let mut cfg = Config::default();
        cfg.vad.threshold = 2.0;
        cfg.audio.agc_max_gain = 0.0;
        let errs = validate(&cfg);
        assert_eq!(errs.len(), 2);
    }

    #[test]
    fn display_messages_are_human_readable() {
        let err = ConfigError::VadThresholdRange(1.2);
        assert_eq!(
            err.to_string(),
            "vad.threshold 1.2 out of range [0.05, 0.95]"
        );
    }
}
