//! Shared by the tests (a dev-dependency only).
//!
//! Tests that drive Node.js, the fixture apps' packages or the real Rayfin SDK skip themselves when
//! those are missing, so `cargo test --workspace` passes on any machine. `scripts/check.mjs` and CI
//! install everything first and set [`STRICT_VAR`], so that a test that would skip fails instead:
//! a moved fixture or a renamed package can't turn a test off unnoticed.
//!
//! Prerequisites that CI doesn't have (an external app checkout, SQL Server, an Azure sign-in) keep
//! skipping with a plain `eprintln!("skipped: …")`.
//!
//! The SDK conformance tests run the real Rayfin SDK on every line rayfin-local serves
//! ([`sdk_lines`]).

use std::{
    fmt::Display,
    path::{Path, PathBuf},
};

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

/// A Rayfin SDK line rayfin-local serves, and the `node_modules` the tests take it from.
pub struct SdkLine {
    /// The version the fixture pins.
    pub version: &'static str,
    /// The variable that overrides [`Self::node_modules`].
    pub var: &'static str,
    pub node_modules: PathBuf,
}

/// Every SDK line rayfin-local serves: 1.36 from `RAYFIN_SDK_NODE_MODULES` (by default the basic
/// fixture's `node_modules`) and 2.0 from `RAYFIN_SDK_V2_NODE_MODULES` (by default the `sdk-v2`
/// fixture's).
pub fn sdk_lines() -> [SdkLine; 2] {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/apps");
    let line = |version, var, fixture: &str| SdkLine {
        version,
        var,
        node_modules: std::env::var_os(var)
            .map(PathBuf::from)
            .unwrap_or_else(|| fixtures.join(fixture).join("node_modules")),
    };
    [
        line("1.36.2", "RAYFIN_SDK_NODE_MODULES", "basic"),
        line("2.0.0", "RAYFIN_SDK_V2_NODE_MODULES", "sdk-v2"),
    ]
}

impl SdkLine {
    /// The major version: what the test scripts branch on where the SDK's API changed.
    pub fn major(&self) -> u64 {
        self.version.split('.').next().unwrap().parse().unwrap()
    }

    /// [`Self::node_modules`] when `package` is installed there on this line; otherwise
    /// [`skip`]s and returns `None`. Another line installed there fails the test: the variable
    /// points at the wrong `node_modules`.
    #[track_caller]
    pub fn with(&self, package: &str) -> Option<&Path> {
        let manifest = self.node_modules.join(package).join("package.json");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            skip(format_args!(
                "{} is missing: set {} to a node_modules with the Rayfin SDK {}",
                manifest.display(),
                self.var,
                self.version
            ));
            return None;
        };
        let installed = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|manifest| manifest["version"].as_str().map(str::to_owned))
            .unwrap_or_default();
        assert_eq!(
            minor_line(&installed),
            minor_line(self.version),
            "{} has {package} {installed}, not the SDK {} line: check {}",
            self.node_modules.display(),
            self.version,
            self.var
        );
        Some(&self.node_modules)
    }
}

fn minor_line(version: &str) -> Vec<&str> {
    version.split(['.', '-']).take(2).collect()
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
