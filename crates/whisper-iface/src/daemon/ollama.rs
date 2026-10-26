//! Local Ollama server discovery and start (D-0017): is it installed, is it answering, and how to
//! launch it. Blocking by design — callers run it on blocking threads (`spawn_blocking`). The
//! only network peer is the configured `ollama_url` (`/api/version`).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::ollama::OllamaClient;

/// How long `start` waits for `/api/version` to answer after launching.
const START_TIMEOUT: Duration = Duration::from_secs(10);
const START_POLL: Duration = Duration::from_millis(250);

const BREW_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];
const APP_PATH: &str = "/Applications/Ollama.app";

/// Result of looking for an Ollama install and asking the configured URL for its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub installed: bool,
    pub running: bool,
    pub version: Option<String>,
}

/// How a local Ollama is installed, which decides how `start` launches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installation {
    /// `ollama` under a Homebrew prefix; started via `brew services` (shared with other apps).
    Brew(PathBuf),
    /// `/Applications/Ollama.app`.
    App,
    /// A bare `ollama` binary elsewhere on PATH; started with `ollama serve`.
    Binary(PathBuf),
}

/// How a command is run: `Wait` for launchers that exit (checked for success), `Detach` for the
/// long-lived server process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    Wait,
    Detach,
}

/// Locate Ollama. `exists` is injected so tests do not touch the filesystem. A Homebrew-prefixed
/// binary wins, then the app bundle, then any other binary found on `PATH`.
pub fn find_installation(
    path_env: Option<&str>,
    exists: impl Fn(&Path) -> bool,
) -> Option<Installation> {
    let mut candidates: Vec<PathBuf> = path_env
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("ollama"))
        .collect();
    // App-bundle launches have a minimal PATH, so always check the standard prefixes too.
    for prefix in BREW_PREFIXES {
        candidates.push(Path::new(prefix).join("bin/ollama"));
    }
    let found: Vec<PathBuf> = candidates.into_iter().filter(|p| exists(p)).collect();

    if let Some(brew) = found
        .iter()
        .find(|p| BREW_PREFIXES.iter().any(|prefix| p.starts_with(prefix)))
    {
        return Some(Installation::Brew(brew.clone()));
    }
    if exists(Path::new(APP_PATH)) {
        return Some(Installation::App);
    }
    found.into_iter().next().map(Installation::Binary)
}

/// Installed (on this machine) and running (answering at the client's URL).
pub fn probe(client: &OllamaClient) -> Probe {
    let path_env = std::env::var("PATH").ok();
    let installed = find_installation(path_env.as_deref(), |p| p.exists()).is_some();
    let version = client.version().ok();
    Probe {
        installed,
        running: version.is_some(),
        version,
    }
}

/// The launch command(s) for an installation, in the order they are tried.
fn launch_plan(inst: &Installation) -> Vec<(Command, Launch)> {
    let serve = |bin: &Path| {
        let mut c = Command::new(bin);
        c.arg("serve");
        (c, Launch::Detach)
    };
    match inst {
        Installation::Brew(bin) => {
            let brew = bin.with_file_name("brew");
            let mut c = Command::new(brew);
            c.args(["services", "start", "ollama"]);
            // If brew refuses (service not registered), fall back to serving directly.
            vec![(c, Launch::Wait), serve(bin)]
        }
        Installation::App => {
            let mut c = Command::new("open");
            c.args(["-a", "Ollama"]);
            vec![(c, Launch::Wait)]
        }
        Installation::Binary(bin) => vec![serve(bin)],
    }
}

/// Run a command for real: `Wait` requires a successful exit, `Detach` spawns and forgets.
fn run_real(cmd: &mut Command, launch: Launch) -> io::Result<()> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match launch {
        Launch::Wait => {
            let status = cmd.status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("exited with {status}")))
            }
        }
        Launch::Detach => cmd.spawn().map(drop),
    }
}

/// Start Ollama and wait (≤ 10 s) for it to answer. Only call for a loopback URL.
pub fn start(inst: &Installation, client: &OllamaClient) -> Result<(), String> {
    start_with(inst, client, run_real, START_TIMEOUT, START_POLL)
}

