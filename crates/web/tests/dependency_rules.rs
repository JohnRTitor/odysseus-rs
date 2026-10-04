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
//! # The rules are parsed from the documents, not transcribed here
//!
//! A hardcoded copy of the rule table is a fourth place to forget to update, and
//! it silently rots the moment a document changes. So this file **parses** the
//! two tables that state the rules -- `AGENTS.md` §8 (the source of truth) and
//! `SOURCE_MAPPING.md` §9.3 -- and asserts against what it finds. A rule added
//! to a document is enforced immediately, with no edit here.
//!
//! Two guards keep that honest:
//!
//!   * `the_two_documents_agree_on_every_rule` fails if §8 and §9.3 diverge.
//!     They had already drifted once (§9 named four crates, §8 named ten) and
//!     nothing noticed, because nothing compared them.
//!   * `every_rule_names_a_real_crate_and_every_crate_is_ruled_on` fails if a
//!     rule names a crate that no longer exists (it would match nothing) or a
//!     new crate has no row (nothing would constrain it).
//!
//! # Rules that cannot be checked here
//!
//! Some boundaries are statements about source code rather than manifests, and
//! are **not** enforced by this test:
//!
//!   * "Only `odysseus-web` knows about HTTP status codes" -- a property of which
//!     types a crate references. Enforcing it would mean parsing Rust source.
//!   * "business logic", "agent-loop control flow", bare "SQL" -- `AGENTS.md` §8
//!     forbids these for three crates, but none of them names a crate, so no
//!     dependency graph can express them. They stay review rules, recorded here
//!     so the gap is visible rather than forgotten.
//!
//! The `odysseus-ml` isolation rule *is* enforced, as far as it is a dependency
//! claim: nothing outside `odysseus-ml` may declare `ort` or `ort-sys`. The rest
//! of that intent -- that inference stays reachable through one crate -- remains
//! a review property.
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

/// The binary crates in `apps/`, which sit above the architecture rather than
/// inside it. `AGENTS.md` §8 tabulates the ten *library* crates, so these two are
/// exempt from the "every crate needs a rule row" check: they exist to compose
/// the library, and requiring a row would mean inventing prohibitions nobody
/// agreed to.
const APPS: &[&str] = &["odysseus-server", "odysseus-cli"];

/// Rules of the form "crate X must never declare D", parsed from the documents.
///
/// The table is **derived, not transcribed**. `AGENTS.md` §8 is the source of
/// truth; `SOURCE_MAPPING.md` §9.3 restates it per crate. `rules_from_agents()`
/// and `rules_from_source_mapping()` parse each, and
/// `the_two_documents_agree_on_every_rule` fails if they diverge. That is what
/// keeps the earlier drift from recurring: a rule added to one document without
/// the other is a test failure, not a stale comment.
///
/// Each entry records which documents stated it, so a failure names its source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DocumentRule {
    crate_name: String,
    deps: Vec<String>,
}

/// Splits a markdown table row into trimmed cells.
fn cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

/// Strips markdown emphasis and code ticks from a cell.
fn plain(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    let mut ticks = false;
    for ch in cell.chars() {
        match ch {
            '`' => ticks = !ticks,
            '*' | '(' | ')' if !ticks => {}
            _ => out.push(ch),
        }
    }
    out.trim().to_owned()
}

/// Parses the rule table out of a markdown table.
///
/// `deps_column` selects which cell holds the prohibition list. It is a parameter
/// because the two tables do not agree on column order: `AGENTS.md` §8 is
/// `Crate | May import | Must NOT import`, while `SOURCE_MAPPING.md` §9.3 is
/// `Crate | Must NOT declare | Source`. Reading the wrong column is a silent,
/// catastrophic failure -- it once reported `odysseus-store`'s *permitted*
/// `rusqlite` and `sqlite-vec` as violations -- so each caller names the column
/// explicitly rather than both assuming the same layout.
///
/// Prose cells are ignored: only cells that reduce to a bare comma-separated list
/// of lowercase crate names contribute entries. That is what lets a row such as
/// `odysseus-domain | (none - see the additional rule below)` parse as "no
/// prohibitions" instead of being mistaken for a dependency called "none".
fn parse_rules(text: &str, deps_column: usize) -> Vec<DocumentRule> {
    let mut rules = Vec::new();
    for line in text.lines() {
        if !line.trim_start().starts_with('|') {
            continue;
        }
        let parsed = cells(line);
        let [name_cell, rest @ ..] = parsed.as_slice() else {
            continue;
        };
        let name = plain(name_cell);
        if !name.starts_with(INTERNAL_PREFIX) {
            continue;
        }
        let Some(deps_cell) = rest.get(deps_column) else {
            continue;
        };
        let deps: Vec<String> = plain(deps_cell)
            .split(',')
            .map(|dep| dep.trim().to_owned())
            .filter(|dep| {
                !dep.is_empty()
                    && dep
                        .chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
            })
            .collect();
        rules.push(DocumentRule {
            crate_name: name,
            deps,
        });
    }
    rules
}

