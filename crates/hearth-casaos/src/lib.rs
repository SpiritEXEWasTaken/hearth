//! Readers for CasaOS formats.
//!
//! - [`compose`]: a CasaOS compose file (installed app or store template): services, devices,
//!   ports, volumes, the `x-casaos` metadata block, and the variables it needs from outside.
//! - [`store`]: app store layouts: v1 sources (`Apps/<App>/docker-compose.yml`, also used by
//!   Big Bear) and v2 build output (`index.json` + `apps/<id>/docker-compose.yml`).
//!
//! Hearth only *reads* these formats (for migration and for seeding its own store); it never
//! writes CasaOS files.

pub mod compose;
pub mod store;

pub use compose::{ComposeFile, PortSpec, Service, VolumeSpec, XCasaos};
pub use store::{StoreLayout, StoreRead, StoreTemplate, read_store};

#[derive(Debug, thiserror::Error)]
pub enum CasaosError {
    #[error("YAML error in {path}: {source}")]
    Yaml {
        path: String,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}
