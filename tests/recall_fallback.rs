//! 在索引命中校验与输出卡片读取之间提交修订，覆盖真实 BM25 通道和待同步兜底。
use chromie::{
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Recall, SpaceId},
};
use migration::MigratorTrait;
use sea_orm::Database;
use serde_json::{Value, json};
use std::{
    sync::{Mutex, mpsc},
    time::Duration,
};
use tokio::sync::oneshot;
use uuid::Uuid;

async fn add(store: &Store, space: SpaceId, text: &str, project: &str) -> Uuid {
    let scope = json!({"kind":"project","project_key":project});
    let row = store
        .remember(
            space,
            serde_json::from_value(json!({
                "content":text,"request_id":Uuid::now_v7(),"scope":scope
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let id = Uuid::parse_str(row["memory_id"].as_str().unwrap()).unwrap();
    store
        .search
        .upsert(space, id, 1, text, Some(project))
        .await
        .unwrap();
    id
}

async fn race(writer: &Store, space: SpaceId, url: &str, query: &str, corrected: &str) -> Value {
    let id = add(writer, space, &format!("{query} 测试环境旧说明"), "test").await;
    // 同名的另一环境即使正文完全匹配，也不能填充测试项目的结果。
    add(
        writer,
        space,
        &format!("{query} 生产环境说明"),
        "production",
    )
    .await;
    let mut reader = Store::new(Database::connect(url).await.unwrap(), writer.search.clone());
    let (start_update, wait_for_head) = oneshot::channel();
    let start_update = Mutex::new(Some(start_update));
    let (update_done, wait_for_update) = mpsc::channel();
    let wait_for_update = Mutex::new(wait_for_update);
    reader.db.set_metric_callback(move |query| {
        if query
            .statement
            .sql
            .starts_with("SELECT `memories`.`space_id`")
            && let Some(start) = start_update.lock().unwrap().take()
        {
            start.send(()).unwrap();
            wait_for_update
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    });
    let update_store = writer.clone();
    let change = serde_json::from_value(json!({"memory_id":id,"expected_revision":1,
        "request_id":Uuid::now_v7(),"reason":"虚构环境变更","content":corrected}))
    .unwrap();
    let update = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10), wait_for_head)
            .await
            .expect("未观察到 BM25 命中后的修订读取")
            .unwrap();
        update_store.correct(space, change).await.unwrap();
        update_done.send(()).unwrap();
    });
    let found = reader
        .recall(
            space,
            Recall {
                query: query.into(),
                project_key: Some("test".into()),
                prefer_secret_refs: false,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap();
    update.await.unwrap();
    assert_eq!(found["index_available"], true);
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "需要独立 MySQL 测试库和 Qdrant"]
async fn indexed_proof_expires_while_current_pending_match_survives() {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let db = Database::connect(&url).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let writer = Store::new(
        db,
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    );
    let suffix: String = Uuid::now_v7().as_bytes()[8..]
        .iter()
        .map(|byte| char::from(b'a' + byte % 26))
        .collect();
    let slug = SpaceKey::parse(&format!("recall-race-{suffix}")).unwrap();
    writer
        .create_space(
            slug.clone(),
            format!("虚构修订回归-{suffix}"),
            "fictional-scope-password".into(),
        )
        .await
        .unwrap();
    let space = writer.resolve_space(&slug).await.unwrap();
    let stale = race(&writer, space, &url, "cedarfold", "已更换供应商").await;
    assert_eq!(stale["memories"], json!([]));
    let latest = race(
        &writer,
        space,
        &url,
        "willowfield",
        "willowfield 测试环境新说明",
    )
    .await;
    let cards = latest["memories"].as_array().unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["revision"], 2);
    assert_eq!(cards[0]["content"], "willowfield 测试环境新说明");
}