/// The rules stated in `AGENTS.md` §8's "Must NOT import" column.
///
/// # Errors
///
/// Returns a boxed error if the file cannot be read.
fn rules_from_agents() -> Result<Vec<DocumentRule>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(repo_root()?.join("AGENTS.md"))?;
    let section = text
        .split("## 8. Architecture")
        .nth(1)
        .and_then(|rest| rest.split("## 9.").next())
        .ok_or("could not locate the AGENTS.md §8 architecture table")?;
    // Column 0 of the remainder is "May import", so the prohibitions are column 1.
    Ok(parse_rules(section, 1))
}

/// The rules stated in `SOURCE_MAPPING.md` §9.3.
///
/// # Errors
///
/// Returns a boxed error if the file cannot be read or §9.3 is missing.
fn rules_from_source_mapping() -> Result<Vec<DocumentRule>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(repo_root()?.join("SOURCE_MAPPING.md"))?;
    let section = text
        .split("### 9.3 Enforced rules")
        .nth(1)
        .and_then(|rest| rest.split("### 9.4").next())
        .ok_or("could not locate the SOURCE_MAPPING.md §9.3 rule table")?;
    // Here column 0 of the remainder is already "Must NOT declare".
    Ok(parse_rules(section, 0))
}

/// The repository root, resolved from this crate's manifest directory.
///
/// # Errors
///
/// Returns a boxed error if `CARGO_MANIFEST_DIR` is somehow unset or is not a
/// path, which would otherwise silently resolve the rules against nothing.
fn repo_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(dir
        .parent()
        .and_then(Path::parent)
        .ok_or("CARGO_MANIFEST_DIR is not inside the workspace")?
        .to_path_buf())
}

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
///
/// The `path+` check keeps a registry package that happens to share a name with
/// a workspace member from being mistaken for it.
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

