//! Cleanup skip-gate heuristic (`ARCHITECTURE.md` §3.2 "cleanup gate", D-0005): a
//! deterministic, dependency-free heuristic deciding whether a transcript shows signs of
//! disfluency/self-correction and therefore needs the LLM cleanup stage, or is already
//! clean enough to deliver as-is. FreeFlow evidence (§3.4): ~40% of dictations are clean,
//! and skipping the LLM there saves ~1.6 s of latency.
//!
//! This module only classifies text; it never modifies it — that is the whole point of a
//! *gate*. The actual cleanup (if any) still happens in the [`crate::ports::CleanupEngine`]
//! the pipeline calls when the gate says yes.
//!
//! Matching is whole-word/whole-phrase and UTF-8/diacritics-safe, deliberately mirroring
//! `dictionary.rs`'s boundary logic (`ARCHITECTURE.md` §3.3): a marker must be flanked by
//! non-word characters (or string edges), so e.g. the filler "prostě" does not match inside
//! "prostěradlo", and "um" does not match inside "umyji".

/// Configurable marker lists + thresholds behind the skip-gate decision. All matching is
/// case-insensitive (via Unicode-aware `to_lowercase`, which folds Czech diacritics
/// correctly) and whole-word/whole-phrase.
///
/// The default lists are a first-pass, Czech-first marker set (`ARCHITECTURE.md` §3.2
/// notes the marker set is tuned on real recordings as a later deliverable); callers that
/// gather more data can build a custom `SkipGateConfig` without touching the pipeline.
#[derive(Debug, Clone)]
pub struct SkipGateConfig {
    /// Multi- or single-word self-correction markers, e.g. "ne vlastně", "pardon" — a
    /// speaker restarting or retracting what they just said.
    pub self_correction_markers: Vec<String>,
    /// Single filler/hesitation words, e.g. "ehm", "hmm".
    pub filler_words: Vec<String>,
    /// Word-count threshold above which a transcript with **no sentence punctuation at
    /// all** is treated as needing cleanup — a long unbroken run-on is exactly the case the
    /// LLM's punctuation/formatting pass is for. Transcripts that do contain punctuation
    /// (e.g. from a formatting pass upstream, or dictated punctuation) never trigger this
    /// signal, however long they are.
    pub long_unpunctuated_word_threshold: usize,
}

impl Default for SkipGateConfig {
    fn default() -> Self {
        Self {
            // Self-correction: a speaker catching and revising themselves mid-dictation.
            self_correction_markers: [
                "ne vlastně",
                "vlastně ne",
                "teda ne",
                "pardon",
                "oprava",
                "škrtni to",
                "ne počkej",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            // Hesitation/filler: includes "prostě" (a very common Czech discourse filler,
            // roughly "like"/"just"), deliberately chosen as the canonical false-positive
            // trap against "prostěradlo" (bedsheet) — whole-word matching must reject it.
            filler_words: ["ehm", "hmm", "eee", "éé", "um", "uh", "prostě"]
                .into_iter()
                .map(String::from)
                .collect(),
            long_unpunctuated_word_threshold: 30,
        }
    }
}

impl SkipGateConfig {
    /// Decide whether `raw` needs LLM cleanup under this configuration. Never modifies
    /// `raw`.
    pub fn needs_cleanup(&self, raw: &str) -> bool {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // Nothing to clean; also avoids the empty-segment corner case in
            // `is_long_and_unpunctuated`/`contains_whole_phrase`.
            return false;
        }
        // One lowercase pass covers both marker lists; `to_lowercase` is Unicode-aware, so
        // Czech diacritics (e.g. "Ně" -> "ně") fold correctly.
        let lower = trimmed.to_lowercase();

        let has_self_correction = self
            .self_correction_markers
            .iter()
            .any(|marker| contains_whole_phrase(&lower, &marker.to_lowercase()));
        if has_self_correction {
            return true;
        }

        let has_filler = self
            .filler_words
            .iter()
            .any(|word| contains_whole_phrase(&lower, &word.to_lowercase()));
        if has_filler {
            return true;
        }

