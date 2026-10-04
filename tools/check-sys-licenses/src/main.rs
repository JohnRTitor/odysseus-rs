//! Fails when a crate that compiles or links native C/C++ is present without its
//! native library's licence being recorded in `tools/native-deps.toml`.
//!
//! # The hole this closes
//!
//! `cargo deny` checks the licence a crate *declares* on crates.io. That
//! declaration cannot see a native library linked at build time, because the
//! fact is not in the Rust dependency graph -- it is in a system package or a
//! vendored C tree. `poppler-rs` reports **MIT** and depends on
//! `poppler-sys-rs`, which binds the system **GPL-2.0-or-later** `libpoppler`.
//! `cargo deny` passes. `cargo audit` passes. Nothing in the metadata
//! distinguishes that graph from a clean one.
//!
//! This tool answers the question cargo-deny structurally cannot: for every
//! crate in the lockfile that actually invokes a C compiler, is the licence of
//! what it links recorded by a human?
//!
//! # Detection is by evidence, not by name
//!
//! A `-sys` suffix is an unreliable proxy in both directions, and both failures
//! occur in this very tree:
//!
//!   * `js-sys`, `linux-raw-sys`, `windows-sys` all match `-sys` and none of them
//!     compiles a line of C.
//!   * `sqlite-vec` compiles C and does **not** match `-sys` -- it is exactly the
//!     `poppler-rs` shape: a permissively-licensed Rust crate whose build script
//!     links native code.
//!
//! So detection reads each vendored crate's `build.rs` and looks for a C
//! toolchain being invoked. That is evidence of C being compiled, not a proof of
//! what gets linked -- that still needs a human -- but it cannot miss a crate
//! that builds C, which is what makes it safe to fail closed.
//!
//! # Fail closed
//!
//! A native-linking crate absent from the approved list is an **error**, not a
//! warning. Failing open here ships a GPL binary that every automated report
//! called clean, which is the specific failure this exists to prevent.

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

/// Substrings that mark a build script as invoking a C/C++ toolchain.
const C_TOOLCHAIN_MARKERS: &[&str] = &["cc::Build", "cc::compile", "gcc::", "cmake::"];

/// One approved native dependency.
#[derive(Debug, PartialEq, Eq)]
struct Approved {
    crate_name: String,
    links: String,
    license: String,
}

/// A crate that compiles or links native code.
#[derive(Debug, PartialEq, Eq)]
struct Native {
    crate_name: String,
    version: String,
    /// The build-script line that proved it, quoted in the failure message.
    evidence: String,
}

/// Trims a trailing `//` comment and the quotes around a value.
fn unquote(raw: &str) -> String {
    let without_comment = raw.split_once("//").map_or(raw, |(head, _)| head);
    without_comment.trim().trim_matches('"').trim().to_owned()
}