/// [`start`] with the command runner and poll timing injected, so tests spawn nothing.
pub fn start_with(
    inst: &Installation,
    client: &OllamaClient,
    run: impl Fn(&mut Command, Launch) -> io::Result<()>,
    timeout: Duration,
    poll: Duration,
) -> Result<(), String> {
    let mut last_err = None;
    let mut launched = false;
    for (mut cmd, launch) in launch_plan(inst) {
        match run(&mut cmd, launch) {
            Ok(()) => {
                launched = true;
                break;
            }
            Err(e) => last_err = Some(format!("{}: {e}", cmd.get_program().to_string_lossy())),
        }
    }
    if !launched {
        return Err(format!(
            "could not start ollama ({})",
            last_err.unwrap_or_default()
        ));
    }
    let deadline = Instant::now() + timeout;
    loop {
        if client.version().is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "ollama did not answer at {} within {}s",
                client.base(),
                timeout.as_secs()
            ));
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn exists_in(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p| paths.iter().any(|x| Path::new(x) == p)
    }

    #[test]
    fn find_installation_prefers_brew() {
        let got = find_installation(
            Some("/usr/bin:/opt/homebrew/bin"),
            exists_in(&["/opt/homebrew/bin/ollama", "/Applications/Ollama.app"]),
        );
        assert_eq!(
            got,
            Some(Installation::Brew(PathBuf::from(
                "/opt/homebrew/bin/ollama"
            )))
        );
    }

    #[test]
    fn find_installation_app() {
        let got = find_installation(Some("/usr/bin"), exists_in(&["/Applications/Ollama.app"]));
        assert_eq!(got, Some(Installation::App));
    }

    #[test]
    fn find_installation_binary_on_path() {
        let got = find_installation(
            Some("/usr/bin:/home/u/bin"),
            exists_in(&["/home/u/bin/ollama"]),
        );
        assert_eq!(
            got,
            Some(Installation::Binary(PathBuf::from("/home/u/bin/ollama")))
        );
    }

    #[test]
    fn find_installation_checks_standard_prefixes_without_path() {
        let got = find_installation(None, exists_in(&["/usr/local/bin/ollama"]));
        assert_eq!(
            got,
            Some(Installation::Brew(PathBuf::from("/usr/local/bin/ollama")))
        );
    }

    #[test]
    fn find_installation_none() {
        assert_eq!(find_installation(Some("/usr/bin"), |_| false), None);
    }

    /// Mock Ollama answering `/api/version` once per connection, forever (detached thread).
    fn mock_ollama() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let body = r#"{"version":"0.12.3"}"#;
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        url
    }

    fn dead_url() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        url
    }

    fn describe(cmd: &Command) -> String {
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        format!("{} {}", cmd.get_program().to_string_lossy(), args.join(" "))
    }

    #[test]
    fn start_brew_runs_brew_services_then_polls_version() {
        let client = OllamaClient::new(mock_ollama());
        let ran = RefCell::new(Vec::new());
        let inst = Installation::Brew(PathBuf::from("/opt/homebrew/bin/ollama"));
        let res = start_with(
            &inst,
            &client,
            |c, l| {
                ran.borrow_mut().push((describe(c), l));
                Ok(())
            },
            Duration::from_secs(2),
            Duration::from_millis(10),
        );
        assert_eq!(res, Ok(()));
        assert_eq!(
            *ran.borrow(),
            vec![(
                "/opt/homebrew/bin/brew services start ollama".to_string(),
                Launch::Wait
            )]
        );
    }

    #[test]
    fn start_brew_falls_back_to_serve_when_brew_fails() {
        let client = OllamaClient::new(mock_ollama());
        let ran = RefCell::new(Vec::new());
        let inst = Installation::Brew(PathBuf::from("/opt/homebrew/bin/ollama"));
        let res = start_with(
            &inst,
            &client,
            |c, l| {
                ran.borrow_mut().push((describe(c), l));
                if l == Launch::Wait {
                    Err(io::Error::other("no such service"))
                } else {
                    Ok(())
                }
            },
            Duration::from_secs(2),
            Duration::from_millis(10),
        );
        assert_eq!(res, Ok(()));
        let ran = ran.borrow();
        assert_eq!(ran.len(), 2);
        assert_eq!(ran[1].0, "/opt/homebrew/bin/ollama serve");
        assert_eq!(ran[1].1, Launch::Detach);
    }

    #[test]
    fn start_app_uses_open_and_binary_uses_serve() {
        let client = OllamaClient::new(mock_ollama());
        let ran = RefCell::new(Vec::new());
        let record = |c: &mut Command, _l| {
            ran.borrow_mut().push(describe(c));
            Ok(())
        };
        let t = Duration::from_secs(2);
        let p = Duration::from_millis(10);
        start_with(&Installation::App, &client, record, t, p).unwrap();
        start_with(
            &Installation::Binary(PathBuf::from("/home/u/bin/ollama")),
            &client,
            record,
            t,
            p,
        )
        .unwrap();
        assert_eq!(
            *ran.borrow(),
            vec!["open -a Ollama", "/home/u/bin/ollama serve"]
        );
    }

    #[test]
    fn start_reports_launch_failure_and_poll_timeout() {
        let client = OllamaClient::new(dead_url());
        let t = Duration::from_millis(100);
        let p = Duration::from_millis(10);
        let inst = Installation::Binary(PathBuf::from("/x/ollama"));

        let err =
            start_with(&inst, &client, |_, _| Err(io::Error::other("boom")), t, p).unwrap_err();
        assert!(err.contains("boom"), "{err}");

        let err = start_with(&inst, &client, |_, _| Ok(()), t, p).unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
    }
}
