//! 消息领域服务：HTTP 与 WebSocket 共用的发送/已读/撤回逻辑。

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use club_common::{AppError, FieldError};

use crate::domain;
use crate::entity::{conversation, conversation_member, message};
use crate::repo;
use crate::state::SharedState;

/// 撤回时限（秒）：发送后 2 分钟内本人可撤回。
pub const RECALL_WINDOW_SECONDS: i64 = 120;

/// 发送消息（幂等），返回消息与是否新建；新建时写 outbox 事件。
pub async fn create_message(
    state: &SharedState,
    sender_id: Uuid,
    conversation_id: Uuid,
    kind: &str,
    content: &Value,
    reply_to_id: Option<Uuid>,
    client_msg_id: Option<String>,
) -> Result<(message::Model, bool), AppError> {
    domain::validate_message(kind, content)?;
    let ctx = repo::get_conversation_for_member(&state.db, conversation_id, sender_id).await?;
    if let Some(reply_to_id) = reply_to_id {
        if repo::find_message(&state.db, conversation_id, reply_to_id)
            .await?
            .is_none()
        {
            return Err(AppError::unprocessable(
                "IM_VALIDATION",
                "引用的消息不存在",
                vec![FieldError::new("replyToId", "消息不存在")],
            ));
        }
    }

    let now = state.now();
    let (model, created) = repo::insert_message(
        &state.db,
        repo::NewMessage {
            conversation_id,
            sender_id,
            kind: kind.to_string(),
            content: content.clone(),
            reply_to_id,
            client_msg_id,
        },
        now,
    )
    .await?;

    if created && state.bus.is_some() {
        let members = repo::list_members(&state.db, conversation_id).await?;
        let targets: Vec<String> = members
            .iter()
            .filter(|member| member.user_id != sender_id)
            .map(|member| member.user_id.to_string())
            .collect();
        let title = match ctx.conversation.r#type.as_str() {
            conversation::TYPE_GROUP => ctx
                .conversation
                .name
                .clone()
                .unwrap_or_else(|| "群聊".to_string()),
            _ => "新消息".to_string(),
        };
        let payload = json!({
            "id": model.id,
            "type": "im.message.created",
            "actorId": sender_id,
            "targetUsers": targets,
            "resource": { "type": "im", "id": conversation_id, "url": format!("/im/{conversation_id}") },
            "title": title,
            "body": domain::preview_of(&model.r#type, &model.content),
            "priority": "high"
        });
        if let Err(err) =
            club_bus::outbox::enqueue(&state.db, "im.message.created", &payload, now).await
        {
            tracing::warn!(error = %err, "消息事件写入 outbox 失败");
        }
    }
    Ok((model, created))
}

/// 更新已读位点并返回成员记录。
pub async fn apply_read(
    state: &SharedState,
    user_id: Uuid,
    conversation_id: Uuid,
    seq: i64,
) -> Result<conversation_member::Model, AppError> {
    repo::update_last_read(&state.db, conversation_id, user_id, seq).await
}

/// 撤回消息（仅发送者、限时 2 分钟；重复撤回幂等返回）。
pub async fn recall_message(
    state: &SharedState,
    user_id: Uuid,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<message::Model, AppError> {
    repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    let model = repo::find_message(&state.db, conversation_id, message_id)
        .await?
        .ok_or_else(|| AppError::not_found("IM_MESSAGE_NOT_FOUND", "消息不存在"))?;
    if model.sender_id != Some(user_id) {
        return Err(AppError::forbidden("IM_FORBIDDEN", "只能撤回自己的消息"));
    }
    if model.status == "recalled" {
        // 本人重复撤回：幂等返回
        return Ok(model);
    }
    let now = Utc::now();
    let age = now.signed_duration_since(model.created_at.with_timezone(&Utc));
    if age.num_seconds() > RECALL_WINDOW_SECONDS {
        return Err(AppError::forbidden(
            "IM_RECALL_EXPIRED",
            "超过 2 分钟的消息不能撤回",
        ));
    }
    repo::mark_recalled(&state.db, &model, now).await
}

/// 已读回执：统计已读到某消息的成员。
pub async fn read_receipts(
    state: &SharedState,
    user_id: Uuid,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<Vec<Value>, AppError> {
    repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    let target = repo::find_message(&state.db, conversation_id, message_id)
        .await?
        .ok_or_else(|| AppError::not_found("IM_MESSAGE_NOT_FOUND", "消息不存在"))?;
    let members = repo::list_members(&state.db, conversation_id).await?;
    Ok(members
        .iter()
        .filter(|member| member.user_id != user_id)
        .map(|member| {
            json!({
                "userId": member.user_id,
                "read": member.last_read_seq >= target.seq,
                "lastReadSeq": member.last_read_seq
            })
        })
        .collect())
}
