fn main() {
    // Re-embed icons when they change. tauri_build reruns on tauri.conf.json by
    // default, so regenerating icons/* without touching the config would
    // otherwise leave the old (e.g. white-fringed) icon baked into the binary
    // until the next unrelated rebuild.
    println!("cargo:rerun-if-changed=icons");

    // Declare every app command in the ACL app manifest. Remote origins (the
    // dashboard served by the selected gateway) can only invoke commands that
    // resolve to an explicit capability grant — without this manifest no
    // `allow-*` permissions exist for app commands and every remote invoke is
    // rejected with "not allowed by ACL". NOTE: once this manifest exists,
    // LOCAL webviews also require the grants — keep capabilities/default.json
    // and capabilities/remote-gateway.json in sync with this list.
    let commands: &[&str] = &[
        "open_main_window",
        "open_external_url",
        "pet_list",
        "pet_generate",
        "pet_active_get",
        "pet_activate",
        "pet_remove",
        "pet_load_active",
        "pet_model_status",
        "pet_model_download",
        "pet_context_menu",
        "pet_set_scale",
        "pet_get_scale",
        "pet_open_studio",
        "gateway_discover",
        "gateway_health",
        "gateway_select",
        "gateway_last",
        "gateway_local_status",
        "gateway_start_local",
        "gateway_open_picker",
    ];
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(commands)),
    )
    .expect("failed to run tauri-build");
}
