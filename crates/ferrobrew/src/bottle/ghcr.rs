//! ghcr.io (GitHub Packages) constants and the `Authorization` header value used for bottle
//! downloads against the OCI Distribution v2 HTTP API.
//!
//! Mirrors the precedence `brew.sh:1118-1132` applies to compute and export
//! `HOMEBREW_GITHUB_PACKAGES_AUTH` before Ruby runs, plus the literal constants from
//! `github_packages.rb`. See `specs/download-ghcr.md` §1–§2.

/// `GitHubPackages::URL_DOMAIN`.
pub const URL_DOMAIN: &str = "ghcr.io";

/// `URL_PREFIX` (note the trailing slash).
pub const URL_PREFIX: &str = "https://ghcr.io/v2/";

/// `HOMEBREW_BOTTLE_DEFAULT_DOMAIN` (`brew.sh:638`).
pub const BOTTLE_DEFAULT_DOMAIN: &str = "https://ghcr.io/v2/homebrew/core";

/// The anonymous-access Bearer token literal (`QQ==` is base64 of ASCII `"A"`), `brew.sh:1131`.
pub const ANONYMOUS_AUTH: &str = "Bearer QQ==";

/// The OCI image-index `Accept` header attached to manifest requests.
pub const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json";

/// The default `HOMEBREW_GITHUB_PACKAGES_AUTH` value used for the registry `Authorization` header.
///
/// Reads the live process environment with the exact `brew.sh` precedence; standalone ferrobrew has
/// no `brew.sh` to pre-export it. Returns the empty string only for the explicit anonymous opt-out
/// (`HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN=none`).
pub fn github_packages_auth() -> String {
    resolve_github_packages_auth(|key| std::env::var(key).ok())
}

/// The testable core of [`github_packages_auth`]: resolve from an arbitrary environment lookup.
///
/// Precedence (`brew.sh:1118-1132`):
/// 1. If `HOMEBREW_GITHUB_PACKAGES_AUTH` is already set, honour it verbatim (this is what `brew.sh`
///    exports; allowing a direct override keeps parity for callers that pre-compute it).
/// 2. Else if `HOMEBREW_DOCKER_REGISTRY_TOKEN` is set: `"Bearer <token>"`.
/// 3. Else if `HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN` is set: `"none"` means anonymous (empty
///    string, i.e. unset); any other value is `"Basic <token>"`.
/// 4. Else the anonymous default `"Bearer QQ=="`.
pub fn resolve_github_packages_auth<F>(lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let get = |key: &str| lookup(key).filter(|v| !v.is_empty());

    if let Some(explicit) = get("HOMEBREW_GITHUB_PACKAGES_AUTH") {
        return explicit;
    }
    if let Some(token) = get("HOMEBREW_DOCKER_REGISTRY_TOKEN") {
        return format!("Bearer {token}");
    }
    if let Some(basic) = get("HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN") {
        if basic == "none" {
            return String::new();
        }
        return format!("Basic {basic}");
    }
    ANONYMOUS_AUTH.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn anonymous_default_is_bearer_qq() {
        assert_eq!(resolve_github_packages_auth(env(&[])), "Bearer QQ==");
        assert_eq!(ANONYMOUS_AUTH, "Bearer QQ==");
    }

    #[test]
    fn explicit_env_is_honoured_verbatim() {
        let auth = resolve_github_packages_auth(env(&[(
            "HOMEBREW_GITHUB_PACKAGES_AUTH",
            "Bearer precomputed",
        )]));
        assert_eq!(auth, "Bearer precomputed");
    }

    #[test]
    fn docker_registry_token_wins_over_basic() {
        let auth = resolve_github_packages_auth(env(&[
            ("HOMEBREW_DOCKER_REGISTRY_TOKEN", "tok123"),
            ("HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN", "base64creds"),
        ]));
        assert_eq!(auth, "Bearer tok123");
    }

    #[test]
    fn basic_auth_token_produces_basic_header() {
        let auth = resolve_github_packages_auth(env(&[(
            "HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN",
            "base64creds",
        )]));
        assert_eq!(auth, "Basic base64creds");
    }

    #[test]
    fn basic_auth_none_means_anonymous_empty() {
        let auth = resolve_github_packages_auth(env(&[(
            "HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN",
            "none",
        )]));
        assert_eq!(auth, "");
    }

    #[test]
    fn empty_values_are_treated_as_unset() {
        let auth = resolve_github_packages_auth(env(&[
            ("HOMEBREW_GITHUB_PACKAGES_AUTH", ""),
            ("HOMEBREW_DOCKER_REGISTRY_TOKEN", ""),
        ]));
        assert_eq!(auth, "Bearer QQ==");
    }
}
