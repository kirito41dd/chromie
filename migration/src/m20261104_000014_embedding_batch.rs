use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("embedding_generations", "batch_size")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("embedding_generations"))
                        .add_column(ColumnDef::new(Alias::new("batch_size")).unsigned().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom("批量设置不能自动删除；请使用向前迁移".into()))
    }
}
