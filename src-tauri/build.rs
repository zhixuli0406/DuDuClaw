fn main() {
    // Re-embed icons when they change. tauri_build reruns on tauri.conf.json by
    // default, so regenerating icons/* without touching the config would
    // otherwise leave the old (e.g. white-fringed) icon baked into the binary
    // until the next unrelated rebuild.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
