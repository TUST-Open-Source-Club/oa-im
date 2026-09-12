//! im schema 迁移（含 outbox 表，事件投递用）。

use sea_orm_migration::prelude::*;

/// 初始化迁移。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("CREATE SCHEMA IF NOT EXISTS im")
            .await?;
        // outbox：与业务事务同库写入，由后台任务投递到 Redis Streams
        manager
            .get_connection()
            .execute_unprepared(club_bus::outbox::DDL)
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Conversations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Conversations::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Conversations::Type)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Conversations::Name).string_len(64))
                    .col(ColumnDef::new(Conversations::OwnerId).uuid())
                    .col(ColumnDef::new(Conversations::Notice).text())
                    .col(
                        ColumnDef::new(Conversations::NextSeq)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Conversations::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Conversations::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ConversationMembers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ConversationMembers::ConversationId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ConversationMembers::UserId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ConversationMembers::Role)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ConversationMembers::LastReadSeq)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(ConversationMembers::Muted)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(ConversationMembers::Pinned)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(ConversationMembers::JoinedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ConversationMembers::LeftAt).timestamp_with_time_zone())
                    .primary_key(
                        Index::create()
                            .col(ConversationMembers::ConversationId)
                            .col(ConversationMembers::UserId),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Messages::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Messages::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Messages::ConversationId).uuid().not_null())
                    .col(ColumnDef::new(Messages::Seq).big_integer().not_null())
                    .col(ColumnDef::new(Messages::SenderId).uuid())
                    .col(ColumnDef::new(Messages::Type).string_len(16).not_null())
                    .col(ColumnDef::new(Messages::Content).json_binary().not_null())
                    .col(ColumnDef::new(Messages::ReplyToId).uuid())
                    .col(ColumnDef::new(Messages::ClientMsgId).string_len(64))
                    .col(ColumnDef::new(Messages::Status).string_len(16).not_null())
                    .col(ColumnDef::new(Messages::RecalledAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(Messages::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ux_messages_conv_seq")
                    .table(Messages::Table)
                    .col(Messages::ConversationId)
                    .col(Messages::Seq)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ux_messages_client_id")
                    .table(Messages::Table)
                    .col(Messages::ConversationId)
                    .col(Messages::SenderId)
                    .col(Messages::ClientMsgId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_messages_conv_created")
                    .table(Messages::Table)
                    .col(Messages::ConversationId)
                    .col(Messages::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Messages::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ConversationMembers::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(Conversations::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// 会话表标识符。
#[derive(DeriveIden)]
pub enum Conversations {
    /// 表。
    Table,
    /// id。
    Id,
    /// type。
    Type,
    /// name。
    Name,
    /// owner_id。
    OwnerId,
    /// notice。
    Notice,
    /// next_seq。
    NextSeq,
    /// created_at。
    CreatedAt,
    /// updated_at。
    UpdatedAt,
}

/// 会话成员表标识符。
#[derive(DeriveIden)]
pub enum ConversationMembers {
    /// 表。
    Table,
    /// conversation_id。
    ConversationId,
    /// user_id。
    UserId,
    /// role。
    Role,
    /// last_read_seq。
    LastReadSeq,
    /// muted。
    Muted,
    /// pinned。
    Pinned,
    /// joined_at。
    JoinedAt,
    /// left_at。
    LeftAt,
}

/// 消息表标识符。
#[derive(DeriveIden)]
pub enum Messages {
    /// 表。
    Table,
    /// id。
    Id,
    /// conversation_id。
    ConversationId,
    /// seq。
    Seq,
    /// sender_id。
    SenderId,
    /// type。
    Type,
    /// content。
    Content,
    /// reply_to_id。
    ReplyToId,
    /// client_msg_id。
    ClientMsgId,
    /// status。
    Status,
    /// recalled_at。
    RecalledAt,
    /// created_at。
    CreatedAt,
}

/// 迁移入口。
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Migration)]
    }
}
