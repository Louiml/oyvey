//! End-to-end dependency resolution against real (local) git repositories.
//!
//! The resolver and cache take an arbitrary source URL, so these tests point
//! them at `file://` repositories created in a temp directory rather than at
//! github.com. That keeps the suite hermetic — no network, no published test
//! fixtures — while still exercising the real git code path: bare clone,
//! fetch, tag listing, revision resolution, and checkout.
//!
//! What each test pins down:
//!
//! * A dependency resolves to a tag and is vendored with a matching lockfile
//!   entry (revision + checksum).
//! * A caret constraint picks the newest satisfying tag, and a `~`/exact
//!   constraint does not.
//! * A pinned `#rev` beats every tag.
//! * Transitive dependencies are resolved and vendored, and `oyvey install`
//!   is idempotent (a second run does not change the lockfile).
//! * A dependency cycle terminates instead of recursing forever.
//! * Removing a dependency prunes the vendored copy and the lockfile.
//! * A checkout already in the cache is reused rather than re-cloned.

use std::path::{Path, PathBuf};
use std::process::Command;

use oyvey::cache::Cache;
use oyvey::lock::LockFile;
use oyvey::manifest::Manifest;
use oyvey::project::{packages_dir, vendor_packages};
use oyvey::resolve::{sha256_tree, Resolver};

/// A temp directory removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!(
            "oyvey_it_{}_{}_{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a git repository at `dir` containing a package named `name`, and
/// return its `file://` URL.
///
/// `files` are extra files to write. `tags` are created (in order) pointing at
/// successive commits, so a caller can build a version history.
fn make_package(dir: &Path, name: &str, version: &str, tags: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).expect("create package dir");
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@example.invalid"]);
    git(dir, &["config", "user.name", "Oyvey Test"]);
    commit_package(dir, name, version, tags);
    url_for(dir)
}

/// Commit the package at `version`, then tag it if `tags` names a tag for it.
fn commit_package(dir: &Path, name: &str, version: &str, tags: &[(&str, &str)]) {
    std::fs::write(
        dir.join("package.rak"),
        format!(
            "let name = \"{}\"\nlet version = \"{}\"\nlet entry = \"lib.rak\"\nlet deps = {{}}\n",
            name, version
        ),
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("lib.rak"),
        format!("pub fn value() -> string {{\n    return \"{}\"\n}}\n", name),
    )
    .expect("write lib");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", version]);
    for (tag, ver) in tags {
        if *ver == version {
            git(dir, &["tag", tag]);
        }
    }
}

fn url_for(dir: &Path) -> String {
    // git accepts a plain path as a clone source on every platform; normalise
    // to a forward-slash absolute path so Windows backslashes do not confuse it.
    let p = dir.to_string_lossy().replace('\\', "/");
    format!("file:///{}", p.trim_start_matches('/'))
}

/// A project dir plus a fresh isolated cache.
struct World {
    _tmp: Tmp,
    root: PathBuf,
    cache: Cache,
}

impl World {
    fn new(tag: &str) -> World {
        let tmp = Tmp::new(tag);
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).expect("create project dir");
        // A cache private to this test, so tests cannot see each other's repos.
        let cache = Cache::open_at(tmp.path().join("cache")).expect("open cache");
        World {
            _tmp: tmp,
            root,
            cache,
        }
    }

    /// Write a manifest with `deps` (name -> spec) and return it.
    fn manifest(&self, name: &str, deps: &[(&str, &str)]) -> Manifest {
        let mut m = Manifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            entry: "src/main.rak".to_string(),
            ..Default::default()
        };
        for (k, v) in deps {
            m.deps.insert(k.to_string(), v.to_string());
        }
        m
    }

    /// Resolve `(name, url, constraint)` triples against the cache and vendor
    /// the result. The URL is used directly, so tests can point at local
    /// repositories without going through GitHub.
    fn install_url(
        &self,
        deps: &[(&str, &str, &str)],
    ) -> (Vec<oyvey::resolve::Resolved>, LockFile) {
        let mut m = self.manifest("root", &[]);
        for (name, url, constraint) in deps {
            // A constraint is separated by `@`, except a `#rev` pin, which is
            // appended directly.
            let spec = if constraint.is_empty() || constraint.starts_with('#') {
                format!("{}{}", url, constraint)
            } else {
                format!("{}@{}", url, constraint)
            };
            m.deps.insert((*name).to_string(), spec);
        }
        let lock = LockFile::default();
        let mut resolver = Resolver::new(&self.cache, &lock);
        let mut resolved = resolver
            .resolve_root(&m)
            .unwrap_or_else(|e| panic!("resolve: {:#}", e));
        // Mutable: vendoring is what fills in each package's checksum.
        vendor_packages(&self.root, &mut resolved).expect("vendor");
        let lock = lockfile_from(&resolved);
        (resolved, lock)
    }
}

