//! Oyvey — the Rak package manager and build system.
//!
//! A Cargo-inspired CLI: a `package.rak` manifest, an `oyvey.lock` lockfile,
//! a global git cache, and tight integration with the `rakc` compiler. See
//! the [crate documentation](oyvey) for the module layout.

use anyhow::Context;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use oyvey::cache::Cache;
use oyvey::lock::{LockEntry, LockFile};
use oyvey::manifest::{Manifest, MANIFEST_FILE};
use oyvey::project::{
    find_project_root, load_lock, load_manifest, packages_dir, scaffold_init, scaffold_new,
    vendor_packages,
};
use oyvey::resolve::Resolver;
use oyvey::spec::parse_dep_spec;
use oyvey::{LOCK_FILE, RAK_PATH_ENV};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_usage() {
    println!("oyvey {} - Rak package manager and build system", VERSION);
    println!();
    println!("USAGE:");
    println!("  oyvey <command> [args]");
    println!();
    println!("COMMANDS:");
    println!("  new <project>          Generate a new Rak project");
    println!("  init                   Initialize an existing directory as a project");
    println!("  add <github-repo>      Add a GitHub package dependency");
    println!("  remove <package>       Remove a dependency");
    println!("  install                Resolve and install dependencies (writes oyvey.lock)");
    println!("  update                 Re-resolve dependencies within constraints");
    println!("  build                  Compile the project through rakc");
    println!("  run                    Run the project entry point");
    println!("  test                   Run the project's tests");
    println!("  clean                  Remove build artifacts");
    println!("  list                   List installed packages");
    println!("  tree                   Print the dependency tree");
    println!("  audit                  Verify installed packages against the lockfile");
    println!("  lock                   Write oyvey.lock without installing");
    println!("  version                Print version");
    println!("  help                   Show this help");
    println!();
    println!("DEPENDENCY SPECS:");
    println!("  user/repo              track the default branch");
    println!("  user/repo@^1.2         caret constraint (compatible with 1.2)");
    println!("  user/repo@~1.2         tilde constraint (compatible with 1.2.x)");
    println!("  user/repo@1.2.3        exact version");
    println!("  user/repo#<rev>        pin to a git revision (tag, branch, or commit)");
    println!();
    println!("ENVIRONMENT:");
    println!("  OYVEY_HOME             global cache location (default: ~/.oyvey)");
    println!(
        "  RAK_PATH               extra module search path (oyvey prepends <project>/packages)"
    );
    println!();
    println!("Run `oyvey help <command>` for details on a specific command.");
}

