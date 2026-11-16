use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261111_134801_memory_graph"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 旧修订默认仍是普通记忆，不扫描或改写正文；可恢复已完成单条 DDL 的中断。
        if !manager
            .has_column("memory_revisions", "memory_type")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("memory_revisions"))
                        .add_column(
                            ColumnDef::new(Alias::new("memory_type"))
                                .string_len(16)
                                .not_null()
                                .default("note"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("memory_links"))
                    .if_not_exists()
                    .character_set("utf8mb4")
                    .collate("utf8mb4_bin")
                    .col(
                        ColumnDef::new(Alias::new("space_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("memory_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("revision")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("target_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("relation"))
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("target_revision"))
                            .integer()
                            .null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("space_id"))
                            .col(Alias::new("memory_id"))
                            .col(Alias::new("revision"))
                            .col(Alias::new("target_id"))
                            .col(Alias::new("relation")),
                    )
                    .index(
                        Index::create()
                            .name("ix_memory_links_target")
                            .col(Alias::new("space_id"))
                            .col(Alias::new("target_id"))
                            .col(Alias::new("memory_id"))
                            .col(Alias::new("revision")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_memory_link_source")
                            .from(
                                Alias::new("memory_links"),
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
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_memory_link_target")
                            .from(
                                Alias::new("memory_links"),
                                (Alias::new("space_id"), Alias::new("target_id")),
                            )
                            .to(
                                Alias::new("memories"),
                                (Alias::new("space_id"), Alias::new("id")),
                            ),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_memory_link_target_revision")
                            .from(
                                Alias::new("memory_links"),
                                (
                                    Alias::new("space_id"),
                                    Alias::new("target_id"),
                                    Alias::new("target_revision"),
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
            "记忆引用和主题类型不能自动删除，请使用显式归档迁移".into(),
        ))
    }
}
