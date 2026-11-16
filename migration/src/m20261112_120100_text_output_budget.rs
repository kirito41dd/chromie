use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261112_120100_text_output_budget"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("text_model_settings", "max_output_tokens")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("text_model_settings"))
                        .add_column(
                            ColumnDef::new(Alias::new("max_output_tokens"))
                                .unsigned()
                                .not_null()
                                .default(4096u32),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "模型输出预算不自动删除，请使用向前迁移".into(),
        ))
    }
}
