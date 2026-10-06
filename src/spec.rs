//! Dependency specs and version constraints.
//!
//! A dependency spec is `user/repo[@constraint][#rev]`:
//!
//! * `user/repo` — track the default branch (HEAD).
//! * `user/repo@^1.2` — caret: compatible with `1.2`. The leading non-zero
//!   component is fixed, so `^1.2` allows `1.9.9` but not `2.0.0`, and `^0.2`
//!   allows `0.2.9` but not `0.3.0`.
//! * `user/repo@~1.2` — tilde: compatible with `1.2.x`, so minor is fixed.
//! * `user/repo@1.2.3` — exact version.
//! * `user/repo@1.2.*` — wildcard.
//! * `user/repo@>=1.0, <2.0` — comparator ranges, comma-separated.
//! * `user/repo#deadbeef` — pin to an exact git revision (tag, branch, or
//!   commit hash). A `#rev` may be combined with a constraint
//!   (`user/repo@^1.2#deadbeef`); the rev wins.

/// A parsed dependency spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepSpec {
    /// The GitHub repository (`user/repo`).
    pub repo: String,
    /// The version constraint, if any.
    pub constraint: Option<String>,
    /// A pinned git revision, if any.
    pub rev: Option<String>,
}

/// Split a `user/repo[@vX.Y.Z | ^X | #rev]` spec into its parts.
pub fn parse_dep_spec(spec: &str) -> DepSpec {
    let mut repo = spec.trim().to_string();
    let mut version: Option<String> = None;
    let mut rev: Option<String> = None;

    if let Some(pos) = repo.find('@') {
        let v = repo[pos + 1..].to_string();
        repo.truncate(pos);
        // The version portion may itself carry a `#rev` (e.g. `@^1.2#deadbeef`).
        if let Some(hash) = v.find('#') {
            let r = v[hash + 1..].to_string();
            version = Some(v[..hash].trim().to_string());
            rev = Some(r.trim().to_string());
        } else {
            version = Some(v.trim().to_string());
        }
    }
    if rev.is_none() {
        if let Some(pos) = repo.find('#') {
            let r = repo[pos + 1..].to_string();
            repo.truncate(pos);
            rev = Some(r.trim().to_string());
        }
    }
    repo = repo.trim().to_string();
    DepSpec {
        repo,
        constraint: version,
        rev,
    }
}

/// A semantic version, with the pieces semver actually defines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// `alpha`, `rc.1`, ... An absent pre-release outranks any pre-release,
    /// which is what makes `1.0.0` newer than `1.0.0-rc.1`.
    pub pre: Option<String>,
}