        if has_immediate_word_repetition(&lower) {
            return true;
        }

        is_long_and_unpunctuated(trimmed, self.long_unpunctuated_word_threshold)
    }
}

/// Decide, using the default marker configuration, whether `raw` needs LLM cleanup.
/// Convenience wrapper around [`SkipGateConfig::default`] + [`SkipGateConfig::needs_cleanup`];
/// see that method for the decision logic.
pub fn needs_cleanup(raw: &str) -> bool {
    SkipGateConfig::default().needs_cleanup(raw)
}

/// Matches `dictionary::is_word_char`: a run of these characters is "one word" for
/// boundary purposes. `char::is_alphanumeric` covers Czech diacritics (á, č, ě, ř, ů, ž…).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True if `phrase` occurs in `haystack` at a whole-word/whole-phrase boundary: the
/// character immediately before/after the match, if any, is not a word character. This is
/// a containment check (no replacement), otherwise identical in spirit to
/// `dictionary::apply_whole_word`'s boundary logic, and just as UTF-8-safe: all slicing
/// happens on `str::find` match boundaries and `char_indices` steps, which are always valid
/// char boundaries.
fn contains_whole_phrase(haystack: &str, phrase: &str) -> bool {
    if phrase.is_empty() {
        return false;
    }

    let mut search_start = 0usize;
    while let Some(rel_idx) = haystack[search_start..].find(phrase) {
        let match_start = search_start + rel_idx;
        let match_end = match_start + phrase.len();

        let before_ok = haystack[..match_start]
            .chars()
            .next_back()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
        let after_ok = haystack[match_end..]
            .chars()
            .next()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);

        if before_ok && after_ok {
            return true;
        }

        // Not a whole-word match: resume searching right after the first char of this
        // occurrence (avoids re-matching the same spot forever), same recovery as
        // `dictionary::apply_whole_word`.
        let mut chars = haystack[match_start..].char_indices();
        let (_, first_char) = chars.next().expect("match is non-empty");
        search_start = match_start + first_char.len_utf8();
    }

    false
}

/// True if two adjacent words are byte-for-byte identical (`lower` is already
/// lowercased) — e.g. "to to", "se se" — a strong signal of a stumble/restart. Words are
/// runs of `is_word_char` characters, so attached punctuation ("ne, ne") doesn't defeat the
/// match, and splitting on `char` boundaries keeps this UTF-8 safe.
fn has_immediate_word_repetition(lower: &str) -> bool {
    let words: Vec<&str> = lower
        .split(|c: char| !is_word_char(c))
        .filter(|w| !w.is_empty())
        .collect();
    words.windows(2).any(|pair| pair[0] == pair[1])
}

