//! Test-only helpers shared by more than one module's tests.
//!
//! `read_data` reaches the REAL shipped `calendars/` and `presets/`, so the golden tests in
//! `calendar.rs` and `preset.rs` run against what actually ships (RELEASE-004). One copy, because
//! two copies of a path helper drift and then only one of them is wrong.

// Read a shipped data file from the repo (CARGO_MANIFEST_DIR is crates/cli). The golden tests below
// exercise the REAL calendars/ and presets/ that ship, through the real engine - so a typo in a
// holiday date, a wrong valid_from, or a broken observation rule fails the suite (RELEASE-004).
pub(crate) fn read_data(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}
