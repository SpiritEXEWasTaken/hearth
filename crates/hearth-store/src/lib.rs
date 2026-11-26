//! Hearth's app store: the entry format, install options and variants, the store's checks, and
//! the signed catalog. Used by Hearth itself and by the `hearth-store` tool in the store's CI.

pub mod catalog;
pub mod lint;
pub mod merge;
pub mod model;
pub mod resolve;
pub mod site;

/// Where Hearth reads the official catalog.
pub const DEFAULT_URL: &str = "https://spiritexewastaken.github.io/hearth-store/";

/// Public keys trusted for the official catalog (minisign, base64): the CI key that signs every
/// publish, and the offline backup key used only to replace a leaked CI key.
pub const TRUSTED_KEYS: &[&str] = &[
    // CI key (GitHub Actions secret in SpiritEXEWasTaken/hearth-store)
    "RWTWSQUtgaZPIQiG4tQYOopKnKwlIN1Y+DCCbuE9PkQt5c+H5/OWAahF",
    // Offline backup key (kept off GitHub)
    "RWRdJK1dj7xZ04jfXTNP7s60BwNXKvyHhaf/Q23HWh3n7Djz5iTmkZyf",
];