fn print_cmd_help(cmd: &str) {
    match cmd {
        "new" => {
            println!("oyvey new <project> — generate a new Rak project");
            println!();
            println!("Creates <project>/ with a package.rak manifest, a src/main.rak entry");
            println!("point, a tests/ directory, a .gitignore, and a README. The project is");
            println!("immediately buildable with `oyvey build` and runnable with `oyvey run`.");
            println!();
            println!("FLAGS:");
            println!("  --lib      generate a library project (src/lib.rak, no main)");
        }
        "init" => {
            println!("oyvey init — initialize an existing directory as a project");
            println!();
            println!("Writes a package.rak manifest (if missing) and a src/main.rak entry");
            println!("point (if missing) in the current directory. Never overwrites.");
            println!();
            println!("FLAGS:");
            println!("  --lib      use a library entry point (src/lib.rak)");
        }
        "add" => {
            println!("oyvey add <github-repo> — add a GitHub package dependency");
            println!();
            println!("Clones the repository, reads its package name, and records it in the");
            println!("manifest's deps. The dependency is then installed and locked.");
            println!();
            println!("EXAMPLES:");
            println!("  oyvey add user/rak-net");
            println!("  oyvey add user/rak-crypto@^1.0");
            println!("  oyvey add user/rak-utils#deadbeef");
        }
        "remove" => {
            println!("oyvey remove <package> — remove a dependency");
            println!();
            println!("Drops the package from the manifest, removes its vendored copy, and");
            println!("prunes the lockfile.");
        }
        "install" => {
            println!("oyvey install — resolve and install dependencies");
            println!();
            println!("Resolves every dependency in package.rak (transitively), records the");
            println!("exact revision and checksum in oyvey.lock, and vendors each package");
            println!("into packages/. Honours the lockfile when it is up to date.");
            println!();
            println!("FLAGS:");
            println!("  --offline   never touch the network; fail if something is not cached");
            println!("  --locked    require an up-to-date lockfile; refuse to change it");
            println!("  --frozen    both --offline and --locked");
        }
        "update" => {
            println!("oyvey update — re-resolve dependencies within constraints");
            println!();
            println!("Ignores the locked revisions and picks the newest version satisfying");
            println!("each constraint, then rewrites oyvey.lock and re-vendors.");
            println!();
            println!("FLAGS:");
            println!("  --offline   never touch the network; fail if something is not cached");
            println!("  --locked    require an up-to-date lockfile; refuse to change it");
            println!("  --frozen    both --offline and --locked");
        }
        "build" => {
            println!("oyvey build — compile the project through rakc");
            println!();
            println!("Ensures dependencies are installed, then invokes `rakc build` on the");
            println!("entry point to produce a standalone executable.");
            println!();
            println!("FLAGS:");
            println!("  --offline   never touch the network; fail if something is not cached");
            println!();
            println!("Any other arguments are forwarded to rakc.");
        }
        "run" => {
            println!("oyvey run — run the project entry point");
            println!();
            println!("Ensures dependencies are installed, then invokes `rakc run` on the");
            println!("entry point with RAK_PATH pointing at the vendored packages.");
            println!();
            println!("FLAGS:");
            println!("  --offline   never touch the network; fail if something is not cached");
            println!();
            println!("Any other arguments are forwarded to the program, after `--` if they");
            println!("start with a dash.");
        }
        "test" => {
            println!("oyvey test — run the project's tests");
            println!();
            println!("Ensures dependencies are installed, then invokes `rakc test` from the");
            println!("project root with RAK_PATH pointing at the vendored packages.");
            println!();
            println!("FLAGS:");
            println!("  --offline   never touch the network; fail if something is not cached");
            println!();
            println!("Any other arguments are forwarded to rakc.");
        }
        "clean" => {
            println!("oyvey clean — remove build artifacts");
            println!();
            println!("Removes the executable produced by `oyvey build`.");
        }
        "list" => {
            println!("oyvey list — list installed packages");
            println!();
            println!("Shows every vendored package with its version and locked revision.");
        }
        "tree" => {
            println!("oyvey tree — print the dependency tree");
            println!();
            println!("Prints the resolved dependency graph from the lockfile, cycle-safe.");
        }
        "audit" => {
            println!("oyvey audit — verify installed packages against the lockfile");
            println!();
            println!("Checks that every locked package is vendored and that the SHA-256");
            println!("over its vendored directory matches the lockfile. Every file counts,");
            println!("so an edited source file and a renamed one are both detected.");
            println!("Exits non-zero on a mismatch, or on a file it could not read.")
        }
        "lock" => {
            println!("oyvey lock — write oyvey.lock without installing");
            println!();
            println!("Resolves dependencies and writes the lockfile, but does not vendor");
            println!("packages into packages/.");
            println!();
            println!("FLAGS:");
            println!("  --offline   never touch the network; fail if something is not cached");
            println!("  --locked    require an up-to-date lockfile; refuse to change it");
            println!("  --frozen    both --offline and --locked");
        }
        _ => print_usage(),
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    let cmd = args[1].as_str();
    let rest = &args[2..];

    let code = match dispatch(cmd, rest) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("oyvey error: {:#}", e);
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(cmd: &str, args: &[String]) -> Result<i32, anyhow::Error> {
    match cmd {
        "--version" | "-V" | "version" => {
            println!("oyvey {}", VERSION);
            Ok(0)
        }
        "--help" | "-h" | "help" => {
            if let Some(sub) = args.first() {
                print_cmd_help(sub);
            } else {
                print_usage();
            }
            Ok(0)
        }
        "new" => cmd_new(args),
        "init" => cmd_init(args),
        "add" => cmd_add(args),
        "remove" => cmd_remove(args),
        "install" => cmd_install(args),
        "update" => cmd_update(args),
        "build" => cmd_build(args),
        "run" => cmd_run(args),
        "test" => cmd_test(args),
        "clean" => cmd_clean(args),
        "list" => cmd_list(args),
        "tree" => cmd_tree(args),
        "audit" => cmd_audit(args),
        "lock" => cmd_lock(args),
        other => {
            eprintln!("unknown command: {}", other);
            print_usage();
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// new / init
// ---------------------------------------------------------------------------

fn cmd_new(args: &[String]) -> Result<i32, anyhow::Error> {
    let (name, lib) = parse_new_init_args(args, "oyvey new <project>")?;
    let path = PathBuf::from(&name);
    let proj_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.clone());
    scaffold_new(&path, &proj_name, lib)?;
    println!("Created project: {}", path.display());
    println!("  {} (manifest)", MANIFEST_FILE);
    println!(
        "  {} (entry point)",
        if lib { "src/lib.rak" } else { "src/main.rak" }
    );
    println!("  tests/ (tests)");
    println!();
    println!("Next steps:");
    println!("  cd {}", path.display());
    println!("  oyvey run");
    Ok(0)
}

fn cmd_init(args: &[String]) -> Result<i32, anyhow::Error> {
    let (_name, lib) = parse_new_init_args(args, "oyvey init")?;
    let root = std::env::current_dir()?;
    let proj_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "my-package".to_string());
    scaffold_init(&root, &proj_name, lib)?;
    println!("Initialized project in {}", root.display());
    Ok(0)
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Network and reproducibility flags, shared by the commands that resolve.
///
/// `--offline` forbids fetching anything. `--locked` forbids changing the lockfile.
/// `--frozen` is both, which is the same pairing Cargo uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Flags {
    offline: bool,
    locked: bool,
    frozen: bool,
}

impl Flags {
    fn is_offline(&self) -> bool {
        self.offline || self.frozen
    }

    fn is_locked(&self) -> bool {
        self.locked || self.frozen
    }
}

/// Pull the recognised flags out of `args`, returning them with the leftovers.
///
/// An unrecognised `-`-prefixed argument is an error naming the accepted flags. Every
/// command used to accept anything: `oyvey build --release` looked for `rakc` and failed
/// there, while `oyvey list --bogus` reported success -- so a typo'd flag silently changed
/// nothing and the user had no way to tell. Silently ignoring an option is the worst
/// outcome, because the command looks like it honoured it.
///
/// Non-flag arguments are returned untouched, so the commands that forward to `rakc`
/// (`build`, `run`, `test`) still pass through whatever the compiler accepts.
fn parse_flags<'a>(
    command: &str,
    known: &[&str],
    args: &'a [String],
) -> Result<(Flags, Vec<&'a String>), anyhow::Error> {
    let mut flags = Flags::default();
    let mut rest = Vec::new();
    let mut accepted: Vec<&str> = known.to_vec();
    accepted.push("--help");
    accepted.push("-h");

    for a in args {
        if !a.starts_with('-') {
            rest.push(a);
            continue;
        }
        // `--flag=value` is spelled `--flag` for the purposes of recognising it.
        let name = a.split('=').next().unwrap_or(a.as_str());
        // `known` is authoritative rather than advisory. An earlier version accepted the
        // three unconditionally and used the list only to build this message, so
        // `oyvey list --offline` was accepted and then ignored -- exactly the silence
        // this replaced.
        match name {
            "--help" | "-h" => {
                print_cmd_help(command);
                std::process::exit(0);
            }
            "--offline" if accepted.contains(&"--offline") => flags.offline = true,
            "--locked" if accepted.contains(&"--locked") => flags.locked = true,
            "--frozen" if accepted.contains(&"--frozen") => flags.frozen = true,
            other => anyhow::bail!(
                "unknown flag '{}' for `oyvey {}` (accepted: {})",
                other,
                command,
                accepted.join(", ")
            ),
        }
    }
    Ok((flags, rest))
}

/// Reject anything left over for a command that takes no positional arguments.
///
/// `install` and friends have nothing to do with a stray argument, so passing one is a
/// mistake worth reporting rather than ignoring.
fn reject_extra(command: &str, rest: &[&String]) -> Result<(), anyhow::Error> {
    match rest.first() {
        Some(a) => anyhow::bail!("unexpected argument '{}' for `oyvey {}`", a, command),
        None => Ok(()),
    }
}

/// Parse `[--lib] [name]` for new/init. Returns (name, lib).
fn parse_new_init_args(args: &[String], usage: &str) -> Result<(String, bool), anyhow::Error> {
    let mut lib = false;
    let mut name: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--lib" => lib = true,
            "--help" | "-h" => {
                print_cmd_help(if usage.contains("new") { "new" } else { "init" });
                std::process::exit(0);
            }
            other => {
                if other.starts_with('-') {
                    anyhow::bail!("unknown flag '{}' (try `oyvey help`)", other);
                }
                if name.is_none() {
                    name = Some(other.to_string());
                } else {
                    anyhow::bail!("unexpected argument '{}' (usage: {})", other, usage);
                }
            }
        }
    }
    let name = name.unwrap_or_else(|| ".".to_string());
    Ok((name, lib))
}

