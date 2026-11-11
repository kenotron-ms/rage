//! Public data types used by the EcosystemPlugin trait.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A task that can run for a package — name, command template, glob hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDef {
    /// The task script name, e.g. `build`, `typecheck`, `test`.
    pub name: String,
    /// Shell command template the plugin would run.
    /// Variable substitution is not implemented in this phase.
    pub command_template: String,
    /// Globs (relative to package root) the task typically reads.
    pub input_globs: Vec<String>,
    /// Globs (relative to package root) the task writes.
    pub output_globs: Vec<String>,
}

/// A path pattern the toolchain is known to read but is not the package's
/// own input — e.g. `**/node_modules/typescript/**`. Excluded from the
/// "undeclared read" warning report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    pub path_pattern: String,
    pub reason: String,
}

/// Per-plugin configuration — extend/exclude lists, sourced from `rage.json`'s
/// `plugins_config.<plugin-name>` block.
///
/// Mirrors `pipeline_config::PluginConfig` but is owned by this crate so the
/// `plugin` crate has no dependency on `pipeline-config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub extend_input_globs: Vec<String>,
    pub exclude_input_globs: Vec<String>,
}

/// Reference to an emitted output file — used by `abi_fingerprint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub path: PathBuf,
}

/// Origin of a dependency edge between two workspace packages or tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeOrigin {
    /// Runtime `dependencies` or `peerDependencies` in `package.json`.
    Dependency,
    /// Development-only `devDependencies` in `package.json`.
    DevDependency,
    /// Explicit task pipeline rule (e.g. `dependsOn: ["^build"]`).
    PipelineDependsOn,
    /// Synthetic workspace-level root barrier (e.g. `workspace#install`).
    RootTask,
}

/// Edge-level policy returned by an ecosystem plugin or build-graph pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgePolicy {
    /// Keep the dependency edge as-is.
    Keep,
    /// Drop the dependency edge (e.g. `devDependency`-only `prepare` edge).
    Drop,
    /// Prune non-producer hops and replace this edge with direct edges to all
    /// transitive producer packages reachable through this dependency.
    ReplaceWithProducersReachableVia(Vec<String>),
}

/// Per-package virtual pathset emitted by a multi-package batch pass (such as
/// workspace-wide `prepare` / `compile` or GraphQL codegen).
///
/// Serialization is backwards-compatible with [`sandbox::PathSet`] /
/// `StoredPathset` (`reads` and `writes` are always present; optional v2
/// fields default cleanly when reading v1 JSON).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualPathset {
    /// Files read on behalf of this package.
    pub reads: Vec<PathBuf>,
    /// Files written on behalf of this package.
    pub writes: Vec<PathBuf>,
    /// Directories enumerated (`readdir`) during resolution for this package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dir_listings: Vec<PathBuf>,
    /// Paths probed for existence (`stat` / `access` returning ENOENT) during
    /// module resolution — if any of these paths is later created, the cache
    /// entry is invalidated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub absent_probes: Vec<PathBuf>,
    /// Optional BLAKE3 hex digest of the transitive closure of resolved symbol
    /// or fragment names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure_names_blake3: Option<String>,
    /// Optional BLAKE3 hex digest of the emitted ABI surface (e.g. `.d.ts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi_blake3: Option<String>,
}

/// Result of verifying the batch union soundness invariant:
/// `⋃_{p ∈ B} vPathset_p.reads ⊇ observed_reads \ allowlist`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchAuditResult {
    /// Non-allowlisted paths observed by the OS sandbox during the batch
    /// execution that were not attributed to any package's [`VirtualPathset`].
    pub unattributed_reads: Vec<PathBuf>,
}

impl BatchAuditResult {
    /// Returns `true` when every non-allowlisted observed read was covered by
    /// at least one package's [`VirtualPathset`].
    pub fn is_sound(&self) -> bool {
        self.unattributed_reads.is_empty()
    }
}

