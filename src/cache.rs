//! The global cache layout.
//!
//! Oyvey keeps a Cargo-style global cache under `~/.oyvey` (override with
//! `OYVEY_HOME`):
//!
//! ```text
//! ~/.oyvey/
//! └── git/
//!     ├── db/                  bare clones, one per source URL
//!     │   └── github.com-user-rak-net-a1b2c3d4
//!     └── checkouts/           one working tree per (source, rev)
//!         └── github.com-user-rak-net-a1b2c3d4/
//!             └── 0123456789abcdef.../
//! ```
//!
//! The db is the source of truth and is fetched on every install; checkouts
//! are keyed by the exact revision, so a locked build never depends on what
//! the default branch happens to point at. Vendored copies under
//! `<project>/packages/` are what the compiler actually reads.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::git;
use crate::OYVEY_HOME_ENV;

/// The global cache handle.
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// Open (creating if necessary) the global cache at the default location.
    pub fn open() -> Result<Cache> {
        let root = cache_root()?;
        Cache::open_at(root)
    }

    /// Open (creating if necessary) the global cache at an explicit root.
    pub fn open_at(root: PathBuf) -> Result<Cache> {
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating cache dir {}", root.display()))?;
        Ok(Cache { root })
    }

    /// The cache root.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// The bare-clone directory for a source URL.
    pub fn db_dir(&self, url: &str) -> PathBuf {
        self.root.join("git").join("db").join(repo_id(url))
    }

    /// The checkout directory for a (source url, rev) pair.
    ///
    /// Keyed by the first 12 hex characters of the revision. The full hash is
    /// still what gets recorded in the lockfile and what `ensure_checkout`
    /// checks out — this is only a cache key, and shortening it keeps the path
    /// inside Windows' 260-character limit once git adds
    /// `.git/objects/pack/pack-<sha>.idx` underneath.
    pub fn checkout_dir(&self, url: &str, rev: &str) -> PathBuf {
        self.root
            .join("git")
            .join("checkouts")
            .join(repo_id(url))
            .join(short_rev(rev))
    }

    /// Ensure the bare db for `url` exists and is up to date, then return its
    /// path. Clones on first use, fetches otherwise.
    pub fn ensure_db(&self, url: &str) -> Result<PathBuf> {
        let db = self.db_dir(url);
        if self.db_is_usable(&db) {
            git::fetch(&db)?;
        } else {
            // A db directory git does not recognise is left over from a clone that
            // never finished: `git clone` creates the destination *before* it
            // transfers anything, so an interrupt, a dropped connection or a full
            // disk leaves one behind. `clone_bare` refuses to write into an existing
            // directory, which used to make that state permanent -- every later run
            // failed with "cache db already exists" and the only way out was to
            // delete `~/.oyvey` by hand.
            //
            // Removing it is safe and is the only thing that un-bricks the cache:
            // the path is inside our own cache root and holds a clone of a public
            // repository that can simply be fetched again.
            if db.exists() {
                std::fs::remove_dir_all(&db).with_context(|| {
                    format!(
                        "removing the unusable cache db at {} (a previous clone did \
                         not finish); delete it by hand if this persists",
                        db.display()
                    )
                })?;
            }
            git::clone_bare(url, &db)?;
        }
        Ok(db)
    }

    /// Whether `db` is a git repository git is willing to work with.
    ///
    /// Asks git rather than probing the filesystem for `HEAD`. A bare repository
    /// keeps `HEAD` inside its git directory, so the old check was right only by
    /// accident, and it could not tell a finished clone from an interrupted one.
    fn db_is_usable(&self, db: &Path) -> bool {
        db.exists() && git::is_git_dir(db).unwrap_or(false)
    }

    /// Ensure a checkout for `(url, rev)` exists and return its path. Reuses
    /// an existing checkout; exports from the db otherwise.
    ///
    /// The reuse check comes *first*. It used to follow `ensure_db`, which fetches
    /// unconditionally, so the fast path could never rescue an offline run -- and
    /// the README's claim that a resolved project "works offline" was false for
    /// `install`, `update`, `add` and `lock`.
    pub fn ensure_checkout(&self, url: &str, rev: &str) -> Result<PathBuf> {
        let co = self.checkout_dir(url, rev);
        if co.join(crate::MANIFEST_FILE).is_file() {
            // Everything needed is already on disk: no network, and in particular no
            // fetch. This is what makes an offline install possible.
            return Ok(co);
        }
        let db = self.ensure_db(url)?;
        git::export_tree(&db, rev, &co)?;
        Ok(co)
    }
}