// ---------------------------------------------------------------------------
// add / remove
// ---------------------------------------------------------------------------

fn cmd_add(args: &[String]) -> Result<i32, anyhow::Error> {
    let spec = require_arg(args, "oyvey add <github-repo>")?;
    let root = find_project_root(Path::new("."))?;
    let mut manifest = load_manifest(&root)?;

    // Resolve the spec to learn the package's declared name.
    let dep = parse_dep_spec(&spec);
    if dep.repo.is_empty() {
        anyhow::bail!("invalid dependency spec '{}' (expected user/repo)", spec);
    }
    let cache = Cache::open()?;
    let resolved = resolve_one(&cache, &dep)?;
    let pkg_name = manifest_key_for(&resolved)?;

    if manifest.deps.contains_key(&pkg_name) {
        println!("{} is already a dependency", pkg_name);
        return Ok(0);
    }

    manifest
        .deps
        .insert(pkg_name.clone(), spec.trim().to_string());
    write_manifest(&root, &manifest)?;
    println!("Added {} -> {}", pkg_name, spec.trim());

    // Install (resolves the whole graph, vendors, writes the lockfile).
    install(&root, false, Flags::default())?;
    Ok(0)
}

fn cmd_remove(args: &[String]) -> Result<i32, anyhow::Error> {
    let name = require_arg(args, "oyvey remove <package>")?;
    let root = find_project_root(Path::new("."))?;
    let mut manifest = load_manifest(&root)?;
    if manifest.deps.remove(&name).is_none() {
        anyhow::bail!("'{}' is not a dependency of this project", name);
    }
    write_manifest(&root, &manifest)?;
    println!("Removed {} from {}", name, MANIFEST_FILE);

    // Re-resolve to prune the lockfile and stale vendored copies.
    install(&root, false, Flags::default())?;
    Ok(0)
}

/// Resolve a single dependency spec to a `Resolved` (used by `add` to learn
/// the package name before recording it).
/// Resolve a single spec and return *that package*, not just any package.
///
/// The synthetic manifest is keyed by the repo name, which is what the resolver
/// walks in on, so the requested entry is found by matching the repo rather than by
/// position. Taking `.next()` from the returned `BTreeMap` used to return the
/// alphabetically-first entry, which for any package with a transitive dependency
/// was the *dependency* -- so `oyvey add user/zebra-lib` could write
/// `let deps = { aaa-lib: "user/zebra-lib" }` and corrupt the manifest.
fn resolve_one(
    cache: &Cache,
    dep: &oyvey::spec::DepSpec,
) -> Result<oyvey::resolve::Resolved, anyhow::Error> {
    let mut deps = std::collections::BTreeMap::new();
    deps.insert(dep.repo.clone(), dep_repo_string(dep));
    let synthetic = Manifest {
        deps,
        ..Default::default()
    };
    let lock = LockFile::default();
    let mut resolver = Resolver::new(cache, &lock);
    let resolved = resolver.resolve_root(&synthetic)?;
    let names: Vec<String> = resolved.iter().map(|r| r.repo.clone()).collect();
    resolved
        .into_iter()
        .find(|r| r.repo == dep.repo || r.name == dep.repo)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "resolved {} but did not find it in the result (resolved: {})",
                dep.repo,
                if names.is_empty() {
                    "nothing".to_string()
                } else {
                    names.join(", ")
                }
            )
        })
}

/// The manifest key to record for a freshly added dependency.
///
/// This has to be something an `import` statement can name. The resolver keys its
/// entries by the dep key it walked in on, so for `oyvey add user/rak-net` that is
/// `"user/rak-net"` -- which contains a `-`, and `-` is a distinct token in the
/// lexer, so no source file could ever reference it. The package's own declared name
/// is both importable and what the package calls itself.
///
/// A package declaring an unusable name is reported rather than silently written: the
/// alternative is a manifest entry that cannot be imported and cannot be removed by
/// name either.
fn manifest_key_for(resolved: &oyvey::resolve::Resolved) -> Result<String, anyhow::Error> {
    let declared = resolved.manifest.name.trim().to_string();
    if !is_rak_identifier(&declared) {
        anyhow::bail!(
            "{} declares the name '{}', which is not a usable import identifier. \
             Add it under a valid name by hand: let deps = {{ your_name: \"{}\" }}",
            resolved.repo,
            declared,
            resolved.repo
        );
    }
    Ok(declared)
}

/// Whether `s` can be written as an identifier in an `import` statement.
///
/// Conservative on purpose: ASCII letters, digits and `_`, not starting with a
/// digit. A dependency key is used both as a path segment and as an import target,
/// so anything outside this set is rejected here rather than discovered later.
fn is_rak_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn dep_repo_string(dep: &oyvey::spec::DepSpec) -> String {
    let mut s = dep.repo.clone();
    if let Some(c) = &dep.constraint {
        s.push('@');
        s.push_str(c);
    }
    if let Some(r) = &dep.rev {
        s.push('#');
        s.push_str(r);
    }
    s
}

// ---------------------------------------------------------------------------
// install / update / lock
// ---------------------------------------------------------------------------

