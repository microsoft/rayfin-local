//! Shared by the tests (a dev-dependency only).
//!
//! Tests that drive Node.js, the fixture apps' packages or the real Rayfin SDK skip themselves when
//! those are missing, so `cargo test --workspace` passes on any machine. `scripts/check.mjs` and CI
//! install everything first and set [`STRICT_VAR`], so that a test that would skip fails instead:
//! a moved fixture or a renamed package can't turn a test off unnoticed.
//!
//! Prerequisites that CI doesn't have (an external app checkout, SQL Server, an Azure sign-in) keep
//! skipping with a plain `eprintln!("skipped: …")`.

use std::fmt::Display;

/// `RAYFIN_LOCAL_TEST_STRICT`: set (to anything but `0`), a missing prerequisite fails the test.
pub const STRICT_VAR: &str = "RAYFIN_LOCAL_TEST_STRICT";

/// Whether [`STRICT_VAR`] is set.
pub fn strict() -> bool {
    std::env::var_os(STRICT_VAR).is_some_and(|value| !value.is_empty() && value != "0")
}

/// Notes that the calling test is skipped because `reason`; in strict mode, fails it instead.
/// The caller returns right after.
#[track_caller]
pub fn skip(reason: impl Display) {
    skip_unless(strict(), &reason);
}

#[track_caller]
fn skip_unless(strict: bool, reason: &dyn Display) {
    assert!(
        !strict,
        "{reason}. {STRICT_VAR} is set, so this fails instead of skipping: run \
         `node scripts/check.mjs setup` to install what the tests need."
    );
    eprintln!("skipped: {reason}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_mode_turns_a_skip_into_a_failure() {
        skip_unless(false, &"a prerequisite is missing");
        let failed = std::panic::catch_unwind(|| skip_unless(true, &"a prerequisite is missing"));
        let message = failed.unwrap_err();
        let message = message.downcast_ref::<String>().unwrap();
        assert!(message.starts_with("a prerequisite is missing. RAYFIN_LOCAL_TEST_STRICT is set"));
    }
}
