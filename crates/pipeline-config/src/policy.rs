//! Glob-based policy resolution.
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 3,
//! `policies` are evaluated in order; first match wins. If no policy matches,
//! the workspace `sandbox.default` is used.

use crate::config::{Policy, RageConfig, SandboxMode};
use std::path::Path;

/// Resolve the sandbox mode that applies to a package at `pkg_relative_path`.
///
/// `pkg_relative_path` is the package directory relative to the workspace root,
/// using forward slashes (e.g. `packages/core`).
///
/// Algorithm:
///   1. Iterate `config.policies` in order.
///   2. Return the first policy whose `selector` glob matches the path.
///   3. If none match, return `config.sandbox.default`.
pub fn resolve_sandbox_mode(config: &RageConfig, pkg_relative_path: &Path) -> SandboxMode {
    let path_str = pkg_relative_path.to_string_lossy().replace('\\', "/");
    for policy in &config.policies {
        if let Some(mode) = matches_policy(policy, &path_str) {
            return mode;
        }
    }
    config.sandbox.default.clone()
}

fn matches_policy(policy: &Policy, path: &str) -> Option<SandboxMode> {
    let glob = globset::Glob::new(&policy.selector).ok()?;
    let matcher = glob.compile_matcher();
    if matcher.is_match(path) {
        return policy.sandbox.clone();
    }
    // Also try with a trailing slash so that selectors like "packages/foo/**"
    // match the directory path "packages/foo" itself (not just its children).
    if !path.ends_with('/') && matcher.is_match(format!("{}/", path)) {
        return policy.sandbox.clone();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CacheConfig, SandboxConfig};
    use std::collections::HashMap;

    fn cfg(default: SandboxMode, policies: Vec<Policy>) -> RageConfig {
        RageConfig {
            plugins: vec![],
            sandbox: SandboxConfig { default },
            cache: CacheConfig::default(),
            policies,
            plugins_config: HashMap::new(),
        }
    }

    #[test]
    fn falls_back_to_default_when_no_policies() {
        let config = cfg(SandboxMode::Observed, vec![]);
        let result = resolve_sandbox_mode(&config, Path::new("packages/core"));
        assert_eq!(result, SandboxMode::Observed);
    }

    #[test]
    fn matches_first_policy() {
        let config = cfg(
            SandboxMode::Observed,
            vec![Policy {
                selector: "packages/core/**".to_string(),
                sandbox: Some(SandboxMode::Strict),
            }],
        );
        let result = resolve_sandbox_mode(&config, Path::new("packages/core/x"));
        assert_eq!(result, SandboxMode::Strict);
    }

    #[test]
    fn first_match_wins() {
        let config = cfg(
            SandboxMode::Observed,
            vec![
                Policy {
                    selector: "packages/legacy/**".to_string(),
                    sandbox: Some(SandboxMode::Loose),
                },
                Policy {
                    selector: "packages/**".to_string(),
                    sandbox: Some(SandboxMode::Strict),
                },
            ],
        );
        assert_eq!(
            resolve_sandbox_mode(&config, Path::new("packages/legacy/old")),
            SandboxMode::Loose
        );
        assert_eq!(
            resolve_sandbox_mode(&config, Path::new("packages/core/new")),
            SandboxMode::Strict
        );
    }

    #[test]
    fn no_match_falls_back_to_default() {
        let config = cfg(
            SandboxMode::Loose,
            vec![Policy {
                selector: "packages/core/**".to_string(),
                sandbox: Some(SandboxMode::Strict),
            }],
        );
        let result = resolve_sandbox_mode(&config, Path::new("apps/web"));
        assert_eq!(result, SandboxMode::Loose);
    }
}
