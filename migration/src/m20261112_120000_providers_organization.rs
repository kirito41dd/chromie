use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261112_120000_providers_organization"
    }
}
fn a(name: &str) -> Alias {
    Alias::new(name)
}
fn timestamp(name: &str) -> ColumnDef {
    let mut column = ColumnDef::new(a(name));
    column.custom(a("DATETIME(6)")).not_null();
    column
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(a("model_providers"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(a("id"))
                            .binary_len(16)
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(a("name")).string_len(128).not_null())
                    .col(ColumnDef::new(a("base_url")).string_len(2048).not_null())
                    .col(ColumnDef::new(a("credential_id")).binary_len(16).not_null())
                    .col(ColumnDef::new(a("key_cipher")).custom(a("BLOB")).null())
                    .col(ColumnDef::new(a("key_nonce")).binary_len(12).null())
                    .col(ColumnDef::new(a("key_id")).string_len(128).null())
                    .col(ColumnDef::new(a("revision")).big_integer().not_null())
                    .col(timestamp("created_at"))
                    .col(timestamp("updated_at"))
                    .to_owned(),
            )
            .await?;
        for table in ["text_model_settings", "embedding_generations"] {
            if !manager.has_column(table, "provider_id").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(a(table))
                            .add_column(ColumnDef::new(a("provider_id")).binary_len(16).null())
                            .to_owned(),
                    )
                    .await?;
            }
        }
        if !manager.has_column("knowledge_jobs", "automatic").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(a("knowledge_jobs"))
                        .add_column(
                            ColumnDef::new(a("automatic"))
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_table(
                Table::create()
                    .table(a("organization_settings"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(a("id"))
                            .tiny_unsigned()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(a("revision")).big_integer().not_null())
                    .col(ColumnDef::new(a("enabled")).boolean().not_null())
                    .col(ColumnDef::new(a("batch_size")).unsigned().not_null())
                    .col(ColumnDef::new(a("interval_seconds")).unsigned().not_null())
                    .col(ColumnDef::new(a("daily_limit")).unsigned().not_null())
                    .col(ColumnDef::new(a("usage_date")).date().not_null())
                    .col(ColumnDef::new(a("attempts_today")).unsigned().not_null())
                    .col(timestamp("next_run_at"))
                    .col(timestamp("updated_at"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(a("organization_items"))
                    .if_not_exists()
                    .col(ColumnDef::new(a("space_id")).binary_len(16).not_null())
                    .col(ColumnDef::new(a("memory_id")).binary_len(16).not_null())
                    .col(ColumnDef::new(a("revision")).integer().not_null())
                    .col(
                        ColumnDef::new(a("config_revision"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(a("state")).string_len(16).not_null())
                    .col(ColumnDef::new(a("job_id")).binary_len(16).null())
                    .col(timestamp("run_after"))
                    .col(ColumnDef::new(a("last_error")).string_len(64).null())
                    .col(
                        ColumnDef::new(a("completed_at"))
                            .custom(a("DATETIME(6)"))
                            .null(),
                    )
                    .col(timestamp("updated_at"))
                    .primary_key(Index::create().col(a("space_id")).col(a("memory_id")))
                    .index(
                        Index::create()
                            .name("ix_organization_items_job")
                            .col(a("space_id"))
                            .col(a("job_id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_organization_memory")
                            .from(a("organization_items"), (a("space_id"), a("memory_id")))
                            .to(a("memories"), (a("space_id"), a("id"))),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "模型连接和自动整理进度不能自动删除，请使用显式归档迁移".into(),
        ))
    }
}
