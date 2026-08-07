use std::time::{SystemTime, UNIX_EPOCH};

fn timestamp_from_env(name: &str) -> Option<u64> {
    match std::env::var(name) {
        Ok(value) => Some(
            value
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("{name} must be a Unix timestamp in seconds")),
        ),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("{name} must contain valid Unicode")
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=PETALDESK_BUILD_TIMESTAMP");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let build_timestamp = timestamp_from_env("PETALDESK_BUILD_TIMESTAMP")
        .or_else(|| timestamp_from_env("SOURCE_DATE_EPOCH"))
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after the Unix epoch")
                .as_secs()
        });
    println!("cargo:rustc-env=PETALDESK_BUILD_TIMESTAMP={build_timestamp}");

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
