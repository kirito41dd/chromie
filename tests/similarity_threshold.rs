mod support;
use axum::{Json, Router, extract::State, routing::post};
use chromie::{
    embedding::Save,
    entity::{embedding_generations as generations, embedding_tasks as tasks},
    retrieval_policy::SimilarityThreshold,
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Correct, Recall, Remember, Scope, SpaceId},
};
use migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database, EntityTrait, PaginatorTrait, Set};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Default)]
struct Mock {
    calls: AtomicUsize,
    fail: AtomicBool,
}
async fn embed(
    State(mock): State<Arc<Mock>>,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    if mock.fail.load(Ordering::SeqCst) {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, Json(json!({})));
    }
    let data = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let text = text.as_str().unwrap();
            let mut vector = if text == "你好" {
                vec![0.0, 0.0, 1.0]
            } else if text.starts_with("stored-low:") {
                vec![0.0, 1.0, 0.1]
            } else {
                vec![1.0, 0.0, 0.1]
            };
            vector.resize(body["dimensions"].as_u64().unwrap() as usize, 0.0);
            json!({"index":index,"embedding":vector})
        })
        .rev()
        .collect::<Vec<_>>();
    (
        axum::http::StatusCode::OK,
        Json(json!({"data":data,"usage":{"total_tokens":1}})),
    )
}
fn input(state: &Value, url: &str) -> Save {
    serde_json::from_value(json!({"expected_revision":state["revision"],"base_url":url,"model":"text-embedding-v4","model_version":"","dimensions":1024,"api_key":null,"confirm_rebuild":true})).unwrap()
}
async fn put(store: &Store, space: SpaceId, text: &str, scope: Scope) -> Uuid {
    Uuid::parse_str(
        store
            .remember(
                space,
                Remember {
                    content: text.into(),
                    scope,
                    source_refs: vec![],
                    request_id: Uuid::now_v7(),
                },
            )
            .await
            .unwrap()["memory_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}
async fn query(store: &Store, space: SpaceId, text: &str) -> Value {
    store
        .recall(
            space,
            Recall {
                query: text.into(),
                project_key: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap()
}
fn contains(result: &Value, id: Uuid) -> bool {
    result["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["memory_id"] == id.to_string())
}
async fn drain(store: &Store, space: SpaceId) -> Value {
    for _ in 0..100 {
        store.sync_due().await.unwrap();
        store.sync_embedding_due().await.unwrap();
        let value = store.model_settings(space).await.unwrap();
        if value["pending"].is_null() {
            return value;
        }
    }
    panic!("测试向量构建未完成")
}

#[tokio::test]
#[ignore = "需要独立空 MySQL 与 Qdrant，仅使用本机确定性虚构模型"]
async fn per_generation_threshold_filters_both_lanes_without_losing_fresh_or_literal_keywords() {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    // 升级前有一条旧代际，新增 nullable 列不能改变已有密文或向量身份。
    migration::Migrator::up(&db, Some(12)).await.unwrap();
    let legacy_id = Uuid::now_v7();
    let at = chromie::types::now();
    generations::Entity::insert(generations::ActiveModel {
        id: Set(legacy_id.as_bytes().to_vec()),
        profile: Set(
            json!({"model":"legacy","model_version":"","dimensions":3,"encoding":"plain-v1"}),
        ),
        fingerprint: Set("legacy-fingerprint".into()),
        base_url: Set("http://127.0.0.1:1/v1".into()),
        key_id: Set(Some("legacy-key-id".into())),
        key_nonce: Set(Some(vec![7; 12])),
        key_cipher: Set(Some(vec![8; 32])),
        connection_revision: Set(4),
        state: Set("retired".into()),
        scan_complete: Set(1),
        created_at: Set(at),
        updated_at: Set(at),
        ..Default::default()
    })
    .exec_without_returning(&db)
    .await
    .unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let legacy = generations::Entity::find_by_id(legacy_id.as_bytes().to_vec())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(legacy.similarity_threshold, None);
    assert_eq!(legacy.batch_size, None);
    assert_eq!(legacy.key_cipher, Some(vec![8; 32]));
    assert_eq!(legacy.connection_revision, 4);
    assert_eq!(legacy.fingerprint, "legacy-fingerprint");
    let store = support::enable(Store::new(
        db.clone(),
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    ))
    .await;
    for name in ["threshold-owner", "threshold-other"] {
        store
            .create_space(
                SpaceKey::parse(name).unwrap(),
                format!("虚构阈值测试-{name}"),
                "fictional-policy-password".into(),
            )
            .await
            .unwrap();
    }
    let owner = store
        .resolve_space(&SpaceKey::parse("threshold-owner").unwrap())
        .await
        .unwrap();
    let other = store
        .resolve_space(&SpaceKey::parse("threshold-other").unwrap())
        .await
        .unwrap();
    store.initialize_model_admin(Some(owner)).await.unwrap();
    let low = put(&store, owner, "stored-low: zebra sunny", Scope::Global).await;
    let exact = put(&store, owner, "stored-low: key-code-1234", Scope::Global).await;
    let short_exact = put(
        &store,
        owner,
        "stored-low: 虚构 UI 记忆；AI 与 GPU 术语",
        Scope::Global,
    )
    .await;
    // 三十五个更近的向量让低分 BM25 候选落在稠密 Top30 以外，必须额外按 ID 评分。
    for n in 0..35 {
        put(&store, owner, &format!("filler note {n}"), Scope::Global).await;
    }
    let foreign = put(&store, other, "stored-low: key-code-1234", Scope::Global).await;
    let other_project = put(
        &store,
        owner,
        "stored-low: key-code-1234",
        Scope::Project {
            project_key: "other-project".into(),
        },
    )
    .await;
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embed))
        .with_state(mock.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = input(&store.model_settings(owner).await.unwrap(), &model_url);
    config.api_key = Some("fictional-policy-key".into());
    store.save_model_settings(owner, config).await.unwrap();
    let mut state = drain(&store, owner).await;
    assert_eq!(state["active"]["similarity_threshold"], 0.56);
    assert_eq!(state["active"]["recommended_similarity_threshold"], 0.56);
    let id = Uuid::parse_str(state["active"]["generation_id"].as_str().unwrap())
        .unwrap()
        .as_bytes()
        .to_vec();
    let before = generations::Entity::find_by_id(id.clone())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let calls = mock.calls.load(Ordering::SeqCst);
    let task_count = tasks::Entity::find().count(&db).await.unwrap();
    let mut change = input(&state, &model_url);
    change.similarity_threshold = Some(Some(SimilarityThreshold::new(0.8).unwrap()));
    change.max_concurrency = Some(2);
    state = store.save_model_settings(owner, change).await.unwrap();
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        calls,
        "只改阈值与并发不能调用模型"
    );
    assert_eq!(tasks::Entity::find().count(&db).await.unwrap(), task_count);
    let after = generations::Entity::find_by_id(id.clone())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.fingerprint, after.fingerprint);
    assert_eq!(before.key_cipher, after.key_cipher);
    assert_eq!(before.connection_revision, after.connection_revision);
    assert!(state["pending"].is_null());
    let greeting = query(&store, owner, "你好").await;
    assert!(greeting["memories"].as_array().unwrap().is_empty());
    assert_eq!(greeting["similarity_threshold"], 0.8);
    let calls = mock.calls.load(Ordering::SeqCst);
    assert!(!contains(&query(&store, owner, "zebra rain").await, low));
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        calls + 1,
        "两路候选评分复用同一次查询向量"
    );
    let literal = query(&store, owner, "key-code-1234").await;
    assert!(contains(&literal, exact));
    assert!(!contains(&literal, foreign));
    // 查询与该记录的模型向量近乎正交，仍应保留 BM25 完整短词命中。
    for term in ["虚构", "AI", "GPU"] {
        let result = query(&store, owner, term).await;
        assert!(
            contains(&result, short_exact),
            "短关键词 {term} 不能被语义门槛误删"
        );
    }
    let scoped = store
        .recall(
            owner,
            Recall {
                query: "key-code-1234".into(),
                project_key: Some("current-project".into()),
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap();
    assert!(contains(&scoped, exact));
    assert!(!contains(&scoped, other_project));
    assert!(!contains(&scoped, foreign));
    let fresh = put(&store, owner, "stored-low: fresh current", Scope::Global).await;
    assert!(contains(
        &query(&store, owner, "fresh missing").await,
        fresh
    ));
    store.sync_due().await.unwrap();
    assert!(
        contains(&query(&store, owner, "fresh missing").await, fresh),
        "BM25 已同步而向量未生成时保留关键词"
    );
    store
        .correct(
            owner,
            Correct {
                memory_id: low,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构修订验证".into(),
                content: Some("stored-low: zebra revised".into()),
                scope: None,
                source_refs: None,
            },
        )
        .await
        .unwrap();
    store.sync_due().await.unwrap();
    assert!(
        contains(&query(&store, owner, "zebra rain").await, low),
        "旧修订的低分不能过滤新正文"
    );
    // 省略保持自定义值；显式 null 才恢复自动。变更仍不产生重建。
    let mut omitted = input(&state, &model_url);
    omitted.max_concurrency = Some(3);
    state = store.save_model_settings(owner, omitted).await.unwrap();
    assert_eq!(state["active"]["similarity_threshold"], 0.8);
    let mut reset = input(&state, &model_url);
    reset.similarity_threshold = Some(None);
    state = store.save_model_settings(owner, reset).await.unwrap();
    assert_eq!(state["active"]["similarity_threshold"], 0.56);
    let mut disabled = input(&state, &model_url);
    disabled.similarity_threshold = Some(Some(SimilarityThreshold::new(0.0).unwrap()));
    state = store.save_model_settings(owner, disabled).await.unwrap();
    assert!(
        !query(&store, owner, "你好").await["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut custom = input(&state, &model_url);
    custom.similarity_threshold = Some(Some(SimilarityThreshold::new(0.8).unwrap()));
    state = store.save_model_settings(owner, custom).await.unwrap();
    let mut next = input(&state, &model_url);
    next.model = "fictional-next-model".into();
    next.dimensions = 3;
    let pending = store.save_model_settings(owner, next).await.unwrap();
    assert_eq!(pending["active"]["similarity_threshold"], 0.8);
    assert_eq!(pending["pending"]["similarity_threshold"], 0.0);
    assert!(pending["pending"]["similarity_threshold_override"].is_null());
    assert!(pending["pending"]["recommended_similarity_threshold"].is_null());
    mock.fail.store(true, Ordering::SeqCst);
    let fallback = query(&store, owner, "key-code-1234").await;
    assert_eq!(fallback["retrieval_mode"], "keyword");
    assert!(contains(&fallback, exact));
    task.abort();
}