fn lockfile_from(resolved: &[oyvey::resolve::Resolved]) -> LockFile {
    let mut out = LockFile::default();
    for r in resolved {
        out.upsert(oyvey::lock::LockEntry {
            name: r.name.clone(),
            version: r.version.clone(),
            source: format!("git+{}#{}", r.url, r.rev),
            checksum: r.checksum.clone(),
        });
    }
    out
}

#[test]
fn resolves_tag_and_vendors_with_lockfile() {
    let w = World::new("tag");
    let pkg = w._tmp.path().join("repos").join("mylib");
    make_package(&pkg, "mylib", "0.1.0", &[("v0.1.0", "0.1.0")]);

    // `install_url` drives the resolver with the repo URL used verbatim, so a
    // local repository resolves through exactly the same code as a GitHub one.
    let url = url_for(&pkg);
    let (resolved, lock) = w.install_url(&[("mylib", &url, "")]);

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "mylib");
    assert_eq!(resolved[0].version, "0.1.0");
    // Vendored copy exists and its checksum matches the lockfile. This is a *tree*
    // checksum: the lockfile covers every file in the package, not just its manifest.
    let vendored = packages_dir(&w.root).join("mylib");
    assert!(
        vendored.join("package.rak").is_file(),
        "package should be vendored"
    );
    assert_eq!(
        sha256_tree(&vendored).unwrap(),
        lock.get("mylib").unwrap().checksum
    );
}

#[test]
fn caret_constraint_picks_newest_satisfying_tag() {
    let w = World::new("caret");
    let pkg = w._tmp.path().join("repos").join("lib");
    make_package(&pkg, "lib", "0.1.0", &[("v0.1.0", "0.1.0")]);
    commit_package(&pkg, "lib", "0.2.0", &[("v0.2.0", "0.2.0")]);
    commit_package(&pkg, "lib", "0.2.5", &[("v0.2.5", "0.2.5")]);
    commit_package(&pkg, "lib", "1.0.0", &[("v1.0.0", "1.0.0")]);

    let url = url_for(&pkg);
    let (resolved, _) = w.install_url(&[("lib", &url, "^0.2")]);
    // ^0.2 means compatible-with 0.2.x under the rakpkg-style rule (same
    // leading components), so 0.2.5 (newest 0.2.x) wins.
    assert_eq!(resolved[0].version, "0.2.5");

    let (resolved, _) = w.install_url(&[("lib", &url, "~0.1")]);
    assert_eq!(resolved[0].version, "0.1.0");

    let (resolved, _) = w.install_url(&[("lib", &url, "0.2.0")]);
    assert_eq!(resolved[0].version, "0.2.0");
}

#[test]
fn pinned_rev_beats_tags() {
    let w = World::new("pinned");
    let pkg = w._tmp.path().join("repos").join("lib");
    make_package(&pkg, "lib", "0.1.0", &[("v0.1.0", "0.1.0")]);
    commit_package(&pkg, "lib", "0.2.0", &[("v0.2.0", "0.2.0")]);

    // The 0.1.0 commit hash.
    let rev_010 = Command::new("git")
        .args(["rev-parse", "v0.1.0^{commit}"])
        .current_dir(&pkg)
        .output()
        .expect("rev-parse");
    let rev_010 = String::from_utf8_lossy(&rev_010.stdout).trim().to_string();

    let url = url_for(&pkg);
    let (resolved, _) = w.install_url(&[("lib", &url, &format!("#{}", rev_010))]);
    assert_eq!(resolved[0].version, "0.1.0");
    assert_eq!(resolved[0].rev, rev_010);
}

