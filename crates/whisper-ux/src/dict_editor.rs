//! Dictionary editor model (D-0013 §3). Holds the editable table of substitution
//! pairs the settings window renders, with client-side validation mirroring what
//! the daemon enforces on `PUT /v1/dictionary` (D-0010). Kept UI-free so the
//! add/remove/validate logic is unit-tested directly.

use crate::api::{DictionaryFile, DictionaryPair};

/// A validation problem with the current table, for display next to the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Row `index` has an empty `from` (an empty match is a no-op / rejected).
    EmptyFrom { index: usize },
    /// Two rows share the same `from`; the later one would never fire.
    DuplicateFrom { index: usize, from: String },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::EmptyFrom { index } => {
                write!(f, "row {}: 'from' must not be empty", index + 1)
            }
            ValidationError::DuplicateFrom { index, from } => {
                write!(f, "row {}: duplicate 'from' \"{from}\"", index + 1)
            }
        }
    }
}

/// The editable dictionary. Order is preserved (substitutions apply in order,
/// D-0010).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DictEditor {
    pub pairs: Vec<DictionaryPair>,
}

impl DictEditor {
    /// Build from a `GET /v1/dictionary` body.
    #[must_use]
    pub fn from_file(file: DictionaryFile) -> Self {
        Self { pairs: file.pairs }
    }

    /// Parse from a raw `GET /v1/dictionary` JSON value.
    pub fn from_json_value(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        let file: DictionaryFile = serde_json::from_value(value.clone())?;
        Ok(Self::from_file(file))
    }

    /// Append a blank row for the user to fill in.
    pub fn add_blank(&mut self) {
        self.pairs.push(DictionaryPair {
            from: String::new(),
            to: String::new(),
        });
    }

    /// Remove the row at `index` if it exists.
    pub fn remove(&mut self, index: usize) {
        if index < self.pairs.len() {
            self.pairs.remove(index);
        }
    }

    /// Validate the whole table. Returns every problem so the UI can flag all bad
    /// rows at once. `from` values are compared as-is (Czech diacritics are
    /// significant — no case folding, no normalization).
    #[must_use]
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for (index, pair) in self.pairs.iter().enumerate() {
            if pair.from.trim().is_empty() {
                errors.push(ValidationError::EmptyFrom { index });
                continue;
            }
            if seen.contains(&pair.from.as_str()) {
                errors.push(ValidationError::DuplicateFrom {
                    index,
                    from: pair.from.clone(),
                });
            } else {
                seen.push(&pair.from);
            }
        }
        errors
    }

    /// Serialize to the `PUT /v1/dictionary` body `{"pairs":[...]}`. Blank/empty
    /// `from` rows are dropped so a half-typed row does not reach the daemon.
    #[must_use]
    pub fn to_file(&self) -> DictionaryFile {
        DictionaryFile {
            pairs: self
                .pairs
                .iter()
                .filter(|p| !p.from.trim().is_empty())
                .cloned()
                .collect(),
        }
    }

    /// Convenience: the JSON value to send on `PUT /v1/dictionary`.
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self.to_file()).unwrap_or_else(|_| serde_json::json!({ "pairs": [] }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(from: &str, to: &str) -> DictionaryPair {
        DictionaryPair {
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    #[test]
    fn parses_get_response_with_czech_pairs() {
        let v = serde_json::json!({
            "pairs": [
                {"from": "merknout", "to": "mergnout"},
                {"from": "packa json", "to": "package.json"},
            ]
        });
        let ed = DictEditor::from_json_value(&v).unwrap();
        assert_eq!(ed.pairs.len(), 2);
        assert_eq!(ed.pairs[0], pair("merknout", "mergnout"));
    }

    #[test]
    fn round_trips_czech_diacritics_byte_exact() {
        let ed = DictEditor {
            pairs: vec![pair("žluťoučký", "žluťoučký kůň")],
        };
        let v = ed.to_json_value();
        let back = DictEditor::from_json_value(&v).unwrap();
        assert_eq!(ed.pairs, back.pairs);
        assert_eq!(back.pairs[0].to, "žluťoučký kůň");
    }

    #[test]
    fn empty_from_is_flagged() {
        let ed = DictEditor {
            pairs: vec![pair("", "x"), pair("ok", "y")],
        };
        let errs = ed.validate();
        assert_eq!(errs, vec![ValidationError::EmptyFrom { index: 0 }]);
    }

    #[test]
    fn whitespace_only_from_is_flagged() {
        let ed = DictEditor {
            pairs: vec![pair("   ", "x")],
        };
        assert_eq!(ed.validate(), vec![ValidationError::EmptyFrom { index: 0 }]);
    }

    #[test]
    fn duplicate_from_is_flagged() {
        let ed = DictEditor {
            pairs: vec![pair("a", "1"), pair("a", "2")],
        };
        assert_eq!(
            ed.validate(),
            vec![ValidationError::DuplicateFrom {
                index: 1,
                from: "a".to_string()
            }]
        );
    }

    #[test]
    fn to_file_drops_blank_rows() {
        let ed = DictEditor {
            pairs: vec![pair("a", "1"), pair("", ""), pair("b", "2")],
        };
        let file = ed.to_file();
        assert_eq!(file.pairs.len(), 2);
        assert_eq!(file.pairs[0].from, "a");
        assert_eq!(file.pairs[1].from, "b");
    }

    #[test]
    fn add_and_remove() {
        let mut ed = DictEditor::default();
        ed.add_blank();
        assert_eq!(ed.pairs.len(), 1);
        ed.pairs[0] = pair("x", "y");
        ed.add_blank();
        ed.remove(0);
        assert_eq!(ed.pairs.len(), 1);
        assert_eq!(ed.pairs[0].from, "");
        ed.remove(99); // out of range is a no-op
        assert_eq!(ed.pairs.len(), 1);
    }
}
