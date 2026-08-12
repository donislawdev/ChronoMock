//! Architecture guard for untouchable rule 16: the logical core must not depend on
//! any interface layer. A guard written in prose but absent from code is worse than
//! none (untouchable rule 12) - so this is a real test that fails on violation.

use std::path::PathBuf;

#[test]
fn core_does_not_depend_on_interface_layers() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("core")
        .join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", manifest.display()));
    let value: toml::Value = text.parse().expect("core Cargo.toml is valid TOML");

    // The core may never pull in the protocol, mechanism, or CLI layers.
    let forbidden = ["chrono-proto", "chrono-mech", "chrono-cli"];

    for table_key in ["dependencies", "build-dependencies"] {
        if let Some(deps) = value.get(table_key).and_then(|v| v.as_table()) {
            for name in deps.keys() {
                assert!(
                    !forbidden.contains(&name.as_str()),
                    "Rule 16 violated: chrono-core depends on '{name}' in [{table_key}]"
                );
            }
        }
    }
}
