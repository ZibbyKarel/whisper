//! whisperd lifecycle management for the tray shell.
//!
//! The constitution keeps the UX shell a pure `/v1` client (D-0013), so this
//! module never links the engine — it only *starts* the `whisperd` process the
//! user would otherwise launch from a terminal, so "open the app" is enough
//! (real user feedback). Two modes:
//!
//! - **managed**: on startup `GET /v1/health` was unreachable, so we resolved the
//!   `whisperd` binary and spawned it as a child; the tray owns its lifecycle
//!   (restart, and kill on quit).
//! - **external**: a daemon was already reachable (started from a terminal or a
//!   separate launch), so we adopt it and never manage it — restart is disabled.
//!
//! Everything here is GUI-free and unit-tested. Binary resolution is driven by an
//! injected [`BinEnv`] so its order is tested without touching the real PATH; the
//! spawn/signal paths are exercised against a stub script, never the real daemon.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long a graceful stop waits for the child to exit after `SIGINT` before
/// escalating to `SIGKILL`. Restart uses this too (task: SIGINT → wait ≤5 s →
/// spawn again).
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Injectable seam for [`resolve_whisperd`] so the resolution *order* is unit
/// tested with fakes — no real filesystem, PATH, or spawn required.
pub trait BinEnv {
    /// Non-empty value of environment variable `key`, or `None`.
    fn env_var(&self, key: &str) -> Option<String>;
    /// Directory containing the current executable (the app bundle's
    /// `Contents/MacOS`, where `whisperd` is a sibling — see `scripts/package.sh`).
    fn current_exe_dir(&self) -> Option<PathBuf>;
    /// First `name` found on `PATH`, or `None`.
    fn which(&self, name: &str) -> Option<PathBuf>;
    /// Whether `path` is an existing regular file.
    fn is_file(&self, path: &Path) -> bool;
}

/// Production [`BinEnv`] backed by the real process environment.
pub struct RealBinEnv;

impl BinEnv for RealBinEnv {
    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn current_exe_dir(&self) -> Option<PathBuf> {
        std::env::current_exe()
            .ok()?
            .parent()
            .map(Path::to_path_buf)
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        let paths = std::env::var_os("PATH")?;
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// Resolve the `whisperd` binary. Order (task): `$WHISPERD_BIN` (explicit
/// override, honored only if it points at a real file) → a sibling of the current
/// executable (the app-bundle layout) → `which whisperd` on `PATH`.
pub fn resolve_whisperd<E: BinEnv>(env: &E) -> Option<PathBuf> {
    if let Some(explicit) = env.env_var("WHISPERD_BIN") {
        let p = PathBuf::from(explicit);
        // A stale override must not wedge resolution — fall through if it is gone.
        if env.is_file(&p) {
            return Some(p);
        }
    }
    if let Some(dir) = env.current_exe_dir() {
        let sibling = dir.join("whisperd");
        if env.is_file(&sibling) {
            return Some(sibling);
        }
    }
    env.which("whisperd")
}

/// Default log path: `~/Library/Logs/whisper-engine/whisperd.log` on macOS (the
/// standard user log location), with a temp fallback elsewhere so tests never
/// depend on a real HOME.
#[must_use]
pub fn default_log_path() -> PathBuf {
    log_dir().join("whisperd.log")
}

fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            return home.join("Library").join("Logs").join("whisper-engine");
        }
    }
    dirs::state_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("whisper-engine")
}

/// Spawn `whisperd`, redirecting stdout+stderr (appended) to `log_path`; the log
/// directory is created if missing. stdin is `/dev/null` so a spawned daemon
/// never blocks on terminal input.
pub fn spawn_whisperd(bin: &Path, log_path: &Path) -> io::Result<Child> {
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let err = log.try_clone()?;
    Command::new(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
}

/// Owns the daemon process when the tray started it. Kept in [`crate`]'s GUI state
/// and consulted for the "Restart daemon" action and quit-time teardown.
pub struct DaemonController {
    log_path: PathBuf,
    /// Resolved binary, retained so a managed daemon can be re-spawned on restart.
    bin: Option<PathBuf>,
    /// `Some` only in managed mode (we own the process).
    child: Option<Child>,
    /// True when a daemon was already reachable and we merely adopted it.
    external: bool,
}

impl DaemonController {
    /// Production entry: probe result in hand (`reachable` = `GET /v1/health`
    /// succeeded), decide managed vs. external and spawn if needed.
    #[must_use]
    pub fn start(reachable: bool) -> Self {
        Self::start_with(reachable, &RealBinEnv, default_log_path())
    }

    /// Testable core of [`Self::start`]: injectable env + explicit log path.
    #[must_use]
    pub fn start_with<E: BinEnv>(reachable: bool, env: &E, log_path: PathBuf) -> Self {
        if reachable {
            // Adopt the running daemon; do not resolve/spawn (external mode).
            return Self {
                log_path,
                bin: None,
                child: None,
                external: true,
            };
        }
        let bin = resolve_whisperd(env);
        let child = bin.as_ref().and_then(|b| spawn_whisperd(b, &log_path).ok());
        Self {
            log_path,
            bin,
            child,
            external: false,
        }
    }

    /// True when the tray owns the daemon process (restart/kill are allowed).
    #[must_use]
    pub fn is_managed(&self) -> bool {
        self.child.is_some()
    }

    /// True when a daemon was already running and we only adopted it.
    #[must_use]
    pub fn is_external(&self) -> bool {
        self.external
    }

    /// Where the managed daemon's stdout/stderr are appended (surfaced in the UI
    /// Status section).
    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Restart the managed daemon: `SIGINT` → wait ≤5 s → spawn again. Errors when
    /// not in managed mode (the GUI disables the button, but guard anyway).
    pub fn restart(&mut self) -> Result<(), String> {
        if !self.is_managed() {
            return Err("daemon not managed by this app".to_string());
        }
        let bin = self
            .bin
            .clone()
            .ok_or_else(|| "whisperd binary path lost".to_string())?;
        if let Some(mut child) = self.child.take() {
            stop_child(&mut child, STOP_TIMEOUT);
        }
        let child = spawn_whisperd(&bin, &self.log_path).map_err(|e| e.to_string())?;
        self.child = Some(child);
        Ok(())
    }

    /// Kill a managed daemon (`SIGINT` → `SIGKILL` fallback). No-op in external
    /// mode. Called explicitly before the tray exits because `process::exit`
    /// skips `Drop`.
    pub fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            stop_child(&mut child, STOP_TIMEOUT);
        }
    }
}

