use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("recall_runs"))
                    .if_not_exists()
                    .character_set("utf8mb4")
                    .collate("utf8mb4_bin")
                    .col(
                        ColumnDef::new(Alias::new("space_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("id")).binary_len(16).not_null())
                    .col(ColumnDef::new(Alias::new("agent_id")).binary_len(16).null())
                    .col(
                        ColumnDef::new(Alias::new("channel"))
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .custom(Alias::new("DATETIME(6)"))
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("space_id"))
                            .col(Alias::new("id")),
                    )
                    .index(
                        Index::create()
                            .name("ix_recall_runs_channel_time")
                            .col(Alias::new("space_id"))
                            .col(Alias::new("channel"))
                            .col(Alias::new("created_at")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_recall_run_space")
                            .from(Alias::new("recall_runs"), Alias::new("space_id"))
                            .to(Alias::new("spaces"), Alias::new("id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_recall_run_agent")
                            .from(
                                Alias::new("recall_runs"),
                                (Alias::new("space_id"), Alias::new("agent_id")),
                            )
                            .to(
                                Alias::new("agent_connections"),
                                (Alias::new("space_id"), Alias::new("id")),
                            ),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("recall_items"))
                    .if_not_exists()
                    .character_set("utf8mb4")
                    .collate("utf8mb4_bin")
                    .col(
                        ColumnDef::new(Alias::new("space_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("recall_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("memory_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("revision")).integer().not_null())
                    .col(ColumnDef::new(Alias::new("useful")).boolean().null())
                    .col(
                        ColumnDef::new(Alias::new("evaluated_at"))
                            .custom(Alias::new("DATETIME(6)"))
                            .null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("space_id"))
                            .col(Alias::new("recall_id"))
                            .col(Alias::new("memory_id")),
                    )
                    .index(
                        Index::create()
                            .name("ix_recall_items_memory_revision")
                            .col(Alias::new("space_id"))
                            .col(Alias::new("memory_id"))
                            .col(Alias::new("revision")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_recall_item_run")
                            .from(
                                Alias::new("recall_items"),
                                (Alias::new("space_id"), Alias::new("recall_id")),
                            )
                            .to(
                                Alias::new("recall_runs"),
                                (Alias::new("space_id"), Alias::new("id")),
                            ),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_recall_item_memory")
                            .from(
                                Alias::new("recall_items"),
                                (Alias::new("space_id"), Alias::new("memory_id")),
                            )
                            .to(
                                Alias::new("memories"),
                                (Alias::new("space_id"), Alias::new("id")),
                            ),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_recall_item_revision")
                            .from(
                                Alias::new("recall_items"),
                                (
                                    Alias::new("space_id"),
                                    Alias::new("memory_id"),
                                    Alias::new("revision"),
                                ),
                            )
                            .to(
                                Alias::new("memory_revisions"),
                                (
                                    Alias::new("space_id"),
                                    Alias::new("memory_id"),
                                    Alias::new("revision"),
                                ),
                            ),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "召回与评价记录不能自动删除，请使用显式归档迁移".into(),
        ))
    }
}
