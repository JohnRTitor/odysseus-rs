//! CI gate for the crate dependency rules (T0008).
//!
//! `SOURCE_MAPPING.md` §9 and `AGENTS.md` §8 both state architectural boundaries
//! as prose. Prose is not enforced by anything, so this test turns each rule that
//! is expressible as a dependency constraint into an assertion that fails the
//! build. A violation is an architectural defect (`AGENTS.md` §8), not a style
//! preference, so it belongs in the same gate as the rest of the suite.
//!
//! # Direct or transitive
//!
//! **Direct, and only direct.** Each rule is asserted against the dependencies a
//! crate *declares in its own `Cargo.toml`*, which is exactly what
//! `cargo metadata --no-deps` reports in `packages[].dependencies`.
//!
//! Transitive reach is deliberately not checked. "odysseus-store never imports
//! `axum`" is a statement about what `odysseus-store` was allowed to ask for. If
//! some unrelated crate pulls `axum` in underneath it, the defect belongs to that
//! crate and to the rules that name it -- and asserting on the transitive closure
//! would mean a single upstream release could break an unrelated crate's build
//! with a change nobody here made. The blast radius of a rule should match the
//! blast radius of the mistake.
//!
//! Development and build dependencies *are* included. A `dev-dependencies` entry
//! is still a declaration in that crate's manifest, and a boundary that leaks
//! through test-only edges is not a boundary.
//!
//! # Rules that cannot be checked here
//!
//! Two §9 rules are statements about source code rather than manifests and are
//! **not** enforced by this test:
//!
//!   * "Only `odysseus-web` knows about HTTP status codes" -- a property of which
//!     types a crate references. Enforcing it would mean parsing Rust source.
//!   * "`odysseus-ml` is isolated so a swap is a single-crate change" -- enforced
//!     here only as far as it is a dependency claim: nothing outside
//!     `odysseus-ml` may declare `ort`. The rest is a review property.
//!
//! `AGENTS.md` §8 additionally forbids three things that name no crate --
//! "business logic", "agent-loop control flow", and bare "SQL". A dependency graph
//! cannot express those; they stay review rules, named here so the gap is
//! recorded rather than silently forgotten.
//!
//! # Test seam
//!
//! `ODYSSEUS_ARCH_MANIFEST` overrides which manifest is checked. It exists so the
//! failure path can be demonstrated end-to-end against a synthetic workspace
//! instead of by editing a real `Cargo.toml`. CI leaves it unset.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Deserialize;

/// Overrides the manifest to check. Unset in CI, so CI always checks this repo.
const MANIFEST_ENV: &str = "ODYSSEUS_ARCH_MANIFEST";

/// The slice of `cargo metadata` this test needs.
#[derive(Debug, Deserialize)]
struct Metadata {
    /// Workspace members only, because `--no-deps` omits transitive packages.
    packages: Vec<Package>,
}

/// One workspace member.
#[derive(Debug, Deserialize)]
struct Package {
    /// Crate name, e.g. `odysseus-store`.
    name: String,
    /// Fully-qualified id, used to test membership.
    id: String,
    /// Path to the crate's own `Cargo.toml`.
    manifest_path: PathBuf,
    /// Declared (not resolved) dependencies.
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

/// One declared dependency.
#[derive(Debug, Deserialize)]
struct Dependency {
    /// The real crate name. This is the upstream name even when the dependency
    /// is renamed, so `axum2 = { package = "axum" }` still reports `axum`.
    name: String,
    /// `null` for a normal dependency, else `"dev"` or `"build"`.
    #[serde(default)]
    kind: Option<String>,
}

/// "This crate must never declare these dependencies."
#[derive(Debug)]
struct Forbidden<'a> {
    /// Crate the rule constrains.
    crate_name: &'a str,
    /// Dependency names it must not declare, under any dependency kind.
    deps: &'a [&'a str],
    /// Where the rule is written, quoted in the failure message.
    source: &'a str,
}

/// "This dependency may be declared only by this crate."
#[derive(Debug)]
struct Isolated<'a> {
    /// The dependency that must stay confined.
    dep: &'a str,
    /// The only crate permitted to declare it.
    owner: &'a str,
    /// Where the rule is written.
    source: &'a str,
}

/// `odysseus-domain` must depend on nothing internal (`SOURCE_MAPPING.md` §9,
/// `AGENTS.md` §8 "May import: nothing internal").
const DOMAIN: &str = "odysseus-domain";

/// Prefix identifying a first-party crate.
const INTERNAL_PREFIX: &str = "odysseus-";

