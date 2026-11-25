//! Docker image reference parsing with Docker's normalisation rules
//! (`nginx` → `docker.io/library/nginx:latest`).

use crate::RegistryError;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef {
    /// Registry host as users write it: `docker.io`, `ghcr.io`, `lscr.io`, `localhost:5000`.
    pub registry: String,
    /// Repository path, e.g. `library/nginx`, `linuxserver/jellyfin`.
    pub repository: String,
    /// Tag; `latest` when none was given.
    pub tag: String,
    /// Pinned digest (`sha256:…`), if the reference had `@sha256:…`.
    pub digest: Option<String>,
}

pub const DOCKER_HUB: &str = "docker.io";

impl ImageRef {
    pub fn parse(s: &str) -> Result<Self, RegistryError> {
        let s = s.trim();
        if s.is_empty() || s.contains(char::is_whitespace) {
            return Err(RegistryError::InvalidReference(s.to_string()));
        }
        let (name_tag, digest) = match s.split_once('@') {
            Some((n, d)) => {
                if !d.starts_with("sha256:") && !d.starts_with("sha512:") {
                    return Err(RegistryError::InvalidReference(s.to_string()));
                }
                (n, Some(d.to_string()))
            }
            None => (s, None),
        };
        // The tag is after the last ':' only if that ':' is after the last '/' (else it's a port).
        let last_slash = name_tag.rfind('/');
        let (name, tag) = match name_tag.rfind(':') {
            Some(i) if last_slash.is_none_or(|j| i > j) => (&name_tag[..i], &name_tag[i + 1..]),
            _ => (name_tag, "latest"),
        };
        if name.is_empty() || tag.is_empty() {
            return Err(RegistryError::InvalidReference(s.to_string()));
        }
        let (registry, repository) = match name.split_once('/') {
            Some((first, rest)) if first.contains('.') || first.contains(':') || first == "localhost" => {
                (first.to_string(), rest.to_string())
            }
            _ => (DOCKER_HUB.to_string(), name.to_string()),
        };
        let registry = match registry.as_str() {
            "index.docker.io" | "registry-1.docker.io" => DOCKER_HUB.to_string(),
            _ => registry,
        };
        let repository = if registry == DOCKER_HUB && !repository.contains('/') {
            format!("library/{repository}")
        } else {
            repository
        };
        if repository.is_empty() || repository.contains("//") {
            return Err(RegistryError::InvalidReference(s.to_string()));
        }
        Ok(ImageRef {
            registry,
            repository: repository.to_ascii_lowercase(),
            tag: tag.to_string(),
            digest,
        })
    }

    /// Host to talk the registry API to.
    pub fn api_host(&self) -> &str {
        if self.registry == DOCKER_HUB {
            "registry-1.docker.io"
        } else {
            &self.registry
        }
    }

    /// `registry/repository` without tag or digest: the identity used for de-duplication.
    pub fn name(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }

    /// The same image, other tag.
    pub fn with_tag(&self, tag: &str) -> Self {
        ImageRef {
            tag: tag.to_string(),
            digest: None,
            ..self.clone()
        }
    }
}

impl fmt::Display for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}:{}", self.registry, self.repository, self.tag)?;
        if let Some(d) = &self.digest {
            write!(f, "@{d}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ImageRef {
        match ImageRef::parse(s) {
            Ok(r) => r,
            Err(e) => panic!("{s}: {e}"),
        }
    }

    #[test]
    fn docker_hub_normalisation() {
        let r = p("postgres");
        assert_eq!(
            (r.registry.as_str(), r.repository.as_str(), r.tag.as_str()),
            ("docker.io", "library/postgres", "latest")
        );
        let r = p("linuxserver/jellyfin:latest");
        assert_eq!(r.repository, "linuxserver/jellyfin");
        assert_eq!(r.api_host(), "registry-1.docker.io");
        let r = p(
            "docker.io/valkey/valkey:9@sha256:3eeb09785cd61ec8e3be35f8804c8892080f3ca21934d628abc24ee4ed1698f6",
        );
        assert_eq!(r.tag, "9");
        assert!(r.digest.as_deref().unwrap_or_default().starts_with("sha256:3eeb"));
        assert_eq!(
            p("index.docker.io/library/nginx:1").name(),
            "docker.io/library/nginx"
        );
    }

    #[test]
    fn other_registries() {
        let r = p("ghcr.io/immich-app/immich-server:v3");
        assert_eq!(
            (r.registry.as_str(), r.repository.as_str(), r.tag.as_str()),
            ("ghcr.io", "immich-app/immich-server", "v3")
        );
        let r = p("localhost:5000/foo");
        assert_eq!(
            (r.registry.as_str(), r.repository.as_str(), r.tag.as_str()),
            ("localhost:5000", "foo", "latest")
        );
        let r = p("registry.example.com:8443/team/app:1.2.3");
        assert_eq!(
            (r.registry.as_str(), r.tag.as_str()),
            ("registry.example.com:8443", "1.2.3")
        );
        let r = p("ghcr.io/immich-app/postgres:14-vectorchord0.4.3-pgvectors0.2.0");
        assert_eq!(r.tag, "14-vectorchord0.4.3-pgvectors0.2.0");
    }

    #[test]
    fn invalid() {
        for s in ["", "a b", "foo@md5:abc", "foo:", ":tag"] {
            assert!(ImageRef::parse(s).is_err(), "{s:?} should be invalid");
        }
    }

    #[test]
    fn display_roundtrip() {
        assert_eq!(p("nginx:1.27").to_string(), "docker.io/library/nginx:1.27");
        assert_eq!(p(&p("ghcr.io/a/b:v1").to_string()), p("ghcr.io/a/b:v1"));
    }
}