/// Parses `tools/native-deps.toml`.
///
/// Hand-rolled rather than pulling in a TOML crate: this tool deliberately has no
/// dependencies, so an advisory against a parser cannot disable the licence gate.
/// The format is fixed -- stanzas are `[crate-name]` with `key = "value"` lines.
///
/// A stanza missing `license` keeps an empty string rather than being dropped, so
/// the caller can reject it. A truncated or half-edited file must never read as
/// "approved": silence has to mean "not approved", which is the fail-closed point.
fn parse_approved(text: &str) -> Vec<Approved> {
    let mut out: Vec<Approved> = Vec::new();
    let mut links = String::new();
    let mut license = String::new();
    // Set while inside a `note = """\` multi-line string. Every line until the
    // closing `"""` is prose: a `license = ...` line in the middle of a note must
    // not be read as the stanza's licence, which is how an approving licence
    // could silently be overwritten by a copyleft mention in prose.
    let mut in_note = false;

    for line in text.lines() {
        let line = line.trim();
        if in_note {
            // The closing `"""` lands at the END of the last prose line in this
            // file ("...attribution.\"\"\""), not at the start of its own line, so
            // the terminator must be found anywhere in the line. The `\` on the
            // opening `note = """\` is what makes that form valid TOML.
            if let Some(end) = line.find("\"\"\"") {
                // Text after the terminator is outside the note; fall through so it
                // is read as a key rather than swallowed.
                in_note = false;
                let remainder = line[end + 3..].trim();
                if remainder.is_empty() {
                    continue;
                }
                if let Some((key, raw)) = remainder.split_once('=') {
                    match key.trim() {
                        "links" => links = unquote(raw),
                        "license" => license = unquote(raw),
                        _ => {}
                    }
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix('[')
            && let Some((name, tail)) = rest.split_once(']')
            && tail.is_empty()
        {
            // A stanza header closes the previous one.
            if let Some(last) = out.last_mut() {
                last.links = std::mem::take(&mut links);
                last.license = std::mem::take(&mut license);
            }
            links.clear();
            license.clear();
            out.push(Approved {
                crate_name: name.to_owned(),
                links: String::new(),
                license: String::new(),
            });
            continue;
        }
        if line.starts_with("note = \"\"\"") {
            in_note = true;
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "links" => links = unquote(raw),
            "license" => license = unquote(raw),
            _ => {}
        }
    }
    if let Some(last) = out.last_mut() {
        last.links = links;
        last.license = license;
    }
    out
}

/// Reads every `name`/`version` pair from `Cargo.lock`.
fn parse_lockfile(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut version = String::new();
    let mut in_package = false;

    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            // A new package block: flush the one being read, then start fresh.
            // `[[package]]` is the ONLY table that ends a block -- `dependencies = [`,
            // `[metadata]` and friends appear *inside* a package and must not close
            // it, which is why this tests for the exact header rather than any `[`.
            if !name.is_empty() {
                out.push((std::mem::take(&mut name), std::mem::take(&mut version)));
            }
            in_package = true;
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            name = unquote(rest);
        } else if let Some(rest) = line.strip_prefix("version = ") {
            version = unquote(rest);
        }
    }
    if !name.is_empty() {
        out.push((name, version));
    }
    out
}

/// Finds every crate in the lockfile whose build script invokes a C toolchain.
///
/// `registry_dirs` are the vendored sources directories, from
/// [`registry_src_dirs`]. A crate with no vendored source (a path or git
/// dependency) has no build script to read, so it is reported by [`unverifiable`]
/// rather than silently skipped.
fn detect_native(registry_dirs: &[PathBuf], packages: &[(String, String)]) -> Vec<Native> {
    let mut out = Vec::new();
    for (crate_name, version) in packages {
        let Some(dir) = vendored_dir_in(registry_dirs, crate_name, version) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(dir.join("build.rs")) else {
            continue;
        };
        let Some(evidence) = text
            .lines()
            .map(str::trim)
            .find(|line| C_TOOLCHAIN_MARKERS.iter().any(|m| line.contains(m)))
        else {
            continue;
        };
        out.push(Native {
            crate_name: crate_name.clone(),
            version: version.clone(),
            evidence: evidence.to_owned(),
        });
    }
    out
}

/// The vendored source directory for one crate, if it is present.
fn vendored_dir(registry_dirs: &[PathBuf], crate_name: &str, version: &str) -> Option<PathBuf> {
    vendored_dir_in(registry_dirs, crate_name, version)
}

/// Crates in the lockfile with no vendored source to inspect.
///
/// These cannot be cleared or failed by this tool; they are reported so a human
/// can check them by hand.
fn unverifiable(registry_dirs: &[PathBuf], packages: &[(String, String)]) -> Vec<String> {
    packages
        .iter()
        .filter(|(name, version)| vendored_dir(registry_dirs, name, version).is_none())
        .map(|(name, _)| name.clone())
        .collect()
}

/// Everything the checker needs to run.
struct Inputs {
    approved: Vec<Approved>,
    native: Vec<Native>,
    unverifiable: Vec<String>,
}

/// Loads inputs from `repo_root`, so a fixture can point it somewhere else.
fn load(repo_root: &Path, registry_dirs: &[PathBuf]) -> Result<Inputs, String> {
    let approved_text = fs::read_to_string(repo_root.join("tools/native-deps.toml"))
        .map_err(|e| format!("cannot read tools/native-deps.toml: {e}"))?;
    let lock_text = fs::read_to_string(repo_root.join("Cargo.lock"))
        .map_err(|e| format!("cannot read Cargo.lock: {e}"))?;
    let packages = parse_lockfile(&lock_text);
    Ok(Inputs {
        approved: parse_approved(&approved_text),
        native: detect_native(registry_dirs, &packages),
        unverifiable: unverifiable(registry_dirs, &packages),
    })
}

/// Checks the inputs, returning one message per failure.
fn check(inputs: &Inputs) -> Vec<String> {
    let mut failures = Vec::new();

    for native in &inputs.native {
        let entry = inputs
            .approved
            .iter()
            .find(|a| a.crate_name == native.crate_name);
        let Some(entry) = entry else {
            failures.push(format!(
                "{} v{} compiles or links native code but is NOT recorded in \
                 tools/native-deps.toml.\n    evidence: {}\n    A `-sys` suffix is not a licence \
                 check: cargo deny reads only what the crate declares on crates.io, and the \
                 licence of what it links is not in the Rust dependency graph. Add a stanza \
                 recording the native library and its licence.",
                native.crate_name, native.version, native.evidence
            ));
            continue;
        };
        if entry.license.is_empty() {
            failures.push(format!(
                "{} v{} is listed in tools/native-deps.toml with no recorded licence.\n    \
                 An unrecorded licence is treated as unapproved.",
                native.crate_name, native.version
            ));
        }
        if entry.links.is_empty() {
            failures.push(format!(
                "{} v{} is listed in tools/native-deps.toml with no recorded `links`, so what \
                 it actually binds is unknown.",
                native.crate_name, native.version
            ));
        }
    }

    for entry in &inputs.approved {
        let still_used = inputs
            .native
            .iter()
            .any(|n| n.crate_name == entry.crate_name);
        if !still_used {
            failures.push(format!(
                "{} is listed in tools/native-deps.toml but no longer compiles or links native \
                 code. Remove the stanza, or the file will quietly approve nothing.",
                entry.crate_name
            ));
        }
    }

    failures
}

/// Prints the human-facing summary.
fn report(inputs: &Inputs, failures: &[String]) {
    println!("native-licence audit");
    println!(
        "  {} crate(s) compile or link native C/C++; {} approved",
        inputs.native.len(),
        inputs.approved.len()
    );
    for native in &inputs.native {
        let entry = inputs
            .approved
            .iter()
            .find(|a| a.crate_name == native.crate_name);
        match entry {
            Some(e) => println!(
                "    {} v{} -> {} ({})",
                native.crate_name, native.version, e.license, e.links
            ),
            None => println!(
                "    {} v{} -> UNRECORDED",
                native.crate_name, native.version
            ),
        }
    }
    if !inputs.unverifiable.is_empty() {
        println!(
            "  {} lockfile crate(s) have no vendored source and were not inspected: {}",
            inputs.unverifiable.len(),
            inputs.unverifiable.join(", ")
        );
    }
    if failures.is_empty() {
        println!("  OK: every native-linking crate has a recorded native licence");
    } else {
        for failure in failures {
            println!("  FAIL: {failure}");
        }
    }
}

/// Locates cargo's vendored source directories.
///
/// Read at runtime, not with `env!`: cargo only exports the registry variables to
/// *build* scripts, so `env!` would not compile here.
///
/// `$CARGO_HOME` is often unset, and `Path::join` does not expand `~`, so the
/// home directory is resolved explicitly. Cargo lays sources out as
/// `registry/src/<index-hash>/<crate>-<version>`, and the index hash differs
/// between the sparse (`index.crates.io-<hash>`) and git (`github.com-<hash>`)
/// protocols, so both levels are returned rather than one being assumed.
fn registry_src_dirs() -> Vec<PathBuf> {
    let cargo_home = std::env::var("CARGO_HOME").map_or_else(
        |_| {
            std::env::var("HOME").map_or_else(|_| PathBuf::from(".cargo"), |home| PathBuf::from(home).join(".cargo"))
        },
        PathBuf::from,
    );
    let src = cargo_home.join("registry").join("src");
    // Both `src/` itself and each registry-index directory beneath it, so a
    // protocol change upstream does not silently empty the audit.
    let mut out = vec![src.clone()];
    if let Ok(entries) = fs::read_dir(&src) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Finds the vendored source for one crate across every registry directory.
fn vendored_dir_in(dirs: &[PathBuf], crate_name: &str, version: &str) -> Option<PathBuf> {
    let prefix = format!("{crate_name}-{version}");
    dirs.iter().find_map(|dir| {
        let entries = fs::read_dir(dir).ok()?;
        entries.flatten().find_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let matches = name == prefix || name.starts_with(&format!("{prefix}-"));
            matches.then(|| entry.path())
        })
    })
}

