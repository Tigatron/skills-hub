use std::{fs, path::PathBuf};

#[test]
fn committed_typescript_bindings_match_rust_commands() {
    let output = tempfile::tempdir().unwrap().path().join("bindings.ts");
    skills_hub_lib::export_typescript_bindings(&output).unwrap();

    let expected_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
    let expected = fs::read_to_string(&expected_path).unwrap_or_else(|error| {
        panic!(
            "read committed bindings at {}: {error}; run pnpm bindings:generate",
            expected_path.display()
        )
    });
    let actual = fs::read_to_string(output).unwrap();

    assert_eq!(
        actual, expected,
        "Rust command contracts changed; run pnpm bindings:generate"
    );
}
