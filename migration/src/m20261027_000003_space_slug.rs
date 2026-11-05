use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn suffix(mut index: usize) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push((b'a' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    letters.into_iter().rev().collect()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // MySQL 的 DDL 非原子；中断后重试不重复建列，也不覆盖已经分配的标识。
        if !manager.has_column("spaces", "slug").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("spaces"))
                        .add_column(ColumnDef::new(Alias::new("slug")).string_len(32).null())
                        .to_owned(),
                )
                .await?;
        }
        let query = Query::select()
            .columns([Alias::new("id"), Alias::new("slug")])
            .from(Alias::new("spaces"))
            .order_by(Alias::new("id"), Order::Asc)
            .to_owned();
        let db = manager.get_connection();
        let rows = db.query_all(&query).await?;
        let mut used = std::collections::HashSet::new();
        for row in &rows {
            if let Some(slug) = row.try_get::<Option<String>>("", "slug")? {
                used.insert(slug);
            }
        }
        let mut next = 0;
        for row in rows {
            if row.try_get::<Option<String>>("", "slug")?.is_some() {
                continue;
            }
            let slug = loop {
                let candidate = format!("space-{}", suffix(next));
                next += 1;
                if used.insert(candidate.clone()) {
                    break candidate;
                }
            };
            let id: Vec<u8> = row.try_get("", "id")?;
            db.execute(
                &Query::update()
                    .table(Alias::new("spaces"))
                    .value(Alias::new("slug"), slug)
                    .and_where(Expr::col(Alias::new("id")).eq(id))
                    .to_owned(),
            )
            .await?;
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("spaces"))
                    .modify_column(ColumnDef::new(Alias::new("slug")).string_len(32).not_null())
                    .to_owned(),
            )
            .await?;
        if !manager.has_index("spaces", "uq_spaces_slug").await? {
            manager
                .create_index(
                    Index::create()
                        .name("uq_spaces_slug")
                        .table(Alias::new("spaces"))
                        .col(Alias::new("slug"))
                        .unique()
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("uq_spaces_slug")
                    .table(Alias::new("spaces"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("spaces"))
                    .drop_column(Alias::new("slug"))
                    .to_owned(),
            )
            .await
    }
}
