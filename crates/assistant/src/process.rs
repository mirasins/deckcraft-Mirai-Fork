//! Bounded, cancellable execution of the Claude Code CLI. No shell is involved.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::Error;
use crate::protocol::{MAX_AUTH_OUTPUT_BYTES, MAX_OUTPUT_BYTES, OUTPUT_SCHEMA, SYSTEM_PROMPT};

pub(crate) const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const GENERATE_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(20);

/// Generation flags: safe mode, no tools, no MCP, no session files, no permission prompts.
/// Prompt data arrives on stdin only.
pub(crate) const GENERATION_ARGS: &[&str] = &[
    "--safe-mode",
    "-p",
    "--system-prompt",
    SYSTEM_PROMPT,
    "--tools",
    "",
    "--disallowedTools",
    "mcp__*",
    "--strict-mcp-config",
    "--mcp-config",
    "{\"mcpServers\":{}}",
    "--no-session-persistence",
    "--permission-mode",
    "dontAsk",
    "--permission-prompts",
    "none",
    "--output-format",
    "json",
    "--json-schema",
    OUTPUT_SCHEMA,
];

/// Same safe-mode settings as generation, so the preflight sees the method generation will use.
pub(crate) const AUTH_ARGS: &[&str] = &["--safe-mode", "auth", "status", "--json"];

/// Environment variables that would switch Claude Code away from the chosen login.
/// Removed for the child process only; the parent environment is never modified.
/// `CLAUDE_CODE_OAUTH_TOKEN` is removed unless the env-token probe or generation allows it.
pub(crate) fn credential_override_keys<I: IntoIterator<Item = OsString>>(keys: I, allow_oauth_token: bool) -> Vec<OsString> {
    keys.into_iter().filter(|key| is_override(key, allow_oauth_token)).collect()
}

/// True when a variable with this name exists (case-insensitive). Never reads the value.
pub(crate) fn key_present<I: IntoIterator<Item = OsString>>(keys: I, name: &str) -> bool {
    keys.into_iter().any(|key| key.eq_ignore_ascii_case(name))
}

fn is_override(key: &OsStr, allow_oauth_token: bool) -> bool {
    let Some(name) = key.to_str() else {
        return true;
    };
    let upper = name.to_ascii_uppercase();
    if upper == "CLAUDE_CODE_OAUTH_TOKEN" {
        return !allow_oauth_token;
    }
    upper.starts_with("ANTHROPIC_")
        || matches!(upper.as_str(), "CLAUDE_CODE_USE_BEDROCK" | "CLAUDE_CODE_USE_VERTEX" | "CLAUDE_CODE_USE_FOUNDRY" | "AWS_BEARER_TOKEN_BEDROCK")
}

/// Private, app-owned, empty working directory. Only removed when empty (never recursive).
pub(crate) struct ScratchDir(PathBuf);

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

impl ScratchDir {
    pub(crate) fn create() -> Result<Self, Error> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let count = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("deckcraft-assistant-{}-{nanos}-{count}", std::process::id()));
        fs::create_dir(&path).map_err(|_| Error::Unavailable)?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // remove_dir fails on a non-empty directory, so no unexpected files are deleted.
        let _ = fs::remove_dir(&self.0);
    }
}

pub(crate) struct Limits {
    pub stdout: usize,
    pub deadline: Duration,
}

pub(crate) struct Captured {
    pub success: bool,
    pub stdout: Vec<u8>,
}

/// Kills and reaps the child unless it already exited (timeout, cancel, overflow, error paths).
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Runs `executable` with `args` in a fresh scratch directory. Credential overrides are removed from the
/// child only. `allow_oauth_token` must be true only for the env-token probe and for generation after an
/// `OAuthToken` preflight; it is false for the stored-session probe and for `Account` generation.
pub(crate) fn run_claude(
    executable: &Path,
    args: &[&str],
    stdin_data: &[u8],
    limits: Limits,
    cancel: &AtomicBool,
    allow_oauth_token: bool,
) -> Result<Captured, Error> {
    let scratch = ScratchDir::create()?;
    let mut command = Command::new(executable);
    command.args(args).current_dir(scratch.path());
    for key in credential_override_keys(std::env::vars_os().map(|(key, _)| key), allow_oauth_token) {
        command.env_remove(key);
    }
    run(command, stdin_data, limits, cancel)
}

/// Runs a prepared command with bounded stdout, a deadline and cooperative cancellation.
/// The child is always killed and reaped before returning on any error path.
pub(crate) fn run(mut command: Command, stdin_data: &[u8], limits: Limits, cancel: &AtomicBool) -> Result<Captured, Error> {
    if cancel.load(Ordering::SeqCst) {
        return Err(Error::Cancelled);
    }
    let child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|_| Error::Unavailable)?;
    let mut child = KillOnDrop(child);

    let stdin = child.0.stdin.take();
    let stdout = child.0.stdout.take().ok_or(Error::Failed)?;
    let stderr = child.0.stderr.take().ok_or(Error::Failed)?;
    let data = stdin_data.to_vec();
    // Builder::spawn reports resource exhaustion as an error instead of panicking.
    spawn_detached("deckcraft-assistant-stdin", move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&data);
        }
    })?;
    spawn_detached("deckcraft-assistant-stderr", move || {
        let mut stderr = stderr;
        let _ = io::copy(&mut stderr, &mut io::sink());
    })?;

    let cap = limits.stdout as u64;
    let mut out_handle: Option<JoinHandle<io::Result<Vec<u8>>>> = Some(spawn_named("deckcraft-assistant-stdout", move || {
        let mut buf = Vec::new();
        stdout.take(cap.saturating_add(1)).read_to_end(&mut buf)?;
        Ok(buf)
    })?);

    let deadline = Instant::now() + limits.deadline;
    let mut status: Option<ExitStatus> = None;
    let mut stdout_bytes: Option<Vec<u8>> = None;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        if status.is_none() {
            status = child.0.try_wait().map_err(|_| Error::Failed)?;
        }
        if out_handle.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = out_handle.take()
        {
            let bytes = handle.join().map_err(|_| Error::Failed)?.map_err(|_| Error::Failed)?;
            stdout_bytes = Some(bytes);
        }
        if let Some(bytes) = &stdout_bytes
            && bytes.len() as u64 > cap
        {
            return Err(Error::InvalidResponse);
        }
        // Bytes are taken only once the exit status is known; otherwise they stay for the next poll.
        if let Some(status) = status
            && let Some(bytes) = stdout_bytes.take()
        {
            return Ok(Captured { success: status.success(), stdout: bytes });
        }
        thread::sleep(POLL);
    }
}

fn spawn_detached<F>(name: &str, f: F) -> Result<(), Error>
where
    F: FnOnce() + Send + 'static,
{
    spawn_named(name, f).map(drop)
}

fn spawn_named<F, T>(name: &str, f: F) -> Result<JoinHandle<T>, Error>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new().name(name.to_owned()).spawn(f).map_err(|_| Error::Failed)
}

pub(crate) fn auth_limits() -> Limits {
    Limits { stdout: MAX_AUTH_OUTPUT_BYTES, deadline: AUTH_TIMEOUT }
}

pub(crate) fn generate_limits() -> Limits {
    Limits { stdout: MAX_OUTPUT_BYTES, deadline: GENERATE_TIMEOUT }
}