impl Version {
    /// Parse a version, tolerating a leading `v`, a missing minor or patch, and
    /// a build-metadata suffix (`1.2.3+build` is the same version as `1.2.3`).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('v');
        // Build metadata does not affect precedence.
        let s = s.split('+').next().unwrap_or(s);
        let (core, pre) = match s.find('-') {
            Some(i) => (&s[..i], Some(s[i + 1..].to_string())),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        let patch = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        // A fourth numeric component means this is not a version we understand,
        // and guessing would be worse than refusing.
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    fn is_pre_release(&self) -> bool {
        self.pre.is_some()
    }
}

/// Compare two versions, ignoring build metadata.
fn cmp_versions(a: &Version, b: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.major, a.minor, a.patch).cmp(&(b.major, b.minor, b.patch)) {
        Ordering::Equal => {}
        other => return other,
    }
    match (&a.pre, &b.pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => x.cmp(y),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Exact,
    Greater,
    GreaterEq,
    Less,
    LessEq,
}

/// One comparator in a range: a lower bound, and optionally an exclusive
/// upper bound (which is how `^` and `~` are expressed).
#[derive(Clone, Debug)]
struct Comparator {
    op: Op,
    version: Version,
    /// `Some(v)` means "and strictly below `v`".
    upper_exclusive: Option<Version>,
}

impl Comparator {
    fn matches(&self, v: &Version) -> bool {
        use std::cmp::Ordering::*;
        let ord = cmp_versions(v, &self.version);
        let lower_ok = match self.op {
            Op::Exact => ord == Equal,
            Op::Greater => ord == Greater,
            Op::GreaterEq => ord != Less,
            Op::Less => ord == Less,
            Op::LessEq => ord != Greater,
        };
        if !lower_ok {
            return false;
        }
        match &self.upper_exclusive {
            Some(upper) => cmp_versions(v, upper) == Less,
            None => true,
        }
    }
}

/// The first version above every version `^v` allows, or `None` when `v` is
/// `0.0.0` (where a bare caret is unbounded).
fn caret_upper(v: &Version) -> Option<Version> {
    let next = |major, minor, patch| {
        Some(Version {
            major,
            minor,
            patch,
            pre: None,
        })
    };
    if v.major > 0 {
        next(v.major + 1, 0, 0)
    } else if v.minor > 0 {
        next(0, v.minor + 1, 0)
    } else if v.patch > 0 {
        next(0, 0, v.patch + 1)
    } else {
        None
    }
}

/// How many of `major.minor.patch` a version literal actually spelled out.
///
/// The distinction matters for a tilde: `~1.2` is bounded by `<1.3.0`, while
/// `~1` is bounded by `<2.0.0`. A tilde fixes the component *before* the last
/// one written, so with only a major there is nothing before it and the major
/// itself is what moves.
fn written_components(text: &str) -> usize {
    let core = text
        .trim()
        .trim_start_matches('v')
        .split('+')
        .next()
        .unwrap_or("");
    let core = core.split('-').next().unwrap_or(core);
    core.split('.').count()
}

/// The first version above every version `~v` allows.
fn tilde_upper(v: &Version, written: usize) -> Option<Version> {
    Some(if written <= 1 {
        Version {
            major: v.major + 1,
            minor: 0,
            patch: 0,
            pre: None,
        }
    } else {
        Version {
            major: v.major,
            minor: v.minor + 1,
            patch: 0,
            pre: None,
        }
    })
}

/// Parse one comparator: `1.2.3`, `^1.2`, `~1.2`, `>=1.0`, `=1.0`, `1.2.*`.
fn parse_comparator(text: &str) -> Result<Comparator, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty version comparator".to_string());
    }

    let at_least = |version, upper_exclusive| Comparator {
        op: Op::GreaterEq,
        version,
        upper_exclusive,
    };

    if let Some(rest) = text.strip_prefix('^') {
        let version = Version::parse(rest).ok_or_else(|| format!("cannot parse '{}'", text))?;
        return Ok(at_least(version.clone(), caret_upper(&version)));
    }
    if let Some(rest) = text.strip_prefix('~') {
        let written = written_components(rest);
        let version = Version::parse(rest).ok_or_else(|| format!("cannot parse '{}'", text))?;
        return Ok(at_least(version.clone(), tilde_upper(&version, written)));
    }

    let (op, rest) = if let Some(r) = text.strip_prefix(">=") {
        (Op::GreaterEq, r)
    } else if let Some(r) = text.strip_prefix("<=") {
        (Op::LessEq, r)
    } else if let Some(r) = text.strip_prefix('>') {
        (Op::Greater, r)
    } else if let Some(r) = text.strip_prefix('<') {
        (Op::Less, r)
    } else if let Some(r) = text.strip_prefix('=') {
        (Op::Exact, r)
    } else {
        (Op::Exact, text)
    };

    // A bare `1.2.*` is a wildcard over the patch.
    if let Some(core) = rest.trim().strip_suffix(".*") {
        let base = Version::parse(core).ok_or_else(|| format!("cannot parse '{}'", text))?;
        let upper = Version {
            major: base.major,
            minor: base.minor + 1,
            patch: 0,
            pre: None,
        };
        return Ok(Comparator {
            op: Op::GreaterEq,
            version: base,
            upper_exclusive: Some(upper),
        });
    }

    let version = Version::parse(rest).ok_or_else(|| format!("cannot parse '{}'", text))?;
    Ok(Comparator {
        op,
        version,
        upper_exclusive: None,
    })
}

/// Does `version` satisfy `constraint`?
///
/// An empty constraint or `*` matches anything. **Anything that cannot be
/// parsed is treated as not matching**, because the previous implementation
/// returned `true` for an unparseable constraint — so a typo like `^` silently
/// widened the requirement to "any version" and installed the newest tag.
pub fn version_satisfies(actual: &str, constraint: &str) -> bool {
    check_version(actual, constraint).unwrap_or(false)
}