fn cmd_install(args: &[String]) -> Result<i32, anyhow::Error> {
    let (flags, rest) = parse_flags("install", &["--offline", "--locked", "--frozen"], args)?;
    reject_extra("install", &rest)?;
    let root = find_project_root(Path::new("."))?;
    install(&root, false, flags)?;
    Ok(0)
}

fn cmd_update(args: &[String]) -> Result<i32, anyhow::Error> {
    let (flags, rest) = parse_flags("update", &["--offline", "--locked", "--frozen"], args)?;
    reject_extra("update", &rest)?;
    let root = find_project_root(Path::new("."))?;
    install(&root, true, flags)?;
    Ok(0)
}

fn cmd_lock(args: &[String]) -> Result<i32, anyhow::Error> {
    let (flags, rest) = parse_flags("lock", &["--offline", "--locked", "--frozen"], args)?;
    reject_extra("lock", &rest)?;
    let root = find_project_root(Path::new("."))?;
    let manifest = load_manifest(&root)?;
    let cache = Cache::open()?;
    let lock = LockFile::default();
    let mut resolver = Resolver::new(&cache, &lock);
    resolver.set_offline(flags.is_offline());
    let resolved = resolver.resolve_root(&manifest)?;
    let lock = lockfile_from_resolved(&resolved);
    oyvey::lock::save_lock(&root.join(LOCK_FILE), &lock)?;
    println!(
        "Wrote {} ({} packages)",
        root.join(LOCK_FILE).display(),
        lock.package.len()
    );
    Ok(0)
}

/// Resolve + vendor + write the lockfile. `update` ignores locked revisions.
fn install(root: &Path, update: bool, flags: Flags) -> Result<(), anyhow::Error> {
    let manifest = load_manifest(root)?;
    // Propagated, not defaulted: a present-but-unreadable lockfile is an error by
    // `load_lock`'s own contract. Defaulting here meant a truncated or
    // merge-conflicted lock silently re-resolved to newest and was then overwritten,
    // destroying the reproducibility record -- which is what Cargo's `--locked`
    // exists to prevent.
    let existing = load_lock(root)?;
    let cache = Cache::open()?;

    // `--locked`/`--frozen` promise the lockfile will not change, so it has to be present
    // and have to describe what the manifest asks for. Resolving first and comparing the
    // result afterwards would do the network work the flag is meant to avoid, and would
    // report a difference only after having already fetched.
    // `--locked` promises the lockfile will not change, so it has to already describe
    // what the manifest asks for. Checking the *manifest* rather than "is the lockfile
    // empty" matters: a project with no dependencies has an empty lockfile and is still
    // perfectly locked.
    if flags.is_locked() && !lock_is_usable(root, &manifest, &existing) {
        let missing: Vec<String> = manifest
            .deps
            .keys()
            .filter(|name| existing.get(name).is_none())
            .cloned()
            .collect();
        let detail = if missing.is_empty() {
            format!(
                "{} does not satisfy {}",
                root.join(LOCK_FILE).display(),
                root.join("package.rak").display()
            )
        } else {
            format!(
                "{} has no entry for {}",
                root.join(LOCK_FILE).display(),
                missing.join(", ")
            )
        };
        anyhow::bail!(
            "`--locked` requires an up-to-date lockfile, but {}. Run without `--locked` to update it.",
            detail
        );
    }

    // For a plain install, reuse the lockfile's revisions (reproducible).
    // For update, resolve fresh within constraints.
    let base_lock = if update {
        LockFile::default()
    } else {
        existing.clone()
    };
    let mut resolver = Resolver::new(&cache, &base_lock);
    if flags.is_offline() {
        resolver.set_offline(true);
    }
    let mut resolved = resolver.resolve_root(&manifest)?;

    // `vendor_packages` fills in each package's checksum, so the lockfile written below
    // covers the vendored tree rather than the cache checkout.
    vendor_packages(root, &mut resolved)?;
    let lock = lockfile_from_resolved(&resolved);

    // Checked before the write, so a `--locked` run that would have changed the lockfile
    // leaves the file alone and the build reproducible.
    if flags.is_locked() {
        if let Some(difference) = lockfile_difference(&existing, &lock) {
            anyhow::bail!(
                "`--locked` would change {}: {}. Run without `--locked` to accept it.",
                root.join(LOCK_FILE).display(),
                difference
            );
        }
        println!("Lockfile is up to date (--locked)");
        return Ok(());
    }
    oyvey::lock::save_lock(&root.join(LOCK_FILE), &lock)?;

    if resolved.is_empty() {
        println!("No dependencies to install");
    } else {
        let verb = if update { "Updated" } else { "Installed" };
        println!(
            "{} {} package(s) into {}",
            verb,
            resolved.len(),
            packages_dir(root).display()
        );
        println!("Wrote {}", root.join(LOCK_FILE).display());
    }
    Ok(())
}

/// The first way `want` differs from `have`, or `None` when they agree.
///
/// Compares the resolved set rather than the file text, so reordering or reformatting is
/// not reported as a change -- only a package, version, source, or checksum that actually
/// differs.
fn lockfile_difference(have: &LockFile, want: &LockFile) -> Option<String> {
    for entry in &want.package {
        match have.get(&entry.name) {
            None => return Some(format!("{} is not in the lockfile", entry.name)),
            Some(current) if current.version != entry.version => {
                return Some(format!(
                    "{} is locked at {} but resolves to {}",
                    entry.name, current.version, entry.version
                ))
            }
            Some(current) if current.source != entry.source => {
                return Some(format!(
                    "{} is locked at {} but resolves to {}",
                    entry.name, current.source, entry.source
                ))
            }
            Some(current) if current.checksum != entry.checksum => {
                return Some(format!(
                    "{} has a different checksum than recorded",
                    entry.name
                ))
            }
            Some(_) => {}
        }
    }
    for entry in &have.package {
        if want.get(&entry.name).is_none() {
            return Some(format!("{} would be removed", entry.name));
        }
    }
    None
}

fn lockfile_from_resolved(resolved: &[oyvey::resolve::Resolved]) -> LockFile {
    let mut lock = LockFile::default();
    for r in resolved {
        lock.upsert(LockEntry {
            name: r.name.clone(),
            version: r.version.clone(),
            source: format!("git+{}#{}", r.url, r.rev),
            checksum: r.checksum.clone(),
        });
    }
    lock
}

