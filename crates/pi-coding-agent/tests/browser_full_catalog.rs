use pi_coding_agent::modes::agents_view::{agents_view_mode::deserialize_saved_session_info, native_wire::normalize_browser_numbers};

#[test]
#[ignore = "requires a private full-catalogue capture, supplied explicitly"]
fn real_copied_catalog_survives_browser_deserialization() {
    let path = std::env::var("OPTIMUS_BROWSER_CATALOG_FIXTURE").expect("explicit private fixture path");
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let value = normalize_browser_numbers(value);
    let rows = value["data"]["sessions"].as_array().unwrap();
    assert!(rows.len() > 100, "must test the full copied catalogue");
    let mut failures = std::collections::BTreeMap::new();
    let mut passed = 0;
    for row in rows {
        match deserialize_saved_session_info(row) {
            Ok(_) => passed += 1,
            Err(error) => *failures.entry(error).or_insert(0) += 1,
        }
    }
    eprintln!("Catalog rows={} accepted={} errors={:?}", rows.len(), passed, failures);
    assert!(failures.is_empty(), "browser silently rejects saved rows: {:?}", failures);
}
