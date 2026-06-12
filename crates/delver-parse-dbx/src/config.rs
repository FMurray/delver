//! Environment-driven configuration for the ai_parse_document backend
//! (DA-007). Resolution is strict and fail-loud: a missing piece of config
//! produces one error naming exactly the variables that are required.
//!
//! Variables:
//! * `DATABRICKS_HOST` + `DATABRICKS_TOKEN` — workspace URL and PAT; OR
//! * `DELVER_DBX_PROFILE` — the name of a `~/.databrickscfg` profile whose
//!   `host`/`token` keys supply the two values (PAT profiles only; OAuth
//!   `auth_type` profiles carry no token and are rejected by name).
//! * `DELVER_DBX_WAREHOUSE_ID` — SQL warehouse for Statement Execution.
//! * `DELVER_DBX_VOLUME` — Unity Catalog volume directory for the temp
//!   upload (e.g. `/Volumes/main/default/scratch`).
//!
//! Explicit `DATABRICKS_HOST`/`DATABRICKS_TOKEN` take precedence over the
//! profile. Nothing here ever contacts a workspace.

use crate::ParseDbxError;

#[derive(Debug, Clone)]
pub struct DbxConfig {
    /// Workspace base URL, normalized to `https://…` without trailing slash.
    pub host: String,
    pub token: String,
    pub warehouse_id: String,
    /// `/Volumes/<catalog>/<schema>/<volume>[/dir]`, no trailing slash.
    pub volume: String,
}

impl DbxConfig {
    /// Read configuration from the process environment (and, when
    /// `DELVER_DBX_PROFILE` is set, `~/.databrickscfg`).
    pub fn from_env() -> Result<Self, ParseDbxError> {
        Self::from_lookup(&|key| std::env::var(key).ok(), &read_databrickscfg)
    }

    /// Pure-resolvable variant for tests: `env` looks up environment
    /// variables, `read_cfg` returns the `~/.databrickscfg` contents.
    pub fn from_lookup(
        env: &dyn Fn(&str) -> Option<String>,
        read_cfg: &dyn Fn() -> Result<String, ParseDbxError>,
    ) -> Result<Self, ParseDbxError> {
        let non_empty = |key: &str| env(key).filter(|v| !v.trim().is_empty());

        let mut missing: Vec<&str> = Vec::new();

        let host_env = non_empty("DATABRICKS_HOST");
        let token_env = non_empty("DATABRICKS_TOKEN");
        let profile = non_empty("DELVER_DBX_PROFILE");

        let (host, token) = match (host_env, token_env) {
            (Some(host), Some(token)) => (Some(host), Some(token)),
            (host, token) => match &profile {
                Some(profile_name) => {
                    let (profile_host, profile_token) =
                        profile_host_token(&read_cfg()?, profile_name)?;
                    // Explicit env vars win over profile values per key.
                    (host.or(Some(profile_host)), token.or(Some(profile_token)))
                }
                None => (host, token),
            },
        };
        if host.is_none() {
            missing.push("DATABRICKS_HOST (or DELVER_DBX_PROFILE)");
        }
        if token.is_none() {
            missing.push("DATABRICKS_TOKEN (or DELVER_DBX_PROFILE)");
        }

        let warehouse_id = non_empty("DELVER_DBX_WAREHOUSE_ID");
        if warehouse_id.is_none() {
            missing.push("DELVER_DBX_WAREHOUSE_ID");
        }
        let volume = non_empty("DELVER_DBX_VOLUME");
        if volume.is_none() {
            missing.push("DELVER_DBX_VOLUME");
        }

        if !missing.is_empty() {
            return Err(ParseDbxError(format!(
                "ai-parse engine is not configured; missing: {}. Required: \
                 DATABRICKS_HOST + DATABRICKS_TOKEN (or DELVER_DBX_PROFILE \
                 naming a ~/.databrickscfg PAT profile), DELVER_DBX_WAREHOUSE_ID \
                 (SQL warehouse id), DELVER_DBX_VOLUME (Unity Catalog volume \
                 path for the temp upload, e.g. /Volumes/main/default/scratch)",
                missing.join(", ")
            )));
        }

        let volume = volume.expect("checked above");
        if !volume.starts_with("/Volumes/") {
            return Err(ParseDbxError(format!(
                "DELVER_DBX_VOLUME must be a Unity Catalog volume path starting \
                 with /Volumes/, got {volume:?}"
            )));
        }

        Ok(Self {
            host: normalize_host(&host.expect("checked above")),
            token: token.expect("checked above"),
            warehouse_id: warehouse_id.expect("checked above"),
            volume: volume.trim_end_matches('/').to_string(),
        })
    }
}

