#[path = "src/installation_policy_build.rs"]
mod installation_policy_build;

fn main() {
    use std::path::{Path, PathBuf};
    println!("cargo:rerun-if-env-changed=DUDUCLAW_INSTALLATION_POLICY_PATH");
    println!("cargo:rerun-if-changed=src/installation_policy_build.rs");
    let policy = match std::env::var_os("DUDUCLAW_INSTALLATION_POLICY_PATH") {
        None => None,
        Some(value) => {
            let value = value
                .to_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| panic!("Installation policy path must be nonempty UTF-8"));
            if value.len() > 4096 || value.chars().any(char::is_control) {
                panic!("Installation policy path contains invalid characters");
            }
            let path = Path::new(value);
            if !path.is_absolute() {
                panic!("Installation policy path must be absolute");
            }
            // Cargo must rebuild when the private input changes. Private build logs
            // and this input belong to the private packaging pipeline.
            println!("cargo:rerun-if-changed={}", path.display());
            Some(
                installation_policy_build::read_policy(path)
                    .unwrap_or_else(|reason| panic!("Installation policy build refused: {reason}")),
            )
        }
    };
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    std::fs::write(
        output.join("installation_policy.rs"),
        installation_policy_build::generate(policy.as_ref()),
    )
    .expect("write generated installation policy");
}
