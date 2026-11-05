use chromie::entity::{memories, memory_revisions, secrets, spaces};
use migration::{Migrator, MigratorTrait, SchemaManager};
use sea_orm::{ConnectOptions, Database, EntityTrait, QuerySelect, Set};
use uuid::Uuid;

#[tokio::test]
#[ignore = "需要一次性空 MySQL 升级库，禁止复用真实数据"]
async fn nullable_contract_migration_preserves_legacy_ciphertext_and_is_repeatable() {
    let url = std::env::var("CHROMIE_CONTRACT_MIGRATION_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    let schema = SchemaManager::new(&db);
    assert!(
        !schema.has_table("spaces").await.unwrap(),
        "拒绝在非空升级库运行"
    );
    Migrator::up(&db, Some(6)).await.unwrap();
    assert!(!schema.has_column("secrets", "contract").await.unwrap());
    let space = Uuid::now_v7().as_bytes().to_vec();
    let id = Uuid::now_v7().as_bytes().to_vec();
    let now = chromie::types::now();
    spaces::Entity::insert(spaces::ActiveModel {
        id: Set(space.clone()),
        slug: Set("contract-upgrade".into()),
        name: Set("虚构迁移空间".into()),
        password_hash: Set("fixture-only".into()),
        created_at: Set(now),
        ..Default::default()
    })
    .exec(&db)
    .await
    .unwrap();
    memories::Entity::insert(memories::ActiveModel {
        space_id: Set(space.clone()),
        id: Set(id.clone()),
        current_revision: Set(1),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec(&db)
    .await
    .unwrap();
    memory_revisions::Entity::insert(memory_revisions::ActiveModel {
        space_id: Set(space.clone()),
        memory_id: Set(id.clone()),
        revision: Set(1),
        content: Set(Some("虚构旧记录".into())),
        created_at: Set(now),
        ..Default::default()
    })
    .exec(&db)
    .await
    .unwrap();
    let nonce = vec![3; 12];
    let ciphertext = vec![7; 60016];
    secrets::Entity::insert(secrets::ActiveModel {
        space_id: Set(space.clone()),
        memory_id: Set(id.clone()),
        format_version: Set(1),
        payload_revision: Set(4),
        key_id: Set("fictional-migration-fixture".into()),
        nonce: Set(nonce.clone()),
        ciphertext: Set(Some(ciphertext.clone())),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec(&db)
    .await
    .unwrap();
    let before = secrets::Entity::find_by_id((space.clone(), id.clone()))
        .select_only()
        .column(secrets::Column::Ciphertext)
        .into_tuple::<Option<Vec<u8>>>()
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.as_ref(), Some(&ciphertext));
    Migrator::up(&db, None).await.unwrap();
    Migrator::up(&db, None).await.unwrap();
    assert!(schema.has_column("secrets", "contract").await.unwrap());
    let after = secrets::Entity::find_by_id((space, id))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.ciphertext, Some(ciphertext));
    assert_eq!(after.nonce, nonce);
    assert_eq!(after.payload_revision, 4);
    assert_eq!(after.key_id, "fictional-migration-fixture");
    assert_eq!(after.contract, None);
    assert_eq!(
        Migrator::get_applied_migrations(&db).await.unwrap().len(),
        15
    );
}
