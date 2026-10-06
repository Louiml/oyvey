//! Transitive dependency resolution.
//!
//! Given the root manifest, the resolver walks the dependency graph
//! depth-first, resolving each `user/repo[@constraint][#rev]` spec to an
//! exact git revision:
//!
//! * A pinned `#rev` always wins.
//! * Otherwise the newest `v*` tag satisfying the constraint is used.
//! * With no constraint (and no tags), the default-branch HEAD is used.
//!
//! A lockfile entry is reused when its source matches and its version still
//! satisfies the manifest constraint — that is what makes builds
//! reproducible. Cycles are tolerated (a package already on the current path
//! is skipped), and two sources providing the same package name is a conflict.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache::Cache;
use crate::git;
use crate::lock::LockFile;
use crate::manifest::{parse_manifest, Manifest, MANIFEST_FILE};
use crate::spec::{check_version, parse_dep_spec, DepSpec};

/// A fully resolved package.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The package name (from its own manifest).
    pub name: String,
    /// The version declared in its manifest.
    pub version: String,
    /// The source repository, as written in the manifest (`user/repo` or a URL).
    pub repo: String,
    /// The resolved git URL that was cloned.
    pub url: String,
    /// The exact git revision.
    pub rev: String,
    /// SHA-256 hex of the whole vendored directory.
    ///
    /// Filled in by `vendor_packages`, after the entry shim is written -- so it
    /// describes exactly the tree `oyvey audit` re-hashes. Hashing the cache checkout
    /// instead produced a different value, because vendoring adds a file.
    pub checksum: String,
    /// The package's own manifest (for transitive deps).
    pub manifest: Manifest,
    /// The checkout path in the global cache.
    pub checkout: std::path::PathBuf,
}

/// The resolver state.
pub struct Resolver<'a> {
    cache: &'a Cache,
    lock: &'a LockFile,
    entries: BTreeMap<String, Resolved>,
    visiting: Vec<String>,
}

impl<'a> Resolver<'a> {
    pub fn new(cache: &'a Cache, lock: &'a LockFile) -> Self {
        Resolver {
            cache,
            lock,
            entries: BTreeMap::new(),
            visiting: Vec::new(),
        }
    }

    /// Resolve every dependency of `root` (transitively), returning the
    /// entries sorted by name.
    pub fn resolve_root(&mut self, root: &Manifest) -> Result<Vec<Resolved>> {
        for (name, spec) in &root.deps {
            let spec = parse_dep_spec(spec);
            self.visit(name, &spec)
                .with_context(|| format!("resolving dependency '{}'", name))?;
        }
        Ok(self.entries.values().cloned().collect())
    }

    /// Resolve a single dependency and, recursively, its own dependencies.
    fn visit(&mut self, name: &str, spec: &DepSpec) -> Result<()> {
        if let Some(existing) = self.entries.get(name) {
            if existing.repo != spec.repo {
                bail!(
                    "dependency conflict: package '{}' is provided by both '{}' and '{}'",
                    name,
                    existing.repo,
                    spec.repo
                );
            }
            return Ok(());
        }
        if self.visiting.iter().any(|n| n == name) {
            // Cycle: the package is already being resolved further up the
            // stack. Its entry will be filled in by that visit.
            return Ok(());
        }
        self.visiting.push(name.to_string());

        let url = crate::spec::repo_url(&spec.repo);
        let rev = self.resolve_rev(name, spec, &url)?;
        let checkout = self.cache.ensure_checkout(&url, &rev)?;
        let manifest_path = checkout.join(MANIFEST_FILE);
        let manifest = parse_manifest(&manifest_path).map_err(|e| {
            anyhow::anyhow!(
                "dependency '{}' ({}) has an invalid {}: {}",
                name,
                spec.repo,
                MANIFEST_FILE,
                e
            )
        })?;
        // Left empty here and filled in by `vendor_packages`, which is the only place that
        // knows the final vendored tree. Hashing the cache checkout would produce a value
        // that never matches what `audit` verifies, because vendoring writes an entry shim.
        let checksum = String::new();

        self.entries.insert(
            name.to_string(),
            Resolved {
                name: name.to_string(),
                version: manifest.version.clone(),
                repo: spec.repo.clone(),
                url,
                rev,
                checksum,
                manifest: manifest.clone(),
                checkout,
            },
        );

        for (sub_name, sub_spec) in &manifest.deps {
            let sub_spec = parse_dep_spec(sub_spec);
            self.visit(sub_name, &sub_spec)
                .with_context(|| format!("resolving transitive dependency '{}'", sub_name))?;
        }

        self.visiting.pop();
        Ok(())
    }

    /// Put the resolver into offline mode.
    ///
    /// The cache is already offline-first, so this mostly matters when something is *not*
    /// on disk: without this, `--offline` would fetch and be a lie.
    pub fn set_offline(&self, offline: bool) {
        self.cache.set_offline(offline);
    }

