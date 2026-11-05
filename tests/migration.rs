use chromie::entity::{source_events, spaces};
use migration::{Migrator, MigratorTrait, SchemaManager};
use sea_orm::{ConnectOptions, Database, EntityTrait, QuerySelect, Set};
use uuid::Uuid;

#[tokio::test]
#[ignore = "需要一次性空 MySQL 测试库；见开发运行文档"]
async fn schema_first_upgrade_preserves_data_and_rejects_truncation() {
    let url = std::env::var("CHROMIE_MIGRATION_TEST_DATABASE_URL").unwrap();
    let parsed = reqwest::Url::parse(&url).unwrap();
    assert!(parsed.path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    let manager = SchemaManager::new(&db);
    assert!(
        !manager.has_table("spaces").await.unwrap(),
        "迁移用例必须使用一次性空库，拒绝回退已有数据库"
    );

    Migrator::up(&db, Some(1)).await.unwrap();
    let space = Uuid::now_v7().as_bytes().to_vec();
    let now = chromie::types::now();
    // 旧表尚无 slug；只执行显式列的插入，不用当前 Entity 回读完整新模型。
    spaces::Entity::insert(spaces::ActiveModel {
        id: Set(space.clone()),
        name: Set("虚构的迁移验证空间".into()),
        password_hash: Set("测试不启用接口".into()),
        created_at: Set(now),
        ..Default::default()
    })
    .exec(&db)
    .await
    .unwrap();
    let short = "中文原始证据，升级后保持完整。";
    let mut ids = Vec::new();
    for body in [short.to_owned(), "x".repeat(60001)] {
        let id = Uuid::now_v7().as_bytes().to_vec();
        source_events::Entity::insert(source_events::ActiveModel {
            space_id: Set(space.clone()),
            id: Set(id.clone()),
            client_type: Set("migration_test".into()),
            session_id: Set("虚构会话".into()),
            turn_id: Set(None),
            role: Set("user".into()),
            body: Set(Some(body)),
            project_key: Set(None),
            occurred_at: Set(now),
            received_at: Set(now),
            forgotten_at: Set(None),
            ..Default::default()
        })
        .exec(&db)
        .await
        .unwrap();
        ids.push(id);
    }

    let error = Migrator::up(&db, None).await.unwrap_err();
    assert!(error.to_string().contains("已有超长事件"));
    let original = source_events::Entity::find_by_id((space.clone(), ids[1].clone()))
        .select_only()
        .column(source_events::Column::Body)
        .into_tuple::<Option<String>>()
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.unwrap().len(), 60001);

    // 只移除本用例创建的超长夹具，不对真实数据做截断或自动清理。
    source_events::Entity::delete_by_id((space.clone(), ids[1].clone()))
        .exec(&db)
        .await
        .unwrap();
    Migrator::up(&db, None).await.unwrap();
    Migrator::up(&db, None).await.unwrap();
    let preserved_space = spaces::Entity::find_by_id(space.clone())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(preserved_space.slug, "space-a");
    assert_eq!(preserved_space.password_hash, "测试不启用接口");
    let preserved = source_events::Entity::find_by_id((space, ids[0].clone()))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(preserved.body.as_deref(), Some(short));
    assert_eq!(
        Migrator::get_applied_migrations(&db).await.unwrap().len(),
        15
    );
}