/// Normalize a workspace host to `https://host` without a trailing slash
/// (same tolerance as delver-embed's endpoint resolution).
fn normalize_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{host}")
    }
}

/// Read `~/.databrickscfg` (fail-loud when HOME or the file is absent).
fn read_databrickscfg() -> Result<String, ParseDbxError> {
    let home = std::env::var("HOME")
        .map_err(|_| ParseDbxError("HOME is not set; cannot locate ~/.databrickscfg".into()))?;
    let path = std::path::Path::new(&home).join(".databrickscfg");
    std::fs::read_to_string(&path).map_err(|e| {
        ParseDbxError(format!(
            "DELVER_DBX_PROFILE is set but {} cannot be read: {e}",
            path.display()
        ))
    })
}

/// Extract `host` and `token` from one INI profile of a `.databrickscfg`
/// file. Minimal INI subset: `[section]` headers, `key = value` lines, `#`/
/// `;` comments. OAuth profiles (no `token` key) are rejected by name so the
/// error explains itself.
pub fn profile_host_token(cfg: &str, profile: &str) -> Result<(String, String), ParseDbxError> {
    let mut in_profile = false;
    let mut found_profile = false;
    let mut host: Option<String> = None;
    let mut token: Option<String> = None;

    for raw_line in cfg.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_profile = section.trim() == profile;
            found_profile |= in_profile;
            continue;
        }
        if !in_profile {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim(), value.trim());
            match key {
                "host" if !value.is_empty() => host = Some(value.to_string()),
                "token" if !value.is_empty() => token = Some(value.to_string()),
                _ => {}
            }
        }
    }

    if !found_profile {
        return Err(ParseDbxError(format!(
            "profile {profile:?} not found in ~/.databrickscfg"
        )));
    }
    let host = host.ok_or_else(|| {
        ParseDbxError(format!(
            "profile {profile:?} in ~/.databrickscfg has no `host` key"
        ))
    })?;
    let token = token.ok_or_else(|| {
        ParseDbxError(format!(
            "profile {profile:?} in ~/.databrickscfg has no `token` key \
             (OAuth/auth_type profiles are not supported; use a PAT profile \
             or set DATABRICKS_TOKEN directly)"
        ))
    })?;
    Ok((host, token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn no_cfg() -> Result<String, ParseDbxError> {
        Err(ParseDbxError("no ~/.databrickscfg in this test".into()))
    }

    #[test]
    fn full_env_config_resolves() {
        let env = env_of(&[
            ("DATABRICKS_HOST", "dbc-x.cloud.databricks.com/"),
            ("DATABRICKS_TOKEN", "dapi-secret"),
            ("DELVER_DBX_WAREHOUSE_ID", "abc123"),
            ("DELVER_DBX_VOLUME", "/Volumes/main/default/scratch/"),
        ]);
        let cfg = DbxConfig::from_lookup(&env, &no_cfg).expect("config resolves");
        assert_eq!(cfg.host, "https://dbc-x.cloud.databricks.com");
        assert_eq!(cfg.token, "dapi-secret");
        assert_eq!(cfg.warehouse_id, "abc123");
        assert_eq!(cfg.volume, "/Volumes/main/default/scratch");
    }

    #[test]
    fn missing_config_lists_every_missing_var() {
        let err = DbxConfig::from_lookup(&env_of(&[]), &no_cfg).expect_err("must fail");
        for needle in [
            "DATABRICKS_HOST",
            "DATABRICKS_TOKEN",
            "DELVER_DBX_PROFILE",
            "DELVER_DBX_WAREHOUSE_ID",
            "DELVER_DBX_VOLUME",
        ] {
            assert!(err.0.contains(needle), "error must name {needle}: {err}");
        }
    }

    #[test]
    fn partial_config_lists_only_missing_vars() {
        let env = env_of(&[
            ("DATABRICKS_HOST", "x.databricks.com"),
            ("DATABRICKS_TOKEN", "t"),
            ("DELVER_DBX_WAREHOUSE_ID", "w"),
        ]);
        let err = DbxConfig::from_lookup(&env, &no_cfg).expect_err("volume missing");
        assert!(err.0.contains("missing: DELVER_DBX_VOLUME"), "got: {err}");
    }

    #[test]
    fn volume_must_be_a_uc_volume_path() {
        let env = env_of(&[
            ("DATABRICKS_HOST", "x.databricks.com"),
            ("DATABRICKS_TOKEN", "t"),
            ("DELVER_DBX_WAREHOUSE_ID", "w"),
            ("DELVER_DBX_VOLUME", "/tmp/nope"),
        ]);
        let err = DbxConfig::from_lookup(&env, &no_cfg).expect_err("bad volume");
        assert!(err.0.contains("/Volumes/"), "got: {err}");
    }

    const CFG: &str = "\
# comment\n\
[DEFAULT]\n\
host = https://default.cloud.databricks.com\n\
auth_type = databricks-cli\n\
\n\
[pat-profile]\n\
host = https://pat.cloud.databricks.com\n\
token = dapi-from-profile\n\
\n\
[oauth-profile]\n\
host = https://oauth.cloud.databricks.com\n\
auth_type = databricks-cli\n";

    #[test]
    fn profile_supplies_host_and_token() {
        let env = env_of(&[
            ("DELVER_DBX_PROFILE", "pat-profile"),
            ("DELVER_DBX_WAREHOUSE_ID", "w"),
            ("DELVER_DBX_VOLUME", "/Volumes/m/d/v"),
        ]);
        let cfg = DbxConfig::from_lookup(&env, &|| Ok(CFG.to_string())).expect("profile resolves");
        assert_eq!(cfg.host, "https://pat.cloud.databricks.com");
        assert_eq!(cfg.token, "dapi-from-profile");
    }

    #[test]
    fn explicit_env_overrides_profile() {
        let env = env_of(&[
            ("DATABRICKS_HOST", "explicit.databricks.com"),
            ("DELVER_DBX_PROFILE", "pat-profile"),
            ("DELVER_DBX_WAREHOUSE_ID", "w"),
            ("DELVER_DBX_VOLUME", "/Volumes/m/d/v"),
        ]);
        let cfg = DbxConfig::from_lookup(&env, &|| Ok(CFG.to_string())).expect("resolves");
        assert_eq!(cfg.host, "https://explicit.databricks.com");
        assert_eq!(cfg.token, "dapi-from-profile");
    }

    #[test]
    fn oauth_profile_is_rejected_by_name() {
        let err = profile_host_token(CFG, "oauth-profile").expect_err("no token key");
        assert!(
            err.0.contains("oauth-profile") && err.0.contains("token"),
            "got: {err}"
        );
    }

    #[test]
    fn unknown_profile_is_an_error() {
        let err = profile_host_token(CFG, "nope").expect_err("unknown profile");
        assert!(err.0.contains("\"nope\" not found"), "got: {err}");
    }
}
