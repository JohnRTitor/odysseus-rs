//! Per-entry footprint ceilings (AGENTS.md §12.1 rule 7).
//!
//! A comment claiming a struct is small is not evidence. These assertions are:
//! they fail the build if a `User` grows, and §12.1 explains why that matters --
//! the bookkeeping around a value costs more than the value, so every added
//! `String` header is a real regression on a per-session record.
//!
//! The ceilings are set ~15 % above today's measurement, rounded to a round
//! number, so ordinary churn does not trip them but a deliberate layout change
//! (or an accidental `String` where a `Box<str>` belongs) does. The measured
//! values are committed in `docs/baseline/`; the gaps below are asserted too,
//! so the baseline file cannot drift away from the code without someone noticing.

use std::mem::size_of;

use odysseus_store::auth::{Privileges, User};

/// Ceilings, in bytes. Measured on rustc 1.98.1, x86_64-unknown-linux-gnu; the run
/// itself is committed in `docs/baseline/`. A ceiling sits ~15 % above its
/// measurement and is rounded, so ordinary churn does not trip it but a real
/// layout change does.
const USER_CEILING: usize = 224;
const PRIVILEGES_CEILING: usize = 56;
const USER_MEASURED: usize = 192;
const PRIVILEGES_MEASURED: usize = 48;

#[test]
fn user_fits_the_per_session_ceiling() {
    let actual = size_of::<User>();
    assert!(
        actual <= USER_CEILING,
        "User grew to {actual} bytes (ceiling {USER_CEILING}). A User is stored per \
         account and loaded whole on every login; if this crossed the ceiling the fix is \
         §12.1 -- Box<str>/Box<[T]> for the immutable fields, not a bigger allowance."
    );
    assert_eq!(
        actual, USER_MEASURED,
        "layout changed; update the baseline in docs/baseline/"
    );
}

#[test]
fn privileges_fits_the_per_user_ceiling() {
    let actual = size_of::<Privileges>();
    assert!(
        actual <= PRIVILEGES_CEILING,
        "Privileges grew to {actual} bytes (ceiling {PRIVILEGES_CEILING}). It is embedded in \
         every User, so it is multiplied by the account count."
    );
    assert_eq!(
        actual, PRIVILEGES_MEASURED,
        "layout changed; update the baseline in docs/baseline/"
    );
}

/// The reference points the ceilings are derived from. A `String` header is 24
/// bytes and `User` carries seven of them -- 1 `String`, 2 `Option<String>`, 1
/// `Option<Vec<String>>`, 3 `BTreeMap` — so 168 of its 192 bytes are pointer,
/// length and capacity that say nothing once the value is stored. §12.1 rule 1
/// says exactly those become `Box<str>`, and rule 4 says the rare large enum
/// variants get boxed. Asserting the reference keeps that reasoning checkable.
#[test]
fn the_string_header_is_what_the_ceilings_are_buying() {
    assert_eq!(
        size_of::<String>(),
        24,
        "String header grew; §12.1 reasoning needs revisiting"
    );
    assert_eq!(
        size_of::<Option<String>>(),
        24,
        "niche optimisation changed"
    );
    assert!(
        size_of::<Box<str>>() < size_of::<String>(),
        "Box<str> must stay cheaper"
    );
}