/// Rules of the form "crate X must never declare D".
///
/// Every entry cites the document that states it. Where `AGENTS.md` §8 is
/// stricter than `SOURCE_MAPPING.md` §9 the stricter union is enforced, because
/// the tighter boundary is the one the project actually wants held; the
/// discrepancy is recorded for T0008 rather than resolved silently.
const FORBIDDEN: &[Forbidden<'_>] = &[
    Forbidden {
        crate_name: "odysseus-store",
        deps: &["axum", "leptos", "reqwest"],
        source: "SOURCE_MAPPING.md §9 and AGENTS.md §8 (identical)",
    },
    Forbidden {
        crate_name: "odysseus-llm",
        deps: &["axum", "leptos"],
        source: "AGENTS.md §8 (`business logic` also forbidden; not expressible here)",
    },
    Forbidden {
        crate_name: "odysseus-agent",
        deps: &["axum", "leptos"],
        source: "AGENTS.md §8; SOURCE_MAPPING.md §9 lists `axum` only (see report)",
    },
    Forbidden {
        crate_name: "odysseus-tools",
        deps: &["axum"],
        source: "AGENTS.md §8 (`agent-loop control flow` also forbidden; not expressible here)",
    },
    Forbidden {
        crate_name: "odysseus-services",
        deps: &["axum", "leptos"],
        source: "AGENTS.md §8 (absent from SOURCE_MAPPING.md §9)",
    },
    Forbidden {
        crate_name: "odysseus-integrations",
        deps: &["axum", "leptos"],
        source: "AGENTS.md §8 (absent from SOURCE_MAPPING.md §9)",
    },
    Forbidden {
        crate_name: "odysseus-ui",
        deps: &["rusqlite", "reqwest"],
        source: "SOURCE_MAPPING.md §9 and AGENTS.md §8 (identical; §8 also forbids bare `SQL`)",
    },
];

/// Rules of the form "only crate X may declare D".
const ISOLATED: &[Isolated<'_>] = &[
    Isolated {
        dep: "ort",
        owner: "odysseus-ml",
        source: "SOURCE_MAPPING.md §9 (`odysseus-ml` isolated so a swap is a single-crate change)",
    },
    Isolated {
        dep: "ort-sys",
        owner: "odysseus-ml",
        source: "SOURCE_MAPPING.md §9 (same isolation rule, native half)",
    },
];

/// One violated rule, already formatted for display.
#[derive(Debug)]
struct Violation {
    crate_name: String,
    forbidden_dep: String,
    manifest_path: String,
    source: &'static str,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\n  {} declares `{}`, which is forbidden.\n    declared in: {}\n    rule: {}",
            self.crate_name, self.forbidden_dep, self.manifest_path, self.source
        )
    }
}

/// Locates the manifest to check: the override if set, else this workspace's.
///
/// # Errors
///
/// Returns a boxed error if `ODYSSEUS_ARCH_MANIFEST` is set but does not name an
/// existing path, which is treated as a failure rather than silently ignored --
/// a typo'd override must not quietly turn the gate into a no-op.
fn manifest_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::var(MANIFEST_ENV) {
        Ok(value) => {
            let path = PathBuf::from(value);
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "{MANIFEST_ENV} points at {}, which does not exist",
                    path.display()
                )
                .into())
            }
        }
        Err(_) => Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("Cargo.toml")),
    }
}

/// Runs `cargo metadata` over `manifest` and deserialises it.
///
/// `--no-deps` is deliberate: it yields workspace members and their *declared*
/// dependencies, which is precisely the claim these rules are about, and it
/// avoids resolving the full graph just to read a manifest.
///
/// # Errors
///
/// Returns a boxed error if `cargo` is missing, exits non-zero, or emits JSON
/// this file does not expect.
fn load_metadata(manifest: &Path) -> Result<Metadata, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .arg("--manifest-path")
        .arg(manifest)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed for {}: {}",
            manifest.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// The workspace members, identified by their unique names.
///
/// # Errors
///
/// Returns a boxed error if two members share a name, which would make the
/// name-keyed rule table ambiguous.
fn members_by_name(metadata: &Metadata) -> Result<BTreeSet<&str>, Box<dyn std::error::Error>> {
    let mut names: Vec<&str> = metadata
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    if names.len() != count {
        return Err("cargo metadata reported duplicate workspace member names".into());
    }
    Ok(names.into_iter().collect())
}

/// Finds a member by exact crate name.
fn member<'a>(metadata: &'a Metadata, name: &str) -> Option<&'a Package> {
    metadata
        .packages
        .iter()
        .find(|package| package.name == name && package.id.starts_with("path+"))
}

