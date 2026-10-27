/// Sanitized error categories. They never carry raw CLI output, stderr or document text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("the Claude Code CLI is not available")]
    Unavailable,
    #[error("Claude Code is not signed in; sign in with the official `claude` login flow")]
    Authentication,
    #[error("Claude Code uses an authentication method DeckCraft does not allow")]
    UnsupportedAuth,
    #[error("the assistant could not reach the service")]
    Network,
    #[error("the assistant's usage quota or rate limit was reached")]
    Quota,
    #[error("the assistant timed out")]
    Timeout,
    #[error("the assistant request was cancelled")]
    Cancelled,
    #[error("the text is too long for the assistant")]
    TooLong,
    #[error("the assistant request contains unsupported content")]
    InvalidRequest,
    #[error("the assistant returned an invalid response")]
    InvalidResponse,
    #[error("the assistant failed")]
    Failed,
}