/// The same check as [`version_satisfies`], reporting why it failed.
///
/// A `false` from the boolean form is ambiguous between "no such version" and
/// "that is not a constraint I understand", and the resolver needs to tell them
/// apart to say something useful, so this is the real implementation.
pub fn check_version(actual: &str, constraint: &str) -> Result<bool, String> {
    let c = constraint.trim();
    if c.is_empty() || c == "*" || c == "x" || c == "X" {
        // `*` means any *release*, not any tag. This used to return `true` outright,
        // which put `*` above the pre-release rule below rather than through it -- so
        // `@*` was the one constraint that would select `1.1.0-rc.1`, while `@^1.0`
        // correctly would not. That is the most likely constraint to pull an rc by
        // accident, since it is what someone writes when they mean "whatever is
        // current".
        //
        // Asked as "is this a pre-release" rather than by parsing first, so a tag form
        // that fails to parse still matches `*` exactly as it did before.
        let is_pre = Version::parse(actual).is_some_and(|v| v.is_pre_release());
        return Ok(!is_pre);
    }

    let version = Version::parse(actual)
        .ok_or_else(|| format!("cannot parse version '{}' (from a tag name)", actual))?;

    // Comma-separated comparators must all hold.
    let mut comparators: Vec<Comparator> = Vec::new();
    for part in c.split(',') {
        comparators.push(parse_comparator(part)?);
    }
    if comparators.is_empty() {
        return Err(format!("no version comparator in '{}'", constraint));
    }

    // A pre-release only satisfies a range that names a pre-release of the same
    // major.minor.patch. `1.1.0-rc.1` is greater than `1.0.0` and less than
    // `2.0.0`, so without this rule `^1.0` would select it -- and a constraint
    // written for the release would quietly install the release candidate.
    if version.is_pre_release() {
        let allowed = comparators.iter().any(|c| {
            c.version.pre.is_some()
                && c.version.major == version.major
                && c.version.minor == version.minor
                && c.version.patch == version.patch
        });
        if !allowed {
            return Ok(false);
        }
    }

    Ok(comparators.iter().all(|cmp| cmp.matches(&version)))
}

/// The git URL for a `user/repo` spec.
///
/// A bare `user/repo` is a GitHub repository. A spec that already looks like a
/// URL (`https://`, `ssh://`, `git@`, `file://`) is passed through unchanged,
/// which is what lets a project depend on a self-hosted Git server — and what
/// lets the test suite resolve real repositories without the network.
pub fn repo_url(repo: &str) -> String {
    let r = repo.trim();
    if r.contains("://") || r.starts_with("git@") {
        return r.to_string();
    }
    format!("https://github.com/{}.git", r)
}

