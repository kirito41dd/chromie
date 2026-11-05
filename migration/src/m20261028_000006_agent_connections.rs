use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn id(name: &str) -> ColumnDef {
    ColumnDef::new(Alias::new(name))
        .binary_len(16)
        .not_null()
        .to_owned()
}

fn text(name: &str, size: u32) -> ColumnDef {
    ColumnDef::new(Alias::new(name))
        .string_len(size)
        .not_null()
        .to_owned()
}

fn date(name: &str, nullable: bool) -> ColumnDef {
    let mut column = ColumnDef::new(Alias::new(name));
    column.custom(Alias::new("DATETIME(6)"));
    if !nullable {
        column.not_null();
    }
    column
}

fn agent_fk(table: &str) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .name(format!("fk_{table}_agent"))
        .from(
            Alias::new(table),
            (Alias::new("space_id"), Alias::new("agent_id")),
        )
        .to(
            Alias::new("agent_connections"),
            (Alias::new("space_id"), Alias::new("id")),
        )
        .to_owned()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(Alias::new("agent_connections"))
                    .character_set("utf8mb4")
                    .collate("utf8mb4_bin")
                    .col(id("space_id"))
                    .col(id("id"))
                    .col(text("name", 100))
                    .col(text("client_type", 32))
                    .col(
                        ColumnDef::new(Alias::new("enabled"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("allow_secrets"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(date("created_at", false))
                    .col(date("updated_at", false))
                    .col(date("last_mcp_at", true))
                    .col(date("last_hook_at", true))
                    .primary_key(
                        Index::create()
                            .col(Alias::new("space_id"))
                            .col(Alias::new("id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_space")
                            .from(Alias::new("agent_connections"), Alias::new("space_id"))
                            .to(Alias::new("spaces"), Alias::new("id")),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(Alias::new("agent_credentials"))
                    .character_set("utf8mb4")
                    .collate("utf8mb4_bin")
                    .col(id("space_id"))
                    .col(id("agent_id"))
                    .col(text("kind", 8))
                    .col(ColumnDef::new(Alias::new("revision")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("key_hash"))
                            .binary_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("format_version"))
                            .integer()
                            .not_null(),
                    )
                    .col(text("key_id", 64))
                    .col(
                        ColumnDef::new(Alias::new("nonce"))
                            .binary_len(12)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("ciphertext")).blob().not_null())
                    .col(date("updated_at", false))
                    .primary_key(
                        Index::create()
                            .col(Alias::new("space_id"))
                            .col(Alias::new("agent_id"))
                            .col(Alias::new("kind")),
                    )
                    .index(
                        Index::create()
                            .name("uq_agent_key_hash")
                            .col(Alias::new("space_id"))
                            .col(Alias::new("kind"))
                            .col(Alias::new("key_hash"))
                            .unique(),
                    )
                    .foreign_key(&mut agent_fk("agent_credentials"))
                    .to_owned(),
            )
            .await?;
        // MySQL DDL 不具备事务回滚；允许已成功的列在明确重试迁移时保留。
        for table in [
            "source_events",
            "memories",
            "memory_revisions",
            "write_receipts",
        ] {
            if !manager.has_column(table, "agent_id").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(
                                ColumnDef::new(Alias::new("agent_id")).binary_len(16).null(),
                            )
                            .add_foreign_key(agent_fk(table).get_foreign_key())
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "接入迁移包含凭据与历史来源，禁止自动删除；请先完成显式数据迁移".into(),
        ))
    }
}