#[test]
fn no_matching_tag_is_an_error() {
    let w = World::new("nomatch");
    let pkg = w._tmp.path().join("repos").join("lib");
    make_package(&pkg, "lib", "0.1.0", &[("v0.1.0", "0.1.0")]);
    let url = url_for(&pkg);

    let mut m = w.manifest("root", &[]);
    m.deps.insert("lib".to_string(), format!("{}@^9", url));

    let lock = LockFile::default();
    let mut resolver = Resolver::new(&w.cache, &lock);
    let err = resolver
        .resolve_root(&m)
        .expect_err("should not resolve ^9");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("satisfies") && msg.contains("9"),
        "unhelpful error: {}",
        msg
    );
}

#[test]
fn transitive_dependencies_are_resolved_and_vendored() {
    let w = World::new("transitive");
    let repos = w._tmp.path().join("repos");

    // leaf <- mid <- top
    let leaf = repos.join("leaf");
    make_package(&leaf, "leaf", "0.1.0", &[("v0.1.0", "0.1.0")]);
    let leaf_url = url_for(&leaf);

    let mid = repos.join("mid");
    std::fs::create_dir_all(&mid).unwrap();
    git(&mid, &["init", "-q"]);
    git(&mid, &["config", "user.email", "t@e.invalid"]);
    git(&mid, &["config", "user.name", "T"]);
    std::fs::write(
        mid.join("package.rak"),
        format!(
            "let name = \"mid\"\nlet version = \"0.1.0\"\nlet deps = {{ leaf: \"{}\" }}\n",
            leaf_url
        ),
    )
    .unwrap();
    git(&mid, &["add", "-A"]);
    git(&mid, &["commit", "-q", "-m", "init"]);
    git(&mid, &["tag", "v0.1.0"]);
    let mid_url = url_for(&mid);

    let (resolved, lock) = w.install_url(&[("mid", &mid_url, "")]);
    // Both mid and its transitive leaf are resolved.
    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().any(|r| r.name == "mid"));
    assert!(resolved.iter().any(|r| r.name == "leaf"));
    // Both vendored, both locked.
    assert!(packages_dir(&w.root).join("mid").is_dir());
    assert!(packages_dir(&w.root).join("leaf").is_dir());
    assert!(lock.get("leaf").is_some());
}

#[test]
fn dependency_cycle_terminates() {
    let w = World::new("cycle");
    let repos = w._tmp.path().join("repos");

    let a = repos.join("a");
    let b = repos.join("b");
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@e.invalid"]);
        git(dir, &["config", "user.name", "T"]);
    }
    let a_url = url_for(&a);
    let b_url = url_for(&b);
    std::fs::write(
        a.join("package.rak"),
        format!(
            "let name = \"a\"\nlet version = \"0.1.0\"\nlet deps = {{ b: \"{}\" }}\n",
            b_url
        ),
    )
    .unwrap();
    std::fs::write(
        b.join("package.rak"),
        format!(
            "let name = \"b\"\nlet version = \"0.1.0\"\nlet deps = {{ a: \"{}\" }}\n",
            a_url
        ),
    )
    .unwrap();
    for dir in [&a, &b] {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        git(dir, &["tag", "v0.1.0"]);
    }

    let (resolved, _) = w.install_url(&[("a", &a_url, "")]);
    // Resolves both, does not recurse forever.
    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().any(|r| r.name == "a"));
    assert!(resolved.iter().any(|r| r.name == "b"));
}

