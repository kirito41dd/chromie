use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // binary() 在 MySQL 上是 BINARY(1)，密文必须显式使用 BLOB，不能截断认证标签。
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("secrets"))
                    .modify_column(ColumnDef::new(Alias::new("ciphertext")).blob().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "禁止缩窄或删除 Secret 密文；请保留数据库与原私钥，使用显式迁移".into(),
        ))
    }
}
