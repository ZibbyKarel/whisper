//! Pure representation of a `PUT /v1/config` response (D-0013, feedback item 2:
//! "show which fields were applied live vs staged"). Kept as data + formatting
//! functions, separate from the `egui` rendering, so the "restart to apply"
//! notice logic is unit-tested without a window.

/// Which fields a `PUT /v1/config` took effect on immediately vs. staged for
/// the next daemon restart. Held in the shell's state across frames so the
/// staged notice stays visible until the next save (a `GET`/reload does not
/// clear it — only a fresh `PUT /v1/config` response does).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveReport {
    pub applied: Vec<String>,
    pub staged: Vec<String>,
}

impl SaveReport {
    /// Build from the `applied`/`staged` arrays of a `ConfigUpdateResponse`.
    #[must_use]
    pub fn new(applied: Vec<String>, staged: Vec<String>) -> Self {
        Self { applied, staged }
    }

    /// True while any field is staged and therefore needs a daemon restart.
    #[must_use]
    pub fn has_staged(&self) -> bool {
        !self.staged.is_empty()
    }

    /// One-line summary of fields that took effect immediately, or `None` if
    /// nothing changed live.
    #[must_use]
    pub fn applied_summary(&self) -> Option<String> {
        if self.applied.is_empty() {
            None
        } else {
            Some(format!("Applied: {}", self.applied.join(", ")))
        }
    }

    /// The persistent notice shown while [`Self::has_staged`] is true, or
    /// `None` once there is nothing pending a restart.
    #[must_use]
    pub fn staged_notice(&self) -> Option<String> {
        if self.staged.is_empty() {
            None
        } else {
            Some(format!(
                "Restart whisperd to apply: {}",
                self.staged.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_has_no_notices() {
        let r = SaveReport::default();
        assert!(!r.has_staged());
        assert_eq!(r.applied_summary(), None);
        assert_eq!(r.staged_notice(), None);
    }

    #[test]
    fn applied_only_has_no_staged_notice() {
        let r = SaveReport::new(vec!["language".to_string()], vec![]);
        assert!(!r.has_staged());
        assert_eq!(r.applied_summary(), Some("Applied: language".to_string()));
        assert_eq!(r.staged_notice(), None);
    }

    #[test]
    fn staged_fields_produce_a_persistent_notice() {
        let r = SaveReport::new(vec![], vec!["hotkey".to_string(), "port".to_string()]);
        assert!(r.has_staged());
        assert_eq!(
            r.staged_notice(),
            Some("Restart whisperd to apply: hotkey, port".to_string())
        );
    }

    #[test]
    fn mixed_applied_and_staged_both_report() {
        let r = SaveReport::new(vec!["vad".to_string()], vec!["audio".to_string()]);
        assert_eq!(r.applied_summary(), Some("Applied: vad".to_string()));
        assert_eq!(
            r.staged_notice(),
            Some("Restart whisperd to apply: audio".to_string())
        );
    }
}
