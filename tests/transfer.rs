mod support;
use chromie::{
    entity::{
        agent_connections, memories, memory_imports, memory_revisions, outbox_tasks, source_events,
        transfer_receipts,
    },
    search::Search,
    secret_contract::{SecretContract, TextKind},
    secret_crypto::SecretCipher,
    server_key::ServerKey,
    space_key::SpaceKey,
    store::Store,
    transfer::{Bundle, Record, Secret, SecretBytes},
    types::{Correct, Forget, GetMemory, Remember, Scope, SecretCreate, SpaceId, View},
    web_session::WebSessions,
};
use migration::MigratorTrait;
use p256::{
    SecretKey,
    elliptic_curve::zeroize::Zeroizing,
    pkcs8::{EncodePrivateKey, LineEnding},
};
use sea_orm::{ColumnTrait, ConnectOptions, Database, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::{Value, json};
use std::io::Write;
use uuid::Uuid;
const PASSWORD: &str = "fictional-transfer-password";
const PHRASE: &str = "fictional-migration-passphrase";
struct Fixture {
    store: Store,
    space: SpaceId,
    slug: String,
    url: String,
    cookie: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(variable: &str, seed: u8) -> Fixture {
    let url = std::env::var(variable).unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let pem = SecretKey::from_slice(&[seed; 32])
        .unwrap()
        .to_pkcs8_pem(LineEnding::LF)
        .unwrap();
    let root = ServerKey::from_pem(&pem).unwrap();
    let store = Store::new(
        db,
        Search::new(&std::env::var("CHROMIE_QDRANT_URL").unwrap(), None).unwrap(),
    )
    .enable_secrets(SecretCipher::from_key(&root).unwrap())
    .await
    .unwrap();
    let slug = format!(
        "soul-{}",
        Uuid::now_v7()
            .simple()
            .to_string()
            .chars()
            .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
            .collect::<String>()
    );
    let slug = slug.chars().take(30).collect::<String>();
    let key = SpaceKey::parse(&slug).unwrap();
    store
        .create_space(key.clone(), format!("虚构迁移-{seed}"), PASSWORD.into())
        .await
        .unwrap();
    let space = store.resolve_space(&key).await.unwrap();
    let router = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&root).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/spaces/{slug}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let response = reqwest::Client::new()
        .post(format!("{url}/login"))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Fixture {
        store,
        space,
        slug,
        url,
        cookie,
        server,
    }
}
async fn remember(f: &Fixture, content: String) -> Uuid {
    let v = f
        .store
        .remember(
            f.space,
            Remember {
                content,
                request_id: Uuid::now_v7(),
                scope: Scope::Project {
                    project_key: "fictional-project".into(),
                },
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    Uuid::parse_str(v["memory_id"].as_str().unwrap()).unwrap()
}
async fn correct(f: &Fixture, id: Uuid, revision: i32, content: &str) {
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: id,
                expected_revision: revision,
                request_id: Uuid::now_v7(),
                reason: "虚构更正".into(),
                content: Some(content.into()),
                scope: None,
                source_refs: None,
            },
        )
        .await
        .unwrap();
}
async fn forget(f: &Fixture, id: Uuid, revision: i32) {
    f.store
        .forget(
            f.space,
            Forget {
                memory_id: Some(id),
                event_id: None,
                expected_revision: Some(revision),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
}
async fn get(f: &Fixture, id: Uuid) -> Value {
    f.store
        .get(
            f.space,
            GetMemory {
                memory_id: Some(id),
                event_id: None,
                view: View::Current,
                cursor: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap()
}
async fn export(f: &Fixture) -> Vec<u8> {
    let r = reqwest::Client::new()
        .post(format!("{}/transfer/export", f.url))
        .header("cookie", &f.cookie)
        .json(&json!({"space_password":PASSWORD,"passphrase":PHRASE}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    r.bytes().await.unwrap().to_vec()
}
fn decode(bytes: &[u8]) -> Bundle {
    let d = age::Decryptor::new(bytes).unwrap();
    let i = age::scrypt::Identity::new(age::secrecy::SecretString::from(PHRASE.to_string()));
    serde_json::from_reader(
        d.decrypt(std::iter::once(&i as &dyn age::Identity))
            .unwrap(),
    )
    .unwrap()
}
fn encode(bundle: &Bundle) -> Vec<u8> {
    let mut r = age::scrypt::Recipient::new(age::secrecy::SecretString::from(PHRASE.to_string()));
    r.set_work_factor(2);
    let e = age::Encryptor::with_recipients(std::iter::once(&r as &dyn age::Recipient)).unwrap();
    let mut out = Vec::new();
    let mut writer = e.wrap_output(&mut out).unwrap();
    writer
        .write_all(&serde_json::to_vec(bundle).unwrap())
        .unwrap();
    writer.finish().unwrap();
    out
}
fn multipart(bytes: &[u8], password: Option<&str>, phrase: &str, digest: Option<&str>) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in password
        .map(|p| ("space_password", p))
        .into_iter()
        .chain(std::iter::once(("passphrase", phrase)))
        .chain(digest.map(|v| ("preview_digest", v)))
    {
        body.extend_from_slice(format!("--soul-boundary\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend_from_slice(b"--soul-boundary\r\nContent-Disposition: form-data; name=\"package\"; filename=\"fictional.chromie\"\r\nContent-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n--soul-boundary--\r\n");
    body
}
async fn upload(
    f: &Fixture,
    op: &str,
    bytes: &[u8],
    password: &str,
    phrase: &str,
    digest: Option<&str>,
    cookie: bool,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!("{}/transfer/{op}", f.url))
        .header(
            "Content-Type",
            "multipart/form-data; boundary=soul-boundary",
        )
        .body(multipart(bytes, Some(password), phrase, digest));
    if cookie {
        request = request.header("cookie", &f.cookie);
    }
    request.send().await.unwrap()
}
async fn preview(f: &Fixture, bytes: &[u8]) -> Value {
    let r = upload(f, "preview", bytes, PASSWORD, PHRASE, None, true).await;
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}
async fn confirm(f: &Fixture, bytes: &[u8], preview: &Value) -> Value {
    let r = upload(
        f,
        "confirm",
        bytes,
        PASSWORD,
        PHRASE,
        preview["preview_digest"].as_str(),
        true,
    )
    .await;
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

async fn preview_page(
    f: &Fixture,
    bytes: &[u8],
    page: &str,
    digest: Option<&str>,
) -> reqwest::Response {
    let mut body = format!(
        "--soul-boundary\r\nContent-Disposition: form-data; name=\"page\"\r\n\r\n{page}\r\n"
    )
    .into_bytes();
    body.extend(multipart(bytes, Some(PASSWORD), PHRASE, digest));
    reqwest::Client::new()
        .post(format!("{}/transfer/preview", f.url))
        .header(
            "Content-Type",
            "multipart/form-data; boundary=soul-boundary",
        )
        .header("cookie", &f.cookie)
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn unlock_request(f: &Fixture, cookie: &str, password: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/transfer/unlock", f.url))
        .header("cookie", cookie)
        .json(&json!({"space_password":password}))
        .send()
        .await
        .unwrap()
}
async fn unlocked_status(f: &Fixture, cookie: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("{}/transfer/unlock", f.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
async fn grant_upload(
    f: &Fixture,
    cookie: &str,
    op: &str,
    bytes: &[u8],
    digest: Option<&str>,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/transfer/{op}", f.url))
        .header("cookie", cookie)
        .header(
            "Content-Type",
            "multipart/form-data; boundary=soul-boundary",
        )
        .body(multipart(bytes, None, PHRASE, digest))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "需要隔离 MySQL 库及本地 Qdrant"]
async fn transfer_unlock_is_reused_by_the_same_login_and_revoked_by_lock_logout_and_password_change()
 {
    let target = fixture("CHROMIE_TRANSFER_TARGET_DATABASE_URL", 72).await;
    let client = reqwest::Client::new();
    let export = || {
        client
            .post(format!("{}/transfer/export", target.url))
            .header("cookie", &target.cookie)
            .json(&json!({"passphrase":PHRASE}))
    };
    assert_eq!(
        unlocked_status(&target, &target.cookie).await["unlocked"],
        false
    );
    let denied = export().send().await.unwrap();
    assert_eq!(denied.status(), 403);
    assert_eq!(
        denied.json::<Value>().await.unwrap()["code"],
        "transfer_locked"
    );
    let wrong = unlock_request(&target, &target.cookie, "fictional-wrong-password").await;
    assert_eq!(wrong.status(), 401);
    assert_eq!(
        wrong.json::<Value>().await.unwrap()["code"],
        "transfer_password_invalid"
    );
    let (_, mcp, _) = support::grant(&target.store, target.space, false).await;
    let denied = client
        .post(format!("{}/transfer/unlock", target.url))
        .bearer_auth(mcp)
        .json(&json!({"space_password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        401,
        "Agent 密钥和空间密码不能替代 Web 登录态"
    );
    let unlocked = unlock_request(&target, &target.cookie, PASSWORD).await;
    assert_eq!(unlocked.status(), 200);
    let first: Value = unlocked.json().await.unwrap();
    assert_eq!(first["unlocked"], true);
    let again = unlocked_status(&target, &target.cookie).await;
    assert_eq!(again["unlocked"], true);
    assert!(
        again["expires_in"].as_u64().unwrap() <= first["expires_in"].as_u64().unwrap(),
        "状态查询不能延长授权"
    );
    let response = client
        .post(format!("{}/login", target.url))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    let second = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert!(second != target.cookie, "同秒登录也必须是独立会话");
    assert_eq!(unlocked_status(&target, &second).await["unlocked"], false);
    assert_eq!(
        export().send().await.unwrap().status(),
        200,
        "同一登录可不再传空间密码进行炼化"
    );
    let bundle = Bundle {
        format: "chromie-soul".into(),
        version: 1,
        package_id: Uuid::now_v7(),
        source_space: "fictional-grant".into(),
        source_name: "虚构解锁验收".into(),
        exported_at: "2026-11-05T00:00:00Z".into(),
        records: vec![Record {
            memory_id: Uuid::now_v7(),
            source_revision: 1,
            content: "【虚构解锁验收】凭据只用于本次测试。".into(),
            project_key: None,
            secret: Some(Secret {
                contract: SecretContract::Text {
                    version: 1,
                    kind: TextKind::Password,
                },
                value: SecretBytes(Zeroizing::new(b"fictional-only-secret".to_vec())),
            }),
        }],
    };
    let bytes = encode(&bundle);
    let response = grant_upload(&target, &target.cookie, "preview", &bytes, None).await;
    assert_eq!(response.status(), 200);
    let preview: Value = response.json().await.unwrap();
    let response = grant_upload(
        &target,
        &target.cookie,
        "confirm",
        &bytes,
        preview["preview_digest"].as_str(),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["imported"], 1);
    let locked = client
        .post(format!("{}/transfer/lock", target.url))
        .header("cookie", &target.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(locked.status(), 200);
    assert_eq!(locked.json::<Value>().await.unwrap()["unlocked"], false);
    assert_eq!(export().send().await.unwrap().status(), 403);
    assert_eq!(
        unlock_request(&target, &target.cookie, PASSWORD)
            .await
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{}/logout", target.url))
            .header("cookie", &target.cookie)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        unlocked_status(&target, &target.cookie).await["unlocked"],
        false,
        "退出必须在服务端撤销，重放旧 Cookie 也不恢复解锁"
    );
    assert_eq!(
        unlock_request(&target, &second, PASSWORD).await.status(),
        200
    );
    assert_eq!(
        unlocked_status(&target, &target.cookie).await["unlocked"],
        false
    );
    target
        .store
        .admin_set_password(
            target.space.as_uuid(),
            "fictional-updated-space-password".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{}/transfer/unlock", target.url))
            .header("cookie", &second)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL 库及本地 Qdrant"]
async fn paged_preview_covers_the_whole_package_and_confirmation_is_not_limited_to_the_visible_page()
 {
    let target = fixture("CHROMIE_TRANSFER_TARGET_DATABASE_URL", 61).await;
    let mut bundle = Bundle {
        format: "chromie-soul".into(),
        version: 1,
        package_id: Uuid::now_v7(),
        source_space: "fictional-source".into(),
        source_name: "虚构分页验收".into(),
        exported_at: "2026-11-05T00:00:00Z".into(),
        records: (0..101)
            .map(|i| Record {
                memory_id: Uuid::now_v7(),
                source_revision: 1,
                content: format!("虚构分页记忆 {i}"),
                project_key: None,
                secret: None,
            })
            .collect(),
    };
    let bytes = encode(&bundle);
    let first = preview(&target, &bytes).await;
    assert_eq!(first["new"], 101);
    assert_eq!(first["page_size"], 5);
    assert_eq!(first["items"].as_array().unwrap().len(), 5);
    let digest = first["preview_digest"].as_str().unwrap();
    let mut ids = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["memory_id"].clone())
        .collect::<Vec<_>>();
    for page in 1..=20 {
        let response = preview_page(&target, &bytes, &page.to_string(), Some(digest)).await;
        assert_eq!(response.status(), 200);
        let p: Value = response.json().await.unwrap();
        assert_eq!(
            p["preview_digest"], digest,
            "翻页摘要必须绑定同一份全包决定"
        );
        assert_eq!(p["page"], page);
        assert_eq!(p["has_more"], page < 20);
        ids.extend(
            p["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["memory_id"].clone()),
        );
    }
    assert_eq!(
        ids,
        bundle
            .records
            .iter()
            .map(|r| json!(r.memory_id))
            .collect::<Vec<_>>(),
        "第100条之后也必须可见，不能重复页或漏页"
    );
    assert_eq!(
        memories::Entity::find()
            .filter(memories::Column::SpaceId.eq(target.space.as_uuid().as_bytes().to_vec()))
            .count(&target.store.db)
            .await
            .unwrap(),
        0,
        "分页预览不能写入"
    );
    for page in ["21", "-1", "2000"] {
        assert_eq!(
            preview_page(&target, &bytes, page, Some(digest))
                .await
                .status(),
            400
        );
    }
    assert_eq!(preview_page(&target, &bytes, "1", None).await.status(), 400);
    let receipt = confirm(&target, &bytes, &first).await;
    assert_eq!(receipt["imported"], 101, "确认导入全包，不是可见的5条");
    assert_eq!(confirm(&target, &bytes, &first).await["imported"], 101);

    // 新包的分页尚在查看时，目标相关内容变更必须使翻页与最终确认同时失效。
    bundle.package_id = Uuid::now_v7();
    let next = encode(&bundle);
    let before = preview(&target, &next).await;
    assert_eq!(before["new"], 0);
    assert_eq!(before["skipped"], 101);
    correct(&target, bundle.records[0].memory_id, 1, "虚构分页后的更正").await;
    let changed = preview_page(&target, &next, "1", before["preview_digest"].as_str()).await;
    assert_eq!(changed.status(), 409);
    assert_eq!(
        changed.json::<Value>().await.unwrap()["code"],
        "transfer_preview_changed"
    );
    let changed = upload(
        &target,
        "confirm",
        &next,
        PASSWORD,
        PHRASE,
        before["preview_digest"].as_str(),
        true,
    )
    .await;
    assert_eq!(changed.status(), 409);
}
#[tokio::test]
#[ignore = "需要双隔离 MySQL 库及本地 Qdrant"]
async fn soul_transfer_cross_instance_auth_conflicts_replay_and_revision_guards() {
    let source = fixture("CHROMIE_TEST_DATABASE_URL", 51).await;
    let target = fixture("CHROMIE_TRANSFER_TARGET_DATABASE_URL", 52).await;
    let secret_value = b"00123-fictional-only";
    let text = SecretContract::Text {
        version: 1,
        kind: TextKind::Password,
    };
    let v = source
        .store
        .create_secret(
            source.space,
            SecretCreate {
                label: "虚构密码".into(),
                request_id: Uuid::now_v7(),
                project_key: None,
                contract: Some(text.clone()),
            },
            secret_value,
        )
        .await
        .unwrap();
    let secret_id = Uuid::parse_str(v["memory_id"].as_str().unwrap()).unwrap();
    let contract=SecretContract::parse(br#"{"format":"json","version":1,"fields":{"password":{"pointer":"/mysql/password","type":"string"}}}"#).unwrap();
    let json_bytes = br#"{"mysql":{"password":"fake-only","port":3306}}"#;
    let v = source
        .store
        .create_secret(
            source.space,
            SecretCreate {
                label: "虚构数据库".into(),
                request_id: Uuid::now_v7(),
                project_key: Some("fictional-project".into()),
                contract: Some(contract.clone()),
            },
            json_bytes,
        )
        .await
        .unwrap();
    let json_id = Uuid::parse_str(v["memory_id"].as_str().unwrap()).unwrap();
    let id = remember(
        &source,
        format!("凭据位于 secret://{}/{secret_id}", source.slug),
    )
    .await;
    correct(
        &source,
        id,
        1,
        &format!(
            "当前凭据 secret://{}/{secret_id}，不保存旧修订",
            source.slug
        ),
    )
    .await;
    let forgotten = remember(&source, "不应导出的虚构遗忘记忆".into()).await;
    forget(&source, forgotten, 1).await;
    let unrelated = remember(&target, "目标已有记忆，必须保留".into()).await;
    let bytes = export(&source).await;
    let bundle = decode(&bytes);
    assert_eq!(bundle.records.len(), 3);
    assert_eq!(
        bundle
            .records
            .iter()
            .find(|r| r.memory_id == id)
            .unwrap()
            .source_revision,
        2
    );
    assert!(!bytes.windows(secret_value.len()).any(|b| b == secret_value));
    assert_eq!(
        upload(&target, "preview", &bytes, PASSWORD, PHRASE, None, false)
            .await
            .status(),
        401
    );
    assert_eq!(
        upload(
            &target,
            "preview",
            &bytes,
            "wrong-password",
            PHRASE,
            None,
            true
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        upload(
            &target,
            "preview",
            &bytes,
            PASSWORD,
            "wrong-passphrase",
            None,
            true
        )
        .await
        .status(),
        400
    );
    let mut bad = bytes.clone();
    bad.pop();
    assert_eq!(
        upload(&target, "preview", &bad, PASSWORD, PHRASE, None, true)
            .await
            .status(),
        400
    );
    let (agent, token, _) = support::grant(&target.store, target.space, true).await;
    let agent_result = reqwest::Client::new()
        .post(format!("{}/transfer/export", target.url))
        .bearer_auth(token)
        .header("x-chromie-password", PASSWORD)
        .json(&json!({"space_password":PASSWORD,"passphrase":PHRASE}))
        .send()
        .await
        .unwrap();
    assert_eq!(agent_result.status(), 401);
    let p = preview(&target, &bytes).await;
    assert_eq!(p["new"], 3);
    assert!(!p.to_string().contains("00123"));
    assert!(!p.to_string().contains("fake-only"));
    assert!(
        memories::Entity::find_by_id((
            target.space.as_uuid().as_bytes().to_vec(),
            id.as_bytes().to_vec()
        ))
        .one(&target.store.db)
        .await
        .unwrap()
        .is_none(),
        "预览不写入"
    );
    let result = confirm(&target, &bytes, &p).await;
    assert_eq!(result["imported"], 3);
    let content = get(&target, id).await;
    assert_eq!(content["revision"], 1);
    assert_eq!(content["origin"]["imported_from"]["source_revision"], 2);
    assert!(
        content["content"]
            .as_str()
            .unwrap()
            .contains(&format!("secret://{}/{secret_id}", target.slug))
    );
    assert!(content["source_refs"].as_array().unwrap().is_empty());
    assert_eq!(
        target
            .store
            .read_secret(target.space, secret_id)
            .await
            .unwrap()
            .as_slice(),
        secret_value
    );
    assert_eq!(
        target
            .store
            .read_secret(target.space, json_id)
            .await
            .unwrap()
            .as_slice(),
        json_bytes
    );
    assert_eq!(
        get(&target, json_id).await["contract"],
        serde_json::to_value(contract).unwrap()
    );
    assert_eq!(
        source_events::Entity::find()
            .filter(source_events::Column::SpaceId.eq(target.space.as_uuid().as_bytes().to_vec()))
            .count(&target.store.db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        agent_connections::Entity::find()
            .filter(
                agent_connections::Column::SpaceId.eq(target.space.as_uuid().as_bytes().to_vec())
            )
            .count(&target.store.db)
            .await
            .unwrap(),
        1
    );
    let _ = agent;
    assert!(
        get(&target, unrelated).await["content"]
            .as_str()
            .unwrap()
            .contains("目标已有")
    );
    assert_eq!(
        memory_imports::Entity::find()
            .filter(memory_imports::Column::SpaceId.eq(target.space.as_uuid().as_bytes().to_vec()))
            .count(&target.store.db)
            .await
            .unwrap(),
        3
    );
    for id in [id, secret_id, json_id] {
        assert_eq!(
            outbox_tasks::Entity::find_by_id((
                target.space.as_uuid().as_bytes().to_vec(),
                id.as_bytes().to_vec()
            ))
            .one(&target.store.db)
            .await
            .unwrap()
            .unwrap()
            .target_revision,
            1
        );
    }
    let replay = confirm(&target, &bytes, &p).await;
    assert_eq!(replay["replayed"], true);
    let mut changed: Bundle =
        serde_json::from_slice(&serde_json::to_vec(&bundle).unwrap()).unwrap();
    changed.records[0].content.push_str("篡改");
    let changed_bytes = encode(&changed);
    assert_eq!(
        upload(
            &target,
            "preview",
            &changed_bytes,
            PASSWORD,
            PHRASE,
            None,
            true
        )
        .await
        .status(),
        409
    );
    // 新包中目标 Secret 已遗忘，其依赖记忆也必须冲突，不能指向另一份凭据。
    forget(&target, secret_id, 1).await;
    let replay = confirm(&target, &bytes, &p).await;
    assert_eq!(replay["replayed"], true);
    assert!(
        target
            .store
            .read_secret(target.space, secret_id)
            .await
            .is_err()
    );
    let mut next: Bundle = serde_json::from_slice(&serde_json::to_vec(&bundle).unwrap()).unwrap();
    next.package_id = Uuid::now_v7();
    let next_bytes = encode(&next);
    let conflicts = preview(&target, &next_bytes).await;
    assert_eq!(conflicts["conflicts"], 2);
    assert_eq!(conflicts["skipped"], 1);
    assert_eq!(conflicts["new"], 0);
    // 预览后更正目标相关记录，旧摘要不能确认；失败没有留下包回执。
    let mut race: Bundle = serde_json::from_slice(&serde_json::to_vec(&bundle).unwrap()).unwrap();
    race.package_id = Uuid::now_v7();
    race.records.retain(|r| r.memory_id == json_id);
    let race_bytes = encode(&race);
    let before = preview(&target, &race_bytes).await;
    correct(&target, json_id, 1, "目标更新后的公开说明").await;
    let refused = upload(
        &target,
        "confirm",
        &race_bytes,
        PASSWORD,
        PHRASE,
        before["preview_digest"].as_str(),
        true,
    )
    .await;
    assert_eq!(refused.status(), 409);
    assert!(
        transfer_receipts::Entity::find_by_id((
            target.space.as_uuid().as_bytes().to_vec(),
            race.package_id.as_bytes().to_vec()
        ))
        .one(&target.store.db)
        .await
        .unwrap()
        .is_none()
    );
    // 两个并发确认共享同一回执，只会写入一份。
    let mut parallel: Bundle =
        serde_json::from_slice(&serde_json::to_vec(&bundle).unwrap()).unwrap();
    parallel.package_id = Uuid::now_v7();
    parallel.records = vec![Record {
        memory_id: Uuid::now_v7(),
        source_revision: 3,
        content: "并发导入虚构记忆".into(),
        project_key: None,
        secret: None,
    }];
    let parallel_id = parallel.records[0].memory_id;
    let parallel_bytes = encode(&parallel);
    let pp = preview(&target, &parallel_bytes).await;
    let (a, b) = tokio::join!(
        confirm(&target, &parallel_bytes, &pp),
        confirm(&target, &parallel_bytes, &pp)
    );
    assert!(a["replayed"] == true || b["replayed"] == true);
    assert_eq!(
        memory_revisions::Entity::find()
            .filter(memory_revisions::Column::MemoryId.eq(parallel_id.as_bytes().to_vec()))
            .count(&target.store.db)
            .await
            .unwrap(),
        1
    );
    // 原始二进制 Secret 保留全部字节，来源缺失的引用不可新建。
    let mut custom = parallel;
    custom.package_id = Uuid::now_v7();
    custom.records = vec![
        Record {
            memory_id: Uuid::now_v7(),
            source_revision: 1,
            content: "二进制虚构载荷".into(),
            project_key: None,
            secret: Some(Secret {
                contract: SecretContract::Binary { version: 1 },
                value: SecretBytes(Zeroizing::new(vec![0, 255, 1, 0])),
            }),
        },
        Record {
            memory_id: Uuid::now_v7(),
            source_revision: 1,
            content: format!("secret://{}/{}", source.slug, Uuid::now_v7()),
            project_key: None,
            secret: None,
        },
    ];
    let bin_id = custom.records[0].memory_id;
    let b = encode(&custom);

    let p = preview(&target, &b).await;
    assert_eq!(p["conflicts"], 1);
    let r = confirm(&target, &b, &p).await;
    assert_eq!(r["imported"], 1);
    assert_eq!(
        target
            .store
            .read_secret(target.space, bin_id)
            .await
            .unwrap()
            .as_slice(),
        [0, 255, 1, 0]
    );
}
