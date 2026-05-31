//! Minimal JSON API client.
//!
//! For now this shells out to `curl` (as Homebrew itself does) to fetch a single formula's JSON
//! from `{HOMEBREW_API_DOMAIN}/formula/{name}.json`. The full cached, JWS-verified bulk client
//! described in `specs/json-api.md` lands with the installer.

use crate::error::{FerroError, Result};
use crate::formula::RawFormula;

/// The default public API domain.
pub const DEFAULT_API_DOMAIN: &str = "https://formulae.brew.sh/api";

/// The configured API domain, honouring `HOMEBREW_API_DOMAIN`.
pub fn api_domain() -> String {
    std::env::var("HOMEBREW_API_DOMAIN")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_API_DOMAIN.to_string())
}

/// Fetch and parse a single formula's JSON from the API.
pub fn fetch_formula(name: &str) -> Result<RawFormula> {
    let url = format!("{}/formula/{name}.json", api_domain());

    let output = std::process::Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", &url])
        .output()
        .map_err(|e| FerroError::Other(format!("failed to run curl: {e}")))?;

    if !output.status.success() {
        return Err(FerroError::NotFound(format!(
            "formula {name:?} (via {url})"
        )));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|e| FerroError::Other(format!("parsing formula {name:?}: {e}")))
}
