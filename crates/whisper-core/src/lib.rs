//! `whisper-core`: pure Rust core engine — session FSM, pipeline orchestrator, dictionary
//! pass, and the ports (traits) that adapters implement.
//!
//! No UI/OS dependencies; see `ARCHITECTURE.md` §3 ("Core Engine") and D-0006. Production
//! adapters (whisper.cpp, llama.cpp, cpal, pasteboard) live outside this crate
//! (`whisper-iface`, `whisper-platform`); this crate depends only on the port traits, so
//! it stays hardware-free and fully testable in CI with fixture audio (`ARCHITECTURE.md`
//! §3.1). The session FSM and cancellation model are described in §3.2.

pub mod dictionary;
pub mod error;
pub mod pipeline;
pub mod ports;
pub mod session;
pub mod types;
