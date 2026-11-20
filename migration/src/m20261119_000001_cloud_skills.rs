use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn id(name: &str) -> ColumnDef {
    ColumnDef::new(Alias::new(name))
        .binary_len(16)
        .not_null()
        .to_owned()
}

fn time(name: &str) -> ColumnDef {
    ColumnDef::new(Alias::new(name))
        .custom(Alias::new("DATETIME(6)"))
        .not_null()
        .to_owned()
}

fn key(names: &[&str]) -> IndexCreateStatement {
    let mut index = Index::create();
    for name in names {
        index.col(Alias::new(*name));
    }
    index
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // MySQL 的 DDL 各自提交，允许在首次建表中断后继续。
        m.create_table(
            Table::create()
                .table("skills")
                .if_not_exists()
                .col(id("space_id"))
                .col(id("id"))
                .col(ColumnDef::new("name").string_len(64).not_null())
                .col(ColumnDef::new("description").text().not_null())
                .col(ColumnDef::new("current_revision").integer().not_null())
                .col(ColumnDef::new("usage_counts").json().not_null())
                .primary_key(&mut key(&["space_id", "id"]))
                .index(key(&["space_id", "name"]).name("uk_skill_name").unique())
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table("skill_revisions")
                .if_not_exists()
                .col(id("space_id"))
                .col(id("skill_id"))
                .col(ColumnDef::new("revision").integer().not_null())
                .col(ColumnDef::new("base_revision").integer().null())
                .col(id("agent_id"))
                .col(ColumnDef::new("frontmatter").json().not_null())
                .col(ColumnDef::new("reason").text().not_null())
                .col(ColumnDef::new("usage_counts").json().not_null())
                .col(ColumnDef::new("retained").boolean().not_null())
                .col(time("created_at"))
                .primary_key(&mut key(&["space_id", "skill_id", "revision"]))
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table("stored_objects")
                .if_not_exists()
                .col(id("space_id"))
                .col(id("id"))
                .col(id("agent_id"))
                .col(ColumnDef::new("digest").string_len(71).not_null())
                .col(ColumnDef::new("canonical_digest").string_len(71).null())
                .col(ColumnDef::new("size").integer().not_null())
                .col(ColumnDef::new("state").string_len(16).not_null())
                .col(time("created_at"))
                .primary_key(&mut key(&["space_id", "id"]))
                .index(
                    key(&["space_id", "canonical_digest"])
                        .name("uk_skill_object_digest")
                        .unique(),
                )
                .index(key(&["state", "created_at"]).name("ix_skill_object_gc"))
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table("skill_revision_files")
                .if_not_exists()
                .col(id("space_id"))
                .col(id("skill_id"))
                .col(ColumnDef::new("revision").integer().not_null())
                .col(
                    ColumnDef::new("path")
                        .custom(Alias::new(
                            "VARCHAR(512) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin",
                        ))
                        .not_null(),
                )
                .col(id("object_id"))
                .col(ColumnDef::new("media_type").string_len(64).not_null())
                .primary_key(&mut key(&["space_id", "skill_id", "revision", "path"]))
                .index(key(&["space_id", "object_id"]).name("ix_skill_file_object"))
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table("skill_usage_reports")
                .if_not_exists()
                .col(id("space_id"))
                .col(id("request_id"))
                .col(id("skill_id"))
                .col(id("agent_id"))
                .col(ColumnDef::new("revision").integer().null())
                .col(ColumnDef::new("outcome").string_len(24).not_null())
                .col(ColumnDef::new("reason").text().null())
                .col(time("created_at"))
                .primary_key(&mut key(&["space_id", "request_id"]))
                .index(key(&["space_id", "created_at"]).name("ix_skill_report_age"))
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Skill 包及使用记录须先备份，禁止自动删除".into(),
        ))
    }
}
