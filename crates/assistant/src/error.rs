/// Sanitized error categories. They never carry raw CLI output, stderr or slide text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("Mirai Assistant needs the Claude Code CLI, which is not available")]
    Unavailable,
    #[error("Mirai Assistant: Claude Code is not signed in; sign in with the official `claude` login flow")]
    Authentication,
    #[error("Mirai Assistant: Claude Code uses an authentication method DeckCraft does not allow")]
    UnsupportedAuth,
    #[error("Mirai Assistant could not reach the service")]
    Network,
    #[error("Mirai Assistant's usage quota or rate limit was reached")]
    Quota,
    #[error("Mirai Assistant timed out")]
    Timeout,
    #[error("the Mirai Assistant request was cancelled")]
    Cancelled,
    #[error("the text is too long for Mirai Assistant")]
    TooLong,
    #[error("the Mirai Assistant request contains unsupported content")]
    InvalidRequest,
    #[error("Mirai Assistant returned an invalid response")]
    InvalidResponse,
    #[error("Mirai Assistant failed")]
    Failed,
}
