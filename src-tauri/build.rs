fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut overlay = std::env::var("TAURI_CONFIG")
            .ok()
            .map(|value| serde_json::from_str::<serde_json::Value>(&value))
            .transpose()
            .expect("TAURI_CONFIG must contain valid JSON")
            .unwrap_or_else(|| serde_json::json!({}));
        let root = overlay
            .as_object_mut()
            .expect("TAURI_CONFIG must be a JSON object");
        root.insert(
            "productName".into(),
            serde_json::Value::String("飞花 - PetalDesk".into()),
        );
        let bundle = root
            .entry("bundle")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .expect("TAURI_CONFIG.bundle must be a JSON object");
        bundle.insert(
            "publisher".into(),
            serde_json::Value::String("飞花 - PetalDesk".into()),
        );
        std::env::set_var(
            "TAURI_CONFIG",
            serde_json::to_string(&overlay).expect("serialize Windows resource branding"),
        );
    }

    tauri_build::build()
}