/// The `user/repo` form of a URL, for display in the manifest.
pub fn repo_display(url: &str) -> String {
    url.strip_prefix("https://github.com/")
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_repo() {
        let s = parse_dep_spec("u/r");
        assert_eq!(s.repo, "u/r");
        assert_eq!(s.constraint, None);
        assert_eq!(s.rev, None);
    }

    #[test]
    fn caret_and_rev() {
        let s = parse_dep_spec("u/r@^1.2#deadbeef");
        assert_eq!(s.repo, "u/r");
        assert_eq!(s.constraint.as_deref(), Some("^1.2"));
        assert_eq!(s.rev.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn rev_only() {
        let s = parse_dep_spec("u/r#abc123");
        assert_eq!(s.repo, "u/r");
        assert_eq!(s.rev.as_deref(), Some("abc123"));
    }

    #[test]
    fn caret_excludes_the_next_major() {
        assert!(version_satisfies("1.2.3", "^1.2"));
        assert!(!version_satisfies("2.0.0", "^1.2"));
        assert!(version_satisfies("1.9.9", "^1.2"));
    }

    /// Caret on a 0.x version fixes the minor, which is the rule that stops
    /// `^0.2` from quietly pulling in `0.3.0`.
    #[test]
    fn caret_on_zero_fixes_the_minor() {
        assert!(version_satisfies("0.2.9", "^0.2"));
        assert!(!version_satisfies("0.3.0", "^0.2"));
        // `^0.2.5` is `>=0.2.5, <0.3.0` -- the patch is a floor, not a hint.
        assert!(version_satisfies("0.2.5", "^0.2.5"));
        assert!(!version_satisfies("0.2.0", "^0.2.5"));
        assert!(version_satisfies("0.2.9", "^0.2.5"));
    }

    /// The bug that motivated real semver: `>=1.0` parsed to `[0]` and matched
    /// nothing, silently, so a valid-looking constraint selected no version.
    #[test]
    fn comparison_operators_work() {
        assert!(version_satisfies("1.0.0", ">=1.0"));
        assert!(version_satisfies("9.9.9", ">=1.0"));
        assert!(!version_satisfies("0.9.9", ">=1.0"));
        assert!(version_satisfies("0.9.9", "<1.0"));
        assert!(version_satisfies("1.0.0", "=1.0.0"));
        assert!(!version_satisfies("1.0.1", "=1.0.0"));
    }

    #[test]
    fn comparator_ranges() {
        assert!(version_satisfies("1.5.0", ">=1.0, <2.0"));
        assert!(!version_satisfies("2.0.0", ">=1.0, <2.0"));
        assert!(!version_satisfies("0.9.0", ">=1.0, <2.0"));
    }

    /// The other half of that bug: an unparseable constraint used to report a
    /// match, so a typo widened the requirement to "anything".
    #[test]
    fn an_unparseable_constraint_is_an_error_not_a_match() {
        assert!(check_version("1.0.0", "^").is_err());
        assert!(check_version("1.0.0", "not-a-version").is_err());
        assert!(!version_satisfies("1.0.0", "^"));
    }

    #[test]
    fn wildcards() {
        assert!(version_satisfies("1.2.9", "1.2.*"));
        assert!(!version_satisfies("1.3.0", "1.2.*"));
    }

    /// `*` means any release, not any tag.
    ///
    /// The wildcard short-circuited before the version was parsed, so it sat *above* the
    /// pre-release rule rather than through it. `@*` was the one constraint that would
    /// install an rc, while `@^1.0` correctly would not -- and `@*` is exactly what
    /// someone writes when they mean "whatever is current", so it is the most likely
    /// constraint to pull one by accident.
    #[test]
    fn a_wildcard_does_not_select_a_pre_release() {
        for constraint in ["*", "x", "X", ""] {
            assert!(
                !version_satisfies("1.1.0-rc.1", constraint),
                "{:?} should not match a pre-release",
                constraint
            );
            assert!(
                !version_satisfies("2.0.0-beta.2", constraint),
                "{:?} should not match a pre-release",
                constraint
            );
            // A plain release is still what `*` is for.
            assert!(
                version_satisfies("1.0.0", constraint),
                "{:?} should match a release",
                constraint
            );
        }
    }

    /// Every constraint form now agrees about pre-releases.
    ///
    /// The inconsistency is the point: `@*` and `@^1.0` answering differently about the
    /// same tag is invisible until it installs the wrong thing.
    #[test]
    fn every_constraint_form_agrees_about_pre_releases() {
        let pre = "1.1.0-rc.1";
        for constraint in ["*", "^1.0", "~1.1", ">=1.0, <2.0", ">=1.0.0, <2.0.0"] {
            assert!(
                !version_satisfies(pre, constraint),
                "{:?} should exclude {}",
                constraint,
                pre
            );
        }
    }

    /// Naming a pre-release still works.
    ///
    /// Excluding them by default is only safe if there is a way to ask for one, and the
    /// existing rule already allowed a constraint naming the same major.minor.patch.
    #[test]
    fn a_named_pre_release_is_still_selectable() {
        assert!(version_satisfies("1.1.0-rc.1", "1.1.0-rc.1"));
        assert!(version_satisfies("1.1.0-rc.1", ">=1.1.0-rc.1, <2.0.0"));
        // ...and only for that exact version.
        assert!(!version_satisfies("1.1.0-rc.2", "1.1.0-rc.1"));
        assert!(!version_satisfies("1.2.0-rc.1", ">=1.1.0-rc.1, <2.0.0"));
    }

    /// A version `Version::parse` cannot read still matches `*`.
    ///
    /// The fix asks "is this a pre-release" rather than parsing first, precisely so no tag
    /// form that used to resolve stops resolving. Without this the change would trade one
    /// silent selection for a hard failure.
    #[test]
    fn an_unparseable_version_still_matches_a_wildcard() {
        assert!(version_satisfies("not-a-version", "*"));
        assert!(version_satisfies("", "*"));
    }

    /// `1.0.0-alpha` used to parse as `1.0.0`, so a pre-release tag could
    /// satisfy a constraint meant for the release.
    #[test]
    fn pre_releases_are_distinguished_from_releases() {
        let v = Version::parse("1.0.0-alpha").unwrap();
        assert_eq!(v.pre.as_deref(), Some("alpha"));
        // `^1.0` is >=1.0.0 and <2.0.0, so a pre-release of 1.1.0 is outside it.
        assert!(!version_satisfies("1.1.0-rc.1", "^1.0"));
        assert!(version_satisfies("1.0.0-rc.1", ">=1.0.0-rc.0, <1.0.0"));
        assert!(
            cmp_versions(
                &Version::parse("1.0.0").unwrap(),
                &Version::parse("1.0.0-rc.1").unwrap()
            ) == std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn build_metadata_is_ignored() {
        assert!(version_satisfies("1.0.0+build7", "1.0.0"));
        assert!(version_satisfies("1.0.0", "1.0.0+other"));
    }

    #[test]
    fn version_parse_rejects_nonsense() {
        assert!(Version::parse("not.a.version").is_none());
        assert!(Version::parse("1.2.3.4").is_none());
    }

    #[test]
    fn tilde_bounds_the_minor() {
        assert!(version_satisfies("1.2.9", "~1.2"));
        assert!(!version_satisfies("1.3.0", "~1.2"));
        // A tilde on a major-only version allows any minor: `~1` is
        // `>=1.0.0, <2.0.0`, not `<1.1.0`.
        assert!(version_satisfies("1.9.0", "~1"));
        assert!(!version_satisfies("2.0.0", "~1"));
        // `~1.2` does pin the minor.
        assert!(!version_satisfies("1.9.0", "~1.2"));
    }

    #[test]
    fn empty_and_star_match_everything() {
        assert!(version_satisfies("1.0.0", ""));
        assert!(version_satisfies("1.0.0", "*"));
    }
}
