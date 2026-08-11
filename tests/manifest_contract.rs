use std::path::Path;

use serde_json::Value;

#[test]
fn manifest_release_files_exist_and_versions_match() {
    let manifest: Value = serde_json::from_str(include_str!("../.tabularium")).unwrap();
    assert_eq!(manifest["name"], "bigquery");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["executable"], "bigquery-plugin");
    for extension in manifest["ui_extensions"].as_array().unwrap() {
        let module = extension["module"].as_str().unwrap();
        assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join(module).is_file());
    }
}

#[test]
fn manifest_declares_safe_bigquery_defaults() {
    let manifest: Value = serde_json::from_str(include_str!("../.tabularium")).unwrap();
    assert_eq!(manifest["capabilities"]["connection_string"], false);
    assert_eq!(manifest["capabilities"]["supports_ssl"], false);
    assert_eq!(manifest["capabilities"]["explain"], true);
    assert_eq!(manifest["capabilities"]["identifier_quote"], "`");
}