impl Drop for DaemonController {
    fn drop(&mut self) {
        // Belt-and-suspenders for the paths that do run destructors; the tray's
        // Quit handler calls `shutdown()` explicitly because it uses process::exit.
        self.shutdown();
    }
}

/// Whether a stop completed on the graceful signal or had to be forced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopOutcome {
    /// Exited after `SIGINT` within the grace window.
    Graceful,
    /// Ignored `SIGINT`; had to be `SIGKILL`ed.
    Forced,
}

/// Stop a child gracefully: send `SIGINT` (whisperd handles ctrl-c cleanly,
/// draining the active session), poll for exit up to `timeout`, then escalate to
/// `SIGKILL` (`Child::kill`). We prefer `libc::kill` for the graceful signal
/// because `std` only exposes `SIGKILL` via `Child::kill`.
fn stop_child(child: &mut Child, timeout: Duration) -> StopOutcome {
    send_sigint(child);
    if wait_for_exit(child, timeout) {
        StopOutcome::Graceful
    } else {
        // Escalation: SIGKILL is uncatchable, so this always terminates the child.
        let _ = child.kill();
        let _ = child.wait();
        StopOutcome::Forced
    }
}

/// Send `SIGINT` to a child (no-op signalling on non-unix, where the wait/kill
/// fallback still applies).
fn send_sigint(child: &Child) {
    #[cfg(unix)]
    // SAFETY: `kill(2)` with a pid we own and a fixed signal number; no memory is
    // touched. A failure (already-reaped pid) is benign and ignored.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    #[cfg(not(unix))]
    let _ = child;
}

/// Poll `child` for exit up to `timeout`. Returns `true` if it exited (or was
/// already reaped) within the window, `false` if it is still alive at `timeout`
/// (the caller then escalates to `SIGKILL`). Split out so the escalation policy
/// is unit-tested deterministically — a long-running child times out here without
/// depending on any particular signal-handling behavior.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // Already reaped or errored — treat as gone.
            Err(_) => return true,
        }
    }
}

