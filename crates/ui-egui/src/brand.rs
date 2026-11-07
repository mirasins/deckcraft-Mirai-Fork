//! The Mirai fork's display name for the app.
//!
//! Upstream calls the app DeckCraft; the fork shows it as "Mirai Presentations" wherever the
//! interface names the app (window and title bar, About, Backstage, messages). Identifiers stay
//! upstream's: the binary, crate names, the app id, the `.deckcraft` format, the built-in
//! "DeckCraft" theme, the settings folder and the installer's registry keys, so settings, files and
//! installs carry over.

/// The name the fork shows for the app.
pub const APP_NAME: &str = "Mirai Presentations";