// ---------------------------------------------------------------------------
// build / run / test
// ---------------------------------------------------------------------------

/// Arguments for the child, after `oyvey run` / `oyvey build`.
///
/// Passed through verbatim, including a leading `--`. That separator has to reach
/// rakc rather than be consumed here: rakc is what decides which arguments are
/// flags, and it only stops at `--`. Stripping it here meant
/// `oyvey run -- --port 8080` reached rakc as `run <entry> --port 8080`, so rakc
/// claimed `--port` as its own and the program received `[8080]`.
///
/// `--help` and `-h` are consumed by the caller before this runs, so they are not
/// special cased here: forwarding a program's own `--help` is the caller's
/// decision.
fn child_args(args: &[String]) -> Vec<String> {
    args.to_vec()
}

/// The child's exit code, treating death by signal as a failure rather than
/// success.
///
/// `ExitStatus::code()` is `None` when a process is killed by a signal, so
/// `unwrap_or(0)` turned a segfault or an OOM kill into exit code 0 -- in CI that
/// reads as "the tests passed" for a program that crashed. Cargo reports
/// `128 + signal` on Unix; there is no portable equivalent, so this reports the
/// fact and fails.
fn exit_code_of(status: &std::process::ExitStatus) -> i32 {
    match status.code() {
        Some(code) => code,
        None => {
            eprintln!("oyvey: the child process was terminated by a signal");
            1
        }
    }
}

fn cmd_build(args: &[String]) -> Result<i32, anyhow::Error> {
    // `--offline` is oyvey's, so it is stripped here rather than forwarded: the
    // compiler would reject it as an unknown flag.
    let forwarded: Vec<String> = parse_flags("build", &["--offline"], args)?
        .1
        .into_iter()
        .map(|a| a.to_string())
        .collect();
    let root = find_project_root(Path::new("."))?;
    ensure_deps(&root)?;
    let manifest = load_manifest(&root)?;
    let entry = manifest.resolved_entry(&root);
    if !root.join(&entry).is_file() {
        anyhow::bail!(
            "entry point '{}' not found (declared in {})",
            manifest.entry,
            MANIFEST_FILE
        );
    }
    let rakc = rakc_path()?;
    // rakc already prints `Built: <path>` and the embedded-source size, so
    // oyvey does not repeat it here.
    // Forwarded so a flag aimed at rakc reaches it instead of being dropped.
    let mut argv = vec!["build".to_string(), entry.clone()];
    argv.extend(child_args(&forwarded));
    let status = invoke_rakc(&root, &rakc, &argv)?;
    Ok(exit_code_of(&status))
}

fn cmd_run(args: &[String]) -> Result<i32, anyhow::Error> {
    // `--offline` is oyvey's, so it is stripped here rather than forwarded: the
    // compiler would reject it as an unknown flag.
    let forwarded: Vec<String> = parse_flags("run", &["--offline"], args)?
        .1
        .into_iter()
        .map(|a| a.to_string())
        .collect();
    let root = find_project_root(Path::new("."))?;
    ensure_deps(&root)?;
    let manifest = load_manifest(&root)?;
    let entry = manifest.resolved_entry(&root);
    if !root.join(&entry).is_file() {
        anyhow::bail!(
            "entry point '{}' not found (declared in {})",
            manifest.entry,
            MANIFEST_FILE
        );
    }
    let rakc = rakc_path()?;
    // The program's own arguments. Without this, `fn main(argv)` in every
    // `oyvey new` project always received an empty array -- which is exactly what
    // oyvey's own scaffold template and `docs/content/cli.md` promise it gets.
    let mut argv = vec!["run".to_string(), entry.clone()];
    argv.extend(child_args(&forwarded));
    let status = invoke_rakc(&root, &rakc, &argv)?;
    Ok(exit_code_of(&status))
}

fn cmd_test(args: &[String]) -> Result<i32, anyhow::Error> {
    // `--offline` is oyvey's, so it is stripped here rather than forwarded: the
    // compiler would reject it as an unknown flag.
    let forwarded: Vec<String> = parse_flags("test", &["--offline"], args)?
        .1
        .into_iter()
        .map(|a| a.to_string())
        .collect();
    let root = find_project_root(Path::new("."))?;
    // When no flags were passed, discover the suite the same way `rakc test`
    // does and bail out with a clear message if the project has none. Left to
    // rakc it falls back to `test.rak`, which for a project without one is a
    // read error reported as a failing test.
    let mut files: Vec<String> = Vec::new();
    let mut forwarded_flags: Vec<String> = forwarded;
    if args.iter().all(|a| a == "--help" || a == "-h") {
        forwarded_flags.clear();
    }
    let explicit_file = args
        .iter()
        .find(|a| !a.starts_with('-') && a.ends_with(".rak"))
        .cloned();
    if explicit_file.is_none() {
        if let Ok(entries) = std::fs::read_dir(root.join("tests")) {
            let mut raks: Vec<String> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "rak").unwrap_or(false))
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            raks.sort();
            files = raks;
        }
        if files.is_empty() && root.join("test.rak").is_file() {
            files.push(root.join("test.rak").to_string_lossy().to_string());
        }
        if files.is_empty() {
            println!("No tests found (looked in tests/ and test.rak)");
            return Ok(0);
        }
    }
    ensure_deps(&root)?;
    let rakc = rakc_path()?;
    // Forward any extra flags (e.g. --filter) plus the discovered files.
    let mut forward = vec!["test".to_string()];
    forward.extend(forwarded_flags);
    if let Some(f) = explicit_file {
        forward.push(f);
    } else {
        forward.extend(files);
    }
    let status = invoke_rakc(&root, &rakc, &forward)?;
    Ok(exit_code_of(&status))
}

/// Make sure dependencies are resolved and vendored. Skips network when the
/// lockfile is fresh and every package is already vendored.
/// Make sure dependencies are resolved and vendored. Skips the network when the
/// lockfile still satisfies the manifest and every locked package is on disk.
fn ensure_deps(root: &Path) -> Result<(), anyhow::Error> {
    let manifest = load_manifest(root)?;
    let lock = load_lock(root)?;
    if lock_is_usable(root, &manifest, &lock) {
        return Ok(());
    }
    install(root, false, Flags::default())
}

