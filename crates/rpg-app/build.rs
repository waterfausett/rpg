fn main() {
    // App commands get generated `allow-<command>` permissions; the settings capability grants them.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
        "settings_get",
        "settings_set",
        "settings_choose_sound",
        "settings_add_folder",
        "settings_preview_sound",
        "settings_open_sounds_folder",
        "settings_open_config_file",
    ])))
    .expect("running tauri-build");
}