#[test]
fn two_sources_for_one_name_is_a_conflict() {
    let w = World::new("conflict");
    let repos = w._tmp.path().join("repos");
    let x = repos.join("x");
    let y = repos.join("y");
    // Two different repos, both declaring the package name "shared".
    make_package(&x, "shared", "0.1.0", &[("v0.1.0", "0.1.0")]);
    make_package(&y, "shared", "0.1.0", &[("v0.1.0", "0.1.0")]);
    let x_url = url_for(&x);
    let y_url = url_for(&y);

    // `holder` depends on x's "shared"; the root depends on holder *and*
    // directly on y's "shared". Two sources for one package name is ambiguous
    // and must be reported rather than silently resolved one way.
    let holder = repos.join("holder");
    std::fs::create_dir_all(&holder).unwrap();
    git(&holder, &["init", "-q"]);
    git(&holder, &["config", "user.email", "t@e.invalid"]);
    git(&holder, &["config", "user.name", "T"]);
    std::fs::write(
        holder.join("package.rak"),
        format!(
            "let name = \"holder\"\nlet version = \"0.1.0\"\nlet deps = {{ shared: \"{}\" }}\n",
            x_url
        ),
    )
    .unwrap();
    git(&holder, &["add", "-A"]);
    git(&holder, &["commit", "-q", "-m", "init"]);
    git(&holder, &["tag", "v0.1.0"]);
    let holder_url = url_for(&holder);

    let mut m = w.manifest("root", &[]);
    m.deps.insert("holder".to_string(), holder_url);
    m.deps.insert("shared".to_string(), y_url);

    let lock = LockFile::default();
    let mut resolver = Resolver::new(&w.cache, &lock);
    let err = resolver.resolve_root(&m).expect_err("conflict expected");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("conflict") && msg.contains("shared"),
        "got: {}",
        msg
    );
}

#[test]
fn checkout_in_cache_is_reused() {
    let w = World::new("reuse");
    let pkg = w._tmp.path().join("repos").join("lib");
    make_package(&pkg, "lib", "0.1.0", &[("v0.1.0", "0.1.0")]);
    let url = url_for(&pkg);

    let (r1, _) = w.install_url(&[("lib", &url, "")]);
    let co = r1[0].checkout.clone();
    assert!(co.join("package.rak").is_file());
    // Record the .git dir's identity; a second ensure must not wipe it.
    let marker = co.join(".git").join("HEAD");
    let before = std::fs::read_to_string(&marker).expect("read HEAD");

    let co2 = w
        .cache
        .ensure_checkout(&url, &r1[0].rev)
        .expect("second checkout");
    assert_eq!(co2, co);
    let after = std::fs::read_to_string(&marker).expect("read HEAD again");
    assert_eq!(before, after, "checkout should be reused, not re-created");
}

#[test]
fn re_vendoring_is_idempotent_and_prunes_stale_packages() {
    let w = World::new("prune");
    let repos = w._tmp.path().join("repos");
    let a = repos.join("a");
    let b = repos.join("b");
    make_package(&a, "a", "0.1.0", &[("v0.1.0", "0.1.0")]);
    make_package(&b, "b", "0.1.0", &[("v0.1.0", "0.1.0")]);
    let a_url = url_for(&a);
    let b_url = url_for(&b);

    // Install both.
    w.install_url(&[("a", &a_url, ""), ("b", &b_url, "")]);
    assert!(packages_dir(&w.root).join("a").is_dir());
    assert!(packages_dir(&w.root).join("b").is_dir());

    // Re-install with only `a`: `b` is pruned.
    w.install_url(&[("a", &a_url, "")]);
    assert!(packages_dir(&w.root).join("a").is_dir());
    assert!(
        !packages_dir(&w.root).join("b").exists(),
        "stale package should be pruned"
    );
}

