use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(Alias::new("model_limits"))
                    .col(
                        ColumnDef::new(Alias::new("model_key"))
                            .binary_len(32)
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("model_name"))
                            .string_len(256)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("max_concurrency"))
                            .unsigned()
                            .not_null()
                            .default(4u32),
                    )
                    .col(
                        ColumnDef::new(Alias::new("revision"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .custom(Alias::new("DATETIME(6)"))
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "模型并发配置不可自动删除，请显式归档迁移".into(),
        ))
    }
}
