//! DeckCraft writing assistant.
//!
//! The assistant only runs the official, unmodified Claude Code CLI (`claude`) with the user's own
//! signed-in account (`claude.ai` or `oauth_token`). DeckCraft never reads, copies or stores
//! credentials, never implements an API-key or HTTP fallback, and never runs `claude login` itself.
//! Document text reaches the CLI only through stdin, as data.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod client;
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod process;
mod protocol;

pub use client::{Client, Provider};
pub use error::Error;
pub use protocol::model_name;
pub use protocol::{
    AuthState, DeckOperation, DeckProposal, GeometryTarget, MAX_DECK_COORDINATE, MAX_DECK_FONT_PT, MAX_DECK_OPERATIONS, MAX_EXPLANATION_BYTES,
    MAX_INSTRUCTION_BYTES, MAX_MODEL_BYTES, MAX_OUTPUT_BYTES, MAX_STRUCTURE_BYTES, MAX_TARGETS, MAX_TEXT_BYTES, Request, Response, Suggestion,
    TextStyle,
};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
