//! OCI registry access for Hearth.
//!
//! Updates are found by asking the registry what the image's *own tag* points to now
//! (digest comparison), and by listing tags to suggest newer versions on the same track.
//! Nothing here ever decides to change a tag: it only reports.

pub mod client;
pub mod reference;
pub mod track;

pub use client::{Platform, RegistryClient, RemoteImage};
pub use reference::ImageRef;
pub use track::{TagTrack, TrackReport};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("invalid image reference: {0}")]
    InvalidReference(String),
    #[error("registry request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("registry returned {status} for {url}")]
    Status { status: u16, url: String },
    #[error("registry authentication failed: {0}")]
    Auth(String),
    #[error("unexpected registry response: {0}")]
    Protocol(String),
}
