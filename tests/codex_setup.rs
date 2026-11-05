#![cfg(unix)]

use chromie::codex_setup;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use toml_edit::DocumentMut;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    codex: PathBuf,
    data: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("chromie-codex-setup-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.join("download.json");
        let value = json!({"url":"http://127.0.0.1:8732","space":"demo","agent_id":Uuid::now_v7(),
            "mcp_key":format!("cm_mcp_{}", "a".repeat(64)),"hook_key":format!("cm_hook_{}", "b".repeat(64))});
        private(&source, &serde_json::to_vec(&value).unwrap());
        Self {
            source,
            codex: root.join("codex"),
            data: root.join("private"),
            root,
        }
    }
    fn run(&self, replace: bool) -> chromie::error::Result<Value> {
        codex_setup::run(&self.source, &self.codex, &self.data, replace)
    }
    fn existing(&self, text: &str) {
        fs::create_dir_all(&self.codex).unwrap();
        private(&self.codex.join("config.toml"), text.as_bytes());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn setup_preserves_other_servers_comments_and_private_backups_and_is_repeatable() {
    let f = Fixture::new();
    let initial = "# 用户保留的注释\nmodel = \"user-model\"\n\n[mcp_servers.other]\nurl = \"https://example.com/mcp\" # 保留该注释\n";
    f.existing(initial);
    let result = f.run(false).unwrap();
    assert_eq!(
        fs::read_to_string(result["backup"].as_str().unwrap()).unwrap(),
        initial
    );
    let text = fs::read_to_string(f.codex.join("config.toml")).unwrap();
    assert!(text.contains("# 用户保留的注释") && text.contains("# 保留该注释"));
    let document = text.parse::<DocumentMut>().unwrap();
    assert_eq!(document["model"].as_str(), Some("user-model"));
    assert_eq!(
        document["mcp_servers"]["other"]["url"].as_str(),
        Some("https://example.com/mcp")
    );
    assert_eq!(
        document["mcp_servers"]["chromie"]["url"].as_str(),
        Some("http://127.0.0.1:8732/spaces/demo/mcp")
    );
    for path in [
        f.codex.join("config.toml"),
        f.data.join("client.json"),
        PathBuf::from(result["backup"].as_str().unwrap()),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&f.data).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!result.to_string().contains(&"a".repeat(64)));
    assert!(!result.to_string().contains(&"b".repeat(64)));
    fs::set_permissions(
        f.codex.join("config.toml"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(f.run(false).unwrap()["changed"], false);
    assert_eq!(
        fs::metadata(f.codex.join("config.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!f.codex.join("chromie-setup.lock").exists());
}

#[test]
fn setup_refuses_different_connections_invalid_toml_and_stale_lock_without_overwrite() {
    for initial in [
        "[mcp_servers.chromie]\nurl = \"https://another.example/mcp\"\n",
        "invalid = [",
    ] {
        let f = Fixture::new();
        f.existing(initial);
        assert!(f.run(false).is_err());
        assert_eq!(
            fs::read_to_string(f.codex.join("config.toml")).unwrap(),
            initial
        );
        assert!(!f.data.join("client.json").exists());
    }
    let f = Fixture::new();
    f.existing("# existing\n");
    private(&f.codex.join("chromie-setup.lock"), b"another setup");
    assert!(f.run(false).is_err());
    assert_eq!(
        fs::read_to_string(f.codex.join("chromie-setup.lock")).unwrap(),
        "another setup"
    );
}

#[test]
fn setup_rejects_public_or_symlinked_inputs_targets_and_wrong_key_purpose() {
    let f = Fixture::new();
    fs::set_permissions(&f.source, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(f.run(false).is_err());
    fs::set_permissions(&f.source, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = f.root.join("alias.json");
    symlink(&f.source, &alias).unwrap();
    assert!(codex_setup::run(&alias, &f.codex, &f.data, false).is_err());
    f.existing("# 原始文件\n");
    let original = f.root.join("original.toml");
    fs::rename(f.codex.join("config.toml"), &original).unwrap();
    symlink(&original, f.codex.join("config.toml")).unwrap();
    assert!(f.run(false).is_err());
    assert_eq!(fs::read_to_string(original).unwrap(), "# 原始文件\n");
    let mut value: Value = serde_json::from_slice(&fs::read(&f.source).unwrap()).unwrap();
    value["hook_key"] = value["mcp_key"].clone();
    private(&f.source, &serde_json::to_vec(&value).unwrap());
    assert!(f.run(false).is_err());
}

#[test]
fn key_rotation_keeps_identity_and_replacement_requires_explicit_flag() {
    let f = Fixture::new();
    f.run(false).unwrap();
    let mut value: Value = serde_json::from_slice(&fs::read(&f.source).unwrap()).unwrap();
    value["mcp_key"] = json!(format!("cm_mcp_{}", "c".repeat(64)));
    private(&f.source, &serde_json::to_vec(&value).unwrap());
    assert!(f.run(false).is_ok());
    value["agent_id"] = json!(Uuid::now_v7());
    private(&f.source, &serde_json::to_vec(&value).unwrap());
    assert!(f.run(false).is_err());
    assert!(f.run(true).is_ok());
}

#[test]
fn preflight_is_read_only_and_uses_the_same_conflict_rules_as_application() {
    let f = Fixture::new();
    assert!(!codex_setup::needs_replacement(Some(&f.source), &f.codex, &f.data).unwrap());
    assert!(!f.codex.exists() && !f.data.exists());
    f.existing("# 原有配置\n[mcp_servers.chromie]\nurl=\"https://different.example/mcp\"\n");
    let before = fs::read(f.codex.join("config.toml")).unwrap();
    assert!(codex_setup::needs_replacement(None, &f.codex, &f.data).unwrap());
    assert!(codex_setup::needs_replacement(Some(&f.source), &f.codex, &f.data).unwrap());
    assert_eq!(fs::read(f.codex.join("config.toml")).unwrap(), before);
    assert!(!f.data.exists());
    assert_eq!(f.run(false).unwrap_err().code, "local_connection_conflict");
    f.run(true).unwrap();
    assert!(!codex_setup::needs_replacement(Some(&f.source), &f.codex, &f.data).unwrap());
    assert!(codex_setup::needs_replacement(None, &f.codex, &f.data).unwrap());
}

#[test]
fn environment_authorization_cannot_silently_override_managed_static_authentication() {
    let f = Fixture::new();
    f.run(false).unwrap();
    let path = f.codex.join("config.toml");
    let mut document = fs::read_to_string(&path)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
    document["mcp_servers"]["chromie"]["env_http_headers"]["authorization"] =
        toml_edit::value("FICTIONAL_AUTH");
    f.existing(&document.to_string());
    assert!(codex_setup::needs_replacement(Some(&f.source), &f.codex, &f.data).unwrap());
    assert_eq!(f.run(false).unwrap_err().code, "local_connection_conflict");
    f.run(true).unwrap();
    let fixed = fs::read_to_string(path).unwrap();
    assert!(!fixed.contains("env_http_headers"));
}