/// Every rule violation in `metadata`, given the rules parsed from `AGENTS.md` §8.
fn check(metadata: &Metadata, rules: &[DocumentRule]) -> Vec<Violation> {
    let mut violations = Vec::new();

    for rule in rules {
        let Some(package) = member(metadata, &rule.crate_name) else {
            continue;
        };
        for dep in &package.dependencies {
            if rule.deps.iter().any(|forbidden| forbidden == &dep.name) {
                violations.push(Violation {
                    crate_name: package.name.clone(),
                    forbidden_dep: format!("{} ({})", dep.name, kind_label(dep)),
                    manifest_path: package.manifest_path.display().to_string(),
                    source: "AGENTS.md §8",
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
                    source: "AGENTS.md §8 (`odysseus-domain` depends on nothing internal)",
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
/// Propagates any error from locating the manifest, running `cargo metadata`, or
/// reading the rule documents.
#[test]
fn no_crate_violates_the_architecture_dependency_rules() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = manifest_path()?;
    let metadata = load_metadata(&manifest)?;
    let names = members_by_name(&metadata)?;
    let rules = rules_from_agents()?;

    let violations = check(&metadata, &rules);
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
             Fix the dependency, or change AGENTS.md §8 and SOURCE_MAPPING.md §9.3 \
             together if the boundary itself moved.",
        );
        return Err(report.into());
    }

    println!(
        "checked {} workspace members against {} documented rules, {} isolation rules, and \
         the `odysseus-domain` internal-dependency rule",
        names.len(),
        rules.len(),
        ISOLATED.len()
    );
    Ok(())
}

/// The two documents must state the same rules.
///
/// This is the anti-drift property. `SOURCE_MAPPING.md` §9.3 drifted from
/// `AGENTS.md` §8 precisely because nothing compared them: §9 named four crates,
/// §8 named ten, and the two were never reconciled. If a rule is added to one
/// document and not the other, this fails -- so the "which document is right"
/// question cannot be answered by picking whichever one the CI happens to read.
///
/// # Errors
///
/// Propagates any error reading either document.
#[test]
fn the_two_documents_agree_on_every_rule() -> Result<(), Box<dyn std::error::Error>> {
    let agents = rules_from_agents()?;
    let mapping = rules_from_source_mapping()?;

    assert!(
        !agents.is_empty(),
        "no rules parsed from AGENTS.md §8; the parser has drifted from the table"
    );

    let mut agents_sorted = agents.clone();
    let mut mapping_sorted = mapping.clone();
    agents_sorted.sort();
    mapping_sorted.sort();

    assert_eq!(
        agents_sorted, mapping_sorted,
        "AGENTS.md §8 and SOURCE_MAPPING.md §9.3 disagree. AGENTS.md §8 is the source of \
         truth, so update §9.3 to match it in the same commit."
    );
    Ok(())
}

/// Every rule must name a crate that exists, and every **library crate** must be
/// ruled on.
///
/// Two failure modes this catches: a rule naming a crate that was renamed (the
/// rule would silently never match anything), and a new library crate being
/// added with no row in either document (so nothing constrains it at all).
///
/// The second half covers `crates/*` only. `AGENTS.md` §8 is deliberately a table
/// of the ten *library* crates; `apps/odysseus-server` and `odysseus-cli` are
/// binaries whose whole purpose is to compose the library, so they sit above the
/// architecture rather than inside it. Requiring a row for them would mean
/// inventing prohibitions nobody agreed to.
///
/// # Errors
///
/// Propagates any error reading the documents or running `cargo metadata`.
#[test]
fn every_rule_names_a_real_crate_and_every_library_crate_is_ruled_on()
-> Result<(), Box<dyn std::error::Error>> {
    let metadata = load_metadata(&manifest_path()?)?;
    let names = members_by_name(&metadata)?;
    let rules = rules_from_agents()?;

    for rule in &rules {
        assert!(
            names.contains(rule.crate_name.as_str()),
            "AGENTS.md §8 rules `{}`, which is not a workspace member; a rule for a crate \
             that does not exist matches nothing",
            rule.crate_name
        );
    }

    for name in &names {
        if APPS.contains(name) {
            continue;
        }
        assert!(
            rules.iter().any(|rule| rule.crate_name == *name),
            "{name} is a library crate with no row in the AGENTS.md §8 rule table. Add one, \
             even if its prohibition set is empty, so the omission is deliberate."
        );
    }
    Ok(())
}

/// The `odysseus-ml` isolation rule must exist in both documents.
///
/// `odysseus-ml` is the one crate with no `axum`/`leptos` row in either document
/// before this revision, and the `ort` isolation rule is the only thing keeping
/// the inference runtime swappable. If a future edit drops the crate from the
/// table, nothing should notice silently.
///
/// # Errors
///
/// Propagates any error reading either document.
#[test]
fn the_ml_isolation_rule_is_present_in_both_documents() -> Result<(), Box<dyn std::error::Error>> {
    let agents = std::fs::read_to_string(repo_root()?.join("AGENTS.md"))?;
    let mapping = std::fs::read_to_string(repo_root()?.join("SOURCE_MAPPING.md"))?;

    assert!(
        agents.contains("| `odysseus-ml` |"),
        "AGENTS.md §8 has no `odysseus-ml` row"
    );
    assert!(
        agents.contains("`ort`, `ort-sys`"),
        "AGENTS.md §8's `odysseus-ml` row does not name the ort dependencies it owns"
    );
    assert!(
        mapping.contains("### 9.4 Additional rules"),
        "SOURCE_MAPPING.md §9.4 is missing"
    );
    assert!(
        mapping.contains("`ort` and `ort-sys` may be declared only by"),
        "SOURCE_MAPPING.md §9.4 does not state the ort isolation rule"
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
    let violations = check(&metadata, &rules_from_agents()?);

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
    let violations = check(&metadata, &rules_from_agents()?);

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
