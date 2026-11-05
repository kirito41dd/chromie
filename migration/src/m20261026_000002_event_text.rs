use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // sea-schema 0.18.1 对 LONGTEXT 生成 ignore 字段；MVP 使用可反向生成的 TEXT。
        // 调整前禁止缩窄已有超长数据，避免升级时静默截断。
        let query = Query::select()
            .expr(Func::count(Expr::val(1)))
            .from(Alias::new("source_events"))
            .and_where(
                Expr::expr(
                    Func::cust(Alias::new("OCTET_LENGTH")).arg(Expr::col(Alias::new("body"))),
                )
                .gt(60000),
            )
            .to_owned();
        let result = manager.get_connection().query_one(&query).await?;
        if let Some(row) = result {
            let count: i64 = row.try_get_by_index(0)?;
            if count != 0 {
                return Err(DbErr::Custom("已有超长事件，拒绝缩窄正文列".into()));
            }
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("source_events"))
                    .modify_column(ColumnDef::new(Alias::new("body")).text().null())
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("source_events"))
                    .modify_column(
                        ColumnDef::new(Alias::new("body"))
                            .custom(Alias::new("LONGTEXT"))
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }
}
