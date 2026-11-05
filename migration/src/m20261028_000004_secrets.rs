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
                    .table(Alias::new("secrets"))
                    .character_set("utf8mb4")
                    .collate("utf8mb4_bin")
                    .col(
                        ColumnDef::new(Alias::new("space_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("memory_id"))
                            .binary_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("format_version"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("payload_revision"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("key_id"))
                            .string_len(64)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("nonce"))
                            .binary_len(12)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("ciphertext")).binary())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .custom(Alias::new("DATETIME(6)"))
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .custom(Alias::new("DATETIME(6)"))
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("space_id"))
                            .col(Alias::new("memory_id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_secret_memory")
                            .from(
                                Alias::new("secrets"),
                                (Alias::new("space_id"), Alias::new("memory_id")),
                            )
                            .to(
                                Alias::new("memories"),
                                (Alias::new("space_id"), Alias::new("id")),
                            ),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // 自动回退会永久删除密文；恢复旧版本也必须保留此表及根私钥。
        Err(DbErr::Custom(
            "Secret 迁移不支持自动回退；请保留密文表和根私钥，先完成明确的数据迁移".into(),
        ))
    }
}