/// Whether the lockfile and `packages/` still satisfy the manifest.
///
/// The previous check looked only at whether each direct dependency was *present by
/// name*. It never re-checked that the locked version still satisfied the manifest's
/// constraint, so editing `net: "user/rak-net@^1.0"` to `^2.0` and running
/// `oyvey build` silently built against 1.x -- nothing printed, no exit code, and no
/// reason to suspect anything. It also examined only direct dependencies, so a
/// deleted *transitive* one surfaced later as a compiler error in the user's own
/// code, which blames them for oyvey's staleness check.
///
/// So the constraints are re-checked here, exactly as `Resolver::resolve_rev` does,
/// and the whole locked set has to still be vendored.
fn lock_is_usable(root: &Path, manifest: &Manifest, lock: &LockFile) -> bool {
    for (name, spec) in &manifest.deps {
        let Some(entry) = lock.get(name) else {
            return false;
        };
        let constraint = match parse_dep_spec(spec).constraint {
            Some(c) if !c.is_empty() && c != "*" => c,
            // An unconstrained dependency is satisfied by whatever is locked.
            _ => continue,
        };
        match oyvey::spec::check_version(&entry.version, &constraint) {
            Ok(true) => {}
            // A version that no longer parses, or one that no longer matches, both
            // mean the lock is stale for this constraint.
            Ok(false) | Err(_) => return false,
        }
    }
    // Every locked package, not just the direct ones, must still be vendored.
    lock.package
        .iter()
        .all(|e| packages_dir(root).join(&e.name).is_dir())
}

/// Invoke rakc with CWD = project root and RAK_PATH pointing at the vendored
/// packages, so `import <pkg>` resolves.
fn invoke_rakc(root: &Path, rakc: &str, args: &[String]) -> Result<ExitStatus, anyhow::Error> {
    let pkgs = packages_dir(root);
    let mut rak_path = pkgs.to_string_lossy().to_string();
    if let Ok(existing) = env::var(RAK_PATH_ENV) {
        if !existing.is_empty() {
            let sep = if cfg!(windows) { ';' } else { ':' };
            rak_path.push(sep);
            rak_path.push_str(&existing);
        }
    }
    let status = Command::new(rakc)
        .args(args)
        .current_dir(root)
        .env(RAK_PATH_ENV, &rak_path)
        .status()
        .with_context(|| format!("running {} (is rakc installed?)", rakc))?;
    Ok(status)
}

/// The file `rakc build` produces for an entry point (`src/main.rak` ->
/// `src/main`, plus `.exe` on Windows).
fn build_output_name(entry: &str) -> String {
    let base = entry.strip_suffix(".rak").unwrap_or(entry);
    if cfg!(windows) {
        format!("{}.exe", base)
    } else {
        base.to_string()
    }
}

// ---------------------------------------------------------------------------
// clean / list / tree / audit
// ---------------------------------------------------------------------------

fn cmd_clean(args: &[String]) -> Result<i32, anyhow::Error> {
    let (_flags, rest) = parse_flags("clean", &[], args)?;
    reject_extra("clean", &rest)?;
    let root = find_project_root(Path::new("."))?;
    let manifest = load_manifest(&root)?;
    let exe = build_output_name(&manifest.resolved_entry(&root));
    let mut removed = 0;
    for cand in [&exe, &format!("{}.exe", exe)] {
        let p = root.join(cand);
        if p.is_file() {
            std::fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
            println!("Removed {}", p.display());
            removed += 1;
        }
    }
    if removed == 0 {
        println!("Nothing to clean");
    }
    Ok(0)
}

fn cmd_list(args: &[String]) -> Result<i32, anyhow::Error> {
    let (_flags, rest) = parse_flags("list", &[], args)?;
    reject_extra("list", &rest)?;
    let root = find_project_root(Path::new("."))?;
    let lock = load_lock(&root)?;
    if lock.package.is_empty() {
        println!("No packages installed");
        return Ok(0);
    }
    println!("Installed packages ({}):", lock.package.len());
    for entry in &lock.package {
        let short: String = entry
            .source
            .rsplit('#')
            .next()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect();
        println!("  {} v{} ({})", entry.name, entry.version, short);
    }
    Ok(0)
}

fn cmd_tree(args: &[String]) -> Result<i32, anyhow::Error> {
    let (_flags, rest) = parse_flags("tree", &[], args)?;
    reject_extra("tree", &rest)?;
    let root = find_project_root(Path::new("."))?;
    let manifest = load_manifest(&root)?;
    let lock = load_lock(&root)?;
    println!("{} v{} (root)", manifest.name, manifest.version);
    let mut seen = std::collections::HashSet::new();
    for name in manifest.deps.keys() {
        if let Some(entry) = lock.get(name) {
            print_tree_entry(entry, &lock, 1, &mut seen);
        } else {
            println!("  {} (not installed)", name);
        }
    }
    Ok(0)
}

fn print_tree_entry(
    entry: &LockEntry,
    lock: &LockFile,
    depth: usize,
    seen: &mut std::collections::HashSet<String>,
) {
    let indent = "  ".repeat(depth);
    if !seen.insert(entry.name.clone()) {
        println!("{}{} v{} (cyclic)", indent, entry.name, entry.version);
        return;
    }
    println!("{}{} v{}", indent, entry.name, entry.version);
    // Read this package's deps from its vendored manifest.
    let vendored = packages_dir(Path::new("."))
        .join(&entry.name)
        .join(MANIFEST_FILE);
    if let Ok(m) = oyvey::manifest::parse_manifest(&vendored) {
        for sub in m.deps.keys() {
            if let Some(sub_entry) = lock.get(sub) {
                print_tree_entry(sub_entry, lock, depth + 1, seen);
            } else {
                println!("{}  {} (not installed)", indent, sub);
            }
        }
    }
    seen.remove(&entry.name);
}