/// True if `text` contains no sentence punctuation at all and exceeds `threshold` words —
/// an unbroken run-on that the cleanup pass's punctuation/formatting step is meant to fix.
fn is_long_and_unpunctuated(text: &str, threshold: usize) -> bool {
    let has_punctuation = text
        .chars()
        .any(|c| matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | '…'));
    if has_punctuation {
        return false;
    }
    text.split_whitespace().count() > threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- clean dictations: no cleanup needed ----

    #[test]
    fn clean_czech_sentence_is_skipped() {
        let text = "Příliš žluťoučký kůň úpěl ďábelské ódy.";
        assert!(!needs_cleanup(text));
    }

    #[test]
    fn clean_mixed_czech_english_sentence_is_skipped() {
        let text =
            "Pushni ten branch do repa a udělej pull request, ať to můžeme mergnout do mainu.";
        assert!(!needs_cleanup(text));
    }

    #[test]
    fn empty_and_whitespace_only_are_skipped() {
        assert!(!needs_cleanup(""));
        assert!(!needs_cleanup("   \n\t "));
    }

    // ---- disfluent dictations: cleanup needed ----

    #[test]
    fn self_correction_marker_ne_vlastne_triggers_cleanup() {
        let text = "Pošli mi ten soubor zítra, ne vlastně dneska večer.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn self_correction_marker_pardon_triggers_cleanup() {
        let text = "Otevři soubor config.toml, pardon, config.json.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn self_correction_marker_skrtni_to_triggers_cleanup() {
        let text = "Napiš mi email, škrtni to, radši mi zavolej.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn filler_word_ehm_triggers_cleanup() {
        let text = "Chci ehm otevřít ten dokument.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn filler_word_hmm_triggers_cleanup() {
        let text = "Hmm, tak to zkusíme jinak.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn immediate_word_repetition_to_to_triggers_cleanup() {
        let text = "Chci to to nastavit jinak.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn immediate_word_repetition_se_se_triggers_cleanup() {
        let text = "Musíme se se na to podívat.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn immediate_word_repetition_is_case_insensitive() {
        let text = "To To musíme udělat.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn long_unpunctuated_transcript_triggers_cleanup() {
        // Five distinct words repeated 8x (40 words total, threshold is 30) with no
        // immediately-repeated word at the repeat boundary either — isolates the
        // length/no-punctuation signal from the word-repetition signal.
        let phrase = ["auto", "jede", "rychle", "po", "silnici"];
        let words: Vec<&str> = phrase.iter().cycle().take(40).copied().collect();
        let text = words.join(" ");
        assert!(needs_cleanup(&text));
    }

    #[test]
    fn short_unpunctuated_transcript_is_skipped() {
        let phrase = ["auto", "jede", "rychle", "po", "silnici"];
        let words: Vec<&str> = phrase.iter().cycle().take(10).copied().collect();
        let text = words.join(" ");
        assert!(!needs_cleanup(&text));
    }

    #[test]
    fn long_transcript_with_punctuation_is_skipped() {
        // Same length as the run-on case above, but with periods — already segmented, not
        // a run-on the cleanup pass needs to fix.
        let sentence = "Toto je krátká věta s tečkou. ";
        let text = sentence.repeat(10);
        assert!(!needs_cleanup(&text));
    }

    // ---- false positives: substrings inside unrelated words must not match ----

    #[test]
    fn prostyeradlo_does_not_match_filler_prostye() {
        let text = "Polož to prostěradlo na postel.";
        assert!(!needs_cleanup(text));
    }

    #[test]
    fn umyji_does_not_match_filler_um() {
        let text = "Umyji nádobí a pak jdu spát.";
        assert!(!needs_cleanup(text));
    }

    #[test]
    fn uhel_does_not_match_filler_uh() {
        let text = "Nakresli mi trojúhelník s pravým úhlem.";
        assert!(!needs_cleanup(text));
    }

    #[test]
    fn oprava_as_word_prefix_does_not_falsely_match_marker_at_boundary() {
        // "oprava" itself is a marker; make sure a longer word containing it as a prefix
        // ("opravář" = repairman) does NOT match, while the standalone word still does.
        let compound = "Zavolal opravář kvůli pračce.";
        assert!(!needs_cleanup(compound));
        let standalone = "Zavolej mi zítra, oprava, dneska večer.";
        assert!(needs_cleanup(standalone));
    }

    // ---- UTF-8 / diacritics boundary safety ----

    #[test]
    fn marker_at_string_start_and_end_matches() {
        assert!(needs_cleanup("pardon"));
        assert!(needs_cleanup("Chtěl jsem říct oprava"));
    }

    #[test]
    fn multi_word_phrase_marker_matches_across_word_boundaries() {
        let text = "Chci to poslat teda ne až zítra.";
        assert!(needs_cleanup(text));
    }

    #[test]
    fn custom_config_can_disable_default_markers() {
        let config = SkipGateConfig {
            self_correction_markers: vec![],
            filler_words: vec![],
            long_unpunctuated_word_threshold: 1000,
        };
        // Would trigger the default filler-word signal, but the custom config has none.
        let text = "Ehm, tak to zkusíme.";
        assert!(!config.needs_cleanup(text));
    }

    #[test]
    fn custom_config_can_add_project_specific_markers() {
        let config = SkipGateConfig {
            self_correction_markers: vec!["moje chyba".to_string()],
            ..SkipGateConfig::default()
        };
        assert!(config.needs_cleanup("Otevři to, moje chyba, zavři to."));
    }
}
