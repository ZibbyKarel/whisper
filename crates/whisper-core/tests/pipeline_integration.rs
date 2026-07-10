//! Headless full-pipeline integration tests (`ARCHITECTURE.md` §3.1, §7): drives
//! `Pipeline` with fixture-audio/canned-STT/identity-cleanup/capture-sink test adapters —
//! no hardware, no network. Czech assertions are byte-exact.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use whisper_core::dictionary::Dictionary;
use whisper_core::error::{AudioError, CleanupError, DeliverError, SttError};
use whisper_core::pipeline::{Pipeline, PipelineConfig, PipelineEvent};
use whisper_core::ports::{AudioSource, AudioStream, CleanupEngine, NoopVad, SttEngine, TextSink};
use whisper_core::session::{CancellationToken, SessionState};
use whisper_core::types::{AudioCfg, CleanupCtx, SttOpts, TranscriptRaw};

const FRAME_LEN: usize = 1600; // 100 ms @ 16 kHz mono

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name)
}

// ---- Test adapters (ARCHITECTURE.md §3.1: "Test adapters ... testable without hardware") ----

/// Reads a fixture WAV via hound and yields it as fixed-size frames.
struct FixtureAudioSource {
    path: PathBuf,
}

impl FixtureAudioSource {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl AudioSource for FixtureAudioSource {
    fn start(&self, _cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError> {
        let reader = hound::WavReader::open(&self.path)
            .map_err(|e| AudioError::StartFailed(e.to_string()))?;
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => reader
                .into_samples::<i16>()
                .map(|s| s.unwrap_or(0) as f32 / i16::MAX as f32)
                .collect(),
            hound::SampleFormat::Float => reader
                .into_samples::<f32>()
                .map(|s| s.unwrap_or(0.0))
                .collect(),
        };
        let mono: Vec<f32> = if spec.channels > 1 {
            samples
                .chunks(spec.channels as usize)
                .map(|c| c.iter().sum::<f32>() / c.len() as f32)
                .collect()
        } else {
            samples
        };
        Ok(Box::new(FixtureAudioStream {
            samples: mono,
            pos: 0,
        }))
    }
}

struct FixtureAudioStream {
    samples: Vec<f32>,
    pos: usize,
}

impl AudioStream for FixtureAudioStream {
    fn next_frame(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        if self.pos >= self.samples.len() {
            return Ok(None);
        }
        let end = (self.pos + FRAME_LEN).min(self.samples.len());
        let frame = self.samples[self.pos..end].to_vec();
        self.pos = end;
        Ok(Some(frame))
    }
}

/// Wraps another `AudioSource` and cancels a shared token the moment recording finishes
/// (simulating a cancel request that arrives during recording and is discovered at the
/// next FSM checkpoint, before transcription starts).
struct CancellingAudioSource {
    inner: FixtureAudioSource,
    token: CancellationToken,
}

impl AudioSource for CancellingAudioSource {
    fn start(&self, cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError> {
        let inner = self.inner.start(cfg)?;
        Ok(Box::new(CancellingAudioStream {
            inner,
            token: self.token.clone(),
        }))
    }
}

struct CancellingAudioStream {
    inner: Box<dyn AudioStream>,
    token: CancellationToken,
}

impl AudioStream for CancellingAudioStream {
    fn next_frame(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        let result = self.inner.next_frame();
        if matches!(result, Ok(None)) {
            self.token.cancel();
        }
        result
    }
}

/// Yields one frame then simulates a device disconnect (e.g. Bluetooth mic drop).
struct FlakyAudioSource {
    frame: Vec<f32>,
    reason: String,
}

impl AudioSource for FlakyAudioSource {
    fn start(&self, _cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError> {
        Ok(Box::new(FlakyAudioStream {
            frame: self.frame.clone(),
            reason: self.reason.clone(),
            yielded: false,
        }))
    }
}

struct FlakyAudioStream {
    frame: Vec<f32>,
    reason: String,
    yielded: bool,
}

impl AudioStream for FlakyAudioStream {
    fn next_frame(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        if !self.yielded {
            self.yielded = true;
            Ok(Some(self.frame.clone()))
        } else {
            Err(AudioError::DeviceLost {
                reason: self.reason.clone(),
            })
        }
    }
}

/// Canned STT: ignores the PCM content and returns a fixed transcript (fails on empty
/// PCM, mirroring a real engine refusing to decode silence).
struct FakeStt {
    text: String,
    lang: String,
}

impl SttEngine for FakeStt {
    fn transcribe(&self, pcm: &[f32], _opts: &SttOpts) -> Result<TranscriptRaw, SttError> {
        if pcm.is_empty() {
            return Err(SttError::Failed("empty pcm".to_string()));
        }
        Ok(TranscriptRaw {
            text: self.text.clone(),
            lang: self.lang.clone(),
            segments: vec![],
        })
    }
}

struct IdentityCleanup;

impl CleanupEngine for IdentityCleanup {
    fn clean(&self, text: &str, _ctx: &CleanupCtx) -> Result<String, CleanupError> {
        Ok(text.to_string())
    }
}

struct FailingCleanup;

impl CleanupEngine for FailingCleanup {
    fn clean(&self, _text: &str, _ctx: &CleanupCtx) -> Result<String, CleanupError> {
        Err(CleanupError::Failed(
            "simulated cleanup failure".to_string(),
        ))
    }
}

#[derive(Clone, Default)]
struct CaptureSink {
    captured: Arc<Mutex<Option<String>>>,
}

impl TextSink for CaptureSink {
    fn deliver(&self, text: &str) -> Result<(), DeliverError> {
        *self.captured.lock().unwrap() = Some(text.to_string());
        Ok(())
    }
}

// ---- Tests ----

#[test]
fn full_pipeline_round_trips_czech_diacritics_byte_exact() {
    let reference = "Příliš žluťoučký kůň úpěl ďábelské ódy.";

    let audio = FixtureAudioSource::new(fixture_path("cs-01.wav"));
    let stt = FakeStt {
        text: reference.to_string(),
        lang: "cs".to_string(),
    };
    let sink = CaptureSink::default();

    let mut pipeline = Pipeline::new(
        Box::new(audio),
        Box::new(NoopVad),
        Box::new(stt),
        Dictionary::empty(),
        Some(Box::new(IdentityCleanup)),
        Box::new(sink.clone()),
        PipelineConfig {
            cleanup_enabled: false,
        },
    );

    let token = CancellationToken::new();
    let state = pipeline.run(AudioCfg::default(), &CleanupCtx::default(), &token);

    assert_eq!(state, SessionState::Idle);
    let delivered = sink
        .captured
        .lock()
        .unwrap()
        .clone()
        .expect("text delivered");
    // Byte-exact: diacritics must survive the full pipeline untouched.
    assert_eq!(delivered.as_bytes(), reference.as_bytes());
    assert_eq!(delivered, reference);
}

#[test]
fn dictionary_pass_fixes_merknout_to_mergnout() {
    let raw = "Pushni ten branch do repa a udělej pull request, ať to můžeme merknout do mainu.";
    let expected =
        "Pushni ten branch do repa a udělej pull request, ať to můžeme mergnout do mainu.";

    let audio = FixtureAudioSource::new(fixture_path("mix-01.wav"));
    let stt = FakeStt {
        text: raw.to_string(),
        lang: "cs".to_string(),
    };
    let dictionary =
        Dictionary::from_json_str(r#"{"pairs": [{"from": "merknout", "to": "mergnout"}]}"#)
            .expect("valid dictionary json");
    let sink = CaptureSink::default();

    let mut pipeline = Pipeline::new(
        Box::new(audio),
        Box::new(NoopVad),
        Box::new(stt),
        dictionary,
        None,
        Box::new(sink.clone()),
        PipelineConfig {
            cleanup_enabled: false,
        },
    );

    let token = CancellationToken::new();
    let state = pipeline.run(AudioCfg::default(), &CleanupCtx::default(), &token);

    assert_eq!(state, SessionState::Idle);
    assert_eq!(sink.captured.lock().unwrap().clone().unwrap(), expected);
}

#[test]
fn cancellation_mid_pipeline_stops_before_delivery() {
    let token = CancellationToken::new();
    let audio = CancellingAudioSource {
        inner: FixtureAudioSource::new(fixture_path("cs-01.wav")),
        token: token.clone(),
    };
    let stt = FakeStt {
        text: "should never be delivered".to_string(),
        lang: "cs".to_string(),
    };
    let sink = CaptureSink::default();

    let mut pipeline = Pipeline::new(
        Box::new(audio),
        Box::new(NoopVad),
        Box::new(stt),
        Dictionary::empty(),
        None,
        Box::new(sink.clone()),
        PipelineConfig {
            cleanup_enabled: false,
        },
    );

    let rx = pipeline.subscribe();
    let state = pipeline.run(AudioCfg::default(), &CleanupCtx::default(), &token);

    assert_eq!(state, SessionState::Cancelled);
    assert!(
        sink.captured.lock().unwrap().is_none(),
        "TextSink must not run once the session is cancelled"
    );
    let events: Vec<_> = rx.try_iter().collect();
    assert!(matches!(
        events.last(),
        Some(PipelineEvent::Cancelled { .. })
    ));
}

#[test]
fn cleanup_failure_carries_best_text_so_far() {
    let raw = "Otevři si terminál a spusť npm install, pak zkontroluj packa json.";
    let expected_partial = "Otevři si terminál a spusť npm install, pak zkontroluj package.json.";

    let dictionary =
        Dictionary::from_json_str(r#"{"pairs": [{"from": "packa json", "to": "package.json"}]}"#)
            .unwrap();
    let audio = FixtureAudioSource::new(fixture_path("mix-02.wav"));
    let stt = FakeStt {
        text: raw.to_string(),
        lang: "cs".to_string(),
    };
    let sink = CaptureSink::default();

    let mut pipeline = Pipeline::new(
        Box::new(audio),
        Box::new(NoopVad),
        Box::new(stt),
        dictionary,
        Some(Box::new(FailingCleanup)),
        Box::new(sink.clone()),
        PipelineConfig {
            cleanup_enabled: true,
        },
    );

    let token = CancellationToken::new();
    let state = pipeline.run(AudioCfg::default(), &CleanupCtx::default(), &token);

    match state {
        SessionState::Failed(info) => {
            assert_eq!(info.partial_text.as_deref(), Some(expected_partial));
        }
        other => panic!("expected Failed state, got {other:?}"),
    }
    assert!(sink.captured.lock().unwrap().is_none());
}

#[test]
fn device_loss_surfaces_as_failed_with_partial_text() {
    let partial_transcript = "částečný přepis před ztrátou zařízení";
    let audio = FlakyAudioSource {
        frame: vec![0.01_f32; FRAME_LEN],
        reason: "bluetooth mic disconnected".to_string(),
    };
    let stt = FakeStt {
        text: partial_transcript.to_string(),
        lang: "cs".to_string(),
    };
    let sink = CaptureSink::default();

    let mut pipeline = Pipeline::new(
        Box::new(audio),
        Box::new(NoopVad),
        Box::new(stt),
        Dictionary::empty(),
        None,
        Box::new(sink.clone()),
        PipelineConfig {
            cleanup_enabled: false,
        },
    );

    let token = CancellationToken::new();
    let state = pipeline.run(AudioCfg::default(), &CleanupCtx::default(), &token);

    match state {
        SessionState::Failed(info) => {
            assert!(info.error.contains("bluetooth mic disconnected"));
            assert_eq!(info.partial_text.as_deref(), Some(partial_transcript));
        }
        other => panic!("expected Failed state, got {other:?}"),
    }
    assert!(sink.captured.lock().unwrap().is_none());
}