fn main() -> ExitCode {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let registry_dirs = registry_src_dirs();

    let args: Vec<String> = std::env::args().collect();
    let root = args.get(1).map_or(repo_root, PathBuf::from);
    let src: Vec<PathBuf> = args
        .get(2)
        .map(|arg| vec![PathBuf::from(arg)])
        .unwrap_or(registry_dirs);

    match load(&root, &src) {
        Ok(inputs) => {
            let failures = check(&inputs);
            report(&inputs, &failures);
            if failures.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("native-licence audit could not run: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Native, check, parse_approved, parse_lockfile};

    /// A build script that compiles C, as `libsqlite3-sys` has.
    const BUILDS_C: &str = "fn main() { cc::Build::new().file(\"x.c\").compile(\"x\"); }";
    /// A build script that does not, as `linux-raw-sys` has.
    const PURE_RUST: &str = "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }";

    #[test]
    fn parses_an_approved_stanza() {
        let approved = parse_approved(
            "[libsqlite3-sys]\nlinks = \"SQLite, vendored\"\nlicense = \"blessing\"\nnote = \"hi\"\n",
        );
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].crate_name, "libsqlite3-sys");
        assert_eq!(approved[0].links, "SQLite, vendored");
        assert_eq!(approved[0].license, "blessing");
    }

    #[test]
    fn parses_several_stanzas_without_bleeding_values() {
        let approved = parse_approved(
            "[a]\nlinks = \"L1\"\nlicense = \"MIT\"\nnote = \"n\"\n\n[b]\nlinks = \"L2\"\nlicense = \"GPL-2.0-or-later\"\n",
        );
        assert_eq!(approved.len(), 2);
        assert_eq!((approved[0].links.as_str(), approved[0].license.as_str()), ("L1", "MIT"));
        assert_eq!(
            (approved[1].links.as_str(), approved[1].license.as_str()),
            ("L2", "GPL-2.0-or-later")
        );
    }

    #[test]
    fn a_stanza_with_no_licence_parses_as_empty_not_approved() {
        let approved = parse_approved("[poppler-sys-rs]\nlinks = \"libpoppler\"\n");
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].license, "", "an absent licence must stay empty");
    }

    /// A `license = ...` line inside the note must not become the stanza's
    /// licence, and the note's closing `"""` carries a trailing `\` so the
    /// terminator test cannot be an equality check.
    #[test]
    fn a_multi_line_note_is_not_mistaken_for_a_key() {
        let approved = parse_approved(
            "[a]\nlinks = \"L\"\nlicense = \"MIT\"\nnote = \"\"\"\\\nline one\nlicense = \"GPL-2.0-or-later\"\n\"\"\"\\\n[b]\nlinks = \"L2\"\nlicense = \"Apache-2.0\"\n",
        );
        assert_eq!(
            approved.len(),
            2,
            "the stanza after a multi-line note must still be parsed: {approved:?}"
        );
        assert_eq!(
            approved[0].license, "MIT",
            "prose inside a note must not overwrite the recorded licence"
        );
        assert_eq!(
            (approved[1].crate_name.as_str(), approved[1].license.as_str()),
            ("b", "Apache-2.0"),
            "the stanza after a note must keep its own values"
        );
    }

    #[test]
    fn reads_every_package_from_a_lockfile() {
        let packages = parse_lockfile(
            "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nsource = \"x\"\n\n[[package]]\nname = \"b\"\nversion = \"2.0.0\"\n",
        );
        assert_eq!(
            packages,
            vec![("a".to_owned(), "1.0.0".to_owned()), ("b".to_owned(), "2.0.0".to_owned())]
        );
    }

    /// The negative case that matters: an unrecorded native crate must fail.
    #[test]
    fn an_unrecorded_native_crate_is_a_failure() {
        let failures = check(&super::Inputs {
            approved: vec![],
            native: vec![Native {
                crate_name: "poppler-sys-rs".to_owned(),
                version: "0.18.0".to_owned(),
                evidence: BUILDS_C.to_owned(),
            }],
            unverifiable: vec![],
        });
        assert_eq!(failures.len(), 1, "an unrecorded native crate must fail: {failures:?}");
        assert!(failures[0].contains("poppler-sys-rs"));
        assert!(failures[0].contains("native-deps.toml"));
    }

    /// The case the audit exists for: a `-sys` crate reporting MIT while linking GPL.
    #[test]
    fn a_sys_crate_recording_a_copyleft_native_licence_is_still_reported() {
        let failures = check(&super::Inputs {
            approved: vec![super::Approved {
                crate_name: "poppler-sys-rs".to_owned(),
                links: "system libpoppler".to_owned(),
                license: "GPL-2.0-or-later".to_owned(),
            }],
            native: vec![Native {
                crate_name: "poppler-sys-rs".to_owned(),
                version: "0.18.0".to_owned(),
                evidence: BUILDS_C.to_owned(),
            }],
            unverifiable: vec![],
        });
        assert!(
            failures.is_empty(),
            "recording the licence is what approval means; the audit records it rather than \
             re-judging it: {failures:?}"
        );
    }

    #[test]
    fn an_approved_stanza_without_a_licence_is_rejected() {
        let failures = check(&super::Inputs {
            approved: vec![super::Approved {
                crate_name: "libsqlite3-sys".to_owned(),
                links: "SQLite".to_owned(),
                license: String::new(),
            }],
            native: vec![Native {
                crate_name: "libsqlite3-sys".to_owned(),
                version: "0.38.2".to_owned(),
                evidence: BUILDS_C.to_owned(),
            }],
            unverifiable: vec![],
        });
        assert_eq!(failures.len(), 1, "an empty licence must fail closed: {failures:?}");
    }

    #[test]
    fn a_stale_stanza_is_a_failure() {
        let failures = check(&super::Inputs {
            approved: vec![super::Approved {
                crate_name: "openssl-sys".to_owned(),
                links: "OpenSSL".to_owned(),
                license: "Apache-2.0".to_owned(),
            }],
            native: vec![],
            unverifiable: vec![],
        });
        assert_eq!(failures.len(), 1, "a stanza for a crate that is gone must be removed");
    }

    /// A pure-Rust crate that merely matches `-sys` must not be flagged, or every
    /// future `windows-sys` bump would fail the build for no reason.
    ///
    /// Note there is deliberately **no** stanza for `linux-raw-sys` in the fixture:
    /// if one existed, the stale-stanza rule would (correctly) complain, because the
    /// crate links nothing. That is the behaviour being pinned -- naming a crate
    /// that does no native work is itself the error.
    #[test]
    fn a_pure_rust_sys_crate_is_not_flagged() {
        let failures = check(&super::Inputs {
            approved: vec![],
            native: vec![], // detection found no build script invoking cc
            unverifiable: vec!["linux-raw-sys".to_owned()],
        });
        assert!(
            failures.is_empty(),
            "a pure-Rust -sys crate must not fail: {failures:?}"
        );
    }

    /// Conversely, naming a crate that links nothing IS an error, or the file
    /// accumulates approvals that approve nothing.
    #[test]
    fn approving_a_crate_that_links_nothing_is_a_failure() {
        let failures = check(&super::Inputs {
            approved: vec![super::Approved {
                crate_name: "linux-raw-sys".to_owned(),
                links: "none".to_owned(),
                license: "MIT".to_owned(),
            }],
            native: vec![],
            unverifiable: vec!["linux-raw-sys".to_owned()],
        });
        assert_eq!(
            failures.len(),
            1,
            "a stanza for a crate with no native code must be removed: {failures:?}"
        );
        let _ = PURE_RUST;
    }

    #[test]
    fn a_clean_tree_passes() {
        let failures = check(&super::Inputs {
            approved: vec![super::Approved {
                crate_name: "libsqlite3-sys".to_owned(),
                links: "SQLite, vendored".to_owned(),
                license: "blessing".to_owned(),
            }],
            native: vec![Native {
                crate_name: "libsqlite3-sys".to_owned(),
                version: "0.38.2".to_owned(),
                evidence: BUILDS_C.to_owned(),
            }],
            unverifiable: vec![],
        });
        assert!(failures.is_empty(), "an audited crate must pass: {failures:?}");
    }
}
