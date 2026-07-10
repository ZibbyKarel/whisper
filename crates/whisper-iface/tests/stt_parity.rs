//! Czech STT parity: `WhisperCppEngine` (whisper-rs) vs the `whisper-cli` reference binary,
//! for every synthetic fixture in `fixtures/audio/manifest.json` (D-0006 consequence: the
//! bindings choice must be proven equivalent to `whisper-cli` on real Czech audio).
//!
//! Marked `#[ignore]` so plain `cargo test` stays hardware/model-free. Run the real check
//! with:
//!
//! ```text
//! cargo test -p whisper-iface -- --ignored
//! ```
//!
//! Requires `models/ggml-large-v3-turbo-q5_0.bin` and `whisper-cli` on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use whisper_core::ports::SttEngine;
use whisper_core::types::SttOpts;
use whisper_iface::stt::{read_wav_mono_f32, WhisperCppEngine};

#[derive(Debug, Deserialize)]
struct Manifest {
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    file: String,
    lang: String,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn model_path() -> PathBuf {
    workspace_root().join("models/ggml-large-v3-turbo-q5_0.bin")
}

/// The whisper decode language for a fixture: `-l cs` for Czech and code-switching
/// (`cs+en`), `-l en` for the English control (`ARCHITECTURE.md` §3.3).
fn stt_lang(fixture_lang: &str) -> &'static str {
    if fixture_lang.starts_with("en") {
        "en"
    } else {
        "cs"
    }
}

fn normalize(s: &str) -> String {
    s.trim().nfc().collect()
}

/// Word-level agreement between two transcripts, over whitespace tokens.
///
/// Returns `(similarity, token_diff)` where `similarity = lcs / max_len` in `[0.0, 1.0]`
/// and `token_diff = max_len - lcs` is the number of non-aligned tokens. 1.0 / 0 means
/// identical word streams; benign whisper nondeterminism (a dropped filler, a casing flip)
/// degrades gracefully rather than failing hard.
fn word_stats(a: &str, b: &str) -> (f64, usize) {
    let wa: Vec<&str> = a.split_whitespace().collect();
    let wb: Vec<&str> = b.split_whitespace().collect();
    if wa.is_empty() && wb.is_empty() {
        return (1.0, 0);
    }
    let (n, m) = (wa.len(), wb.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if wa[i] == wb[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }
    let max_len = n.max(m);
    let lcs = dp[n][m];
    (lcs as f64 / max_len as f64, max_len - lcs)
}

fn whisper_cli_transcribe(model: &Path, lang: &str, wav: &Path) -> String {
    let out = Command::new("whisper-cli")
        .arg("-m")
        .arg(model)
        .arg("-l")
        .arg(lang)
        .arg("-nt")
        .arg("-np")
        .arg("-f")
        .arg(wav)
        .output()
        .expect("failed to spawn whisper-cli — is it on PATH?");
    assert!(
        out.status.success(),
        "whisper-cli failed on {}: {}",
        wav.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    // `-nt` prints just the transcription text (possibly across lines); join and normalize.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
#[ignore = "requires the turbo model + whisper-cli; run with `cargo test -- --ignored`"]
fn whisper_rs_matches_whisper_cli_on_czech_fixtures() {
    let model = model_path();
    assert!(
        model.exists(),
        "missing model {} — parity test needs it present",
        model.display()
    );

    let audio_dir = workspace_root().join("fixtures/audio");
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(audio_dir.join("manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");

    // One resident model for all fixtures (D-0004 / §3.4).
    let engine = WhisperCppEngine::load(&model).expect("load turbo model");

    let mut failures: Vec<String> = Vec::new();
    for fx in &manifest.fixtures {
        // Skip the real/ subdir per the task; the manifest lists only top-level files.
        if fx.file.contains("real/") {
            continue;
        }
        let wav = audio_dir.join(&fx.file);
        let lang = stt_lang(&fx.lang);

        let pcm = read_wav_mono_f32(&wav).expect("read fixture wav");
        let opts = SttOpts {
            lang_hint: Some(lang.to_string()),
        };
        let engine_text = normalize(&engine.transcribe(&pcm, &opts).expect("engine decode").text);
        let cli_text = normalize(&whisper_cli_transcribe(&model, lang, &wav));

        let (sim, token_diff) = word_stats(&engine_text, &cli_text);
        // Parity gate: ≥95% word-level agreement, OR at most one non-aligned token. The
        // one-token allowance covers the short-fixture edge case: `whisper-cli` (Homebrew
        // whisper-cpp 1.9.1, BLAS backend) and whisper-rs's bundled whisper.cpp are
        // different builds with different numeric backends, so a single benign token can
        // differ (e.g. `NPM`/`npm` casing) on a 6–11 word clip where one token already
        // breaches 95%. Two or more diverging tokens still fail — a real regression signal.
        let ok = sim >= 0.95 || token_diff <= 1;
        let verdict = if ok { "PASS" } else { "DIFF" };
        println!(
            "[{verdict}] {} (lang={lang}) word-sim={sim:.3} token-diff={token_diff}",
            fx.file
        );
        if !ok || token_diff > 0 {
            println!("  engine: {engine_text}");
            println!("  cli   : {cli_text}");
        }
        if !ok {
            failures.push(format!(
                "{}: word-sim {sim:.3} < 0.95 and {token_diff} tokens differ",
                fx.file
            ));
        }

        // Diacritic guard on the pangram fixture: composed Czech letters must survive.
        if fx.file == "cs-01.wav" {
            assert!(
                engine_text.contains("žluťoučký"),
                "cs-01 lost diacritics: engine output was {engine_text:?}"
            );
        }
    }

    assert!(
        failures.is_empty(),
        "whisper-rs/whisper-cli parity below 95% on: {}",
        failures.join("; ")
    );
}