fn cmd_audit(args: &[String]) -> Result<i32, anyhow::Error> {
    let (_flags, rest) = parse_flags("audit", &[], args)?;
    reject_extra("audit", &rest)?;
    let root = find_project_root(Path::new("."))?;
    let lock = load_lock(&root)?;
    if lock.package.is_empty() {
        println!("No {} entries to audit", LOCK_FILE);
        return Ok(0);
    }
    let mut issues = 0;
    for entry in &lock.package {
        let dir = packages_dir(&root).join(&entry.name);
        if !dir.join(MANIFEST_FILE).is_file() {
            println!("[WARN] {}: missing (not installed)", entry.name);
            issues += 1;
            continue;
        }
        // The whole vendored directory, which is what the lockfile records. Verifying only
        // `package.rak` -- as this did -- meant every other file, including the entry
        // point, could be changed and audit would still report "checksum verified".
        match oyvey::resolve::sha256_tree(&dir) {
            Ok(checksum) if checksum == entry.checksum => {
                println!(
                    "[ok]   {} v{} (checksum verified)",
                    entry.name, entry.version
                );
            }
            Ok(_) => {
                println!("[FAIL] {}: checksum mismatch (tampered?)", entry.name);
                issues += 1;
            }
            Err(e) => {
                // Not a mismatch: the files could not be read at all, which is a different
                // problem and saying "tampered" would misdescribe it.
                println!("[FAIL] {}: cannot verify: {}", entry.name, e);
                issues += 1;
            }
        }
    }
    if issues == 0 {
        println!("All {} package(s) verified", lock.package.len());
        Ok(0)
    } else {
        println!("{} issue(s) found", issues);
        Ok(1)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn require_arg(args: &[String], usage: &str) -> Result<String, anyhow::Error> {
    match args.first() {
        Some(a) if !a.starts_with('-') => Ok(a.clone()),
        _ => anyhow::bail!("missing argument (usage: {})", usage),
    }
}

/// Locate the rakc binary. Searches PATH, the directory containing the oyvey
/// executable, and the workspace target dirs.
fn rakc_path() -> Result<String, anyhow::Error> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let mut candidates = vec!["rakc".to_string()];
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("rakc").to_string_lossy().to_string());
            candidates.push(
                parent
                    .join(format!("rakc{}", ext))
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    candidates.push(format!("target/release/rakc{}", ext));
    candidates.push(format!("target/debug/rakc{}", ext));
    for c in &candidates {
        if Command::new(c).arg("--version").output().is_ok() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "rakc not found. Oyvey drives the rakc compiler; install it from \
         https://github.com/Louiml/Rak or build it with `cargo build --release -p rakc`."
    )
}

/// Serialize a manifest back to Rak source (used by add/remove).
/// Rewrite only the `deps` block of `package.rak`, leaving everything else alone.
///
/// Surgical on purpose. The previous version regenerated the whole file from six
/// hardcoded keys while `parse_manifest` recognises only those six and ignores
/// everything else -- comments included. So one `oyvey add` permanently deleted
/// `let authors`, `let repository`, `let keywords` and every `//` line in a manifest,
/// with no warning. `package.rak` is Rak source, and it was being replaced by a lossy
/// projection of itself.
///
/// A text edit is the only way to preserve parts of a file the tool does not fully
/// model. If the file has no `deps` block, one is appended rather than synthesising
/// the rest of the file.
fn write_manifest(root: &Path, manifest: &Manifest) -> Result<(), anyhow::Error> {
    let path = root.join(MANIFEST_FILE);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    let block = render_deps_body(&manifest.deps);

    let updated = match deps_block_span(&existing) {
        Some((start, end)) => {
            // Only the contents are replaced. The `let deps = {` prefix and the
            // closing brace are already in the file, so pushing a whole statement
            // here produced `let deps = {let deps = {` -- an unparseable manifest,
            // written by the command whose job is to edit one.
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(&existing[..start]);
            out.push_str(&block);
            out.push_str(&existing[end..]);
            out
        }
        None => {
            // No `deps` statement at all. Append one whole, keeping the rest of the
            // file byte-for-byte.
            let mut out = existing.clone();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str("let deps = {");
            out.push_str(&block);
            out.push_str("}\n");
            out
        }
    };

    std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The byte range of the `deps` block's *contents*, so it can be replaced alone.
///
/// Matches `let deps = {` and the brace that closes it, tracking nesting so a
/// dependency value containing `{` cannot end the block early. Returns `None` when
/// there is no block.
fn deps_block_span(src: &str) -> Option<(usize, usize)> {
    let start = src.find("let deps")?;
    let open = src[start..].find('{')? + start;
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, i));
                }
                i += 1;
            }
            // Skip a line comment: a brace in prose must not change the nesting.
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i += src[i..].find('\n').map(|n| i + n).unwrap_or(bytes.len());
            }
            _ => i += 1,
        }
    }
    None
}

/// Render the *contents* of a `deps` block, without the surrounding braces.
///
/// The contents, not the whole statement: `deps_block_span` returns the span
/// between the braces, so substituting a full `let deps = { ... }` into it produced
/// `let deps = {let deps = {` -- an unparseable file, from a command whose entire
/// job is to edit one.
fn render_deps_body(deps: &std::collections::BTreeMap<String, String>) -> String {
    if deps.is_empty() {
        return "\n".to_string();
    }
    let mut out = String::from("\n");
    for (name, spec) in deps {
        // A key that is not a bare identifier has to be quoted, or the file we
        // write cannot be read back. Hand-edited manifests contain such keys.
        let key = if is_rak_identifier(name) {
            name.clone()
        } else {
            format!("\"{}\"", escape_string(name))
        };
        out.push_str(&format!("    {}: \"{}\",\n", key, escape_string(spec)));
    }
    out
}

/// Escape a value for a Rak string literal.
///
/// The old writer did no escaping at all, so a `"` in a description or a dependency
/// spec produced a file that could not be parsed back -- a corrupt manifest caused by
/// running a command that was supposed to edit one.
fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_name_strips_rak() {
        if cfg!(windows) {
            assert_eq!(build_output_name("src/main.rak"), "src/main.exe");
        } else {
            assert_eq!(build_output_name("src/main.rak"), "src/main");
        }
        if cfg!(windows) {
            assert_eq!(build_output_name("main.rak"), "main.exe");
        } else {
            assert_eq!(build_output_name("main.rak"), "main");
        }
    }

    #[test]
    fn require_arg_rejects_missing() {
        assert!(require_arg(&[], "usage").is_err());
        assert!(require_arg(&["--flag".to_string()], "usage").is_err());
        assert_eq!(require_arg(&["x".to_string()], "usage").unwrap(), "x");
    }

    /// A manifest as a user would actually write one: a leading comment, keys the
    /// tool does not model, and no explicit `entry`.
    const RICH_MANIFEST: &str = r#"// Rak package manifest