/// The default cache root: `$OYVEY_HOME` or `~/.oyvey`.
pub fn cache_root() -> Result<PathBuf> {
    if let Ok(home) = std::env::var(OYVEY_HOME_ENV) {
        if !home.trim().is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    let home = home_dir().context("locating home directory (set OYVEY_HOME to override)")?;
    Ok(home.join(".oyvey"))
}

/// A stable, filesystem-safe on-disk id for a source URL.
///
/// The id has to survive being a directory name on both platforms, which rules
/// out the characters Windows forbids (`<>:"|?*` and control characters) — a
/// `file:///C:/...` or an `ssh://` URL contains a colon. The human-readable
/// part is therefore also truncated: cache paths nest several directories deep
/// and Windows still applies a 260-character limit to the *whole* path. The
/// trailing hash keeps truncated ids distinct.
pub fn repo_id(url: &str) -> String {
    let normalized = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        // Strip the scheme entirely (`https://`, `ssh://`, `file://`, ...).
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| url.trim_end_matches('/').trim_end_matches(".git"))
        .replace('\\', "/");
    let mut slug: String = normalized
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs of separators so `https://a/b` and `https://a//b` agree.
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let trimmed = slug.trim_matches('-').to_string();
    let slug: String = if trimmed.chars().count() > 60 {
        trimmed.chars().take(60).collect()
    } else {
        trimmed
    };

    let mut h = Sha256::new();
    h.update(normalized.as_bytes());
    let hash = h.finalize();
    let short: String = hash.iter().take(4).map(|b| format!("{:02x}", b)).collect();
    if slug.is_empty() {
        format!("repo-{}", short)
    } else {
        format!("{}-{}", slug, short)
    }
}

/// The user's home directory, cross-platform.
pub fn home_dir() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    if let Ok(h) = std::env::var("USERPROFILE") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    None
}

/// A short, path-safe form of a git revision, for use as a cache key.
pub fn short_rev(rev: &str) -> String {
    let cleaned: String = rev
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if cleaned.is_empty() {
        return "rev".to_string();
    }
    cleaned.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_is_stable_and_distinct() {
        let a = repo_id("https://github.com/user/rak-net.git");
        let b = repo_id("https://github.com/user/rak-net");
        let c = repo_id("https://github.com/user/rak-crypto.git");
        assert_eq!(a, b, "trailing .git should not change the id");
        assert_ne!(a, c);
        assert!(a.starts_with("github.com-user-rak-net-"));
    }

    #[test]
    fn repo_id_is_filesystem_safe() {
        // A colon is what a `file://` or `ssh://` URL carries, and Windows
        // refuses to create a file whose name contains one. Long URLs also have
        // to be bounded, because cache paths nest several directories deep
        // under a 260-character limit.
        let id = repo_id("file:///C:/Users/someone/AppData/Local/Temp/pkg");
        for c in id.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.',
                "illegal character {:?} in {:?}",
                c,
                id
            );
        }
        let long = repo_id(&format!("https://example.invalid/{}", "a".repeat(400)));
        // 60-char slug + '-' + 8 hex chars of hash.
        assert!(long.chars().count() <= 69, "id should be bounded: {}", long);
    }

    #[test]
    fn cache_root_respects_env() {
        std::env::set_var(OYVEY_HOME_ENV, "C:\\tmp\\oyvey-test-home");
        let root = cache_root().unwrap();
        assert_eq!(root, PathBuf::from("C:\\tmp\\oyvey-test-home"));
        std::env::remove_var(OYVEY_HOME_ENV);
    }
}
