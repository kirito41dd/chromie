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
                    .table(Alias::new("model_usage_daily"))
                    .col(ColumnDef::new(Alias::new("usage_date")).date().not_null())
                    .col(
                        ColumnDef::new(Alias::new("model_key"))
                            .binary_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("model_name"))
                            .string_len(256)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("request_count"))
                            .big_unsigned()
                            .not_null()
                            .default(0u64),
                    )
                    .col(
                        ColumnDef::new(Alias::new("total_tokens"))
                            .big_unsigned()
                            .not_null()
                            .default(0u64),
                    )
                    .col(
                        ColumnDef::new(Alias::new("failed_count"))
                            .big_unsigned()
                            .not_null()
                            .default(0u64),
                    )
                    .col(
                        ColumnDef::new(Alias::new("unknown_usage_count"))
                            .big_unsigned()
                            .not_null()
                            .default(0u64),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .custom(Alias::new("DATETIME(6)"))
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("usage_date"))
                            .col(Alias::new("model_key")),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "模型用量历史不可自动删除，请显式归档迁移".into(),
        ))
    }
}