let name = "demo"
let version = "0.1.0"
let description = "a demo"
let license = "MIT"
let authors = ["someone"]
let repository = "https://example.invalid/repo"
let keywords = { demo = "yes" }
"#;

    fn write_to(root: &Path, source: &str, deps: &[(&str, &str)]) -> String {
        std::fs::write(root.join(MANIFEST_FILE), source).expect("seed manifest");
        let mut m = Manifest {
            name: "demo".to_string(),
            ..Default::default()
        };
        for (k, v) in deps {
            m.deps.insert(k.to_string(), v.to_string());
        }
        write_manifest(root, &m).expect("write manifest");
        std::fs::read_to_string(root.join(MANIFEST_FILE)).expect("read back")
    }

    /// The rewrite must not delete a comment or a key it does not model.
    ///
    /// This is the data loss: the old writer emitted six fixed lines and dropped
    /// everything else, so one `oyvey add` destroyed `authors`, `repository`,
    /// `keywords` and every `//` line in the file.
    #[test]
    fn rewrite_preserves_comments_and_unknown_keys() {
        let tmp = std::env::temp_dir().join(format!("oyvey_manifest_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let out = write_to(&tmp, RICH_MANIFEST, &[("net", "user/rak-net")]);

        for line in [
            "// Rak package manifest",
            "let authors = [\"someone\"]",
            "let repository = \"https://example.invalid/repo\"",
            "let keywords = { demo = \"yes\" }",
        ] {
            assert!(out.contains(line), "{} was deleted:\n{}", line, out);
        }
        assert!(
            out.contains("net: \"user/rak-net\""),
            "dep missing:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A manifest with no `entry` must not gain one.
    ///
    /// `Manifest::default` supplies `ENTRY_DEFAULT`, so the old writer always emitted
    /// `let entry = "src/main.rak"`. That pinned a package relying on the documented
    /// entry-inference order to one specific file, because of an unrelated
    /// `oyvey add`.
    #[test]
    fn rewrite_does_not_invent_an_entry_line() {
        let tmp = std::env::temp_dir().join(format!("oyvey_entry_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let out = write_to(&tmp, RICH_MANIFEST, &[("net", "user/rak-net")]);
        assert!(
            !out.contains("let entry"),
            "the rewrite invented an `entry` line:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A quote in a value must not produce a file that cannot be read back.
    #[test]
    fn rewrite_escapes_quotes_in_values() {
        let tmp = std::env::temp_dir().join(format!("oyvey_escape_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let out = write_to(&tmp, RICH_MANIFEST, &[("weird", "a \"quoted\" value")]);
        assert!(
            out.contains("weird: \"a \\\"quoted\\\" value\""),
            "value not escaped:\n{}",
            out
        );
        // The round trip is the real assertion: it has to parse.
        let parsed =
            oyvey::manifest::parse_manifest(&tmp.join(MANIFEST_FILE)).expect("must reparse");
        assert_eq!(
            parsed.deps.get("weird").map(String::as_str),
            Some("a \"quoted\" value")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Everything outside `deps` must come back byte-identical.
    #[test]
    fn rewrite_leaves_the_rest_of_the_file_untouched() {
        let tmp = std::env::temp_dir().join(format!("oyvey_bytes_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let src = "let name = \"demo\"\nlet deps = {\n}\n\n// trailing comment\nlet entry = \"src/main.rak\"\n";
        let out = write_to(&tmp, src, &[("net", "user/rak-net")]);
        let expected = "let name = \"demo\"\nlet deps = {\n    net: \"user/rak-net\",\n}\n\n// trailing comment\nlet entry = \"src/main.rak\"\n";
        assert_eq!(out, expected);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A manifest with no `deps` block gets one appended, keeping the rest.
    #[test]
    fn rewrite_appends_a_deps_block_when_absent() {
        let tmp = std::env::temp_dir().join(format!("oyvey_nodeps_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let out = write_to(
            &tmp,
            "// only a comment\nlet name = \"demo\"\n",
            &[("net", "user/rak-net")],
        );
        assert!(out.contains("// only a comment"), "comment lost:\n{}", out);
        assert!(
            out.contains("let deps = {"),
            "deps block not appended:\n{}",
            out
        );
        assert!(
            out.contains("net: \"user/rak-net\""),
            "dep missing:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A key that is not a bare identifier has to be quoted, or the file we write
    /// cannot be read back.
    #[test]
    fn rewrite_quotes_awkward_dependency_keys() {
        let tmp = std::env::temp_dir().join(format!("oyvey_awkward_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let out = write_to(&tmp, RICH_MANIFEST, &[("user/rak-net", "user/rak-net")]);
        assert!(
            out.contains("\"user/rak-net\": \"user/rak-net\""),
            "an awkward key must be quoted:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A brace inside a comment must not end the `deps` block early.
    #[test]
    fn deps_block_span_ignores_braces_in_comments() {
        let src = "let deps = {\n    // a brace } in prose\n    net: \"user/rak-net\",\n}\nlet entry = \"src/main.rak\"\n";
        let (start, end) = deps_block_span(src).expect("span");
        assert_eq!(
            &src[start..end],
            "\n    // a brace } in prose\n    net: \"user/rak-net\",\n"
        );
    }

    /// Dependency keys must be things an `import` can name.
    #[test]
    fn is_rak_identifier_accepts_only_importable_names() {
        assert!(is_rak_identifier("net"));
        assert!(is_rak_identifier("_private"));
        assert!(is_rak_identifier("mylib2"));
        assert!(!is_rak_identifier("user/rak-net"), "contains - and /");
        assert!(!is_rak_identifier("2fast"), "starts with a digit");
        assert!(!is_rak_identifier(""));
        assert!(!is_rak_identifier("has space"));
    }
}
