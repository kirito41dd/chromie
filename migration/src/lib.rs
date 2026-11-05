#![cfg_attr(
    not(test),
    deny(clippy::string_slice, clippy::expect_used, clippy::unwrap_used)
)]
#![cfg_attr(not(test), forbid(unsafe_code))]

pub use sea_orm_migration::prelude::*;

mod m20261026_000001_memory;
mod m20261026_000002_event_text;
mod m20261027_000003_space_slug;
mod m20261028_000004_secrets;
mod m20261028_000005_secret_blob;
mod m20261028_000006_agent_connections;
mod m20261030_000007_secret_contract;
mod m20261030_000008_secret_access_audit;
mod m20261103_000009_recall_usage;
mod m20261103_000010_instance_embedding;
mod m20261103_000011_model_usage_daily;
mod m20261103_000012_model_limits;
mod m20261104_000013_similarity_threshold;
mod m20261104_000014_embedding_batch;

mod m20261104_000015_soul_transfer;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261026_000001_memory::Migration),
            Box::new(m20261026_000002_event_text::Migration),
            Box::new(m20261027_000003_space_slug::Migration),
            Box::new(m20261028_000004_secrets::Migration),
            Box::new(m20261028_000005_secret_blob::Migration),
            Box::new(m20261028_000006_agent_connections::Migration),
            Box::new(m20261030_000007_secret_contract::Migration),
            Box::new(m20261030_000008_secret_access_audit::Migration),
            Box::new(m20261103_000009_recall_usage::Migration),
            Box::new(m20261103_000010_instance_embedding::Migration),
            Box::new(m20261103_000011_model_usage_daily::Migration),
            Box::new(m20261103_000012_model_limits::Migration),
            Box::new(m20261104_000013_similarity_threshold::Migration),
            Box::new(m20261104_000014_embedding_batch::Migration),
            Box::new(m20261104_000015_soul_transfer::Migration),
        ]
    }
}
