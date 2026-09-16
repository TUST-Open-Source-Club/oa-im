//! 数据访问层：只操作 im schema。

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use serde_json::Value;
use uuid::Uuid;

use club_common::{new_id, AppError};

use crate::domain::unread_of;
use crate::entity::{conversation, conversation_member, message};

/// 将数据库错误映射为统一错误。
pub fn map_db_err(err: DbErr) -> AppError {
    AppError::internal(err)
}

/// 会话 + 当前用户成员关系。
#[derive(Debug, Clone)]
pub struct ConversationWithMember {
    /// 会话。
    pub conversation: conversation::Model,
    /// 当前用户成员记录。
    pub member: conversation_member::Model,
}

/// 创建（或复用）单聊会话。
///
/// 通过扫描自己参与的单聊会话判断是否已有对方，存在则直接返回。
pub async fn create_direct_conversation(
    db: &DatabaseConnection,
    self_id: Uuid,
    other_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(conversation::Model, bool), AppError> {
    if self_id == other_id {
        return Err(AppError::bad_request("IM_SELF_CHAT", "不能与自己创建单聊"));
    }
    // 找自己参与的单聊
    let my_direct_ids: Vec<Uuid> = conversation_member::Entity::find()
        .filter(conversation_member::Column::UserId.eq(self_id))
        .filter(conversation_member::Column::LeftAt.is_null())
        .all(db)
        .await
        .map_err(map_db_err)?
        .into_iter()
        .map(|member| member.conversation_id)
        .collect();
    if !my_direct_ids.is_empty() {
        let direct_convs = conversation::Entity::find()
            .filter(conversation::Column::Id.is_in(my_direct_ids))
            .filter(conversation::Column::Type.eq(conversation::TYPE_DIRECT))
            .all(db)
            .await
            .map_err(map_db_err)?;
        let direct_ids: Vec<Uuid> = direct_convs.iter().map(|conv| conv.id).collect();
        if !direct_ids.is_empty() {
            // 在候选单聊中查找对方也是活跃成员的会话
            let other_memberships = conversation_member::Entity::find()
                .filter(conversation_member::Column::ConversationId.is_in(direct_ids))
                .filter(conversation_member::Column::UserId.eq(other_id))
                .filter(conversation_member::Column::LeftAt.is_null())
                .all(db)
                .await
                .map_err(map_db_err)?;
            if let Some(membership) = other_memberships.first() {
                if let Some(existing) = direct_convs
                    .into_iter()
                    .find(|conv| conv.id == membership.conversation_id)
                {
                    return Ok((existing, false));
                }
            }
        }
    }

    let conversation = conversation::ActiveModel {
        id: Set(new_id()),
        r#type: Set(conversation::TYPE_DIRECT.to_string()),
        name: Set(None),
        owner_id: Set(None),
        notice: Set(None),
        next_seq: Set(0),
        only_admins_speak: Set(false),
        created_at: Set(now.fixed_offset()),
        updated_at: Set(now.fixed_offset()),
    }
    .insert(db)
    .await
    .map_err(map_db_err)?;

    for user_id in [self_id, other_id] {
        insert_member(
            db,
            conversation.id,
            user_id,
            conversation_member::ROLE_MEMBER,
            now,
        )
        .await?;
    }
    Ok((conversation, true))
}

/// 创建群聊。
pub async fn create_group_conversation(
    db: &DatabaseConnection,
    owner_id: Uuid,
    member_ids: &[Uuid],
    name: &str,
    now: DateTime<Utc>,
) -> Result<conversation::Model, AppError> {
    let conversation = conversation::ActiveModel {
        id: Set(new_id()),
        r#type: Set(conversation::TYPE_GROUP.to_string()),
        name: Set(Some(name.to_string())),
        owner_id: Set(Some(owner_id)),
        notice: Set(None),
        next_seq: Set(0),
        only_admins_speak: Set(false),
        created_at: Set(now.fixed_offset()),
        updated_at: Set(now.fixed_offset()),
    }
    .insert(db)
    .await
    .map_err(map_db_err)?;

    insert_member(
        db,
        conversation.id,
        owner_id,
        conversation_member::ROLE_OWNER,
        now,
    )
    .await?;
    for member_id in member_ids {
        if *member_id != owner_id {
            insert_member(
                db,
                conversation.id,
                *member_id,
                conversation_member::ROLE_MEMBER,
                now,
            )
            .await?;
        }
    }
    Ok(conversation)
}

/// 写入成员（幂等：重复加入时重置 left_at）。
pub async fn insert_member(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
    role: &str,
    now: DateTime<Utc>,
) -> Result<conversation_member::Model, AppError> {
    if let Some(existing) = conversation_member::Entity::find_by_id((conversation_id, user_id))
        .one(db)
        .await
        .map_err(map_db_err)?
    {
        let mut active: conversation_member::ActiveModel = existing.into();
        active.left_at = Set(None);
        active.role = Set(role.to_string());
        return active.update(db).await.map_err(map_db_err);
    }
    conversation_member::ActiveModel {
        conversation_id: Set(conversation_id),
        user_id: Set(user_id),
        role: Set(role.to_string()),
        last_read_seq: Set(0),
        muted_until: Set(None),
        can_speak: Set(true),
        muted: Set(false),
        pinned: Set(false),
        joined_at: Set(now.fixed_offset()),
        left_at: Set(None),
    }
    .insert(db)
    .await
    .map_err(map_db_err)
}

/// 查询活跃成员记录。
pub async fn find_member(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
) -> Result<Option<conversation_member::Model>, AppError> {
    conversation_member::Entity::find_by_id((conversation_id, user_id))
        .filter(conversation_member::Column::LeftAt.is_null())
        .one(db)
        .await
        .map_err(map_db_err)
}

/// 校验访问并返回会话 + 成员。
pub async fn get_conversation_for_member(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
) -> Result<ConversationWithMember, AppError> {
    let member = find_member(db, conversation_id, user_id)
        .await?
        .ok_or_else(|| AppError::forbidden("IM_NOT_MEMBER", "你不是该会话成员"))?;
    let conversation = conversation::Entity::find_by_id(conversation_id)
        .one(db)
        .await
        .map_err(map_db_err)?
        .ok_or_else(|| AppError::not_found("IM_CONVERSATION_NOT_FOUND", "会话不存在"))?;
    Ok(ConversationWithMember {
        conversation,
        member,
    })
}

/// 列出用户参与的全部会话。
pub async fn list_conversations(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Vec<ConversationWithMember>, AppError> {
    let members = conversation_member::Entity::find()
        .filter(conversation_member::Column::UserId.eq(user_id))
        .filter(conversation_member::Column::LeftAt.is_null())
        .all(db)
        .await
        .map_err(map_db_err)?;
    let mut result = Vec::with_capacity(members.len());
    for member in members {
        if let Some(conversation) = conversation::Entity::find_by_id(member.conversation_id)
            .one(db)
            .await
            .map_err(map_db_err)?
        {
            result.push(ConversationWithMember {
                conversation,
                member,
            });
        }
    }
    result.sort_by_key(|row| std::cmp::Reverse(row.conversation.updated_at));
    Ok(result)
}

/// 列出会话成员（活跃）。
pub async fn list_members(
    db: &DatabaseConnection,
    conversation_id: Uuid,
) -> Result<Vec<conversation_member::Model>, AppError> {
    conversation_member::Entity::find()
        .filter(conversation_member::Column::ConversationId.eq(conversation_id))
        .filter(conversation_member::Column::LeftAt.is_null())
        .all(db)
        .await
        .map_err(map_db_err)
}

/// 将成员移出会话。
pub async fn remove_member(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    let result = conversation_member::Entity::update_many()
        .col_expr(
            conversation_member::Column::LeftAt,
            Expr::value(now.fixed_offset()),
        )
        .filter(conversation_member::Column::ConversationId.eq(conversation_id))
        .filter(conversation_member::Column::UserId.eq(user_id))
        .filter(conversation_member::Column::LeftAt.is_null())
        .exec(db)
        .await
        .map_err(map_db_err)?;
    Ok(result.rows_affected > 0)
}

/// 原子分配会话内下一个消息序号并返回。
pub async fn allocate_seq(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    now: DateTime<Utc>,
) -> Result<i64, AppError> {
    let statement = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE conversations SET next_seq = next_seq + 1, updated_at = $2 WHERE id = $1 RETURNING next_seq",
        vec![conversation_id.into(), now.fixed_offset().into()],
    );
    let row = db
        .query_one_raw(statement)
        .await
        .map_err(map_db_err)?
        .ok_or_else(|| AppError::not_found("IM_CONVERSATION_NOT_FOUND", "会话不存在"))?;
    row.try_get_by_index(0).map_err(map_db_err)
}

/// 发送消息的输入。
#[derive(Debug, Clone)]
pub struct NewMessage {
    /// 会话。
    pub conversation_id: Uuid,
    /// 发送者。
    pub sender_id: Uuid,
    /// 类型。
    pub kind: String,
    /// 内容。
    pub content: Value,
    /// 引用。
    pub reply_to_id: Option<Uuid>,
    /// 客户端幂等 ID。
    pub client_msg_id: Option<String>,
}

/// 插入消息（clientMsgId 幂等：重复发送返回既有消息与 `false`）。
pub async fn insert_message(
    db: &DatabaseConnection,
    new: NewMessage,
    now: DateTime<Utc>,
) -> Result<(message::Model, bool), AppError> {
    if let Some(client_msg_id) = new.client_msg_id.as_deref() {
        if let Some(existing) = message::Entity::find()
            .filter(message::Column::ConversationId.eq(new.conversation_id))
            .filter(message::Column::SenderId.eq(new.sender_id))
            .filter(message::Column::ClientMsgId.eq(client_msg_id))
            .one(db)
            .await
            .map_err(map_db_err)?
        {
            return Ok((existing, false));
        }
    }
    let seq = allocate_seq(db, new.conversation_id, now).await?;
    let model = message::ActiveModel {
        id: Set(new_id()),
        conversation_id: Set(new.conversation_id),
        seq: Set(seq),
        sender_id: Set(Some(new.sender_id)),
        r#type: Set(new.kind),
        content: Set(new.content),
        reply_to_id: Set(new.reply_to_id),
        client_msg_id: Set(new.client_msg_id),
        status: Set("normal".to_string()),
        recalled_at: Set(None),
        created_at: Set(now.fixed_offset()),
    }
    .insert(db)
    .await
    .map_err(map_db_err)?;
    Ok((model, true))
}

/// 按序号倒序分页拉取历史消息（返回按 seq 升序）。
pub async fn list_messages(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    before_seq: Option<i64>,
    limit: u64,
) -> Result<Vec<message::Model>, AppError> {
    let mut select = message::Entity::find()
        .filter(message::Column::ConversationId.eq(conversation_id))
        .order_by_desc(message::Column::Seq)
        .limit(limit);
    if let Some(before_seq) = before_seq {
        select = select.filter(message::Column::Seq.lt(before_seq));
    }
    let mut items = select.all(db).await.map_err(map_db_err)?;
    items.reverse();
    Ok(items)
}

/// 更新已读位点（只前进不回退）。
pub async fn update_last_read(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
    seq: i64,
) -> Result<conversation_member::Model, AppError> {
    let member = find_member(db, conversation_id, user_id)
        .await?
        .ok_or_else(|| AppError::forbidden("IM_NOT_MEMBER", "你不是该会话成员"))?;
    if seq <= member.last_read_seq {
        return Ok(member);
    }
    let mut active: conversation_member::ActiveModel = member.into();
    active.last_read_seq = Set(seq);
    active.update(db).await.map_err(map_db_err)
}

/// 未读数。
pub fn unread_for(conversation: &conversation::Model, member: &conversation_member::Model) -> i64 {
    unread_of(conversation.next_seq, member.last_read_seq)
}

/// 标记消息已撤回。
pub async fn mark_recalled(
    db: &DatabaseConnection,
    model: &message::Model,
    now: DateTime<Utc>,
) -> Result<message::Model, AppError> {
    let mut active: message::ActiveModel = model.clone().into();
    active.status = Set("recalled".to_string());
    active.recalled_at = Set(Some(now.fixed_offset()));
    active.update(db).await.map_err(map_db_err)
}

/// 查询某会话某序号的消息是否存在（引用校验用）。
pub async fn find_message(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<Option<message::Model>, AppError> {
    message::Entity::find()
        .filter(message::Column::ConversationId.eq(conversation_id))
        .filter(message::Column::Id.eq(message_id))
        .one(db)
        .await
        .map_err(map_db_err)
}

/// 条件查询辅助（保留：按发送者统计等）。
#[allow(dead_code)]
pub fn sender_condition(sender_id: Uuid) -> Condition {
    Condition::all().add(message::Column::SenderId.eq(sender_id))
}

/// 设置成员禁言到期时间。
pub async fn set_member_mute(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
    muted_until: Option<DateTime<Utc>>,
) -> Result<conversation_member::Model, AppError> {
    let member = find_member(db, conversation_id, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("IM_MEMBER_NOT_FOUND", "成员不存在"))?;
    let mut active: conversation_member::ActiveModel = member.into();
    active.muted_until = Set(muted_until.map(|value| value.fixed_offset()));
    active.update(db).await.map_err(map_db_err)
}

/// 设置成员发言白名单。
pub async fn set_member_speak(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
    can_speak: bool,
) -> Result<conversation_member::Model, AppError> {
    let member = find_member(db, conversation_id, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("IM_MEMBER_NOT_FOUND", "成员不存在"))?;
    let mut active: conversation_member::ActiveModel = member.into();
    active.can_speak = Set(can_speak);
    active.update(db).await.map_err(map_db_err)
}

/// 设置成员角色（owner/admin/member）。
pub async fn set_member_role(
    db: &DatabaseConnection,
    conversation_id: Uuid,
    user_id: Uuid,
    role: &str,
) -> Result<conversation_member::Model, AppError> {
    let member = find_member(db, conversation_id, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("IM_MEMBER_NOT_FOUND", "成员不存在"))?;
    let mut active: conversation_member::ActiveModel = member.into();
    active.role = Set(role.to_string());
    active.update(db).await.map_err(map_db_err)
}

/// 更新群设置（仅管理员发言/公告）。
pub async fn update_group_settings(
    db: &DatabaseConnection,
    conversation: &conversation::Model,
    only_admins_speak: Option<bool>,
    notice: Option<Option<String>>,
    now: DateTime<Utc>,
) -> Result<conversation::Model, AppError> {
    let mut active: conversation::ActiveModel = conversation.clone().into();
    if let Some(value) = only_admins_speak {
        active.only_admins_speak = Set(value);
    }
    if let Some(value) = notice {
        active.notice = Set(value);
    }
    active.updated_at = Set(now.fixed_offset());
    active.update(db).await.map_err(map_db_err)
}

/// 转让群主：原群主降为管理员，新群主升为 owner。
pub async fn transfer_owner(
    db: &DatabaseConnection,
    conversation: &conversation::Model,
    new_owner: Uuid,
    now: DateTime<Utc>,
) -> Result<conversation::Model, AppError> {
    if let Some(old_owner) = conversation.owner_id {
        if old_owner == new_owner {
            return Ok(conversation.clone());
        }
        set_member_role(db, conversation.id, old_owner, "admin").await?;
    }
    set_member_role(db, conversation.id, new_owner, "owner").await?;
    let mut active: conversation::ActiveModel = conversation.clone().into();
    active.owner_id = Set(Some(new_owner));
    active.updated_at = Set(now.fixed_offset());
    active.update(db).await.map_err(map_db_err)
}

/// 解散群聊：删除消息与成员关系，然后删除会话。
pub async fn dissolve_conversation(
    db: &DatabaseConnection,
    conversation_id: Uuid,
) -> Result<(), AppError> {
    use crate::entity::message;
    message::Entity::delete_many()
        .filter(message::Column::ConversationId.eq(conversation_id))
        .exec(db)
        .await
        .map_err(map_db_err)?;
    conversation_member::Entity::delete_many()
        .filter(conversation_member::Column::ConversationId.eq(conversation_id))
        .exec(db)
        .await
        .map_err(map_db_err)?;
    conversation::Entity::delete_by_id(conversation_id)
        .exec(db)
        .await
        .map_err(map_db_err)?;
    Ok(())
}