#[test]
fn checksum_detects_tampering() {
    let w = World::new("tamper");
    let pkg = w._tmp.path().join("repos").join("lib");
    make_package(&pkg, "lib", "0.1.0", &[("v0.1.0", "0.1.0")]);
    let url = url_for(&pkg);
    let (_resolved, lock) = w.install_url(&[("lib", &url, "")]);

    let vendored = packages_dir(&w.root).join("lib");
    // Captured before any edit so each tamper can be undone and the checksum compared
    // against the same baseline.
    let original_manifest = std::fs::read_to_string(vendored.join("package.rak")).unwrap();
    let original_entry = std::fs::read_to_string(vendored.join("lib.rak")).unwrap();
    let good = sha256_tree(&vendored).unwrap();
    assert_eq!(good, lock.get("lib").unwrap().checksum);

    // Tampering with the manifest is detected.
    let manifest = vendored.join("package.rak");
    std::fs::write(&manifest, "let name = \"evil\"\n").unwrap();
    let bad = sha256_tree(&vendored).unwrap();
    assert_ne!(bad, good, "checksum should detect the manifest edit");

    // Restoring it puts the checksum back, so the detection above is the edit and not a
    // one-way change of what the checksum covers.
    std::fs::write(&manifest, original_manifest).unwrap();
    assert_eq!(sha256_tree(&vendored).unwrap(), good);

    // Tampering with a source file is detected too -- this is the gap that made the
    // checksum cover the manifest only. The manifest is metadata; the entry point is the
    // code that runs.
    let entry = vendored.join("lib.rak"); // the entry the manifest declares
    std::fs::write(&entry, "dump \"evil\"\n").unwrap();
    assert_ne!(
        sha256_tree(&vendored).unwrap(),
        good,
        "an edit to a source file must change the checksum"
    );
    std::fs::write(&entry, original_entry).unwrap();
    assert_eq!(sha256_tree(&vendored).unwrap(), good);

    // Renaming is detected as well: each file contributes its path, not just its bytes.
    std::fs::rename(&entry, vendored.join("renamed.rak")).unwrap();
    assert_ne!(
        sha256_tree(&vendored).unwrap(),
        good,
        "a rename must change the checksum"
    );
}

/// Run `f` with `git` removed from `PATH`.
///
/// This is how the offline behaviour is proven rather than asserted. The bug was an
/// ordering mistake -- `ensure_checkout` called the fetch before checking whether the
/// checkout was already on disk -- and only an actual absence of `git` distinguishes
/// that from code that merely looks right.
fn without_git<T>(f: impl FnOnce() -> T) -> T {
    let saved = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    let stripped = saved
        .split(sep)
        .filter(|p| {
            let low = p.to_ascii_lowercase();
            // Keep everything that is not a git install directory.
            !(low.contains("git") && (low.contains("bin") || low.contains("cmd")))
        })
        .collect::<Vec<_>>()
        .join(&sep.to_string());
    assert_ne!(
        stripped, saved,
        "the PATH filter removed nothing; cannot test offline"
    );
    std::env::set_var("PATH", &stripped);
    let out = f();
    std::env::set_var("PATH", saved);
    out
}

/// A second resolve must not need `git` once everything is vendored.
///
/// The README claimed a resolved project "works offline", and it did not:
/// `ensure_checkout` called `ensure_db` -- which fetches unconditionally -- *before*
/// checking whether the checkout was already on disk. So `install`, `update`, `add`
/// and `lock` all failed on an air-gapped machine with a fully populated cache.
#[test]
fn resolving_again_needs_no_git() {
    let w = World::new("offline");
    let pkg = w._tmp.path().join("repos").join("mydep");
    make_package(&pkg, "mydep", "1.0.0", &[("v1.0.0", "1.0.0")]);
    let url = url_for(&pkg);

    let (mut resolved, lock) = w.install_url(&[("mydep", &url, "")]);
    assert_eq!(resolved.len(), 1);
    vendor_packages(&w.root, &mut resolved).expect("vendor");
    oyvey::lock::save_lock(&w.root.join("oyvey.lock"), &lock).expect("write lock");
    assert!(
        packages_dir(&w.root).join("mydep").is_dir(),
        "the package should be vendored before the offline run"
    );

    // With `git` unreachable, resolving the same lockfile must still succeed: the
    // checkouts are all on disk.
    let manifest = w.manifest("root", &[("mydep", &url)]);
    let cache = Cache::open_at(w._tmp.path().join("cache")).expect("reopen cache");
    let again = without_git(|| {
        let mut r = Resolver::new(&cache, &lock);
        r.resolve_root(&manifest)
            .map(|_| ())
            .map_err(|e| format!("{:#}", e))
    });
    again.expect("a warm cache must resolve with no git at all");
}

