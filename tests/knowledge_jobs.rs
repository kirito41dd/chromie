#[path = "knowledge_jobs/autonomous.rs"]
mod autonomous_tests;
#[path = "knowledge_jobs/extraction_batch.rs"]
mod extraction_batch_tests;
#[path = "knowledge_jobs/recovery.rs"]
mod recovery_tests;
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
    } else if input["kind"] == "maintain" && matches!(mode, 22 | 23) {
        let proof = &input["usage"][0];
        json!({"notes":[],"conflicts":[],"revisions":[{"memory_id":proof["memory_id"],"expected_revision":proof["revision"],"content":"虚构数据库检查：先确认版本和权限；新版检查端点为 /health。保留复制状态核对。","reason":"Agent 报告旧检查端点已迁移，建议确认新端点","feedback_refs":[if mode==23{json!(Uuid::now_v7())}else{proof["recall_id"].clone()}]}]})
    } else if input["kind"] == "review" {
        let target = *model.target.lock().unwrap();
        json!({"decisions":input["candidates"].as_array().unwrap().iter().enumerate().map(|(index,_)| {
            if matches!(mode, 7 | 11 | 12 | 14 | 15 | 21 | 28) { let old=input["memories"].as_array().unwrap().iter().find(|m|m["id"].as_str().and_then(|s|Uuid::parse_str(s).ok())==target).unwrap(); json!({"candidate_index":index,"action":"propose_revision","reason":"用户明确改为周五","replacement":{"memory_id":old["id"],"expected_revision":old["revision"],"content":"虚构数据库检查：确认连接和复制状态；用户确认今后每周五检查。"},"evidence":if mode==11 || mode==12 { let user=input["events"].as_array().unwrap().iter().find(|e|e["role"]=="user").unwrap();json!([{"event_id":user["event_id"],"quote":if mode==12{json!("伪造的用户同意")}else{user["body"].clone()}}]) }else{json!([])}}) }
            else { json!({"candidate_index":index,"action":"create","reason":"虚构证据已核对","replacement":null}) }
        }).collect::<Vec<_>>()})
    } else if mode == 5 {
        json!({"notes":[],"conflicts":[]})
    } else {
        json!({"notes":[{"content":"数据库运维\n虚构主题入口，检查流程见关联记忆。","memory_type":"topic","source_refs":[],"links":[{"memory_id":input["memories"][0]["id"],"relation":"derived_from","revision":input["memories"][0]["revision"]}]}],"conflicts":[]})
    };
    if input["kind"] == "extract" && mode == 21 {
        let second = content["candidates"][0].clone();
        content["candidates"].as_array_mut().unwrap().push(second);
    }
    if input["kind"] == "review" && matches!(mode, 14 | 15) {
        let event = &input["events"][0];
        content["decisions"][0]["explicit_change"] = json!({"event_id":event["event_id"],"quote":if mode == 15 {json!("不存在的更正原话")} else {event["body"].clone()}});
    }
    if input["kind"] == "extract" && matches!(mode, 16 | 17) {
        content["candidates"][0]["content"] = json!("安装工具：curl -fsSL .../install.sh | sh");
        content["candidates"][0]["category"] = json!("method");
    }
    if input["kind"] == "review" && mode == 17 {
        content["decisions"][0]["content"] =
            json!("虚构工具安装地址为 https://example.invalid/install.sh，下载后按来源说明运行。");
    }
    if input["kind"] == "organize" && matches!(mode, 18 | 26) {
        let topics = input["memories"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["memory_type"] == "topic")
            .collect::<Vec<_>>();
        content["notes"][0]["memory_id"] = topics[0]["id"].clone();
        content["notes"][0]["expected_revision"] = topics[0]["revision"].clone();
        content["notes"][0]["merge_from"] = json!(
            topics
                .iter()
                .skip(1)
                .map(|m| m["id"].clone())
                .collect::<Vec<_>>()
        );
        if mode == 26 {
            content["notes"][0]["links"][0]["revision"] = json!(1);
        }
    }
    if input["kind"] == "organize" && matches!(mode, 24 | 25) {
        let all = input["memories"].as_array().unwrap();
        let topic = all.iter().find(|m| m["memory_type"] == "topic").unwrap();
        let note = all.iter().find(|m| m["memory_type"] == "note").unwrap();
        let link =
            json!({"memory_id":note["id"],"relation":"derived_from","revision":note["revision"]});
        content = json!({"notes":[{"memory_id":topic["id"],"expected_revision":topic["revision"],"content":"虚构满容量主题\n保留全部原入口。","memory_type":"topic","links":if mode==25 {vec![link;35]}else{vec![link]}}],"conflicts":[]});
    }
    if input["kind"] != "probe" && (mode == 19 || mode == 29 && input["kind"] == "review") {
        return Json(json!({"choices":[{"finish_reason":"length","message":{"content":"不得保存的输出片段"}}]})).into_response();
    }
    if input["kind"] == "extract" && matches!(mode, 27 | 28) {
        content["candidates"][0]["category"] = json!("decision");
        if mode == 28 {
            content["candidates"][0]["source_refs"] = json!(
                input["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| e["event_id"].clone())
                    .collect::<Vec<_>>()
            );
        }
    }
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
    let usage = f.store.extraction_usage(f.space).await.unwrap();
    assert_eq!(
        usage["generated"], 1,
        "实际提炼新增普通记忆，之前整理出的主题不计入"
    );
    assert_eq!(usage["not_recalled"], 1);
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
    assert_eq!(job(&f, wrong_type).await.state, "retry");
    recovery_tests::make_job_due(&f, wrong_type).await;
    f.store.sync_knowledge_due().await.unwrap();
    let rejected = job(&f, wrong_type).await;
    assert_eq!(rejected.state, "failed");
    assert_eq!(
        rejected.result.as_ref().unwrap()["error"]["code"],
        "model_output_schema"
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
                maintenance_enabled: false,
                maintenance_interval_hours: 24,
                batch_size: 3,
                interval_seconds: 10,
                daily_limit: limit,
            },
        )
        .await
        .unwrap();
}
#[tokio::test]
#[ignore = "需要独立 MySQL 测试库；模型仅为本机模拟服务"]
async fn saving_text_model_preserves_explicit_automatic_processing_choices() {
    let f = fixture().await;
    let untouched = f.store.organization_settings(f.space).await.unwrap();
    save(&f, "fictional-independent-text").await;
    assert_eq!(
        f.store.organization_settings(f.space).await.unwrap(),
        untouched,
        "保存连接不代表授权处理历史记忆或对话"
    );
    let note = memory(&f).await;
    captured(
        &f,
        "fictional-settings-session",
        "虚构项目使用中文",
        chromie::types::now(),
    )
    .await;
    assert_eq!(f.store.schedule_automatic().await.unwrap(), 0);
    assert_eq!(
        f.model.requests.lock().unwrap().len(),
        1,
        "只有固定连接探针调用模型"
    );
    let manual = queue(&f, note).await;
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(
        job(&f, manual).await.state,
        "done",
        "自动任务关闭不影响手动使用模型"
    );
    f.store
        .save_organization(
            f.space,
            chromie::organization::Save {
                expected_revision: untouched["revision"].as_i64().unwrap(),
                enabled: false,
                extraction_enabled: true,
                maintenance_enabled: false,
                maintenance_interval_hours: 24,
                batch_size: 3,
                interval_seconds: 90,
                daily_limit: 123,
            },
        )
        .await
        .unwrap();
    let chosen = f.store.organization_settings(f.space).await.unwrap();
    let model = save(&f, "fictional-replacement-text").await;
    assert_eq!(
        f.store.organization_settings(f.space).await.unwrap(),
        chosen
    );
    f.store
        .disable_text_model(f.space, model["revision"].as_i64().unwrap())
        .await
        .unwrap();
    assert_eq!(
        f.store.organization_settings(f.space).await.unwrap(),
        chosen,
        "暂停模型保留任务选择及运行参数"
    );
    save(&f, "fictional-replacement-text").await;
    assert_eq!(
        f.store.organization_settings(f.space).await.unwrap(),
        chosen,
        "恢复模型不会重置任务选择"
    );
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
    assert_eq!(
        progress.state, "blocked",
        "单条结构错误停止原样重试，其他资料仍可继续"
    );
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
                maintenance_enabled: false,
                maintenance_interval_hours: 24,
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
    let receipt = detail["result"]["decisions"][0]["receipt"].clone();
    assert_eq!(receipt["revision"], 2);
    assert_eq!(detail["result"]["decisions"][0]["resolution"], "automatic");
    assert_eq!(
        resumed.sync_knowledge_due().await.unwrap(),
        0,
        "完成任务不重复执行"
    );
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
    let detail = f.store.knowledge_review(f.space, id).await.unwrap();
    assert_eq!(detail["result"]["decisions"][0]["action"], "defer");
    assert_eq!(detail["result"]["pending_revisions"], 0);
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
            assert_eq!(d["receipt"]["revision"], 2);
        }
    }
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；自主修订可幂等，旧消息不能覆盖更新的人工决定"]
async fn autonomous_revision_is_idempotent_and_respects_newer_manual_decision() {
    let f = fixture().await;
    save(&f, "fictional-autonomous-replay").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    let event = captured(
        &f,
        "autonomous-replay",
        "用户确认今后每周五检查数据库。",
        chromie::types::now(),
    )
    .await;
    f.model.mode.store(7, Ordering::SeqCst);
    let mut ids = vec![];
    for _ in 0..2 {
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
        ids.push(id);
    }
    let first = f.store.knowledge_review(f.space, ids[0]).await.unwrap();
    assert_eq!(first["result"]["decisions"][0]["resolution"], "automatic");
    assert_eq!(first["result"]["decisions"][0]["receipt"]["revision"], 2);
    let repeated = f.store.knowledge_review(f.space, ids[1]).await.unwrap();
    assert_eq!(repeated["result"]["decisions"][0]["action"], "skip");
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: target,
                expected_revision: 2,
                request_id: Uuid::now_v7(),
                reason: "新的人工决定".into(),
                content: Some("虚构数据库检查：人工确认改为周一。".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
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
                memory_ids: vec![],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    let old = f.store.knowledge_review(f.space, id).await.unwrap();
    assert_eq!(old["result"]["decisions"][0]["action"], "defer");
    assert_eq!(old["result"]["pending_revisions"], 0);
    let head = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":target})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head["revision"], 3);
    assert!(head["content"].as_str().unwrap().contains("周一"));
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；同目标的多份模型修订不能互相覆盖或生成审批"]
async fn duplicate_target_drafts_are_deferred_without_creating_manual_work() {
    let f = fixture().await;
    save(&f, "fictional-duplicate-targets").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(21, Ordering::SeqCst);
    let event = captured(
        &f,
        "duplicate-targets",
        "用户确认每周五检查数据库。",
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
    finish_stage(&f).await;
    let result = f.store.knowledge_review(f.space, id).await.unwrap();
    assert_eq!(result["result"]["pending_revisions"], 0);
    assert!(
        result["result"]["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["action"] == "defer")
    );
    let head = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":target})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head["revision"], 1);
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；残缺方法与伪造更正不发布，真实来源可补齐"]
async fn incomplete_methods_and_invalid_explicit_proofs_are_not_published() {
    let f = fixture().await;
    save(&f, "fictional-standalone-maintenance").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    let event = captured(
        &f,
        "standalone-maintenance",
        "数据库检查明确改为周五；虚构工具安装地址 https://example.invalid/install.sh",
        chromie::types::now(),
    )
    .await;
    for mode in [15, 16, 17] {
        f.model.mode.store(mode, Ordering::SeqCst);
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
        let result = f.store.knowledge_review(f.space, id).await.unwrap();
        assert_eq!(
            result["result"]["decisions"][0]["action"],
            if mode == 17 { "create" } else { "defer" }
        );
    }
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；主题合并保留全部分支和旧链接，不删除普通记忆"]
async fn topic_merge_keeps_branches_history_and_redirects() {
    let f = fixture().await;
    save(&f, "fictional-topic-maintenance").await;
    let notes = [memory(&f).await, memory(&f).await];
    let mut topics = vec![];
    for (i, note) in notes.iter().enumerate() {
        let receipt = f
            .store
            .remember(
                f.space,
                Remember {
                    request_id: Uuid::now_v7(),
                    content: format!("虚构数据库主题{i}\n数据库检查入口。"),
                    scope: Scope::Global,
                    source_refs: vec![],
                    memory_type: MemoryType::Topic,
                    links: vec![chromie::knowledge::Link {
                        memory_id: *note,
                        relation: chromie::knowledge::Relation::DerivedFrom,
                        revision: Some(1),
                    }],
                },
            )
            .await
            .unwrap();
        topics.push(Uuid::parse_str(receipt["memory_id"].as_str().unwrap()).unwrap());
    }
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: notes[1],
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构较晚分支更新".into(),
                content: Some("虚构数据库检查：增加容量核对".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
            },
        )
        .await
        .unwrap();
    f.model.mode.store(18, Ordering::SeqCst);
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Organize,
                memory_ids: vec![notes[0], topics[0], topics[1]],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    assert_eq!(job(&f, id).await.state, "done");
    let alias = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":topics[1],"max_chars":8000})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alias["memory_type"], "topic_alias");
    assert_eq!(alias["redirect_to"], topics[0].to_string());
    let keeper = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":topics[0],"max_chars":8000})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        keeper["knowledge"]["needs_review"], true,
        "保留未读分支的旧依据须提示核对，不能宣称已读新版"
    );
    let outgoing = f
        .store
        .memory_neighbors(
            f.space,
            chromie::knowledge::Neighbors {
                memory_id: topics[0],
                incoming: false,
                cursor: None,
                project_key: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap();
    assert_eq!(outgoing["memories"].as_array().unwrap().len(), 2);
    let navigation = f
        .store
        .navigation(f.space, Navigation::default())
        .await
        .unwrap();
    assert!(
        !navigation["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_id"] == topics[1].to_string())
    );
    for note in notes {
        assert!(
            f.store
                .get(
                    f.space,
                    serde_json::from_value(json!({"memory_id":note,"max_chars":8000})).unwrap()
                )
                .await
                .unwrap()["content"]
                .is_string()
        );
    }
    let history = f
        .store
        .get(
            f.space,
            serde_json::from_value(
                json!({"memory_id":topics[1],"view":"history","max_chars":8000}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(history["history"].as_array().unwrap().len(), 2);
    let change = f
        .store
        .correct(
            f.space,
            Correct {
                memory_id: topics[0],
                expected_revision: 2,
                request_id: Uuid::now_v7(),
                reason: "虚构错误变型".into(),
                content: None,
                scope: None,
                source_refs: None,
                memory_type: Some(MemoryType::Note),
                links: None,
            },
        )
        .await;
    assert!(
        change.is_err(),
        "旧入口仍在使用时不能把有效主题改成普通记忆"
    );
    f.store
        .forget(
            f.space,
            chromie::types::Forget {
                memory_id: Some(topics[0]),
                expected_revision: Some(2),
                event_id: None,
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    let hidden = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":topics[1],"max_chars":8000})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        hidden["hidden"], true,
        "遗忘有效主题后不能留下可迁移的悬空别名"
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；满容量主题不能丢掉未读分支"]
async fn topic_capacity_is_checked_after_preserving_unread_links_and_exact_duplicates_are_folded() {
    use sea_orm::{ColumnTrait, PaginatorTrait, QueryFilter};
    let f = fixture().await;
    save(&f, "fictional-topic-capacity").await;
    let mut notes = Vec::new();
    for _ in 0..33 {
        notes.push(memory(&f).await);
    }
    let receipt = f
        .store
        .remember(
            f.space,
            Remember {
                request_id: Uuid::now_v7(),
                content: "虚构满容量主题\n已有32条有效入口。".into(),
                scope: Scope::Global,
                source_refs: vec![],
                memory_type: MemoryType::Topic,
                links: notes
                    .iter()
                    .take(32)
                    .map(|id| chromie::knowledge::Link {
                        memory_id: *id,
                        relation: chromie::knowledge::Relation::DerivedFrom,
                        revision: Some(1),
                    })
                    .collect(),
            },
        )
        .await
        .unwrap();
    let topic = Uuid::parse_str(receipt["memory_id"].as_str().unwrap()).unwrap();
    // 模型只读了新分支，补回32条旧入口后必须原子拒绝，而非截掉旧引用。
    f.model.mode.store(24, Ordering::SeqCst);
    let failed = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: failed,
                kind: JobKind::Organize,
                memory_ids: vec![topic, notes[32]],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    let row = job(&f, failed).await;
    assert_eq!(row.state, "failed");
    assert_eq!(row.last_error.as_deref(), Some("model_evidence_invalid"));
    assert_eq!(
        f.store
            .get(
                f.space,
                serde_json::from_value(json!({"memory_id":topic,"max_chars":8000})).unwrap()
            )
            .await
            .unwrap()["revision"],
        1
    );
    let request = f
        .model
        .requests
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r.2["kind"] == "organize")
        .unwrap()
        .2
        .clone();
    let sent = request["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == topic.to_string())
        .unwrap();
    assert_eq!(
        sent["link_capacity"],
        json!({"used":32,"remaining":0,"limit":32})
    );
    // 同一条已读依据被模型重复35次时只占一个位置；其余31条未读依据仍保留。
    f.model.mode.store(25, Ordering::SeqCst);
    let duplicate = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: duplicate,
                kind: JobKind::Organize,
                memory_ids: vec![topic, notes[0]],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    assert_eq!(job(&f, duplicate).await.state, "done");
    let current = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":topic,"max_chars":8000})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(current["revision"], 2);
    assert_eq!(
        chromie::entity::memory_links::Entity::find()
            .filter(chromie::entity::memory_links::Column::SpaceId.eq(f.space.bytes()))
            .filter(chromie::entity::memory_links::Column::MemoryId.eq(topic.as_bytes().to_vec()))
            .filter(chromie::entity::memory_links::Column::Revision.eq(2))
            .count(&f.store.db)
            .await
            .unwrap(),
        32
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；旧关系并集不能洗掉模型的错误依据"]
async fn topic_merge_validates_original_model_revision_before_preserving_trusted_links() {
    let f = fixture().await;
    save(&f, "fictional-topic-evidence").await;
    let note = memory(&f).await;
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: note,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构检查项更新".into(),
                content: Some("虚构数据库检查：已更新为第二版。".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
            },
        )
        .await
        .unwrap();
    let mut topics = Vec::new();
    for i in 0..2 {
        let receipt = f
            .store
            .remember(
                f.space,
                Remember {
                    request_id: Uuid::now_v7(),
                    content: format!("虚构同义检查主题{i}\n第二版检查入口。"),
                    scope: Scope::Global,
                    source_refs: vec![],
                    memory_type: MemoryType::Topic,
                    links: vec![chromie::knowledge::Link {
                        memory_id: note,
                        relation: chromie::knowledge::Relation::DerivedFrom,
                        revision: Some(2),
                    }],
                },
            )
            .await
            .unwrap();
        topics.push(Uuid::parse_str(receipt["memory_id"].as_str().unwrap()).unwrap());
    }
    // 两份主题均有合法A@2；模型错误提供A@1，不能在并集时被max替换而通过。
    f.model.mode.store(26, Ordering::SeqCst);
    let invalid = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: invalid,
                kind: JobKind::Organize,
                memory_ids: vec![note, topics[0], topics[1]],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    assert_eq!(
        job(&f, invalid).await.last_error.as_deref(),
        Some("model_evidence_invalid")
    );
    for id in &topics {
        let view = f
            .store
            .get(
                f.space,
                serde_json::from_value(json!({"memory_id":id,"max_chars":8000})).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(view["revision"], 1);
        assert_eq!(view["memory_type"], "topic");
    }
    // 同样的并集使用实际已读A@2仍应正常合并。
    f.model.mode.store(18, Ordering::SeqCst);
    let valid = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: valid,
                kind: JobKind::Organize,
                memory_ids: vec![note, topics[0], topics[1]],
                event_ids: vec![],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    assert_eq!(job(&f, valid).await.state, "done");
    let alias = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":topics[1],"max_chars":8000})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alias["memory_type"], "topic_alias");
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；错误诊断不保留响应原文，结构故障不盲目重试"]
async fn truncated_output_has_safe_stage_diagnostics() {
    let f = fixture().await;
    save(&f, "fictional-diagnostic-maintenance").await;
    let note = memory(&f).await;
    f.model.mode.store(19, Ordering::SeqCst);
    let id = queue(&f, note).await;
    finish_stage(&f).await;
    assert_eq!(job(&f, id).await.state, "retry");
    recovery_tests::make_job_due(&f, id).await;
    f.store.sync_knowledge_due().await.unwrap();
    let result = job(&f, id).await;
    assert_eq!(result.last_error.as_deref(), Some("model_output_truncated"));
    let diagnostic = &result.result.as_ref().unwrap()["diagnostic"];
    assert_eq!(diagnostic["phase"], "organize");
    assert_eq!(diagnostic["automatic_retry"], false);
    assert!(diagnostic["snapshot_bytes"].as_u64().unwrap() > 0);
    assert!(
        !result
            .result
            .unwrap()
            .to_string()
            .contains("不得保存的输出片段")
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；迟到旧更正只能待确认，读取须有明确提示"]
async fn late_explicit_change_does_not_overwrite_newer_memory() {
    let f = fixture().await;
    save(&f, "fictional-late-correction").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    let event = captured(
        &f,
        "late-explicit",
        "用户明确改为周五检查数据库",
        chromie::types::now() - time::Duration::hours(1),
    )
    .await;
    f.model.mode.store(14, Ordering::SeqCst);
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
    let result = f.store.knowledge_review(f.space, id).await.unwrap();
    assert_eq!(result["result"]["pending_revisions"], 0);
    let read = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":target,"max_chars":8000})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read["revision"], 1);
    assert_eq!(read["pending_revisions"], 0);
    assert_eq!(result["result"]["decisions"][0]["action"], "defer");
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；结构失败逐步缩批，单条仍失败后不再重复调用"]
async fn structural_failure_reduces_batches_and_stops_at_single_item() {
    use chromie::entity::organization_items as items;
    use sea_orm::{ColumnTrait, QueryFilter, sea_query::Expr};
    let f = fixture().await;
    save(&f, "fictional-adaptive-batch").await;
    for _ in 0..4 {
        memory(&f).await;
    }
    organization_config(&f, true, 100).await;
    let config_revision = f.store.organization_settings(f.space).await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    f.store
        .save_organization(
            f.space,
            chromie::organization::Save {
                expected_revision: config_revision,
                enabled: true,
                extraction_enabled: false,
                maintenance_enabled: false,
                maintenance_interval_hours: 24,
                batch_size: 4,
                interval_seconds: 10,
                daily_limit: 100,
            },
        )
        .await
        .unwrap();
    f.model.mode.store(19, Ordering::SeqCst);
    let mut sizes = vec![];
    for _ in 0..10 {
        organization_due(&f).await;
        items::Entity::update_many()
            .col_expr(
                items::Column::RunAfter,
                Expr::value(chromie::types::now() - time::Duration::seconds(1)),
            )
            .filter(items::Column::SpaceId.eq(f.space.bytes()))
            .exec(&f.store.db)
            .await
            .unwrap();
        if f.store.schedule_organization().await.unwrap() == 0 {
            break;
        }
        f.store.sync_knowledge_due().await.unwrap();
        // 每个固定快照先有限重试，耗尽后才缩小下一批；不把一次截断当作必然失败。
        let retry = knowledge_jobs::Entity::find()
            .filter(knowledge_jobs::Column::SpaceId.eq(f.space.bytes()))
            .filter(knowledge_jobs::Column::State.eq("retry"))
            .all(&f.store.db)
            .await
            .unwrap();
        for row in retry {
            recovery_tests::make_job_due(&f, Uuid::from_slice(&row.id).unwrap()).await;
            f.store.sync_knowledge_due().await.unwrap();
        }
        sizes.push(
            f.model.requests.lock().unwrap().last().unwrap().2["memories"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["memory_type"] == "note")
                .count(),
        );
    }
    assert_eq!(sizes.first(), Some(&4));
    assert!(sizes.contains(&2) && sizes.contains(&1));
    let progress = items::Entity::find()
        .filter(items::Column::SpaceId.eq(f.space.bytes()))
        .all(&f.store.db)
        .await
        .unwrap();
    assert_eq!(progress.len(), 4);
    assert!(progress.iter().all(|p| p.state == "blocked"));
    organization_due(&f).await;
    assert_eq!(f.store.schedule_organization().await.unwrap(), 0);
    f.model.mode.store(0, Ordering::SeqCst);
    save(&f, "fictional-fixed-output-budget").await;
    organization_due(&f).await;
    assert!(
        f.store.schedule_organization().await.unwrap() > 0,
        "修正模型配置后阻断项应重新获得处理机会"
    );
}

async fn maintain_config(f: &Fixture, enabled: bool, limit: u32) {
    let revision = f.store.organization_settings(f.space).await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    f.store
        .save_organization(
            f.space,
            chromie::organization::Save {
                expected_revision: revision,
                enabled: false,
                extraction_enabled: false,
                maintenance_enabled: enabled,
                maintenance_interval_hours: 24,
                batch_size: 10,
                interval_seconds: 10,
                daily_limit: limit,
            },
        )
        .await
        .unwrap();
}
async fn maintain_due(f: &Fixture) {
    let mut setting = chromie::entity::organization_settings::Entity::find_by_id(1u8)
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    setting.next_run_at = Set(chromie::types::now() - time::Duration::seconds(1));
    setting.update(&f.store.db).await.unwrap();
}
async fn latest_maintain(f: &Fixture) -> knowledge_jobs::Model {
    use sea_orm::{ColumnTrait, QueryFilter, QueryOrder};
    knowledge_jobs::Entity::find()
        .filter(knowledge_jobs::Column::SpaceId.eq(f.space.bytes()))
        .filter(knowledge_jobs::Column::Kind.eq("maintain"))
        .order_by_desc(knowledge_jobs::Column::CreatedAt)
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
}
async fn report_problem(f: &Fixture, id: Uuid) -> Uuid {
    let (agent, _, _) = support::grant(&f.store, f.space, false).await;
    let recall = Uuid::now_v7();
    let now = chromie::types::now();
    chromie::entity::recall_runs::ActiveModel {
        space_id: Set(f.space.bytes()),
        id: Set(recall.as_bytes().to_vec()),
        agent_id: Set(Some(agent.as_bytes().to_vec())),
        channel: Set("agent".into()),
        created_at: Set(now),
    }
    .insert(&f.store.db)
    .await
    .unwrap();
    let head =
        chromie::entity::memories::Entity::find_by_id((f.space.bytes(), id.as_bytes().to_vec()))
            .one(&f.store.db)
            .await
            .unwrap()
            .unwrap();
    chromie::entity::recall_items::ActiveModel {
        negative_reason: Set(None),
        space_id: Set(f.space.bytes()),
        recall_id: Set(recall.as_bytes().to_vec()),
        memory_id: Set(id.as_bytes().to_vec()),
        revision: Set(head.current_revision),
        useful: Set(None),
        evaluated_at: Set(None),
        use_outcome: Set(Some("content_outdated".into())),
        use_reason: Set(Some(
            "虚构检查端点由 /old-health 迁移为 /health；需核对版本".into(),
        )),
        used_at: Set(Some(now)),
    }
    .insert(&f.store.db)
    .await
    .unwrap();
    if let Some(row) = chromie::entity::maintenance_items::Entity::find_by_id((
        f.space.bytes(),
        id.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    {
        let queued = row.state == "queued";
        let mut row = row.into_active_model();
        row.requested_at = Set(now);
        if !queued {
            row.state = Set("pending".into())
        };
        row.update(&f.store.db).await.unwrap();
    }
    recall
}
#[tokio::test]
#[ignore = "需要隔离 MySQL；独立周期维护、共享预算和在途反馈"]
async fn independent_maintenance_cycles_without_new_memories_and_preserves_inflight_feedback() {
    let f = fixture().await;
    save(&f, "fictional-independent-maintenance").await;
    let note = memory(&f).await;
    assert_eq!(
        f.store.organization_settings(f.space).await.unwrap()["maintenance_enabled"],
        false
    );
    maintain_config(&f, true, 100).await;
    assert!(f.store.schedule_maintenance().await.unwrap() > 0);
    let first = latest_maintain(&f).await;
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(
        job(&f, Uuid::from_slice(&first.id).unwrap()).await.state,
        "done"
    );
    maintain_due(&f).await;
    assert_eq!(
        f.store.schedule_maintenance().await.unwrap(),
        1,
        "首轮新建主题也须覆盖一次"
    );
    f.model.mode.store(5, Ordering::SeqCst);
    f.store.sync_knowledge_due().await.unwrap();
    maintain_due(&f).await;
    assert_eq!(
        f.store.schedule_maintenance().await.unwrap(),
        0,
        "无新输入的周期内不反复检查"
    );
    // 不增加记忆，使检查覆盖时间过期；独立维护仍会再次运行。
    use sea_orm::{ColumnTrait, QueryFilter};
    chromie::entity::maintenance_items::Entity::update_many()
        .col_expr(
            chromie::entity::maintenance_items::Column::CompletedAt,
            sea_orm::sea_query::Expr::value(chromie::types::now() - time::Duration::hours(25)),
        )
        .filter(chromie::entity::maintenance_items::Column::SpaceId.eq(f.space.bytes()))
        .exec(&f.store.db)
        .await
        .unwrap();
    maintain_due(&f).await;
    assert!(f.store.schedule_maintenance().await.unwrap() > 0);
    f.model.mode.store(1, Ordering::SeqCst);
    let work = {
        let store = f.store.clone();
        tokio::spawn(async move { store.sync_knowledge_due().await.unwrap() })
    };
    f.model.entered.notified().await;
    report_problem(&f, note).await;
    f.model.mode.store(5, Ordering::SeqCst);
    f.model.release.notify_one();
    work.await.unwrap();
    let progress = chromie::entity::maintenance_items::Entity::find_by_id((
        f.space.bytes(),
        note.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(progress.state, "pending", "在途新报告不能被旧完成边界吞掉");
    maintain_due(&f).await;
    assert!(f.store.schedule_maintenance().await.unwrap() > 0);
    f.store.sync_knowledge_due().await.unwrap();
    maintain_config(&f, false, 100).await;
    assert_eq!(f.store.schedule_maintenance().await.unwrap(), 0);
    maintain_config(&f, true, 1).await;
    assert_eq!(
        f.store.schedule_automatic().await.unwrap(),
        0,
        "每日预算与其他任务共享且不因切换设置清零"
    );
    f.server.abort();
}
#[tokio::test]
#[ignore = "需要隔离 MySQL；使用报告自动维护并复核证据"]
async fn maintenance_revisions_are_automatic_and_require_real_feedback() {
    let f = fixture().await;
    save(&f, "fictional-use-review").await;
    let note = memory(&f).await;
    report_problem(&f, note).await;
    maintain_config(&f, true, 100).await;
    f.model.mode.store(22, Ordering::SeqCst);
    assert_eq!(f.store.schedule_maintenance().await.unwrap(), 1);
    let task = latest_maintain(&f).await;
    let id = Uuid::from_slice(&task.id).unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    let done = job(&f, id).await;
    assert_eq!(done.state, "done", "{}", done.result.unwrap_or_default());
    let head =
        chromie::entity::memories::Entity::find_by_id((f.space.bytes(), note.as_bytes().to_vec()))
            .one(&f.store.db)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(head.current_revision, 2, "AI 核对实际报告后自动维护");
    let review = f.store.knowledge_review(f.space, id).await.unwrap();
    assert_eq!(review["result"]["pending_revisions"], 0);
    assert_eq!(
        review["result"]["decisions"][0]["evidence_details"][0]["role"],
        "agent_reported"
    );
    assert_eq!(review["result"]["decisions"][0]["resolution"], "automatic");
    assert_eq!(review["result"]["decisions"][0]["receipt"]["revision"], 2);
    report_problem(&f, note).await;
    maintain_due(&f).await;
    f.model.mode.store(23, Ordering::SeqCst);
    assert_eq!(f.store.schedule_maintenance().await.unwrap(), 1);
    let bad = latest_maintain(&f).await;
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(
        job(&f, Uuid::from_slice(&bad.id).unwrap())
            .await
            .last_error
            .as_deref(),
        Some("model_evidence_invalid")
    );
    f.server.abort();
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；问题依据不被成功报告淹没，新修订请求优先"]
async fn maintenance_prioritizes_revision_requests_and_keeps_problem_evidence_amid_successes() {
    let f = fixture().await;
    save(&f, "fictional-priority-check").await;
    let target = memory(&f).await;
    maintain_config(&f, true, 100).await;
    f.model.mode.store(5, Ordering::SeqCst);
    f.store.schedule_maintenance().await.unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    // 制造更多旧待处理资料，更新后的目标仍应因问题报告优先。
    for _ in 0..12 {
        memory(&f).await;
    }
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: target,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构方法新版本".into(),
                content: Some("虚构新版本数据库检查 /old-health".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
            },
        )
        .await
        .unwrap();
    let (agent, token, _) = support::grant(&f.store, f.space, false).await;
    // 真实 MCP 反馈走原 Agent 鉴权，绑定新的 revision，触发优先请求。
    let recall = Uuid::now_v7();
    let now = chromie::types::now();
    chromie::entity::recall_runs::ActiveModel {
        space_id: Set(f.space.bytes()),
        id: Set(recall.as_bytes().to_vec()),
        agent_id: Set(Some(agent.as_bytes().to_vec())),
        channel: Set("agent".into()),
        created_at: Set(now),
    }
    .insert(&f.store.db)
    .await
    .unwrap();
    chromie::entity::recall_items::ActiveModel {
        negative_reason: Set(None),
        space_id: Set(f.space.bytes()),
        recall_id: Set(recall.as_bytes().to_vec()),
        memory_id: Set(target.as_bytes().to_vec()),
        revision: Set(2),
        useful: Set(None),
        evaluated_at: Set(None),
        use_outcome: Set(None),
        use_reason: Set(None),
        used_at: Set(None),
    }
    .insert(&f.store.db)
    .await
    .unwrap();
    let router = chromie::http::router_with_sessions(
        f.store.clone(),
        None,
        chromie::web_session::WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let slug = chromie::entity::spaces::Entity::find_by_id(f.space.bytes())
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
        .slug;
    let response:Value=reqwest::Client::new().post(format!("http://{address}/spaces/{slug}/mcp")).bearer_auth(token).header("accept","application/json, text/event-stream").header("mcp-protocol-version","2025-06-18").json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"feedback_recall","arguments":{"recalls":[{"recall_id":recall,"usage":[{"memory_id":target,"outcome":"content_outdated","reason":"虚构旧端点迁移到 /health，仍需核对版本"}]}]}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(response["result"]["isError"], false);
    let progress = chromie::entity::maintenance_items::Entity::find_by_id((
        f.space.bytes(),
        target.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(progress.revision, 2);
    for _ in 0..25 {
        let id = Uuid::now_v7();
        let now = chromie::types::now();
        chromie::entity::recall_runs::ActiveModel {
            space_id: Set(f.space.bytes()),
            id: Set(id.as_bytes().to_vec()),
            agent_id: Set(Some(agent.as_bytes().to_vec())),
            channel: Set("agent".into()),
            created_at: Set(now),
        }
        .insert(&f.store.db)
        .await
        .unwrap();
        chromie::entity::recall_items::ActiveModel {
            negative_reason: Set(None),
            space_id: Set(f.space.bytes()),
            recall_id: Set(id.as_bytes().to_vec()),
            memory_id: Set(target.as_bytes().to_vec()),
            revision: Set(2),
            useful: Set(None),
            evaluated_at: Set(None),
            use_outcome: Set(Some("succeeded".into())),
            use_reason: Set(None),
            used_at: Set(Some(now)),
        }
        .insert(&f.store.db)
        .await
        .unwrap();
    }
    let revision = f.store.organization_settings(f.space).await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    f.store
        .save_organization(
            f.space,
            chromie::organization::Save {
                expected_revision: revision,
                enabled: false,
                extraction_enabled: false,
                maintenance_enabled: true,
                maintenance_interval_hours: 24,
                batch_size: 1,
                interval_seconds: 10,
                daily_limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(f.store.schedule_maintenance().await.unwrap(), 1);
    let task = latest_maintain(&f).await;
    let payload = task.payload.clone().unwrap();
    assert_eq!(payload["automatic"]["memory_ids"][0], json!(target));
    assert!(
        payload["usage"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["recall_id"] == json!(recall)),
        "负向依据不能被25条较新成功挤掉"
    );
    assert!(payload["usage"].as_array().unwrap().len() <= 20);
    server.abort();
}