/// How a dependency was declared, for the failure message.
fn kind_label(dep: &Dependency) -> &str {
    match dep.kind.as_deref() {
        None => "dependencies",
        Some("dev") => "dev-dependencies",
        Some("build") => "build-dependencies",
        Some(other) => other,
    }
}

/// Every rule violation in `metadata`.
fn check(metadata: &Metadata) -> Vec<Violation> {
    let mut violations = Vec::new();

    for rule in FORBIDDEN {
        let Some(package) = member(metadata, rule.crate_name) else {
            continue;
        };
        for dep in &package.dependencies {
            if rule.deps.contains(&dep.name.as_str()) {
                violations.push(Violation {
                    crate_name: package.name.clone(),
                    forbidden_dep: format!("{} ({})", dep.name, kind_label(dep)),
                    manifest_path: package.manifest_path.display().to_string(),
                    source: rule.source,
                });
            }
        }
    }

    for rule in ISOLATED {
        for package in &metadata.packages {
            if package.name == rule.owner {
                continue;
            }
            for dep in &package.dependencies {
                if dep.name == rule.dep {
                    violations.push(Violation {
                        crate_name: package.name.clone(),
                        forbidden_dep: format!("{} ({})", dep.name, kind_label(dep)),
                        manifest_path: package.manifest_path.display().to_string(),
                        source: rule.source,
                    });
                }
            }
        }
    }

    if let Some(domain) = member(metadata, DOMAIN) {
        for dep in &domain.dependencies {
            if dep.name.starts_with(INTERNAL_PREFIX) {
                violations.push(Violation {
                    crate_name: domain.name.clone(),
                    forbidden_dep: format!("{} ({})", dep.name, kind_label(dep)),
                    manifest_path: domain.manifest_path.display().to_string(),
                    source: "SOURCE_MAPPING.md §9 and AGENTS.md §8 (`odysseus-domain` depends on nothing internal)",
                });
            }
        }
    }

    violations
}

/// The real gate. Fails the build on any violation in this workspace.
///
/// # Errors
///
/// Propagates any error from locating the manifest or running `cargo metadata`.
#[test]
fn no_crate_violates_the_architecture_dependency_rules() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = manifest_path()?;
    let metadata = load_metadata(&manifest)?;
    let names = members_by_name(&metadata)?;

    let violations = check(&metadata);
    if !violations.is_empty() {
        let mut report = format!(
            "\n{} architectural dependency violation(s) in {}:{}",
            violations.len(),
            manifest.display(),
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<String>()
        );
        report.push_str(
            "\n\nThese are architectural defects, not style preferences (AGENTS.md §8). \
             Fix the dependency, or change the rule in SOURCE_MAPPING.md §9 and AGENTS.md \
             §8 if the boundary itself moved.",
        );
        return Err(report.into());
    }

    println!(
        "checked {} workspace members against {} `must not declare` rules, {} isolation rules, \
         and the `odysseus-domain` internal-dependency rule",
        names.len(),
        FORBIDDEN.len(),
        ISOLATED.len()
    );
    Ok(())
}

/// Guards the checker itself: the rule table must not be silently emptied.
///
/// A rule table that accidentally loses an entry still passes the gate above,
/// because a checker with no rules reports no violations. This asserts the table
/// still names the crates and dependencies the documents forbid, so deleting a
/// rule is a test failure rather than a silent loss of coverage.
///
/// # Errors
///
/// Propagates any error from locating the manifest or running `cargo metadata`.
#[test]
fn the_rule_table_still_covers_every_documented_rule() -> Result<(), Box<dyn std::error::Error>> {
    let metadata = load_metadata(&manifest_path()?)?;
    let names = members_by_name(&metadata)?;

    for expected in [
        DOMAIN,
        "odysseus-store",
        "odysseus-ui",
        "odysseus-agent",
        "odysseus-llm",
        "odysseus-tools",
        "odysseus-services",
        "odysseus-integrations",
        "odysseus-ml",
        "odysseus-web",
    ] {
        assert!(
            names.contains(expected),
            "expected {expected} to be a workspace member; the rule table is keyed by name \
             and would silently skip a rule for a renamed crate"
        );
    }

    // The exact prohibition sets, so a well-meaning edit to one of them has to be
    // a deliberate change to this assertion too.
    let covered: BTreeSet<(&str, &str)> = FORBIDDEN
        .iter()
        .flat_map(|rule| rule.deps.iter().map(move |dep| (rule.crate_name, *dep)))
        .collect();
    let expected: BTreeSet<(&str, &str)> = [
        ("odysseus-store", "axum"),
        ("odysseus-store", "leptos"),
        ("odysseus-store", "reqwest"),
        ("odysseus-llm", "axum"),
        ("odysseus-llm", "leptos"),
        ("odysseus-agent", "axum"),
        ("odysseus-agent", "leptos"),
        ("odysseus-tools", "axum"),
        ("odysseus-services", "axum"),
        ("odysseus-services", "leptos"),
        ("odysseus-integrations", "axum"),
        ("odysseus-integrations", "leptos"),
        ("odysseus-ui", "rusqlite"),
        ("odysseus-ui", "reqwest"),
    ]
    .into_iter()
    .collect();
    assert_eq!(covered, expected, "the enforced prohibition set changed");

    let isolated: BTreeSet<(&str, &str)> =
        ISOLATED.iter().map(|rule| (rule.dep, rule.owner)).collect();
    assert_eq!(
        isolated,
        BTreeSet::from([("ort", "odysseus-ml"), ("ort-sys", "odysseus-ml")]),
        "the enforced isolation set changed"
    );
    Ok(())
}

