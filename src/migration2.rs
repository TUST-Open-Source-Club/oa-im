//! im schema v2：群管理（成员禁言/发言白名单、仅管理员发言）。

use sea_orm_migration::prelude::*;

/// v2 迁移。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE conversation_members ADD COLUMN muted_until timestamptz",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE conversation_members ADD COLUMN can_speak boolean NOT NULL DEFAULT true",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE conversations ADD COLUMN only_admins_speak boolean NOT NULL DEFAULT false",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
