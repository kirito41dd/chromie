use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("secrets", "contract").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("secrets"))
                        .add_column(ColumnDef::new(Alias::new("contract")).json().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "禁止自动删除已保存的 Secret 契约，请使用显式迁移".into(),
        ))
    }
}
