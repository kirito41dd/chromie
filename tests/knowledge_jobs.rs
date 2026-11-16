mod support;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use chromie::{
    entity::{instance_model_settings, knowledge_jobs},
    knowledge::{MemoryType, Navigation},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    text_model::{JobKind, Queue, Save},
    types::{Correct, Event, Remember, Scope, SpaceId},
};
use migration::MigratorTrait;
use sea_orm::{ActiveModelTrait, ConnectOptions, Database, EntityTrait, IntoActiveModel, Set};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Default)]
struct Model {
    mode: AtomicU8,
    target: Mutex<Option<Uuid>>,
    entered: Notify,
    release: Notify,
    requests: Mutex<Vec<(String, String, Value)>>,
    budgets: Mutex<Vec<u64>>,
}
async fn complete(
    State(model): State<Arc<Model>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    model.requests.lock().unwrap().push((
        headers["user-agent"].to_str().unwrap().into(),
        headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .into(),
        input.clone(),
    ));
    model
        .budgets
        .lock()
        .unwrap()
        .push(body["max_tokens"].as_u64().unwrap());
    let mode = model.mode.load(Ordering::SeqCst);
    if input["kind"] != "probe" && mode == 1 {
        model.entered.notify_one();
        model.release.notified().await;
    }
    if input["kind"] != "probe" && mode == 2 {
        model.mode.store(0, Ordering::SeqCst);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"fictional rate limit"})),
        )
            .into_response();
    }
    let mut content = if input["kind"] == "probe" {
        json!({"notes":[],"conflicts":[]})
    } else if input["kind"] == "extract" {
        let event = input["conversation"]["new_event_ids"][0]
            .as_str()
            .and_then(|id| {
                input["events"]
                    .as_array()?
                    .iter()
                    .find(|e| e["event_id"] == id)
            })
            .unwrap_or(&input["events"][0]);
        let event = if mode == 11 || mode == 12 {
            input["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["role"] == "assistant")
                .unwrap()
        } else {
            event
        };
        let candidates = if mode == 5 || mode == 10 {
            vec![]
        } else {
            vec![
                json!({"content":"虚构用户偏好：每周四检查数据库。","category":"fact","search_query":"数据库 检查","source_refs":[event["event_id"]],"evidence":[{"event_id":event["event_id"],"quote":event["body"]}]}),
            ]
        };
        json!({"candidates":candidates,"conversation_title":if input["conversation"].is_null() || mode==10 {Value::Null}else{json!(if mode==6{"旅行计划"}else{"数据库检查"})}})
    } else if input["kind"] == "review" {
        let target = *model.target.lock().unwrap();
        json!({"decisions":input["candidates"].as_array().unwrap().iter().enumerate().map(|(index,_)| {
            if mode == 7 || mode == 11 || mode == 12 { let old=input["memories"].as_array().unwrap().iter().find(|m|m["id"].as_str().and_then(|s|Uuid::parse_str(s).ok())==target).unwrap(); json!({"candidate_index":index,"action":"propose_revision","reason":"用户明确改为周五","replacement":{"memory_id":old["id"],"expected_revision":old["revision"],"content":"虚构数据库检查：确认连接和复制状态；用户确认今后每周五检查。"},"evidence":if mode==11 || mode==12 { let user=input["events"].as_array().unwrap().iter().find(|e|e["role"]=="user").unwrap();json!([{"event_id":user["event_id"],"quote":if mode==12{json!("伪造的用户同意")}else{user["body"].clone()}}]) }else{json!([])}}) }
            else { json!({"candidate_index":index,"action":"create","reason":"虚构证据已核对","replacement":null}) }
        }).collect::<Vec<_>>()})
    } else if mode == 5 {
        json!({"notes":[],"conflicts":[]})
    } else {
        json!({"notes":[{"content":"数据库运维\n虚构主题入口，检查流程见关联记忆。","memory_type":"topic","source_refs":[],"links":[{"memory_id":input["memories"][0]["id"],"relation":"derived_from","revision":input["memories"][0]["revision"]}]}],"conflicts":[]})
    };
    if input["kind"] == "extract" && mode == 13 {
        let proof = content["candidates"][0]["evidence"][0].clone();
        content["candidates"][0]["evidence"] = json!(vec![proof; 5]);
    }
    if input["kind"] == "extract" && mode == 8 {
        content["candidates"][0]["evidence"][0]["quote"] = json!("不存在于来源的引文");
    }
    if input["kind"] == "extract" && mode == 9 {
        content["candidates"][0]["category"] = json!("preference");
    }
    if input["kind"] == "extract" && mode == 4 {
        content["candidates"][0]["category"] = json!("topic");
    }
    if input["kind"] == "organize" && mode == 3 {
        content["notes"].as_array_mut().unwrap().push(json!({"content":"伪造的范围外依据","memory_type":"topic","source_refs":[],"links":[{"memory_id":Uuid::now_v7(),"relation":"derived_from","revision":1}]}));
    }
    Json(json!({"choices":[{"finish_reason":"stop","message":{"content":content.to_string()}}],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}})).into_response()
}
struct Fixture {
    store: Store,
    space: SpaceId,
    model: Arc<Model>,
    base: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture() -> Fixture {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new("http://127.0.0.1:16334", None).unwrap(),
    ))
    .await;
    let existing = instance_model_settings::Entity::find_by_id(1u8)
        .one(&store.db)
        .await
        .unwrap()
        .and_then(|row| row.admin_space_id);
    let space = if let Some(id) = existing {
        store
            .resolve_space(
                &SpaceKey::parse(
                    &chromie::entity::spaces::Entity::find_by_id(id)
                        .one(&store.db)
                        .await
                        .unwrap()
                        .unwrap()
                        .slug,
                )
                .unwrap(),
            )
            .await
            .unwrap()
    } else {
        let key = SpaceKey::parse("llm-acceptance").unwrap();
        store
            .create_space(
                key.clone(),
                "虚构文本模型验收".into(),
                "fictional-llm-password".into(),
            )
            .await
            .unwrap();
        let space = store.resolve_space(&key).await.unwrap();
        store.initialize_model_admin(Some(space)).await.unwrap();
        space
    };
    let model = Arc::new(Model::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/v1/chat/completions", post(complete))
        .route("/v1/embeddings", post(vectors))
        .with_state(model.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Fixture {
        store,
        space,
        model,
        base,
        server,
    }
}
async fn save(f: &Fixture, model: &str) -> Value {
    let revision = f.store.text_model_settings(f.space).await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    f.store
        .save_text_model(
            f.space,
            Save {
                provider_id: None,
                max_output_tokens: None,
                expected_revision: revision,
                base_url: f.base.clone(),
                model: model.into(),
                api_key: Some("fictional-model-key".into()),
                clear_api_key: false,
                max_concurrency: 2,
            },
        )
        .await
        .unwrap()
}
async fn memory(f: &Fixture) -> Uuid {
    let value = f
        .store
        .remember(
            f.space,
            Remember {
                content: "虚构数据库检查：确认连接和复制状态。".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
                memory_type: MemoryType::Note,
                links: vec![],
            },
        )
        .await
        .unwrap();
    Uuid::parse_str(value["memory_id"].as_str().unwrap()).unwrap()
}
async fn queue(f: &Fixture, memory: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Organize,
                memory_ids: vec![memory],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    id
}
async fn job(f: &Fixture, id: Uuid) -> knowledge_jobs::Model {
    knowledge_jobs::Entity::find_by_id((
        f.space.as_uuid().as_bytes().to_vec(),
        id.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；模型仅为本机模拟服务"]
async fn model_settings_encrypt_keys_and_jobs_publish_cited_topics_and_source_linked_extraction() {
    let f = fixture().await;
    let config = save(&f, "fictional-text-model").await;
    assert_eq!(config["has_api_key"], true);
    assert!(!config.to_string().contains("fictional-model-key"));
    // 即使尚无 Secret，仅有模型凭据时也必须识别根私钥丢失/误换。
    use p256::pkcs8::{EncodePrivateKey, LineEnding};
    let pem = p256::SecretKey::from_slice(&[92; 32])
        .unwrap()
        .to_pkcs8_pem(LineEnding::LF)
        .unwrap();
    let wrong = chromie::server_key::ServerKey::from_pem(&pem).unwrap();
    assert!(
        Store::new(f.store.db.clone(), f.store.search.clone())
            .enable_secrets(chromie::secret_crypto::SecretCipher::from_key(&wrong).unwrap())
            .await
            .is_err(),
        "文本模型 Key 也必须阻止错根私钥启动"
    );
    let restarted = support::enable(Store::new(f.store.db.clone(), f.store.search.clone())).await;
    restarted
        .test_text_model(
            f.space,
            Save {
                provider_id: None,
                max_output_tokens: None,
                expected_revision: config["revision"].as_i64().unwrap(),
                base_url: f.base.clone(),
                model: "fictional-text-model".into(),
                api_key: None,
                clear_api_key: false,
                max_concurrency: 2,
            },
        )
        .await
        .unwrap();
    let note = memory(&f).await;
    let id = queue(&f, note).await;
    assert_eq!(f.store.sync_knowledge_due().await.unwrap(), 1);
    assert_eq!(job(&f, id).await.state, "done");
    let page = f
        .store
        .navigation(f.space, Navigation::default())
        .await
        .unwrap();
    assert!(
        page["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["title"] == "数据库运维")
    );
    let topic = page["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["title"] == "数据库运维")
        .unwrap();
    let current = f
        .store
        .get(
            f.space,
            chromie::types::GetMemory {
                memory_id: Some(Uuid::parse_str(topic["memory_id"].as_str().unwrap()).unwrap()),
                event_id: None,
                view: chromie::types::View::Current,
                cursor: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap();
    assert_eq!(current["origin"]["origin_kind"], "llm");
    assert_eq!(current["created_by"]["agent_name"], "后台整理");
    assert!(
        job(&f, id).await.payload.is_none(),
        "完成后不再保留重复输入快照"
    );
    let event = Uuid::now_v7();
    f.store
        .capture(
            f.space,
            Event {
                id: event,
                client_type: "fictional".into(),
                session_id: "fictional-session".into(),
                turn_id: None,
                role: "user".into(),
                body: "请记住我每周四检查数据库。".into(),
                project_key: None,
                occurred_at: "2026-11-11T10:00:00Z".into(),
            },
        )
        .await
        .unwrap();
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Extract,
                memory_ids: vec![note],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    assert_eq!(job(&f, id).await.state, "done");
    // 提炼即使返回合法枚举，也不能绕过领域约定创建主题。
    let wrong_type = Uuid::now_v7();
    f.model.mode.store(4, Ordering::SeqCst);
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: wrong_type,
                kind: JobKind::Extract,
                memory_ids: vec![note],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    let rejected = job(&f, wrong_type).await;
    assert_eq!(rejected.state, "failed");
    assert_eq!(
        rejected.result.as_ref().unwrap()["error"]["code"],
        "model_response"
    );
    assert!(
        rejected.result.as_ref().unwrap()["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("结构"))
    );
    let requests = f.model.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .all(|(ua, key, _)| ua.starts_with("Chromie/") && key == "Bearer fictional-model-key")
    );
    drop(requests);
    assert!(
        f.store.model_usage(f.space).await.unwrap()["all_time"]["tokens"]
            .as_u64()
            .unwrap()
            >= 54
    );
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；模型仅为本机模拟服务"]
async fn in_flight_outputs_cannot_publish_after_input_or_configuration_changes() {
    let f = fixture().await;
    save(&f, "fictional-stale-model").await;
    let note = memory(&f).await;
    for change_config in [false, true] {
        let id = queue(&f, note).await;
        f.model.mode.store(1, Ordering::SeqCst);
        let store = f.store.clone();
        let worker = tokio::spawn(async move { store.sync_knowledge_due().await.unwrap() });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            f.model.entered.notified(),
        )
        .await
        .unwrap();
        if change_config {
            save(&f, "fictional-changed-model").await;
        } else {
            f.store
                .correct(
                    f.space,
                    Correct {
                        memory_id: note,
                        expected_revision: 1,
                        request_id: Uuid::now_v7(),
                        reason: "虚构验证并发更正".into(),
                        content: Some("虚构检查流程已经更正".into()),
                        scope: None,
                        source_refs: None,
                        memory_type: None,
                        links: None,
                    },
                )
                .await
                .unwrap();
        }
        f.model.mode.store(0, Ordering::SeqCst);
        f.model.release.notify_one();
        worker.await.unwrap();
        assert_eq!(job(&f, id).await.state, "stale");
        assert!(job(&f, id).await.payload.is_none());
    }
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；模型仅为本机模拟服务"]
async fn rate_limits_retry_without_duplicate_publication_and_invalid_output_rolls_back_all_notes() {
    let f = fixture().await;
    save(&f, "fictional-retry-model").await;
    let note = memory(&f).await;
    let id = queue(&f, note).await;
    f.model.mode.store(2, Ordering::SeqCst);
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(job(&f, id).await.state, "retry");
    assert_eq!(
        job(&f, id).await.result.as_ref().unwrap()["error"]["code"],
        "model_rate_limited"
    );
    let mut retry = job(&f, id).await.into_active_model();
    retry.run_after = Set(chromie::types::now());
    retry.update(&f.store.db).await.unwrap();
    finish_stage(&f).await;
    assert_eq!(job(&f, id).await.state, "done");
    assert!(
        job(&f, id)
            .await
            .result
            .as_ref()
            .unwrap()
            .get("error")
            .is_none(),
        "重试成功必须清理旧错误"
    );
    let count = f
        .store
        .navigation(f.space, Navigation::default())
        .await
        .unwrap()["memories"]
        .as_array()
        .unwrap()
        .len();
    let invalid = queue(&f, note).await;
    f.model.mode.store(3, Ordering::SeqCst);
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(job(&f, invalid).await.state, "failed");
    assert_eq!(
        f.store
            .navigation(f.space, Navigation::default())
            .await
            .unwrap()["memories"]
            .as_array()
            .unwrap()
            .len(),
        count,
        "一项非法结果不能留下已经插入的部分主题"
    );
}

async fn vectors(
    State(model): State<Arc<Model>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    model.requests.lock().unwrap().push((
        headers["user-agent"].to_str().unwrap().into(),
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .into(),
        body.clone(),
    ));
    Json(
        json!({"data":body["input"].as_array().unwrap().iter().enumerate().map(|(i,_)|json!({"index":i,"embedding":[1.0,0.0,0.0]})).collect::<Vec<_>>(),"usage":{"total_tokens":3}}),
    )
}

#[tokio::test]
#[ignore = "需要隔离 MySQL 与 Qdrant"]
async fn provider_migration_and_shared_key_rotation_preserve_vector_identity() {
    use chromie::entity::{embedding_generations as generations, model_providers};
    let f = fixture().await;
    save(&f, "fictional-provider-text").await;
    let before = f.store.model_settings(f.space).await.unwrap();
    let embedding:chromie::embedding::Save=serde_json::from_value(json!({"expected_revision":before["revision"],"base_url":f.base,"model":"fictional-provider-vector","dimensions":3,"api_key":"fictional-model-key"})).unwrap();
    let before = f
        .store
        .save_model_settings(f.space, embedding)
        .await
        .unwrap();
    let generation = before["pending"]["generation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.store.migrate_model_providers().await.unwrap();
    f.store.migrate_model_providers().await.unwrap();
    let providers = f.store.model_providers(f.space).await.unwrap();
    let matching = providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["base_url"] == f.base)
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "同地址同Key的两种用途只回填一个Provider，历史其他服务商不受影响"
    );
    let p = matching[0];
    let id = Uuid::parse_str(p["provider_id"].as_str().unwrap()).unwrap();
    let migrated =
        generations::Entity::find_by_id(Uuid::parse_str(&generation).unwrap().as_bytes().to_vec())
            .one(&f.store.db)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(migrated.provider_id, Some(id.as_bytes().to_vec()));
    assert!(migrated.key_cipher.is_none());
    let text = chromie::entity::text_model_settings::Entity::find_by_id(1u8)
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(text.provider_id, migrated.provider_id);
    assert!(text.key_cipher.is_none());
    let connection_revision = migrated.connection_revision;
    f.store
        .save_provider(
            f.space,
            chromie::provider::Save {
                provider_id: Some(id),
                expected_revision: p["revision"].as_i64().unwrap(),
                name: "虚构共享网关".into(),
                base_url: f.base.clone(),
                api_key: Some("fictional-rotated-key".into()),
                clear_api_key: false,
            },
        )
        .await
        .unwrap();
    let changed = f.store.text_model_settings(f.space).await.unwrap();
    let input=serde_json::from_value(json!({"expected_revision":changed["revision"],"provider_id":id,"model":"fictional-provider-text","max_output_tokens":8192})).unwrap();
    f.store.test_text_model(f.space, input).await.unwrap();
    assert_eq!(
        *f.model.budgets.lock().unwrap().last().unwrap(),
        8192,
        "输出预算跟随文本模型用途发送"
    );
    let embedding = f.store.model_settings(f.space).await.unwrap();
    assert_eq!(embedding["pending"]["generation_id"], generation);
    let input=serde_json::from_value(json!({"expected_revision":embedding["revision"],"provider_id":id,"model":"fictional-provider-vector","dimensions":3})).unwrap();
    f.store.test_model_connection(f.space, input).await.unwrap();
    let calls = f.model.requests.lock().unwrap();
    assert!(
        calls
            .iter()
            .rev()
            .take(2)
            .all(|(_, auth, _)| auth == "Bearer fictional-rotated-key")
    );
    drop(calls);
    let row = generations::Entity::find_by_id(migrated.id)
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.fingerprint, migrated.fingerprint);
    assert!(row.connection_revision > connection_revision);
    let row = model_providers::Entity::find_by_id(id.as_bytes().to_vec())
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
    let rejected = f
        .store
        .save_provider(
            f.space,
            chromie::provider::Save {
                provider_id: Some(id),
                expected_revision: row.revision,
                name: "不能静默转移凭据".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: None,
                clear_api_key: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.code, "invalid_argument");
    let note = memory(&f).await;
    let task = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: task,
                kind: JobKind::Organize,
                memory_ids: vec![note],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    f.model.mode.store(1, Ordering::SeqCst);
    let worker = {
        let store = f.store.clone();
        tokio::spawn(async move { store.sync_knowledge_due().await.unwrap() })
    };
    f.model.entered.notified().await;
    f.store
        .save_provider(
            f.space,
            chromie::provider::Save {
                provider_id: Some(id),
                expected_revision: row.revision,
                name: "虚构共享网关".into(),
                base_url: f.base.clone(),
                api_key: Some("fictional-third-key".into()),
                clear_api_key: false,
            },
        )
        .await
        .unwrap();
    f.model.mode.store(0, Ordering::SeqCst);
    f.model.release.notify_one();
    worker.await.unwrap();
    assert_eq!(
        job(&f, task).await.state,
        "stale",
        "连接切换后旧模型响应不能发布"
    );
    let _work = f
        .store
        .create_space(
            SpaceKey::parse("provider-unprivileged").unwrap(),
            "虚构普通空间".into(),
            "fictional-password".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .model_providers(
                f.store
                    .resolve_space(&SpaceKey::parse("provider-unprivileged").unwrap())
                    .await
                    .unwrap()
            )
            .await
            .unwrap_err()
            .code,
        "system_admin_required"
    );
}

async fn organization_due(f: &Fixture) {
    use chromie::entity::organization_settings as s;
    let mut row = s::Entity::find_by_id(1u8)
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    row.next_run_at = Set(chromie::types::now() - time::Duration::seconds(1));
    row.update(&f.store.db).await.unwrap();
}
async fn organization_config(f: &Fixture, enabled: bool, limit: u32) {
    let revision = f.store.organization_settings(f.space).await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    f.store
        .save_organization(
            f.space,
            chromie::organization::Save {
                expected_revision: revision,
                enabled,
                extraction_enabled: false,
                batch_size: 3,
                interval_seconds: 10,
                daily_limit: limit,
            },
        )
        .await
        .unwrap();
}
#[tokio::test]
#[ignore = "需要隔离 MySQL 与 Qdrant"]
async fn automatic_organization_tracks_revisions_budget_context_pause_and_recovery() {
    use chromie::entity::{organization_items as items, organization_settings as settings};
    let f = fixture().await;
    save(&f, "fictional-automatic-text").await;
    let mut notes = vec![];
    for _ in 0..13 {
        notes.push(memory(&f).await);
    }
    f.model.mode.store(5, Ordering::SeqCst);
    organization_config(&f, true, 2).await;
    assert_eq!(f.store.schedule_organization().await.unwrap(), 3);
    assert_eq!(f.store.schedule_organization().await.unwrap(), 0);
    f.store.sync_knowledge_due().await.unwrap();
    let state = f.store.knowledge_jobs(f.space).await.unwrap();
    assert_eq!(
        state["organization"]["completed"], 3,
        "无产物也推进已处理修订"
    );
    organization_due(&f).await;
    // 新进程从持久化进度继续，不能把重启解释成重跑历史。
    let resumed = support::enable(Store::new(f.store.db.clone(), f.store.search.clone())).await;
    assert_eq!(resumed.schedule_organization().await.unwrap(), 3);
    resumed.sync_knowledge_due().await.unwrap();
    organization_due(&f).await;
    assert_eq!(resumed.schedule_organization().await.unwrap(), 0);
    assert_eq!(
        resumed.knowledge_jobs(f.space).await.unwrap()["organization"]["state"],
        "budget_limited"
    );
    let mut row = settings::Entity::find_by_id(1u8)
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    row.usage_date = Set(chromie::types::now().date() - time::Duration::days(1));
    row.update(&f.store.db).await.unwrap();
    assert_eq!(resumed.schedule_organization().await.unwrap(), 3);
    resumed.sync_knowledge_due().await.unwrap();
    organization_config(&f, true, 100).await;
    for _ in 0..8 {
        organization_due(&f).await;
        if f.store.schedule_organization().await.unwrap() == 0 {
            break;
        }
        f.store.sync_knowledge_due().await.unwrap();
    }
    let state = f.store.knowledge_jobs(f.space).await.unwrap();
    assert_eq!(state["organization"]["completed"], 13);
    assert_eq!(state["organization"]["pending"], 0);
    let used = f.store.organization_settings(f.space).await.unwrap()["attempts_today"]
        .as_u64()
        .unwrap() as u32;
    organization_config(&f, true, used).await;
    assert_eq!(
        f.store.knowledge_jobs(f.space).await.unwrap()["organization"]["state"],
        "idle",
        "已经追平时不能误报需等待明日继续"
    );
    organization_config(&f, true, 100).await;
    let source = notes[0];
    f.store
        .remember(
            f.space,
            Remember {
                request_id: Uuid::now_v7(),
                content: "虚构主题\n具体检查见依据。".into(),
                scope: Scope::Global,
                source_refs: vec![],
                memory_type: MemoryType::Topic,
                links: vec![chromie::knowledge::Link {
                    memory_id: source,
                    relation: chromie::knowledge::Relation::DerivedFrom,
                    revision: Some(1),
                }],
            },
        )
        .await
        .unwrap();
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: source,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构验收更正".into(),
                content: Some("虚构新检查方法".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
            },
        )
        .await
        .unwrap();
    organization_due(&f).await;
    assert_eq!(f.store.schedule_organization().await.unwrap(), 1);
    f.store.sync_knowledge_due().await.unwrap();
    let request = f.model.requests.lock().unwrap().last().unwrap().2.clone();
    assert!(
        request.get("automatic").is_none(),
        "后端任务恢复元数据不属于模型输入协议"
    );
    assert!(
        request["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_type"] == "topic"),
        "主题对照不占待处理普通记忆批次"
    );
    let progress = items::Entity::find_by_id((
        f.space.as_uuid().as_bytes().to_vec(),
        source.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(progress.revision, 2);
    assert_eq!(progress.state, "done");
    let failed = memory(&f).await;
    f.model.mode.store(3, Ordering::SeqCst);
    organization_due(&f).await;
    f.store.schedule_organization().await.unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    let progress = items::Entity::find_by_id((
        f.space.as_uuid().as_bytes().to_vec(),
        failed.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(progress.state, "failed");
    let next = memory(&f).await;
    f.model.mode.store(5, Ordering::SeqCst);
    organization_due(&f).await;
    assert_eq!(
        f.store.schedule_organization().await.unwrap(),
        1,
        "最终失败退避，后续记忆仍能继续"
    );
    organization_config(&f, false, 100).await;
    assert_eq!(
        f.store.sync_knowledge_due().await.unwrap(),
        0,
        "暂停撤销未开始的自动任务"
    );
    assert_eq!(
        f.store.knowledge_jobs(f.space).await.unwrap()["organization"]["state"],
        "paused"
    );
    organization_config(&f, true, 100).await;
    organization_due(&f).await;
    assert_eq!(f.store.schedule_organization().await.unwrap(), 1);
    f.store.sync_knowledge_due().await.unwrap();
    let progress = items::Entity::find_by_id((
        f.space.as_uuid().as_bytes().to_vec(),
        next.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(progress.state, "done");
}

#[tokio::test]
#[ignore = "需要隔离 MySQL 与 Qdrant"]
async fn automatic_pause_cancels_quota_waiter_before_http_send() {
    use sea_orm::{ColumnTrait, QueryFilter};
    let f = fixture().await;
    save(&f, "fictional-shared-quota").await;
    let current = f.store.text_model_settings(f.space).await.unwrap();
    f.store.save_text_model(f.space,serde_json::from_value(json!({"expected_revision":current["revision"],"base_url":f.base,"model":"fictional-shared-quota","max_concurrency":1})).unwrap()).await.unwrap();
    let note = memory(&f).await;
    organization_config(&f, true, 100).await;
    let manual = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: manual,
                kind: JobKind::Organize,
                memory_ids: vec![note],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    f.model.mode.store(1, Ordering::SeqCst);
    let first = {
        let s = f.store.clone();
        tokio::spawn(async move { s.sync_knowledge_due().await.unwrap() })
    };
    f.model.entered.notified().await;
    let sent = f.model.requests.lock().unwrap().len();
    organization_due(&f).await;
    assert!(f.store.schedule_organization().await.unwrap() > 0);
    let second = {
        let s = f.store.clone();
        tokio::spawn(async move { s.sync_knowledge_due().await.unwrap() })
    };
    let mut waiting = false;
    for _ in 0..100 {
        waiting = knowledge_jobs::Entity::find()
            .filter(knowledge_jobs::Column::Automatic.eq(1))
            .filter(knowledge_jobs::Column::State.eq("running"))
            .one(&f.store.db)
            .await
            .unwrap()
            .is_some();
        if waiting {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(waiting, "自动任务须已领取并进入共享额度等待");
    organization_config(&f, false, 100).await;
    f.model.mode.store(0, Ordering::SeqCst);
    f.model.release.notify_waiters();
    first.await.unwrap();
    second.await.unwrap();
    assert_eq!(
        f.model.requests.lock().unwrap().len(),
        sent,
        "暂停后释放额度，等待者也不能再发模型HTTP"
    );
    assert_eq!(
        job(&f, manual).await.state,
        "done",
        "暂停自动整理不撤销已经发送的手工任务"
    );
}

async fn extraction_config(f: &Fixture, enabled: bool, organize: bool, limit: u32) {
    let revision = f.store.organization_settings(f.space).await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    f.store
        .save_organization(
            f.space,
            chromie::organization::Save {
                expected_revision: revision,
                enabled: organize,
                extraction_enabled: enabled,
                batch_size: 3,
                interval_seconds: 10,
                daily_limit: limit,
            },
        )
        .await
        .unwrap();
}
async fn captured(
    f: &Fixture,
    session: &str,
    body: &str,
    occurred: time::PrimitiveDateTime,
) -> Uuid {
    let id = Uuid::now_v7();
    f.store
        .capture(
            f.space,
            Event {
                id,
                client_type: "codex".into(),
                session_id: session.into(),
                turn_id: None,
                role: "user".into(),
                body: body.into(),
                project_key: None,
                occurred_at: chromie::types::timestamp(occurred).unwrap(),
            },
        )
        .await
        .unwrap();
    let mut row = chromie::entity::source_events::Entity::find_by_id((
        f.space.as_uuid().as_bytes().to_vec(),
        id.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap()
    .into_active_model();
    // 仅测试让消息越过30秒聚合窗口，无需真实等待。
    row.received_at = Set(chromie::types::now() - time::Duration::seconds(40));
    row.update(&f.store.db).await.unwrap();
    id
}
async fn extraction_state(f: &Fixture, id: Uuid) -> Option<String> {
    chromie::entity::extraction_items::Entity::find_by_id((
        f.space.as_uuid().as_bytes().to_vec(),
        id.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .map(|r| r.state)
}
async fn conversation_info(f: &Fixture, session: &str) -> Value {
    f.store
        .conversation_messages(
            f.space,
            chromie::messages::MessagesQuery {
                agent_id: None,
                session_id: session.into(),
                page: 0,
                as_of: None,
            },
        )
        .await
        .unwrap()
        .conversation_info
        .unwrap()
}
#[tokio::test]
#[ignore = "需要隔离 MySQL；模型仅为本机模拟服务"]
async fn conversation_extraction_advances_per_event_preserves_late_messages_and_title_history() {
    let f = fixture().await;
    save(&f, "fictional-conversation-text").await;
    assert_eq!(
        f.store.organization_settings(f.space).await.unwrap()["extraction_enabled"],
        false
    );
    let session = format!("fictional-{}", Uuid::now_v7());
    let first = captured(&f, &session, "虚构检查约定", chromie::types::now()).await;
    assert_eq!(f.store.schedule_extraction().await.unwrap(), 0);
    extraction_config(&f, true, false, 100).await;
    assert_eq!(f.store.schedule_extraction().await.unwrap(), 1);
    assert_eq!(extraction_state(&f, first).await.as_deref(), Some("queued"));
    let late = captured(
        &f,
        &session,
        "离线补传的更早消息",
        chromie::types::now() - time::Duration::days(1),
    )
    .await;
    finish_stage(&f).await;
    assert_eq!(extraction_state(&f, first).await.as_deref(), Some("done"));
    assert!(
        extraction_state(&f, late).await.is_none(),
        "快照之后新到消息不能被提前消费"
    );
    let info = conversation_info(&f, &session).await;
    assert_eq!(info["title"], "数据库检查");
    assert_eq!(info["extraction"]["completed"], 1);
    organization_due(&f).await;
    // 零产物也必须完成进度，并可更新标题；旧来源不再次消费。
    f.model.mode.store(5, Ordering::SeqCst);
    assert_eq!(f.store.schedule_extraction().await.unwrap(), 1);
    finish_stage(&f).await;
    assert_eq!(extraction_state(&f, late).await.as_deref(), Some("done"));
    organization_due(&f).await;
    assert_eq!(f.store.schedule_extraction().await.unwrap(), 0);
    let next = captured(&f, &session, "主要事项改为旅行", chromie::types::now()).await;
    f.model.mode.store(6, Ordering::SeqCst);
    f.store.schedule_extraction().await.unwrap();
    finish_stage(&f).await;
    assert_eq!(extraction_state(&f, next).await.as_deref(), Some("done"));
    let info = conversation_info(&f, &session).await;
    assert_eq!(info["title"], "旅行计划");
    assert_eq!(info["history"].as_array().unwrap().len(), 2);
    // 已完成新主题后补传旧主题：仍消费旧消息，但不能把标题回拨。
    let historic = captured(
        &f,
        &session,
        "旧的数据库事项离线补传",
        chromie::types::now() - time::Duration::days(2),
    )
    .await;
    f.model.mode.store(5, Ordering::SeqCst);
    organization_due(&f).await;
    f.store.schedule_extraction().await.unwrap();
    finish_stage(&f).await;
    assert_eq!(
        extraction_state(&f, historic).await.as_deref(),
        Some("done")
    );
    assert_eq!(conversation_info(&f, &session).await["title"], "旅行计划");
    let key = chromie::messages::Conversation {
        agent_id: None,
        session_id: session.clone(),
    };
    let locked = f
        .store
        .set_conversation_title(
            f.space,
            key.clone(),
            info["revision"].as_i64().unwrap(),
            Some("我的长期对话".into()),
        )
        .await
        .unwrap();
    captured(&f, &session, "继续处理", chromie::types::now()).await;
    organization_due(&f).await;
    f.store.schedule_extraction().await.unwrap();
    finish_stage(&f).await;
    assert_eq!(
        conversation_info(&f, &session).await["title"],
        "我的长期对话"
    );
    f.store
        .set_conversation_title(f.space, key, locked["revision"].as_i64().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(conversation_info(&f, &session).await["locked"], false);
    // 同空间、同 session_id 的不同 Agent 必须是独立会话；调度也不能跨来源拼接。
    let (agent_a, _, _) = support::grant(&f.store, f.space, false).await;
    let (agent_b, _, _) = support::grant(&f.store, f.space, false).await;
    let shared = format!("same-session-{}", Uuid::now_v7());
    for agent in [agent_a, agent_b] {
        let event = captured(&f, &shared, "虚构不同设备的消息", chromie::types::now()).await;
        let mut row = chromie::entity::source_events::Entity::find_by_id((
            f.space.as_uuid().as_bytes().to_vec(),
            event.as_bytes().to_vec(),
        ))
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
        row.agent_id = Set(Some(agent.as_bytes().to_vec()));
        row.update(&f.store.db).await.unwrap();
    }
    extraction_config(&f, true, true, 10000).await;
    f.model.mode.store(5, Ordering::SeqCst);
    assert!(f.store.schedule_automatic().await.unwrap() > 0);
    finish_stage(&f).await;
    let kind = f.model.requests.lock().unwrap().last().unwrap().2["kind"].clone();
    assert_eq!(kind, "organize", "上批提炼后，应让普通记忆整理获得工作槽");
    organization_due(&f).await;
    assert_eq!(
        f.store.schedule_automatic().await.unwrap(),
        1,
        "同 session 的两个 Agent 不可拼成一批"
    );
    finish_stage(&f).await;
    let input = f.model.requests.lock().unwrap().last().unwrap().2.clone();
    assert_eq!(
        input["events"].as_array().unwrap().len(),
        1,
        "不得读取另一个 Agent 的消息作上下文"
    );
    let key = chromie::messages::Conversation {
        agent_id: Some(agent_a),
        session_id: shared.clone(),
    };
    let info = f
        .store
        .conversation_messages(
            f.space,
            chromie::messages::MessagesQuery {
                agent_id: Some(agent_a),
                session_id: shared.clone(),
                page: 0,
                as_of: None,
            },
        )
        .await
        .unwrap()
        .conversation_info
        .unwrap();
    f.store
        .set_conversation_title(
            f.space,
            key,
            info["revision"].as_i64().unwrap(),
            Some("仅设备A的标题".into()),
        )
        .await
        .unwrap();
    let other = f
        .store
        .conversation_messages(
            f.space,
            chromie::messages::MessagesQuery {
                agent_id: Some(agent_b),
                session_id: shared,
                page: 0,
                as_of: None,
            },
        )
        .await
        .unwrap()
        .conversation_info
        .unwrap();
    assert_ne!(other["title"], "仅设备A的标题");
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；模型仅为本机模拟服务"]
async fn conversation_extraction_pause_retry_budget_and_forgetting_block_publication() {
    use sea_orm::{ColumnTrait, QueryFilter};
    let f = fixture().await;
    save(&f, "fictional-extraction-recovery").await;
    let session = format!("recovery-{}", Uuid::now_v7());
    let first = captured(&f, &session, "虚构检查", chromie::types::now()).await;
    extraction_config(&f, true, false, 10000).await;
    f.store.schedule_extraction().await.unwrap();
    // 暂停清掉快照且释放事件，重新启用时不丢进度。
    extraction_config(&f, false, false, 10000).await;
    assert_eq!(
        extraction_state(&f, first).await.as_deref(),
        Some("pending")
    );
    assert_eq!(f.store.sync_knowledge_due().await.unwrap(), 0);
    extraction_config(&f, true, false, 10000).await;
    f.store.schedule_extraction().await.unwrap();
    f.model.mode.store(2, Ordering::SeqCst);
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(extraction_state(&f, first).await.as_deref(), Some("queued"));
    knowledge_jobs::Entity::update_many()
        .col_expr(
            knowledge_jobs::Column::RunAfter,
            sea_orm::sea_query::Expr::value(chromie::types::now() - time::Duration::seconds(1)),
        )
        .filter(knowledge_jobs::Column::State.eq("retry"))
        .exec(&f.store.db)
        .await
        .unwrap();
    finish_stage(&f).await;
    assert_eq!(extraction_state(&f, first).await.as_deref(), Some("done"));
    let second = captured(&f, &session, "另一次虚构检查", chromie::types::now()).await;
    organization_due(&f).await;
    f.store.schedule_extraction().await.unwrap();
    f.model.mode.store(1, Ordering::SeqCst);
    let store = f.store.clone();
    let running = tokio::spawn(async move { store.sync_knowledge_due().await.unwrap() });
    f.model.entered.notified().await;
    f.store
        .forget(
            f.space,
            chromie::types::Forget {
                request_id: Uuid::now_v7(),
                memory_id: None,
                event_id: Some(second),
                expected_revision: None,
            },
        )
        .await
        .unwrap();
    f.model.release.notify_one();
    running.await.unwrap();
    let info = conversation_info(&f, &session).await;
    assert!(info["title"].is_null());
    assert!(info["history"].as_array().unwrap().is_empty());
    assert_ne!(extraction_state(&f, second).await.as_deref(), Some("done"));
    let today = f.store.organization_settings(f.space).await.unwrap()["attempts_today"]
        .as_u64()
        .unwrap();
    captured(&f, &session, "预算限制后消息", chromie::types::now()).await;
    extraction_config(&f, true, true, today as u32).await;
    assert_eq!(
        f.store.schedule_automatic().await.unwrap(),
        0,
        "两类自动业务共用每日额度"
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；模型仅为本机模拟服务"]
async fn oversized_escaped_event_cools_down_without_blocking_other_work() {
    let f = fixture().await;
    save(&f, "fictional-poison-event").await;
    let poison = captured(&f, "oversized", &"\0".repeat(60_000), chromie::types::now()).await;
    memory(&f).await;
    extraction_config(&f, true, true, 100).await;
    f.model.mode.store(5, Ordering::SeqCst);
    // 合法60000字节在JSON中膨胀；登记失败后，同一轮可让普通整理继续。
    assert!(f.store.schedule_automatic().await.unwrap() > 0);
    assert_eq!(
        extraction_state(&f, poison).await.as_deref(),
        Some("failed")
    );
    f.store.sync_knowledge_due().await.unwrap();
    let normal = captured(
        &f,
        "another-session",
        "其他会话照常提炼",
        chromie::types::now(),
    )
    .await;
    organization_due(&f).await;
    assert_eq!(f.store.schedule_automatic().await.unwrap(), 1);
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(extraction_state(&f, normal).await.as_deref(), Some("done"));
    assert_eq!(
        extraction_state(&f, poison).await.as_deref(),
        Some("failed")
    );
}

async fn finish_stage(f: &Fixture) {
    f.store.sync_knowledge_due().await.unwrap();
    use sea_orm::{ColumnTrait, QueryFilter};
    let pending = knowledge_jobs::Entity::find()
        .filter(knowledge_jobs::Column::SpaceId.eq(f.space.as_uuid().as_bytes().to_vec()))
        .filter(knowledge_jobs::Column::State.eq("pending"))
        .all(&f.store.db)
        .await
        .unwrap();
    if pending
        .iter()
        .any(|j| j.payload.as_ref().is_some_and(|p| !p["review"].is_null()))
    {
        f.store.sync_knowledge_due().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；候选/核对仅使用本机模拟模型"]
async fn extraction_stages_resume_defer_invalid_evidence_and_apply_revision_once() {
    let f = fixture().await;
    save(&f, "fictional-two-stage").await;
    let target = memory(&f).await;
    // 超过旧“最近6条”窗口仍应按主题找回目标，不能靠近期列表碰巧命中。
    for i in 0..8 {
        f.store
            .remember(
                f.space,
                Remember {
                    request_id: Uuid::now_v7(),
                    content: format!("虚构旅行规划 {i}：海边散步"),
                    scope: Scope::Global,
                    source_refs: vec![],
                    memory_type: MemoryType::Note,
                    links: vec![],
                },
            )
            .await
            .unwrap();
    }
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(7, Ordering::SeqCst);
    let session = format!("two-stage-{}", Uuid::now_v7());
    let event = captured(
        &f,
        &session,
        "用户确认今后每周五检查数据库。",
        chromie::types::now(),
    )
    .await;
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Extract,
                memory_ids: vec![],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    let staged = job(&f, id).await;
    assert_eq!(staged.state, "pending");
    let payload = staged.payload.unwrap();
    assert_eq!(payload["review"]["candidates"].as_array().unwrap().len(), 1);
    assert!(
        payload["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == target.to_string()),
        "按候选主题找回对应旧记忆"
    );
    let resumed = support::enable(Store::new(f.store.db.clone(), f.store.search.clone())).await;
    resumed.sync_knowledge_due().await.unwrap();
    let summary = f.store.knowledge_jobs(f.space).await.unwrap();
    assert!(
        summary["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j["result"]["decisions"].is_null()),
        "轮询不返回完整候选正文"
    );
    let detail = f.store.knowledge_review(f.space, id).await.unwrap();
    assert_eq!(
        detail["result"]["decisions"][0]["action"],
        "propose_revision"
    );
    let receipt = f
        .store
        .apply_extraction_revision(
            f.space,
            chromie::extraction::Apply {
                job_id: id,
                candidate_index: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt["revision"], 2);
    let replay = f
        .store
        .apply_extraction_revision(
            f.space,
            chromie::extraction::Apply {
                job_id: id,
                candidate_index: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt, replay);
    assert_eq!(job(&f, id).await.attempts, 2);
    // 后台查找不能增加Agent/Web召回热度。
    use sea_orm::{ColumnTrait, QueryFilter};
    let usage = chromie::entity::recall_items::Entity::find()
        .filter(
            chromie::entity::recall_items::Column::SpaceId.eq(f
                .space
                .as_uuid()
                .as_bytes()
                .to_vec()),
        )
        .filter(chromie::entity::recall_items::Column::MemoryId.eq(target.as_bytes().to_vec()))
        .one(&f.store.db)
        .await
        .unwrap();
    assert!(usage.is_none());
    for mode in [8, 9, 13] {
        f.model.mode.store(mode, Ordering::SeqCst);
        let event = captured(
            &f,
            &session,
            "仅用于引文边界的虚构消息",
            chromie::types::now(),
        )
        .await;
        if mode == 9 {
            let mut row = chromie::entity::source_events::Entity::find_by_id((
                f.space.as_uuid().as_bytes().to_vec(),
                event.as_bytes().to_vec(),
            ))
            .one(&f.store.db)
            .await
            .unwrap()
            .unwrap()
            .into_active_model();
            row.role = Set("assistant".into());
            row.update(&f.store.db).await.unwrap();
        }
        let id = Uuid::now_v7();
        f.store
            .enqueue_knowledge(
                f.space,
                Queue {
                    request_id: id,
                    kind: JobKind::Extract,
                    memory_ids: vec![],
                    event_ids: vec![event],
                },
            )
            .await
            .unwrap();
        finish_stage(&f).await;
        let detail = f.store.knowledge_review(f.space, id).await.unwrap();
        assert_eq!(detail["result"]["decisions"][0]["action"], "defer");
        assert!(
            detail["result"]["memory_ids"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；核对阶段变更与确认边界"]
async fn extraction_review_rechecks_versions_and_forgetting_clears_published_proposals() {
    let f = fixture().await;
    save(&f, "fictional-review-boundaries").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(7, Ordering::SeqCst);
    let session = format!("review-boundary-{}", Uuid::now_v7());
    let event = captured(
        &f,
        &session,
        "用户确认今后每周五检查数据库。",
        chromie::types::now(),
    )
    .await;
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Extract,
                memory_ids: vec![],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    let calls = f.model.requests.lock().unwrap().len();
    f.store
        .correct(
            f.space,
            Correct {
                request_id: Uuid::now_v7(),
                memory_id: target,
                expected_revision: 1,
                content: Some("虚构数据库检查：已由其他Agent更新".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
                reason: "测试并发修订".into(),
            },
        )
        .await
        .unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(job(&f, id).await.state, "stale");
    assert_eq!(
        f.model.requests.lock().unwrap().len(),
        calls,
        "旧快照在模型调用前拒绝"
    );
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Extract,
                memory_ids: vec![],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    assert_eq!(
        f.store.knowledge_review(f.space, id).await.unwrap()["result"]["decisions"][0]["action"],
        "propose_revision"
    );
    f.store
        .correct(
            f.space,
            Correct {
                request_id: Uuid::now_v7(),
                memory_id: target,
                expected_revision: 2,
                content: Some("虚构数据库检查：另一个更新".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
                reason: "测试确认前版本变化".into(),
            },
        )
        .await
        .unwrap();
    let error = f
        .store
        .apply_extraction_revision(
            f.space,
            chromie::extraction::Apply {
                job_id: id,
                candidate_index: 0,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "revision_conflict");
    let otherkey = SpaceKey::parse("review-other-space").unwrap();
    f.store
        .create_space(
            otherkey.clone(),
            "虚构其他空间".into(),
            "fictional-other-space".into(),
        )
        .await
        .unwrap();
    let other = f.store.resolve_space(&otherkey).await.unwrap();
    assert!(f.store.knowledge_review(other, id).await.is_err());
    assert!(
        f.store
            .apply_extraction_revision(
                other,
                chromie::extraction::Apply {
                    job_id: id,
                    candidate_index: 0
                }
            )
            .await
            .is_err()
    );
    f.store
        .forget(
            f.space,
            chromie::types::Forget {
                request_id: Uuid::now_v7(),
                memory_id: None,
                event_id: Some(event),
                expected_revision: None,
            },
        )
        .await
        .unwrap();
    let cleared = f.store.knowledge_review(f.space, id).await.unwrap();
    assert!(cleared["result"]["decisions"].is_null());
    assert!(
        f.store
            .apply_extraction_revision(
                f.space,
                chromie::extraction::Apply {
                    job_id: id,
                    candidate_index: 0
                }
            )
            .await
            .is_err()
    );
    // 无明确事项的零候选仍消费，不因模型返回null标题而失败。
    f.model.mode.store(10, Ordering::SeqCst);
    let greet = captured(&f, &session, "你好", chromie::types::now()).await;
    extraction_config(&f, true, false, 1000).await;
    f.store.schedule_extraction().await.unwrap();
    finish_stage(&f).await;
    assert_eq!(extraction_state(&f, greet).await.as_deref(), Some("done"));
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；核对补充证据必须逐字属于固定输入"]
async fn review_can_add_missing_user_evidence_but_cannot_invent_it() {
    let f = fixture().await;
    save(&f, "fictional-proof-supplement").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    let user = captured(
        &f,
        "proof-supplement",
        "确认把数据库检查改为周五",
        chromie::types::now(),
    )
    .await;
    let assistant = captured(
        &f,
        "proof-supplement",
        "虚构数据库检查约定已修改",
        chromie::types::now(),
    )
    .await;
    let mut row = chromie::entity::source_events::Entity::find_by_id((
        f.space.bytes(),
        assistant.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap()
    .into_active_model();
    row.role = Set("assistant".into());
    row.update(&f.store.db).await.unwrap();
    for mode in [11, 12] {
        f.model.mode.store(mode, Ordering::SeqCst);
        let id = Uuid::now_v7();
        f.store
            .enqueue_knowledge(
                f.space,
                Queue {
                    request_id: id,
                    kind: JobKind::Extract,
                    memory_ids: vec![],
                    event_ids: vec![user, assistant],
                },
            )
            .await
            .unwrap();
        finish_stage(&f).await;
        let detail = f.store.knowledge_review(f.space, id).await.unwrap();
        let d = &detail["result"]["decisions"][0];
        assert_eq!(
            d["action"],
            if mode == 11 {
                "propose_revision"
            } else {
                "defer"
            }
        );
        if mode == 11 {
            assert!(d["source_refs"].as_array().unwrap().contains(&json!(user)));
            let receipt = f
                .store
                .apply_extraction_revision(
                    f.space,
                    chromie::extraction::Apply {
                        job_id: id,
                        candidate_index: 0,
                    },
                )
                .await
                .unwrap();
            assert_eq!(receipt["revision"], 2);
        }
    }
}
