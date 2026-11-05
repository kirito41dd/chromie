use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut table = Table::create();
        table
            .table(Alias::new("secret_access_audit"))
            .if_not_exists()
            .character_set("utf8mb4")
            .collate("utf8mb4_bin");
        for name in ["space_id", "id", "memory_id"] {
            table.col(ColumnDef::new(Alias::new(name)).binary_len(16).not_null());
        }
        table
            .col(ColumnDef::new(Alias::new("agent_id")).binary_len(16).null())
            .col(ColumnDef::new(Alias::new("revision")).integer().not_null())
            .col(ColumnDef::new(Alias::new("purpose")).string_len(256).null())
            .col(
                ColumnDef::new(Alias::new("outcome"))
                    .string_len(32)
                    .not_null(),
            )
            .col(ColumnDef::new(Alias::new("exit_code")).integer().null())
            .col(
                ColumnDef::new(Alias::new("delivered_at"))
                    .custom(Alias::new("DATETIME(6)"))
                    .not_null(),
            )
            .col(
                ColumnDef::new(Alias::new("reported_at"))
                    .custom(Alias::new("DATETIME(6)"))
                    .null(),
            )
            .primary_key(
                Index::create()
                    .col(Alias::new("space_id"))
                    .col(Alias::new("id")),
            )
            .foreign_key(
                ForeignKey::create()
                    .name("fk_secret_audit_memory")
                    .from(
                        Alias::new("secret_access_audit"),
                        (Alias::new("space_id"), Alias::new("memory_id")),
                    )
                    .to(
                        Alias::new("memories"),
                        (Alias::new("space_id"), Alias::new("id")),
                    ),
            )
            .foreign_key(
                ForeignKey::create()
                    .name("fk_secret_audit_agent")
                    .from(
                        Alias::new("secret_access_audit"),
                        (Alias::new("space_id"), Alias::new("agent_id")),
                    )
                    .to(
                        Alias::new("agent_connections"),
                        (Alias::new("space_id"), Alias::new("id")),
                    ),
            );
        manager.create_table(table.to_owned()).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom("取用审计不可自动删除，请显式归档迁移".into()))
    }
}