/// Proves the checker fails on a violation, without editing a real manifest.
///
/// Builds a synthetic workspace in a temp directory that declares `axum` in
/// `odysseus-store`, runs the same `check()` over its real `cargo metadata`
/// output, and asserts the violation is detected with a usable message. This is
/// the negative case: a gate that has never been seen to fail is not verified.
///
/// # Errors
///
/// Propagates any error writing the fixture, running `cargo metadata`, or loading
/// the metadata back.
#[test]
fn the_checker_detects_a_forbidden_dependency_it_was_built_to_catch()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    for member in ["domain", "store"] {
        std::fs::create_dir_all(root.join(format!("crates/{member}/src")))?;
    }

    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/domain\", \"crates/store\"]\n",
    )?;
    std::fs::write(
        root.join("crates/domain/Cargo.toml"),
        "[package]\nname = \"odysseus-domain\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(root.join("crates/domain/src/lib.rs"), "")?;
    // The violation: `odysseus-store` must never declare `axum`.
    std::fs::write(
        root.join("crates/store/Cargo.toml"),
        "[package]\nname = \"odysseus-store\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
         \n[dependencies]\naxum = \"0.8\"\n",
    )?;
    std::fs::write(root.join("crates/store/src/lib.rs"), "")?;

    let metadata = load_metadata(&root.join("Cargo.toml"))?;
    let violations = check(&metadata);

    assert_eq!(
        violations.len(),
        1,
        "expected exactly one violation, got {violations:#?}"
    );
    let violation = violations.first().ok_or("expected a violation")?;
    assert_eq!(violation.crate_name, "odysseus-store");
    assert!(
        violation.forbidden_dep.starts_with("axum"),
        "expected the axum dependency, got {}",
        violation.forbidden_dep
    );
    assert!(
        violation.manifest_path.ends_with("crates/store/Cargo.toml"),
        "the failure message must name the file the dependency was declared in, got {}",
        violation.manifest_path
    );
    Ok(())
}

/// A crate absent from the rule table must not be able to smuggle a forbidden
/// dependency past it, and a renamed dependency must still be caught.
///
/// `cargo metadata` reports the *upstream* crate name even when a dependency is
/// renamed, so `axum2 = { package = "axum" }` cannot evade the rule. This test
/// pins that behaviour, because "rename the dependency to dodge the check" is
/// exactly the kind of workaround a future contributor would try.
///
/// # Errors
///
/// Propagates any error writing the fixture, running `cargo metadata`, or loading
/// the metadata back.
#[test]
fn a_renamed_dependency_is_still_caught_by_its_upstream_name()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    for member in ["domain", "agent"] {
        std::fs::create_dir_all(root.join(format!("crates/{member}/src")))?;
    }
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/domain\", \"crates/agent\"]\n",
    )?;
    std::fs::write(
        root.join("crates/domain/Cargo.toml"),
        "[package]\nname = \"odysseus-domain\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(root.join("crates/domain/src/lib.rs"), "")?;
    std::fs::write(
        root.join("crates/agent/Cargo.toml"),
        "[package]\nname = \"odysseus-agent\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
         \n[dependencies]\nsneaky = { package = \"axum\", version = \"0.8\" }\n",
    )?;
    std::fs::write(root.join("crates/agent/src/lib.rs"), "")?;

    let metadata = load_metadata(&root.join("Cargo.toml"))?;
    let violations = check(&metadata);

    assert_eq!(
        violations.len(),
        1,
        "a renamed axum dependency must still be caught, got {violations:#?}"
    );
    let violation = violations.first().ok_or("expected a violation")?;
    assert_eq!(violation.crate_name, "odysseus-agent");
    assert!(
        violation.forbidden_dep.starts_with("axum"),
        "expected the upstream name axum, got {}",
        violation.forbidden_dep
    );
    Ok(())
}
