use chromie::secret_contract::{SecretContract, TextKind, inspect_json};
use serde_json::{Value, json};
#[cfg(unix)]
#[allow(dead_code)]
mod support;

fn contract(value: Value) -> SecretContract {
    SecretContract::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn json_contract() -> SecretContract {
    contract(
        json!({"version":1,"format":"json","fields":{"username":{"pointer":"/auth/user","type":"string"},"password":{"pointer":"/auth/pass","type":"string"}}}),
    )
}

#[test]
fn contracts_validate_without_coercion_or_exposing_values() {
    let text = SecretContract::Text {
        version: 1,
        kind: TextKind::Password,
    };
    assert!(text.validate_payload(b"001234\n").is_ok());
    assert!(text.validate_payload(&[0xff]).is_err());
    let schema = json_contract();
    assert!(schema.validate_payload(br#"{"auth":{"user":"fixture","pass":"fictional-value-789"},"extra":"not-published"}"#).is_ok());
    for bytes in [
        br#"{"auth":{"user":9,"pass":"fictional-value-789"}}"#.as_slice(),
        br#"{"auth":{"user":"x"}}"#,
        b"fictional-value-789",
    ] {
        let error = schema.validate_payload(bytes).unwrap_err();
        assert_eq!(error.code, "secret_contract_mismatch");
        assert!(!error.message.contains("fictional-value-789"));
    }
    assert!(
        !schema
            .value()
            .unwrap()
            .to_string()
            .contains("fictional-value-789")
    );
    for invalid in [
        json!({"version":2,"format":"text","kind":"password"}),
        json!({"version":1,"format":"json","fields":{}}),
        json!({"version":1,"format":"text","kind":"password","value":"never-accepted"}),
        json!({"version":1,"format":"json","fields":{"password":{"pointer":"/bad~9","type":"string"}}}),
    ] {
        assert!(SecretContract::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    let escaped = contract(
        json!({"version":1,"format":"json","fields":{"token":{"pointer":"/a~1b/~0","type":"string"}}}),
    );
    assert!(
        escaped
            .validate_payload(br#"{"a/b":{"~":"fictional"}}"#)
            .is_ok()
    );
    let inventory = inspect_json(
        br#"{"auth":{"user":"fictional-username","pass":"fictional-password"},"a/b":[9]}"#,
    )
    .unwrap()
    .to_string();
    assert!(!inventory.contains("fictional-username") && !inventory.contains("fictional-password"));
    assert!(inventory.contains("/a~1b/0"));
}

#[cfg(unix)]
mod integration {
    use super::support;
    use super::*;
    use chromie::{
        entity,
        search::Search,
        space_key::SpaceKey,
        store::Store,
        types::{Forget, GetMemory, SecretContractUpdate, SecretCreate, SecretUpdate, View},
        web_session::WebSessions,
    };
    use migration::MigratorTrait;
    use sea_orm::{ConnectOptions, Database, EntityTrait};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::{Command, Output},
    };
    use uuid::Uuid;

    async fn cli(config: &Path, args: Vec<String>) -> Output {
        let config = config.to_owned();
        tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_chromie"))
                .arg("secret")
                .args(args)
                .arg("--config")
                .arg(config)
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    }
    fn private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn read(id: Uuid) -> GetMemory {
        GetMemory {
            memory_id: Some(id),
            event_id: None,
            view: View::Current,
            cursor: None,
            max_chars: Some(8000),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "需要隔离 MySQL；仅虚构数据"]
    async fn contracts_guard_payload_versions_preserve_legacy_and_work_through_cli() {
        let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
        assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
        let mut opts = ConnectOptions::new(url);
        opts.sqlx_logging(false);
        let db = Database::connect(opts).await.unwrap();
        migration::Migrator::up(&db, None).await.unwrap();
        let store = support::enable(Store::new(
            db,
            Search::new("http://127.0.0.1:9", None).unwrap(),
        ))
        .await;
        let slug: String = Uuid::now_v7()
            .simple()
            .to_string()
            .chars()
            .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
            .collect();
        let key = SpaceKey::parse(&slug).unwrap();
        store
            .create_space(
                key.clone(),
                format!("虚构格式契约-{slug}"),
                "fictional-contract-space-password".into(),
            )
            .await
            .unwrap();
        let space = store.resolve_space(&key).await.unwrap();
        let (agent, mcp, hook) = support::grant(&store, space, true).await;
        let app = chromie::http::router_with_sessions(
            store.clone(),
            None,
            WebSessions::from_key(&support::root()).unwrap(),
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();
        let root = std::env::temp_dir().join(format!("chromie-contract-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("client.json");
        private(
            &config,
            &serde_json::to_vec(
                &json!({"url":base,"space":slug,"agent_id":agent,"mcp_key":mcp,"hook_key":hook}),
            )
            .unwrap(),
        );
        let legacy = store
            .create_secret(
                space,
                SecretCreate {
                    label: "虚构旧文本密码".into(),
                    project_key: None,
                    request_id: Uuid::now_v7(),
                    contract: None,
                },
                b"001234\n",
            )
            .await
            .unwrap();
        let id = Uuid::parse_str(legacy["memory_id"].as_str().unwrap()).unwrap();
        let reference = legacy["secret_ref"].as_str().unwrap();
        assert_eq!(
            store.get(space, read(id)).await.unwrap()["contract"]["format"],
            "unknown"
        );
        let before = entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
        let rid = Uuid::now_v7();
        let args = vec![
            "describe".into(),
            "--ref".into(),
            reference.into(),
            "--revision".into(),
            "1".into(),
            "--format".into(),
            "text".into(),
            "--request-id".into(),
            rid.to_string(),
        ];
        for _ in 0..2 {
            let output = cli(&config, args.clone()).await;
            assert!(output.status.success(), "补契约失败，未回显输出");
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap()["revision"],
                2
            );
        }
        let after = entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.ciphertext, after.ciphertext);
        assert_eq!(before.nonce, after.nonce);
        assert_eq!(before.payload_revision, after.payload_revision);
        let detail = store.get(space, read(id)).await.unwrap();
        assert_eq!(detail["contract"]["format"], "text");
        let (_, denied_key, _) = support::grant(&store, space, false).await;
        let denied = http
            .put(format!("{base}/spaces/{slug}/secrets/{id}/contract"))
            .bearer_auth(denied_key)
            .json(&SecretContractUpdate {
                request_id: Uuid::now_v7(),
                expected_revision: 2,
                contract: SecretContract::Text {
                    version: 1,
                    kind: TextKind::Password,
                },
            })
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 403);
        let historical = store
            .get(
                space,
                GetMemory {
                    cursor: Some("1:0".into()),
                    ..read(id)
                },
            )
            .await
            .unwrap();
        assert!(historical.get("contract").is_none());
        let out = root.join("out.bin");
        private(&out, b"keep-original");
        let get = vec![
            "get".into(),
            "--ref".into(),
            reference.into(),
            "--out".into(),
            out.to_string_lossy().into(),
            "--replace".into(),
            "--revision".into(),
            "1".into(),
        ];
        assert!(!cli(&config, get.clone()).await.status.success());
        assert_eq!(fs::read(&out).unwrap(), b"keep-original");
        let mut get2 = get;
        *get2.last_mut().unwrap() = "2".into();
        assert!(cli(&config, get2).await.status.success());
        assert_eq!(fs::read(&out).unwrap(), b"001234\n");
        let fixture=br#"{"auth":{"user":"fixture-user","pass":"fictional-private-value"},"private_extra":"unshared-value"}"#;
        let source = root.join("source.json");
        private(&source, fixture);
        let schema = root.join("contract.json");
        private(&schema, &serde_json::to_vec(&json_contract()).unwrap());
        let put = cli(
            &config,
            vec![
                "put".into(),
                "--label".into(),
                "虚构 JSON 凭据".into(),
                "--file".into(),
                source.to_string_lossy().into(),
                "--contract".into(),
                schema.to_string_lossy().into(),
            ],
        )
        .await;
        assert!(put.status.success(), "JSON 契约录入失败，未回显输出");
        let saved: Value = serde_json::from_slice(&put.stdout).unwrap();
        let jid = Uuid::parse_str(saved["memory_id"].as_str().unwrap()).unwrap();
        let jref = saved["secret_ref"].as_str().unwrap();
        let info = store.get(space, read(jid)).await.unwrap();
        assert_eq!(info["contract"]["format"], "json");
        assert!(!info.to_string().contains("fictional-private-value"));
        assert!(!info.to_string().contains("private_extra"));
        let full_contract = json_contract();
        let mut changed_contract = full_contract.value().unwrap();
        changed_contract["fields"]["password"]["pointer"] = "/missing".into();
        let mismatch=http.put(format!("{base}/spaces/{slug}/secrets/{jid}/contract")).bearer_auth(&mcp).json(&json!({"request_id":Uuid::now_v7(),"expected_revision":1,"contract":changed_contract})).send().await.unwrap();
        assert_eq!(mismatch.status(), 400);
        assert_eq!(store.get(space, read(jid)).await.unwrap()["revision"], 1);
        let inspection = root.join("structure.json");
        let inspected = cli(
            &config,
            vec![
                "inspect".into(),
                "--ref".into(),
                jref.into(),
                "--revision".into(),
                "1".into(),
                "--out".into(),
                inspection.to_string_lossy().into(),
            ],
        )
        .await;
        assert!(inspected.status.success());
        assert!(!String::from_utf8_lossy(&inspected.stdout).contains("private_extra"));
        let structure = fs::read_to_string(&inspection).unwrap();
        assert!(structure.contains("private_extra"));
        assert!(
            !structure.contains("fictional-private-value") && !structure.contains("unshared-value")
        );
        let error = store
            .update_secret(
                space,
                jid,
                SecretUpdate {
                    request_id: Uuid::now_v7(),
                    expected_revision: 1,
                    contract: None,
                },
                b"wrong-format",
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "secret_contract_mismatch");
        let changed = store
            .update_secret(
                space,
                jid,
                SecretUpdate {
                    request_id: Uuid::now_v7(),
                    expected_revision: 1,
                    contract: Some(SecretContract::Text {
                        version: 1,
                        kind: TextKind::ApiToken,
                    }),
                },
                b"fictional-api-token",
            )
            .await
            .unwrap();
        assert_eq!(changed["revision"], 2);
        assert!(store.read_secret_at(space, jid, Some(1)).await.is_err());
        let response = http
            .get(format!(
                "{base}/spaces/{slug}/secrets/{jid}?expected_revision=1"
            ))
            .bearer_auth(&mcp)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("fictional-api-token")
        );
        store
            .forget(
                space,
                Forget {
                    memory_id: Some(jid),
                    event_id: None,
                    expected_revision: Some(2),
                    request_id: Uuid::now_v7(),
                },
            )
            .await
            .unwrap();
        let row = entity::secrets::Entity::find_by_id((space.bytes(), jid.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
        assert!(row.contract.is_none() && row.ciphertext.is_none());
        assert!(
            store
                .set_secret_contract(
                    space,
                    jid,
                    SecretContractUpdate {
                        request_id: Uuid::now_v7(),
                        expected_revision: 2,
                        contract: json_contract()
                    }
                )
                .await
                .is_err()
        );
        for output in [&put, &inspected] {
            let output = [output.stdout.as_slice(), output.stderr.as_slice()].concat();
            let text = String::from_utf8_lossy(&output);
            assert!(
                !text.contains(&mcp)
                    && !text.contains(&hook)
                    && !text.contains("fictional-private-value")
            );
        }
        task.abort();
        fs::remove_dir_all(root).unwrap();
    }
}