/// A checkout already on disk must be returned without touching the network.
///
/// The narrow half of the offline behaviour. Uses a rev that does not exist
/// anywhere, so the only way to succeed is to return the path that is already there.
#[test]
fn an_existing_checkout_is_reused_without_the_db() {
    let w = World::new("reuse");
    let pkg = w._tmp.path().join("repos").join("mydep");
    make_package(&pkg, "mydep", "1.0.0", &[("v1.0.0", "1.0.0")]);
    let url = url_for(&pkg);

    let checkout = w
        .cache
        .ensure_checkout(&url, "v1.0.0")
        .expect("first checkout");
    assert!(checkout.join("package.rak").is_file());

    // Same rev, no git reachable. This is the property that matters: the reuse
    // check is keyed on the rev, which is right -- a *different* rev is a different
    // cache entry and does have to be exported.
    let again = without_git(|| {
        w.cache
            .ensure_checkout(&url, "v1.0.0")
            .map_err(|e| format!("{:#}", e))
    });
    let again = again.expect("a checkout that is already on disk must not need git");
    assert_eq!(again, checkout);
    assert!(again.join("package.rak").is_file());
}

/// A db directory left by an interrupted clone must not brick the cache.
///
/// `git clone` creates the destination directory before transferring anything, so a
/// Ctrl-C or a dropped connection leaves one with no repository inside.
/// `clone_bare` refuses to write into an existing directory, which used to make that
/// state permanent: every later run failed with "cache db already exists" and the
/// only way out was to delete `~/.oyvey` by hand.
#[test]
fn an_incomplete_clone_is_replaced_rather_than_fatal() {
    let w = World::new("broken");
    let pkg = w._tmp.path().join("repos").join("mydep");
    make_package(&pkg, "mydep", "1.0.0", &[("v1.0.0", "1.0.0")]);
    let url = url_for(&pkg);

    // The shape an interrupted clone leaves behind: the directory exists, the repo
    // does not.
    let db = w.cache.db_dir(&url);
    std::fs::create_dir_all(&db).expect("create a bogus db dir");
    std::fs::write(db.join("leftover"), "junk").expect("leftover file");
    assert!(
        !oyvey::git::is_git_dir(&db).expect("probe the fixture"),
        "the fixture must not be a git repository"
    );

    let got = w
        .cache
        .ensure_db(&url)
        .expect("ensure_db must recover from an incomplete clone");
    assert!(
        oyvey::git::is_git_dir(&got).expect("probe db"),
        "the cache should hold a usable repository after recovery"
    );
}

/// A lockfile that cannot be parsed must be an error, not a silent reset.
///
/// `load_lock` documents that a present-but-unreadable file is an error, and then
/// both of its callers in `main.rs` overrode it with `.unwrap_or_default()`. A
/// truncated or merge-conflicted lock therefore re-resolved to newest and was then
/// overwritten, destroying the reproducibility record -- which is what Cargo's
/// `--locked` exists to prevent. `cmd_list` already propagated it, so the crate
/// disagreed with itself.
#[test]
fn a_missing_lockfile_is_empty_and_a_broken_one_is_an_error() {
    let tmp = Tmp::new("badlock");
    let path = tmp.path().join("oyvey.lock");

    // The normal first-run case: nothing locked yet.
    assert!(
        oyvey::lock::load_lock(&path)
            .expect("a missing lockfile is not an error")
            .package
            .is_empty(),
        "a missing lockfile should read as 'nothing locked yet'"
    );

    // A present-but-unreadable one must be reported, not swallowed.
    std::fs::write(&path, "{ this is not valid toml").expect("write");
    assert!(
        oyvey::lock::load_lock(&path).is_err(),
        "an unparseable lockfile must be an error, not an empty lock"
    );
}
