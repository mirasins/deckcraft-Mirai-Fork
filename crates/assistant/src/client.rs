use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::{AuthState, Error, Request, Response};

#[cfg(not(target_arch = "wasm32"))]
use crate::protocol::{Source, parse_auth_status};

/// A writing-suggestion backend. [`Client`] (the official Claude Code CLI) is the only one: any
/// other backend must keep the rules in `docs/writing-assistant-development.md` (the user's own
/// sign-in, no keys stored by DeckCraft, document text as data) and needs the owner's approval.
pub trait Provider: Send + Sync {
    /// Checks the sign-in without sending any document text.
    fn auth(&self, cancel: &AtomicBool) -> Result<AuthState, Error>;
    /// Asks for suggestions for `request`. Validates the request first.
    fn generate(&self, request: &Request, cancel: &AtomicBool) -> Result<Response, Error>;
}

/// Handle to the official Claude Code CLI. Holds no credentials.
#[derive(Debug, Clone)]
pub struct Client {
    #[cfg(not(target_arch = "wasm32"))]
    executable: PathBuf,
    /// `--model` for generation; `None` uses Claude Code's own default.
    #[cfg(not(target_arch = "wasm32"))]
    model: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Client {
    /// Finds `claude` on the absolute entries of `PATH`. Reads no credentials and runs nothing.
    pub fn discover() -> Result<Self, Error> {
        let path = std::env::var_os("PATH").ok_or(Error::Unavailable)?;
        let name = if cfg!(windows) { "claude.exe" } else { "claude" };
        for dir in std::env::split_paths(&path) {
            if dir.is_absolute() {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Self::with_executable(candidate);
                }
            }
        }
        Err(Error::Unavailable)
    }

    /// Uses an explicit executable path chosen by the trusted host. The path must be absolute.
    /// On Windows it must be an `.exe`: a `.cmd`/`.bat` shim would run through `cmd.exe`, which
    /// can't take the JSON arguments safely.
    pub fn with_executable(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let executable = path.into();
        if !executable.is_absolute() || !executable.is_file() {
            return Err(Error::Unavailable);
        }
        if cfg!(windows) && !executable.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
            return Err(Error::Unavailable);
        }
        Ok(Self { executable, model: None })
    }

    /// Chooses the Claude model by alias or id (`sonnet`, `opus`, `claude-opus-5-5`…), passed to
    /// Claude Code as `--model`, so newer models work without a DeckCraft change. `None` or an
    /// empty name keeps Claude Code's default. Refuses names that could be read as a flag.
    pub fn with_model(mut self, model: Option<&str>) -> Result<Self, Error> {
        self.model = crate::protocol::model_name(model)?;
        Ok(self)
    }

    fn probe(&self, source: Source, cancel: &AtomicBool) -> Result<AuthState, Error> {
        let allow_oauth_token = source == Source::EnvToken;
        let out =
            crate::process::run_claude(&self.executable, crate::process::AUTH_ARGS, &[], crate::process::auth_limits(), cancel, allow_oauth_token)?;
        parse_auth_status(&out.stdout, source)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Provider for Client {
    /// Chooses the account mode: a valid stored session first, then the env OAuth token. Never a key.
    fn auth(&self, cancel: &AtomicBool) -> Result<AuthState, Error> {
        let stored = self.probe(Source::StoredSession, cancel);
        let token_present = crate::process::key_present(std::env::vars_os().map(|(key, _)| key), "CLAUDE_CODE_OAUTH_TOKEN");
        crate::protocol::select_auth(stored, token_present, || self.probe(Source::EnvToken, cancel))
    }

    /// Validates the request, re-checks auth immediately, then asks Claude for structured suggestions.
    /// Generation uses the same credential mode that the preflight chose.
    fn generate(&self, request: &Request, cancel: &AtomicBool) -> Result<Response, Error> {
        self.generate_with_auth(request, cancel).map(|(_, response)| response)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Client {
    /// [`Provider::generate`] that also returns the sign-in it checked, so a caller showing the
    /// connection needs no separate `auth` run (each run starts a Claude Code process).
    pub fn generate_with_auth(&self, request: &Request, cancel: &AtomicBool) -> Result<(AuthState, Response), Error> {
        request.validate()?;
        let payload = crate::protocol::prompt_payload(request)?;
        let mode = self.auth(cancel)?;
        let mut args: Vec<&str> = crate::process::GENERATION_ARGS.to_vec();
        if let Some(model) = &self.model {
            args.extend(["--model", model.as_str()]);
        }
        let out = crate::process::run_claude(
            &self.executable,
            &args,
            payload.as_bytes(),
            crate::process::generate_limits(),
            cancel,
            mode == AuthState::OAuthToken,
        )?;
        let parsed = crate::protocol::parse_response(&out.stdout, request.targets.len());
        match parsed {
            Ok(response) if out.success => Ok((mode, response)),
            Ok(_) => Err(Error::Failed),
            Err(error) => Err(error),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl Client {
    pub fn discover() -> Result<Self, Error> {
        Err(Error::Unavailable)
    }

    pub fn with_executable(_path: impl Into<PathBuf>) -> Result<Self, Error> {
        Err(Error::Unavailable)
    }

    pub fn with_model(self, model: Option<&str>) -> Result<Self, Error> {
        crate::protocol::model_name(model).map(|_| self)
    }
}

#[cfg(target_arch = "wasm32")]
impl Client {
    pub fn generate_with_auth(&self, _request: &Request, _cancel: &AtomicBool) -> Result<(AuthState, Response), Error> {
        Err(Error::Unavailable)
    }
}

#[cfg(target_arch = "wasm32")]
impl Provider for Client {
    fn auth(&self, _cancel: &AtomicBool) -> Result<AuthState, Error> {
        Err(Error::Unavailable)
    }

    fn generate(&self, _request: &Request, _cancel: &AtomicBool) -> Result<Response, Error> {
        Err(Error::Unavailable)
    }
}
