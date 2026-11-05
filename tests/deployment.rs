use chromie::deployment::{self, Deployment};
use std::{fs, path::PathBuf};
use uuid::Uuid;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("chromie-deployment-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn config(&self) -> PathBuf {
        self.0.join("config")
    }
    fn data(&self) -> PathBuf {
        self.0.join("data")
    }
    fn init(&self) {
        assert!(deployment::initialize(&self.config(), &self.data()).unwrap());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn initialization_is_repeatable_and_preserves_all_original_credentials_and_key() {
    let f = Fixture::new();
    f.init();
    let names = [
        "instance-key.pem",
        "credentials.json",
        "initial-space.txt",
        "instance.json",
        "chromie.json",
        "mysql/password",
        "mysql/root-password",
        "qdrant/config.yaml",
    ];
    let before = names.map(|p| fs::read(f.config().join(p)).unwrap());
    assert!(!deployment::initialize(&f.config(), &f.data()).unwrap());
    for (p, bytes) in names.iter().zip(before) {
        assert_eq!(fs::read(f.config().join(p)).unwrap(), bytes);
    }
    let cfg = Deployment::load(&f.config())
        .unwrap()
        .runtime_config()
        .unwrap();
    assert_eq!(cfg.listen.to_string(), "0.0.0.0:8732");
    assert_eq!(cfg.qdrant_url, "http://qdrant:6334");
    assert!(cfg.public_origin.is_none());
    let second = Fixture::new();
    second.init();
    assert_ne!(
        fs::read(f.config().join("credentials.json")).unwrap(),
        fs::read(second.config().join("credentials.json")).unwrap()
    );
}

#[test]
fn populated_data_never_receives_a_new_configuration_or_key() {
    let f = Fixture::new();
    fs::create_dir_all(f.data().join("mysql")).unwrap();
    fs::write(
        f.data().join("mysql/existing-data"),
        b"fictional existing database",
    )
    .unwrap();
    assert!(deployment::initialize(&f.config(), &f.data()).is_err());
    assert!(!f.config().join("instance-key.pem").exists());
    assert!(!f.config().join("credentials.json").exists());
}

#[test]
fn missing_or_replaced_key_and_missing_credentials_are_never_repaired_by_regeneration() {
    for target in [
        "instance-key.pem",
        "credentials.json",
        "qdrant/config.yaml",
        "mysql/password",
    ] {
        let f = Fixture::new();
        f.init();
        fs::remove_file(f.config().join(target)).unwrap();
        assert!(deployment::initialize(&f.config(), &f.data()).is_err());
        assert!(!f.config().join(target).exists());
    }
    let f = Fixture::new();
    f.init();
    let old = fs::read(f.config().join("instance-key.pem")).unwrap();
    let other = Fixture::new();
    other.init();
    fs::write(
        f.config().join("instance-key.pem"),
        fs::read(other.config().join("instance-key.pem")).unwrap(),
    )
    .unwrap();
    assert!(Deployment::load(&f.config()).is_err());
    fs::write(f.config().join("instance-key.pem"), old).unwrap();
    assert!(Deployment::load(&f.config()).is_ok());
}

#[test]
fn partial_empty_bootstrap_resumes_without_changing_existing_key_or_credentials() {
    let f = Fixture::new();
    f.init();
    let key = fs::read(f.config().join("instance-key.pem")).unwrap();
    let creds = fs::read(f.config().join("credentials.json")).unwrap();
    fs::remove_file(f.config().join("instance.json")).unwrap();
    assert!(deployment::initialize(&f.config(), &f.data()).unwrap());
    assert_eq!(fs::read(f.config().join("instance-key.pem")).unwrap(), key);
    assert_eq!(
        fs::read(f.config().join("credentials.json")).unwrap(),
        creds
    );
}

#[test]
fn partial_instance_with_lost_key_is_not_treated_as_brand_new() {
    let f = Fixture::new();
    f.init();
    fs::remove_file(f.config().join("instance.json")).unwrap();
    fs::remove_file(f.config().join("instance-key.pem")).unwrap();
    assert!(deployment::initialize(&f.config(), &f.data()).is_err());
    assert!(!f.config().join("instance-key.pem").exists());
}

#[test]
fn completed_instance_with_missing_manifest_or_database_is_rejected() {
    let f = Fixture::new();
    f.init();
    fs::write(
        f.config().join("initialized.json"),
        br#"{"space_id":"00000000-0000-0000-0000-000000000000"}"#,
    )
    .unwrap();
    assert!(deployment::initialize(&f.config(), &f.data()).is_err());
    fs::remove_file(f.config().join("instance.json")).unwrap();
    assert!(deployment::initialize(&f.config(), &f.data()).is_err());
    assert!(!f.config().join("instance.json").exists());
}

#[test]
fn origin_validation_accepts_http_and_https_without_network_location_rules() {
    let f = Fixture::new();
    f.init();
    let path = f.config().join("chromie.json");
    fs::write(
        &path,
        br#"{"version":1,"public_origin":"ftp://example.com"}"#,
    )
    .unwrap();
    assert!(Deployment::load(&f.config()).is_err());
    fs::write(
        &path,
        br#"{"version":1,"public_origin":"http://public.example.com"}"#,
    )
    .unwrap();
    assert!(Deployment::load(&f.config()).is_ok());
    fs::write(
        &path,
        br#"{"version":1,"public_origin":"https://memory.example.com"}"#,
    )
    .unwrap();
    assert!(!deployment::initialize(&f.config(), &f.data()).unwrap());
    assert_eq!(
        Deployment::load(&f.config())
            .unwrap()
            .runtime_config()
            .unwrap()
            .public_origin
            .as_deref(),
        Some("https://memory.example.com")
    );
}

#[test]
fn initialization_lock_cannot_be_shared_by_concurrent_initializers() {
    let f = Fixture::new();
    f.init();
    let lock = deployment::lock(&f.config()).unwrap();
    assert!(deployment::initialize(&f.config(), &f.data()).is_err());
    drop(lock);
    assert!(deployment::initialize(&f.config(), &f.data()).is_ok());
}

#[cfg(unix)]
#[test]
fn private_permissions_and_symlink_boundaries_are_enforced() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new();
    f.init();
    assert_eq!(
        fs::metadata(f.config()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for p in [
        "instance-key.pem",
        "credentials.json",
        "initial-space.txt",
        "chromie.json",
    ] {
        assert_eq!(
            fs::metadata(f.config().join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let target = f.config().join("credentials.json");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Deployment::load(&f.config()).is_err());
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&target, f.0.join("private.json")).unwrap();
    symlink(f.0.join("private.json"), &target).unwrap();
    assert!(Deployment::load(&f.config()).is_err());
    let other = Fixture::new();
    symlink(f.config(), other.config()).unwrap();
    assert!(deployment::initialize(&other.config(), &other.data()).is_err());
}
