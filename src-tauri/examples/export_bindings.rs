use std::{io, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");

    skills_hub_lib::export_typescript_bindings(&output).map_err(io::Error::other)?;
    println!("generated {}", output.display());

    Ok(())
}