    /// Determine the exact revision for a spec, preferring the lockfile.
    ///
    /// The lockfile is what makes a build reproducible: if it already records
    /// this source at a version that still satisfies the manifest constraint,
    /// that exact revision is reused rather than re-resolved to whatever the
    /// default branch points at today.
    fn resolve_rev(&self, name: &str, spec: &DepSpec, url: &str) -> Result<String> {
        // A pinned rev always wins, lockfile or not.
        if let Some(rev) = &spec.rev {
            return Ok(rev.clone());
        }
        if let Some(entry) = self.lock.get(name) {
            let locked_url = entry.source.strip_prefix("git+").unwrap_or(&entry.source);
            let (locked_base, locked_rev) = match locked_url.rsplit_once('#') {
                Some((base, rev)) => (base, rev),
                None => (locked_url, ""),
            };
            if locked_base == url
                && !locked_rev.is_empty()
                // A locked entry whose version no longer parses cannot be
                // reused either way, so a false here just means re-resolve.
                && check_version(
                    &entry.version,
                    spec.constraint.as_deref().unwrap_or(""),
                )
                .unwrap_or(false)
            {
                return Ok(locked_rev.to_string());
            }
        }
        self.fresh_rev(spec, url)
    }

    /// Resolve a spec to a revision straight from the remote.
    fn fresh_rev(&self, spec: &DepSpec, url: &str) -> Result<String> {
        let db = self.cache.ensure_db(url)?;
        let constraint = spec.constraint.as_deref().unwrap_or("");
        let tags =
            git::list_tags(&db).with_context(|| format!("listing tags for {}", spec.repo))?;
        // Validate the constraint once, before scanning tags, so an
        // unparseable one is reported as such rather than as "no matching
        // version" -- which points the reader at the wrong problem.
        if !constraint.is_empty() && constraint != "*" {
            if let Err(why) = check_version("0.0.0", constraint) {
                bail!(
                    "{}: invalid version constraint '{}': {}",
                    spec.repo,
                    constraint,
                    why
                );
            }
        }

        let found = tags
            .iter()
            .find(|t| check_version(t.trim_start_matches('v'), constraint).unwrap_or(false))
            .cloned();

        if let Some(tag) = found {
            return Ok(tag);
        }
        if constraint.is_empty() || constraint == "*" {
            return git::head_rev(&db).with_context(|| format!("resolving HEAD for {}", spec.repo));
        }
        bail!(
            "no version of {} satisfies constraint '{}' (found tags: {})",
            spec.repo,
            constraint,
            if tags.is_empty() {
                "none".to_string()
            } else {
                tags.join(", ")
            }
        )
    }
}

/// SHA-256 hex of a file's contents.
///
/// An unreadable file is an error rather than the hash of nothing. `unwrap_or_default`
/// produced the SHA-256 of an empty byte string, which is the well-known
/// `e3b0c442...b855` -- a value that appears in the lockfile format documentation as an
/// example. So a file that could not be read was indistinguishable from an empty one, and
/// since `audit` compares this against the recorded checksum, an unreadable file could
/// pass verification.
pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let data = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {}", path.display(), e))?;
    Ok(sha256_bytes(&data))
}

/// SHA-256 hex of a whole vendored package directory.
///
/// The lockfile recorded a hash of `package.rak` and nothing else, so every other file in
/// the package could be changed -- including the entry point that actually runs -- and
/// `oyvey audit` would still report "checksum verified". The manifest is metadata; this is
/// the code.
///
/// Each file contributes its path relative to `dir` and its bytes, in sorted path order, so
/// a rename is detected as well as an edit, and the result does not depend on the order the
/// filesystem happens to return directory entries in.
///
/// Skipped: the lockfile and `oyvey.toml`, which live outside the package, and `.git`,
/// which is not part of the source. Anything unreadable is an error -- a checksum that
/// silently covers less than it claims to is worse than none.
pub fn sha256_tree(dir: &Path) -> anyhow::Result<String> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_tree_files(dir, dir, &mut entries)?;
    entries.sort();

    let mut h = Sha256::new();
    for rel in &entries {
        h.update(sha256_bytes(rel.to_string_lossy().as_bytes()).as_bytes());
        let data = std::fs::read(dir.join(rel))
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", rel.display(), e))?;
        h.update(&data);
    }
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push_str(&format!("{:02x}", b));
    }
    Ok(s)
}

/// Collect files under `root`, storing each path relative to `base`.
fn collect_tree_files(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let read = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {}", dir.display(), e))?;
    for entry in read {
        let entry = entry
            .map_err(|e| anyhow::anyhow!("cannot read an entry in {}: {}", dir.display(), e))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| anyhow::anyhow!("cannot stat {}: {}", path.display(), e))?;
        if meta.is_dir() {
            collect_tree_files(base, &path, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(base).map_err(|_| {
                anyhow::anyhow!("{} is not under {}", path.display(), base.display())
            })?;
            out.push(rel.to_path_buf());
        }
        // A symlink is neither, and is skipped: following one could walk out of the tree,
        // and hashing the target would make the checksum depend on something the package
        // does not contain.
    }
    Ok(())
}

/// SHA-256 hex of a byte slice.
pub fn sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        // SHA-256 of the empty string.
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
