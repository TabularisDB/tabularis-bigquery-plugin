use std::path::Path;

use serde_json::Value;

#[test]
fn manifest_release_files_exist_and_versions_match() {
    let manifest: Value = serde_json::from_str(include_str!("../.tabularium")).unwrap();
    assert_eq!(manifest["name"], "bigquery");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["executable"], "bigquery-plugin");
    let extension = &manifest["ui_extensions"].as_array().unwrap()[0];
    assert_eq!(extension["module"], "ui/dist/connection-fields.js");

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(root.join("ui/src/connection-fields.tsx").is_file());
    assert!(root.join("ui/vite.config.ts").is_file());
}

#[test]
fn manifest_declares_safe_bigquery_defaults() {
    let manifest: Value = serde_json::from_str(include_str!("../.tabularium")).unwrap();
    assert_eq!(manifest["capabilities"]["connection_string"], false);
    assert_eq!(manifest["capabilities"]["supports_ssl"], false);
    assert_eq!(manifest["capabilities"]["explain"], true);
    assert_eq!(manifest["capabilities"]["identifier_quote"], "`");
    assert_eq!(manifest["connection_fields"]["host"]["hidden"], true);
    assert_eq!(manifest["connection_fields"]["port"]["hidden"], true);
    assert_eq!(manifest["connection_fields"]["username"]["hidden"], true);
    assert_eq!(manifest["connection_fields"]["password"]["hidden"], true);
    assert_eq!(
        manifest["connection_fields"]["database"]["label"],
        "GCP Project ID"
    );
}
