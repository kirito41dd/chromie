use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn private_config_validates_permissions_target_and_key_kind_without_password() {
    let directory = std::env::temp_dir().join(format!("chromie-client-config-{}", Uuid::now_v7()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("client.json");
    let mut value = serde_json::json!({"url":"http://127.0.0.1:8732","space":"demo","agent_id":Uuid::now_v7(),"mcp_key":format!("cm_mcp_{}","a".repeat(64)),"hook_key":format!("cm_hook_{}","b".repeat(64))});
    let write = |value: &serde_json::Value| {
        fs::write(&path, value.to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    };
    write(&value);
    let client = load(KeyKind::Hook, Some(&path)).unwrap();
    assert_eq!(client.space_id, "demo");
    assert!(client.key.starts_with("cm_hook_"));
    assert!(
        load(KeyKind::Mcp, Some(&path))
            .unwrap()
            .key
            .starts_with("cm_mcp_")
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load(KeyKind::Hook, Some(&path)).is_err());
    write(&value);
    let link = directory.join("link.json");
    symlink(&path, &link).unwrap();
    assert!(load(KeyKind::Hook, Some(&link)).is_err());
    for url in [
        "ftp://example.com",
        "https://user:password@example.com",
        "https://example.com/?key=credential",
    ] {
        value["url"] = serde_json::json!(url);
        write(&value);
        assert!(load(KeyKind::Hook, Some(&path)).is_err());
    }
    for url in [
        "http://192.0.2.10:8732",
        "http://10.0.0.5:8732",
        "http://192.168.1.5:8732",
    ] {
        value["url"] = serde_json::json!(url);
        write(&value);
        assert!(load(KeyKind::Mcp, Some(&path)).is_ok());
    }
    value["url"] = serde_json::json!("https://example.com");
    value["hook_key"] = value["mcp_key"].clone();
    write(&value);
    assert!(load(KeyKind::Hook, Some(&path)).is_err());
    fs::remove_dir_all(directory).unwrap();
}