/// Verify the batch union soundness invariant:
///
/// `⋃_{p ∈ batch} virtual_pathsets[p].reads ⊇ batch_observed_reads \ allowlist`
///
/// Any read observed by the OS sandbox that is neither matched by `allowlist`
/// (or inside `node_modules`) nor claimed by at least one [`VirtualPathset`] is
/// returned in [`BatchAuditResult::unattributed_reads`] in sorted, deduplicated
/// order.
pub fn verify_batch_audit(
    batch_observed_reads: &[PathBuf],
    virtual_pathsets: &[VirtualPathset],
    allowlist: &[AllowlistEntry],
) -> BatchAuditResult {
    use globset::{Glob, GlobSetBuilder};
    use std::collections::HashSet;

    let claimed: HashSet<&PathBuf> = virtual_pathsets
        .iter()
        .flat_map(|vp| vp.reads.iter().chain(vp.dir_listings.iter()))
        .collect();

    let allow_set = if allowlist.is_empty() {
        None
    } else {
        let mut builder = GlobSetBuilder::new();
        for entry in allowlist {
            if let Ok(g) = Glob::new(&entry.path_pattern) {
                builder.add(g);
            }
        }
        builder.build().ok()
    };

    let mut unattributed: Vec<PathBuf> = batch_observed_reads
        .iter()
        .filter(|p| {
            // node_modules reads are pinned by the workspace lockfile.
            if p.components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new("node_modules"))
            {
                return false;
            }
            if let Some(set) = &allow_set {
                if set.is_match(p.as_path()) {
                    return false;
                }
            }
            !claimed.contains(p)
        })
        .cloned()
        .collect();

    unattributed.sort();
    unattributed.dedup();

    BatchAuditResult {
        unattributed_reads: unattributed,
    }
}

/// Specification of a workspace-wide or wave-level batch pass (such as
/// multi-package typechecking or GraphQL codegen) that processes multiple
/// packages in a single invocation and emits per-package [`VirtualPathset`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchPass {
    /// Identifier for the batch pass (e.g. `"batch-typecheck"`).
    pub id: String,
    /// Task script name this batch pass accelerates (e.g. `"prepare"` or `"compile"`).
    pub script_name: String,
    /// Command template used to invoke the batch pass.
    pub command: String,
    /// Package names participating in this batch invocation.
    pub packages: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskdef_roundtrips_serde() {
        let t = TaskDef {
            name: "build".to_string(),
            command_template: "tsc".to_string(),
            input_globs: vec!["src/**/*.ts".to_string()],
            output_globs: vec!["dist/**".to_string()],
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: TaskDef = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn plugin_config_default_is_empty() {
        let c = PluginConfig::default();
        assert!(c.extend_input_globs.is_empty());
        assert!(c.exclude_input_globs.is_empty());
    }

    #[test]
    fn virtual_pathset_batch_audit_passes_when_covered() {
        let observed = vec![
            PathBuf::from("/ws/packages/a/src/index.ts"),
            PathBuf::from("/ws/packages/b/src/util.ts"),
            PathBuf::from("/ws/node_modules/typescript/lib/lib.d.ts"),
            PathBuf::from("/ws/toolchains/node/bin/node"),
        ];
        let vps = vec![
            VirtualPathset {
                reads: vec![PathBuf::from("/ws/packages/a/src/index.ts")],
                writes: vec![PathBuf::from("/ws/packages/a/dist/index.d.ts")],
                ..Default::default()
            },
            VirtualPathset {
                reads: vec![PathBuf::from("/ws/packages/b/src/util.ts")],
                writes: vec![PathBuf::from("/ws/packages/b/dist/util.d.ts")],
                ..Default::default()
            },
        ];
        let allowlist = vec![AllowlistEntry {
            path_pattern: "**/toolchains/**".to_string(),
            reason: "managed toolchain binary".to_string(),
        }];

        let audit = verify_batch_audit(&observed, &vps, &allowlist);
        assert!(
            audit.is_sound(),
            "expected sound batch audit, got {audit:?}"
        );
    }

    #[test]
    fn virtual_pathset_batch_audit_reports_unattributed_reads() {
        let observed = vec![
            PathBuf::from("/ws/packages/a/src/index.ts"),
            PathBuf::from("/ws/packages/secret/undeclared.ts"),
        ];
        let vps = vec![VirtualPathset {
            reads: vec![PathBuf::from("/ws/packages/a/src/index.ts")],
            ..Default::default()
        }];

        let audit = verify_batch_audit(&observed, &vps, &[]);
        assert!(!audit.is_sound());
        assert_eq!(
            audit.unattributed_reads,
            vec![PathBuf::from("/ws/packages/secret/undeclared.ts")]
        );
    }

    #[test]
    fn virtual_pathset_backwards_compatible_with_v1_pathset_json() {
        let v1_json = r#"{"reads":["src/a.ts"],"writes":["dist/a.js"]}"#;
        let vp: VirtualPathset = serde_json::from_str(v1_json).unwrap();
        assert_eq!(vp.reads, vec![PathBuf::from("src/a.ts")]);
        assert_eq!(vp.writes, vec![PathBuf::from("dist/a.js")]);
        assert!(vp.dir_listings.is_empty());
        assert!(vp.absent_probes.is_empty());
        assert!(vp.closure_names_blake3.is_none());
        assert!(vp.abi_blake3.is_none());
    }
}
