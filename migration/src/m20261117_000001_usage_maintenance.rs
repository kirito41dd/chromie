use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261117_000001_usage_maintenance"
    }
}
fn a(s: &str) -> Alias {
    Alias::new(s)
}
fn dt(s: &str, optional: bool) -> ColumnDef {
    let mut c = ColumnDef::new(a(s));
    c.custom(a("DATETIME(6)"));
    if optional {
        c.null();
    } else {
        c.not_null();
    }
    c
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(
            Table::alter()
                .table(a("recall_items"))
                .add_column(ColumnDef::new(a("use_outcome")).string_len(32).null())
                .add_column(ColumnDef::new(a("use_reason")).text().null())
                .add_column(dt("used_at", true))
                .to_owned(),
        )
        .await?;
        m.alter_table(
            Table::alter()
                .table(a("organization_settings"))
                .add_column(
                    ColumnDef::new(a("maintenance_enabled"))
                        .boolean()
                        .not_null()
                        .default(false),
                )
                .add_column(
                    ColumnDef::new(a("maintenance_interval_hours"))
                        .unsigned()
                        .not_null()
                        .default(24),
                )
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(a("maintenance_items"))
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
                .col(dt("requested_at", false))
                .col(dt("run_after", false))
                .col(dt("completed_at", true))
                .col(dt("updated_at", false))
                .col(ColumnDef::new(a("last_error")).string_len(64).null())
                .primary_key(Index::create().col(a("space_id")).col(a("memory_id")))
                .index(
                    Index::create()
                        .name("ix_maintenance_due")
                        .col(a("space_id"))
                        .col(a("state"))
                        .col(a("run_after")),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_maintenance_memory")
                        .from(a("maintenance_items"), (a("space_id"), a("memory_id")))
                        .to(a("memories"), (a("space_id"), a("id"))),
                )
                .to_owned(),
        )
        .await?;
        Ok(())
    }
    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(a("maintenance_items")).to_owned())
            .await?;
        m.alter_table(
            Table::alter()
                .table(a("organization_settings"))
                .drop_column(a("maintenance_enabled"))
                .drop_column(a("maintenance_interval_hours"))
                .to_owned(),
        )
        .await?;
        m.alter_table(
            Table::alter()
                .table(a("recall_items"))
                .drop_column(a("use_outcome"))
                .drop_column(a("use_reason"))
                .drop_column(a("used_at"))
                .to_owned(),
        )
        .await?;
        Ok(())
    }
}