/// True if `path` is a regular file with any execute bit set (unix) / any file
/// (elsewhere).
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fully injected [`BinEnv`]: no real env/PATH/filesystem touched.
    #[derive(Default)]
    struct FakeEnv {
        env: HashMap<String, String>,
        exe_dir: Option<PathBuf>,
        which: Option<PathBuf>,
        files: Vec<PathBuf>,
    }

    impl BinEnv for FakeEnv {
        fn env_var(&self, key: &str) -> Option<String> {
            self.env.get(key).filter(|v| !v.is_empty()).cloned()
        }
        fn current_exe_dir(&self) -> Option<PathBuf> {
            self.exe_dir.clone()
        }
        fn which(&self, _name: &str) -> Option<PathBuf> {
            self.which.clone()
        }
        fn is_file(&self, path: &Path) -> bool {
            self.files.iter().any(|f| f == path)
        }
    }

    #[test]
    fn resolution_prefers_explicit_env_when_it_exists() {
        let env = FakeEnv {
            env: HashMap::from([("WHISPERD_BIN".into(), "/opt/wd".into())]),
            exe_dir: Some(PathBuf::from("/app/Contents/MacOS")),
            which: Some(PathBuf::from("/usr/local/bin/whisperd")),
            files: vec![
                PathBuf::from("/opt/wd"),
                PathBuf::from("/app/Contents/MacOS/whisperd"),
            ],
        };
        assert_eq!(resolve_whisperd(&env), Some(PathBuf::from("/opt/wd")));
    }

    #[test]
    fn stale_env_override_falls_through_to_sibling() {
        let env = FakeEnv {
            env: HashMap::from([("WHISPERD_BIN".into(), "/gone/wd".into())]),
            exe_dir: Some(PathBuf::from("/app/Contents/MacOS")),
            which: Some(PathBuf::from("/usr/local/bin/whisperd")),
            // /gone/wd is NOT in files → override is skipped.
            files: vec![PathBuf::from("/app/Contents/MacOS/whisperd")],
        };
        assert_eq!(
            resolve_whisperd(&env),
            Some(PathBuf::from("/app/Contents/MacOS/whisperd"))
        );
    }

    #[test]
    fn resolution_falls_back_to_path() {
        let env = FakeEnv {
            env: HashMap::new(),
            exe_dir: Some(PathBuf::from("/app/Contents/MacOS")),
            which: Some(PathBuf::from("/usr/local/bin/whisperd")),
            // No sibling present.
            files: vec![],
        };
        assert_eq!(
            resolve_whisperd(&env),
            Some(PathBuf::from("/usr/local/bin/whisperd"))
        );
    }

    #[test]
    fn resolution_returns_none_when_nothing_found() {
        let env = FakeEnv::default();
        assert_eq!(resolve_whisperd(&env), None);
    }

    #[test]
    fn empty_env_var_is_ignored() {
        let env = FakeEnv {
            env: HashMap::from([("WHISPERD_BIN".into(), String::new())]),
            which: Some(PathBuf::from("/usr/local/bin/whisperd")),
            ..FakeEnv::default()
        };
        assert_eq!(
            resolve_whisperd(&env),
            Some(PathBuf::from("/usr/local/bin/whisperd"))
        );
    }

    #[test]
    fn default_log_path_ends_with_whisperd_log() {
        let p = default_log_path();
        assert!(p.ends_with("whisperd.log"), "log path was {}", p.display());
        assert!(p.to_string_lossy().contains("whisper-engine"));
    }

    #[test]
    fn external_mode_when_reachable_does_not_spawn() {
        let env = FakeEnv::default();
        let ctl = DaemonController::start_with(true, &env, unique_log_path("external"));
        assert!(ctl.is_external());
        assert!(!ctl.is_managed());
    }

    #[test]
    fn unmanaged_when_binary_unresolvable() {
        // Not reachable and no binary anywhere → neither managed nor external.
        let env = FakeEnv::default();
        let ctl = DaemonController::start_with(false, &env, unique_log_path("unmanaged"));
        assert!(!ctl.is_external());
        assert!(!ctl.is_managed());
    }

    #[test]
    fn restart_refused_in_external_mode() {
        let env = FakeEnv::default();
        let mut ctl = DaemonController::start_with(true, &env, unique_log_path("no-restart"));
        assert!(ctl.restart().is_err());
    }

    // -- spawn/signal integration against a stub script (never the real daemon) --

    fn write_stub(name: &str, body: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("whisper-ux-stub-{}-{name}.sh", std::process::id()));
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn unique_log_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "whisper-ux-log-{}-{name}/whisperd.log",
            std::process::id()
        ));
        p
    }

    #[cfg(unix)]
    #[test]
    fn spawn_then_sigint_stops_gracefully() {
        // A plain `sleep` takes SIGINT's default action (terminate), standing in
        // for whisperd's graceful ctrl-c handling. Verifies spawn + SIGINT + reap.
        let script = write_stub("sigint", "#!/bin/sh\nsleep 30\n");
        let log = unique_log_path("sigint");
        let mut child = spawn_whisperd(&script, &log).expect("spawn stub");

        let start = Instant::now();
        let outcome = stop_child(&mut child, Duration::from_secs(5));
        assert_eq!(outcome, StopOutcome::Graceful);
        // SIGINT terminated it well within the grace window (no SIGKILL needed).
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "SIGINT did not stop the child promptly"
        );
        // Log file was created in the parent dir we asked for (append redirect).
        assert!(log.exists());
        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn wait_for_exit_times_out_then_kill_reaps() {
        // Deterministic escalation coverage without relying on any child ignoring
        // SIGINT (signal-disposition inheritance under a spawned child is
        // environment-sensitive): a long-lived child that we do NOT signal is
        // still alive when the grace window elapses, so `wait_for_exit` reports
        // `false` (→ the caller escalates), and `Child::kill` (SIGKILL) then reaps
        // it — the exact fallback `stop_child` performs.
        let script = write_stub("timeout", "#!/bin/sh\nsleep 30\n");
        let log = unique_log_path("timeout");
        let mut child = spawn_whisperd(&script, &log).expect("spawn stub");

        let window = Duration::from_millis(150);
        let start = Instant::now();
        let exited = wait_for_exit(&mut child, window);
        assert!(!exited, "child unexpectedly gone before the grace window");
        assert!(
            start.elapsed() >= window,
            "did not wait the full grace window"
        );

        // SIGKILL fallback always terminates it.
        child.kill().expect("SIGKILL");
        assert!(child.wait().is_ok());
        assert!(matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
    }
}
