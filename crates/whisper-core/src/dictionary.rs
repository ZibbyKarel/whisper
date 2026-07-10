//! Dictionary pass (`ARCHITECTURE.md` §3.2): deterministic, ordered, whole-word literal
//! substitutions loaded from JSON (`{"pairs": [{"from": "...", "to": "..."}]}`) — e.g.
//! `"merknout" -> "mergnout"`, `"packa json" -> "package.json"`. Runs before the cleanup
//! gate and is always on (cheap string ops, no model).
//!
//! v1 matching is exact (case-sensitive) and UTF-8 safe: all slicing happens on
//! `str::find` match boundaries, which are guaranteed valid UTF-8 char boundaries, so
//! Czech diacritics are never split mid-codepoint.

use serde::Deserialize;

use crate::error::DictionaryError;

/// One ordered substitution rule.
#[derive(Debug, Clone, Deserialize)]
pub struct DictionaryPair {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DictionaryFile {
    pairs: Vec<DictionaryPair>,
}

/// An ordered set of whole-word literal substitution rules.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    pairs: Vec<DictionaryPair>,
}

impl Dictionary {
    /// A dictionary with no rules — `apply` is then the identity function.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse `{"pairs": [{"from": ..., "to": ...}]}`.
    pub fn from_json_str(json: &str) -> Result<Self, DictionaryError> {
        let file: DictionaryFile = serde_json::from_str(json)?;
        Ok(Self { pairs: file.pairs })
    }

    /// Apply every rule in order, each pass operating on the previous pass's output.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for pair in &self.pairs {
            out = apply_whole_word(&out, &pair.from, &pair.to);
        }
        out
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Replace every whole-word (or whole-phrase) occurrence of `from` in `text` with `to`.
/// "Whole word" means the character immediately before/after the match, if any, is not a
/// word character — this lets multi-word phrases like `"packa json"` match as a unit while
/// rejecting partial matches inside a longer word (e.g. `"merknout"` must not match inside
/// `"domerknoutovat"`).
fn apply_whole_word(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len());
    let mut search_start = 0usize;

    while let Some(rel_idx) = text[search_start..].find(from) {
        let match_start = search_start + rel_idx;
        let match_end = match_start + from.len();

        let before_ok = text[..match_start]
            .chars()
            .next_back()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
        let after_ok = text[match_end..]
            .chars()
            .next()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);

        if before_ok && after_ok {
            result.push_str(&text[search_start..match_start]);
            result.push_str(to);
            search_start = match_end;
        } else {
            // Not a whole-word match: keep the first char of this occurrence verbatim and
            // resume searching right after it (avoids re-matching the same spot forever).
            result.push_str(&text[search_start..match_start]);
            let mut chars = text[match_start..].char_indices();
            let (_, first_char) = chars.next().expect("match is non-empty");
            result.push(first_char);
            search_start = match_start + first_char.len_utf8();
        }
    }

    result.push_str(&text[search_start..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dictionary_is_identity() {
        let dict = Dictionary::empty();
        let text = "Příliš žluťoučký kůň úpěl ďábelské ódy.";
        assert_eq!(dict.apply(text), text);
    }

    #[test]
    fn single_word_whole_word_substitution() {
        let dict =
            Dictionary::from_json_str(r#"{"pairs": [{"from": "merknout", "to": "mergnout"}]}"#)
                .unwrap();
        let text = "ať to můžeme merknout do mainu";
        assert_eq!(dict.apply(text), "ať to můžeme mergnout do mainu");
    }

    #[test]
    fn does_not_substitute_inside_a_longer_word() {
        let dict =
            Dictionary::from_json_str(r#"{"pairs": [{"from": "merknout", "to": "mergnout"}]}"#)
                .unwrap();
        let text = "domerknoutovat by se to nemělo";
        assert_eq!(dict.apply(text), text);
    }

    #[test]
    fn multi_word_phrase_substitution() {
        let dict = Dictionary::from_json_str(
            r#"{"pairs": [{"from": "packa json", "to": "package.json"}]}"#,
        )
        .unwrap();
        let text = "pak zkontroluj packa json.";
        assert_eq!(dict.apply(text), "pak zkontroluj package.json.");
    }

    #[test]
    fn rules_apply_in_order() {
        let dict = Dictionary::from_json_str(
            r#"{"pairs": [
                {"from": "a", "to": "b"},
                {"from": "b", "to": "c"}
            ]}"#,
        )
        .unwrap();
        // "a" -> "b" first, then that "b" is itself substituted by the second rule.
        assert_eq!(dict.apply("a"), "c");
    }

    #[test]
    fn invalid_json_is_a_typed_error() {
        let err = Dictionary::from_json_str("not json").unwrap_err();
        assert!(matches!(err, DictionaryError::Parse(_)));
    }

    #[test]
    fn unrelated_rule_leaves_czech_diacritics_untouched() {
        let dict =
            Dictionary::from_json_str(r#"{"pairs": [{"from": "merknout", "to": "mergnout"}]}"#)
                .unwrap();
        let text = "Příliš žluťoučký kůň úpěl ďábelské ódy.";
        assert_eq!(dict.apply(text), text);
    }
}
