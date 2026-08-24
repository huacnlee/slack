pub mod actions;
pub mod activity;
pub mod app;
pub mod auth;
pub mod channel;
pub mod icons;
pub mod manifest;
pub mod notify;
pub mod people;
pub mod search;
pub mod theme;
pub mod time;
pub mod workspace;

/// Where this application keeps configuration a person may edit.
///
/// Re-exported so the binary can look for a `.env` beside the token without
/// depending on the API crate's module layout.
pub fn config_dir() -> std::path::PathBuf {
    slack_api::store::config_dir()
}
