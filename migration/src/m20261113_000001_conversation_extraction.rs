use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261113_000001_conversation_extraction"
    }
}
fn a(s: &str) -> Alias {
    Alias::new(s)
}
fn dt(s: &str) -> ColumnDef {
    let mut c = ColumnDef::new(a(s));
    c.custom(a("DATETIME(6)")).not_null();
    c
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for (name, mut col) in [
            (
                "extraction_enabled",
                ColumnDef::new(a("extraction_enabled"))
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            ),
            (
                "last_kind",
                ColumnDef::new(a("last_kind"))
                    .string_len(16)
                    .not_null()
                    .default("organize")
                    .to_owned(),
            ),
        ] {
            if !m.has_column("organization_settings", name).await? {
                m.alter_table(
                    Table::alter()
                        .table(a("organization_settings"))
                        .add_column(&mut col)
                        .to_owned(),
                )
                .await?;
            }
        }
        m.create_table(
            Table::create()
                .table(a("conversation_metadata"))
                .if_not_exists()
                .col(ColumnDef::new(a("space_id")).binary_len(16).not_null())
                .col(
                    ColumnDef::new(a("conversation_key"))
                        .binary_len(32)
                        .not_null(),
                )
                .col(ColumnDef::new(a("title")).string_len(240).null())
                .col(
                    ColumnDef::new(a("title_locked"))
                        .boolean()
                        .not_null()
                        .default(false),
                )
                .col(
                    ColumnDef::new(a("revision"))
                        .big_integer()
                        .not_null()
                        .default(0),
                )
                .col(dt("updated_at"))
                .primary_key(
                    Index::create()
                        .col(a("space_id"))
                        .col(a("conversation_key")),
                )
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(a("conversation_titles"))
                .if_not_exists()
                .col(ColumnDef::new(a("space_id")).binary_len(16).not_null())
                .col(
                    ColumnDef::new(a("conversation_key"))
                        .binary_len(32)
                        .not_null(),
                )
                .col(ColumnDef::new(a("revision")).big_integer().not_null())
                .col(ColumnDef::new(a("title")).string_len(240).not_null())
                .col(ColumnDef::new(a("source")).string_len(16).not_null())
                .col(dt("created_at"))
                .primary_key(
                    Index::create()
                        .col(a("space_id"))
                        .col(a("conversation_key"))
                        .col(a("revision")),
                )
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(a("extraction_items"))
                .if_not_exists()
                .col(ColumnDef::new(a("space_id")).binary_len(16).not_null())
                .col(ColumnDef::new(a("event_id")).binary_len(16).not_null())
                .col(
                    ColumnDef::new(a("config_revision"))
                        .big_integer()
                        .not_null(),
                )
                .col(ColumnDef::new(a("state")).string_len(16).not_null())
                .col(ColumnDef::new(a("job_id")).binary_len(16).null())
                .col(dt("run_after"))
                .col(ColumnDef::new(a("last_error")).string_len(64).null())
                .col(dt("updated_at"))
                .primary_key(Index::create().col(a("space_id")).col(a("event_id")))
                .index(
                    Index::create()
                        .name("ix_extraction_job")
                        .col(a("space_id"))
                        .col(a("job_id")),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_extraction_event")
                        .from(a("extraction_items"), (a("space_id"), a("event_id")))
                        .to(a("source_events"), (a("space_id"), a("id"))),
                )
                .to_owned(),
        )
        .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "会话标题与消费进度不自动删除，请使用向前迁移".into(),
        ))
    }
}
