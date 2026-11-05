use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn id(name: &str) -> ColumnDef {
    ColumnDef::new(Alias::new(name))
        .binary_len(16)
        .not_null()
        .to_owned()
}

fn text(name: &str, length: u32) -> ColumnDef {
    ColumnDef::new(Alias::new(name))
        .string_len(length)
        .not_null()
        .to_owned()
}

fn timestamp(name: &str) -> ColumnDef {
    ColumnDef::new(Alias::new(name))
        .custom(Alias::new("DATETIME(6)"))
        .not_null()
        .to_owned()
}

fn table(name: &str) -> TableCreateStatement {
    Table::create()
        .table(Alias::new(name))
        .character_set("utf8mb4")
        .collate("utf8mb4_bin")
        .to_owned()
}

fn primary(columns: &[&str]) -> IndexCreateStatement {
    let mut index = Index::create();
    for name in columns {
        index.col(Alias::new(*name));
    }
    index.primary().to_owned()
}

fn space_fk(table: &str) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .name(format!("fk_{table}_space"))
        .from(Alias::new(table), Alias::new("space_id"))
        .to(Alias::new("spaces"), Alias::new("id"))
        .to_owned()
}

fn scoped_fk(
    name: &str,
    from: &str,
    columns: &[&str],
    to: &str,
    targets: &[&str],
) -> ForeignKeyCreateStatement {
    let mut key = ForeignKey::create();
    key.name(name);
    for (column, target) in columns.iter().zip(targets) {
        key.from(Alias::new(from), Alias::new(*column))
            .to(Alias::new(to), Alias::new(*target));
    }
    key.to_owned()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                table("spaces")
                    .col(id("id"))
                    .col(text("name", 100).unique_key())
                    .col(text("password_hash", 255))
                    .col(timestamp("created_at"))
                    .primary_key(&mut primary(&["id"]))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                table("source_events")
                    .col(id("space_id"))
                    .col(id("id"))
                    .col(text("client_type", 32))
                    .col(text("session_id", 128))
                    .col(ColumnDef::new(Alias::new("turn_id")).string_len(128))
                    .col(text("role", 16))
                    .col(ColumnDef::new(Alias::new("body")).custom(Alias::new("LONGTEXT")))
                    .col(ColumnDef::new(Alias::new("project_key")).string_len(128))
                    .col(timestamp("occurred_at"))
                    .col(timestamp("received_at"))
                    .col(
                        ColumnDef::new(Alias::new("forgotten_at"))
                            .custom(Alias::new("DATETIME(6)")),
                    )
                    .primary_key(&mut primary(&["space_id", "id"]))
                    .foreign_key(&mut space_fk("source_events"))
                    .index(
                        Index::create()
                            .name("idx_event_session")
                            .col(Alias::new("space_id"))
                            .col(Alias::new("session_id"))
                            .col(Alias::new("received_at")),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                table("memories")
                    .col(id("space_id"))
                    .col(id("id"))
                    .col(
                        ColumnDef::new(Alias::new("current_revision"))
                            .integer()
                            .not_null(),
                    )
                    .col(timestamp("created_at"))
                    .col(timestamp("updated_at"))
                    .col(
                        ColumnDef::new(Alias::new("forgotten_at"))
                            .custom(Alias::new("DATETIME(6)")),
                    )
                    .primary_key(&mut primary(&["space_id", "id"]))
                    .foreign_key(&mut space_fk("memories"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                table("memory_revisions")
                    .col(id("space_id"))
                    .col(id("memory_id"))
                    .col(ColumnDef::new(Alias::new("revision")).integer().not_null())
                    .col(ColumnDef::new(Alias::new("content")).text())
                    .col(ColumnDef::new(Alias::new("project_key")).string_len(128))
                    .col(ColumnDef::new(Alias::new("change_reason")).text())
                    .col(timestamp("created_at"))
                    .primary_key(&mut primary(&["space_id", "memory_id", "revision"]))
                    .foreign_key(&mut scoped_fk(
                        "fk_revision_memory",
                        "memory_revisions",
                        &["space_id", "memory_id"],
                        "memories",
                        &["space_id", "id"],
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                table("memory_sources")
                    .col(id("space_id"))
                    .col(id("memory_id"))
                    .col(ColumnDef::new(Alias::new("revision")).integer().not_null())
                    .col(id("event_id"))
                    .primary_key(&mut primary(&[
                        "space_id",
                        "memory_id",
                        "revision",
                        "event_id",
                    ]))
                    .foreign_key(&mut scoped_fk(
                        "fk_source_revision",
                        "memory_sources",
                        &["space_id", "memory_id", "revision"],
                        "memory_revisions",
                        &["space_id", "memory_id", "revision"],
                    ))
                    .foreign_key(&mut scoped_fk(
                        "fk_source_event",
                        "memory_sources",
                        &["space_id", "event_id"],
                        "source_events",
                        &["space_id", "id"],
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                table("write_receipts")
                    .col(id("space_id"))
                    .col(text("operation", 32))
                    .col(id("request_id"))
                    .col(
                        ColumnDef::new(Alias::new("input_hash"))
                            .binary_len(32)
                            .not_null(),
                    )
                    .col(id("target_id"))
                    .col(ColumnDef::new(Alias::new("revision")).integer())
                    .col(timestamp("created_at"))
                    .primary_key(&mut primary(&["space_id", "operation", "request_id"]))
                    .foreign_key(&mut space_fk("write_receipts"))
                    .to_owned(),
            )
            .await?;

        // 每条记忆只保留一个可恢复的同步目标，更新时合并到最新修订。
        manager
            .create_table(
                table("outbox_tasks")
                    .col(id("space_id"))
                    .col(id("memory_id"))
                    .col(
                        ColumnDef::new(Alias::new("target_revision"))
                            .integer()
                            .not_null(),
                    )
                    .col(text("state", 16))
                    .col(ColumnDef::new(Alias::new("attempts")).integer().not_null())
                    .col(timestamp("run_after"))
                    .col(timestamp("updated_at"))
                    .primary_key(&mut primary(&["space_id", "memory_id"]))
                    .foreign_key(&mut scoped_fk(
                        "fk_task_memory",
                        "outbox_tasks",
                        &["space_id", "memory_id"],
                        "memories",
                        &["space_id", "id"],
                    ))
                    .index(
                        Index::create()
                            .name("idx_task_due")
                            .col(Alias::new("state"))
                            .col(Alias::new("run_after")),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for name in [
            "outbox_tasks",
            "write_receipts",
            "memory_sources",
            "memory_revisions",
            "memories",
            "source_events",
            "spaces",
        ] {
            manager
                .drop_table(Table::drop().table(Alias::new(name)).to_owned())
                .await?;
        }
        Ok(())
    }
}
