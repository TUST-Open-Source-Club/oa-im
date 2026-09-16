//! HTTP 路由。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use club_auth_sdk::AuthUser;
use club_common::{AppError, FieldError};

use crate::entity::{conversation, conversation_member, message};
use crate::repo;
use crate::state::SharedState;

/// 存活检查。
pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// 就绪检查。
pub async fn readyz(State(state): State<SharedState>) -> Json<Value> {
    match state.db.ping().await {
        Ok(_) => Json(json!({ "status": "ready", "database": "ok" })),
        Err(err) => {
            tracing::error!(error = %err, "数据库就绪检查失败");
            Json(json!({ "status": "degraded", "database": "error" }))
        }
    }
}

/// 解析登录用户 ID。
fn user_id_of(auth: &AuthUser) -> Result<Uuid, AppError> {
    auth.claims()
        .sub
        .parse()
        .map_err(|_| AppError::unauthorized("AUTH_INVALID_TOKEN", "访问令牌无效"))
}

/// 消息 DTO。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDto {
    /// ID。
    pub id: String,
    /// 会话内序号。
    pub seq: i64,
    /// 发送者。
    pub sender_id: Option<String>,
    /// 类型。
    #[serde(rename = "type")]
    pub kind: String,
    /// 内容。
    pub content: Value,
    /// 引用消息。
    pub reply_to_id: Option<String>,
    /// 状态。
    pub status: String,
    /// 时间。
    pub created_at: DateTime<FixedOffset>,
}

impl From<&message::Model> for MessageDto {
    fn from(model: &message::Model) -> Self {
        Self {
            id: model.id.to_string(),
            seq: model.seq,
            sender_id: model.sender_id.map(|id| id.to_string()),
            kind: model.r#type.clone(),
            content: model.content.clone(),
            reply_to_id: model.reply_to_id.map(|id| id.to_string()),
            status: model.status.clone(),
            created_at: model.created_at,
        }
    }
}

/// 会话 DTO。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationDto {
    /// ID。
    pub id: String,
    /// 类型。
    #[serde(rename = "type")]
    pub kind: String,
    /// 名称。
    pub name: Option<String>,
    /// 公告。
    pub notice: Option<String>,
    /// 是否仅管理员可发言。
    pub only_admins_speak: bool,
    /// 未读数。
    pub unread: i64,
    /// 是否免打扰。
    pub muted: bool,
    /// 是否置顶。
    pub pinned: bool,
    /// 成员 ID。
    pub member_ids: Vec<String>,
    /// 最近一条消息预览。
    pub last_message: Option<MessageDto>,
    /// 更新时间。
    pub updated_at: DateTime<FixedOffset>,
}

/// `GET /conversations`：会话列表。
pub async fn list_conversations(
    State(state): State<SharedState>,
    auth: AuthUser,
) -> Result<Json<Vec<ConversationDto>>, AppError> {
    let user_id = user_id_of(&auth)?;
    let rows = repo::list_conversations(&state.db, user_id).await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let members = repo::list_members(&state.db, row.conversation.id).await?;
        let last = repo::list_messages(&state.db, row.conversation.id, None, 1).await?;
        items.push(ConversationDto {
            id: row.conversation.id.to_string(),
            kind: row.conversation.r#type.clone(),
            name: row.conversation.name.clone(),
            notice: row.conversation.notice.clone(),
            only_admins_speak: row.conversation.only_admins_speak,
            unread: repo::unread_for(&row.conversation, &row.member),
            muted: row.member.muted,
            pinned: row.member.pinned,
            member_ids: members.iter().map(|m| m.user_id.to_string()).collect(),
            last_message: last.first().map(MessageDto::from),
            updated_at: row.conversation.updated_at,
        });
    }
    Ok(Json(items))
}

/// 创建会话请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConversationRequest {
    /// direct / group。
    #[serde(rename = "type")]
    pub kind: String,
    /// 其他成员。
    pub member_ids: Vec<Uuid>,
    /// 群名（群聊必填）。
    pub name: Option<String>,
}

/// `POST /conversations`：创建单聊/群聊（单聊幂等）。
pub async fn create_conversation(
    State(state): State<SharedState>,
    auth: AuthUser,
    Json(input): Json<CreateConversationRequest>,
) -> Result<(StatusCode, Json<ConversationDto>), AppError> {
    let user_id = user_id_of(&auth)?;
    let now = state.now();
    let (conversation, created) = match input.kind.as_str() {
        conversation::TYPE_DIRECT => {
            let other = input.member_ids.first().ok_or_else(|| {
                AppError::unprocessable(
                    "IM_VALIDATION",
                    "单聊需要一个对方用户",
                    vec![FieldError::new("memberIds", "不能为空")],
                )
            })?;
            repo::create_direct_conversation(&state.db, user_id, *other, now).await?
        }
        conversation::TYPE_GROUP => {
            let name = input
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty());
            let Some(name) = name else {
                return Err(AppError::unprocessable(
                    "IM_VALIDATION",
                    "群聊名称不能为空",
                    vec![FieldError::new("name", "不能为空")],
                ));
            };
            if input.member_ids.is_empty() {
                return Err(AppError::unprocessable(
                    "IM_VALIDATION",
                    "群聊至少邀请一位成员",
                    vec![FieldError::new("memberIds", "不能为空")],
                ));
            }
            let conv =
                repo::create_group_conversation(&state.db, user_id, &input.member_ids, name, now)
                    .await?;
            (conv, true)
        }
        _ => {
            return Err(AppError::unprocessable(
                "IM_VALIDATION",
                "会话类型不合法",
                vec![FieldError::new("type", "仅支持 direct / group")],
            ))
        }
    };

    let members = repo::list_members(&state.db, conversation.id).await?;
    let member = repo::find_member(&state.db, conversation.id, user_id)
        .await?
        .ok_or_else(|| AppError::internal("创建会话后成员记录缺失"))?;
    let response = ConversationDto {
        id: conversation.id.to_string(),
        kind: conversation.r#type.clone(),
        name: conversation.name.clone(),
        notice: conversation.notice.clone(),
        only_admins_speak: conversation.only_admins_speak,
        unread: repo::unread_for(&conversation, &member),
        muted: member.muted,
        pinned: member.pinned,
        member_ids: members.iter().map(|m| m.user_id.to_string()).collect(),
        last_message: None,
        updated_at: conversation.updated_at,
    };
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(response)))
}

/// 历史消息查询。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageQuery {
    /// 拉取该序号之前（不含）。
    pub before_seq: Option<i64>,
    /// 条数。
    pub limit: Option<u64>,
}

/// `GET /conversations/{id}/messages`：历史消息。
pub async fn list_messages(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
    Query(query): Query<MessageQuery>,
) -> Result<Json<Vec<MessageDto>>, AppError> {
    let user_id = user_id_of(&auth)?;
    repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let items = repo::list_messages(&state.db, conversation_id, query.before_seq, limit).await?;
    Ok(Json(items.iter().map(MessageDto::from).collect()))
}

/// 发送消息请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    /// 客户端幂等 ID。
    pub client_msg_id: Option<String>,
    /// 类型。
    #[serde(rename = "type")]
    pub kind: String,
    /// 内容。
    pub content: Value,
    /// 引用。
    pub reply_to_id: Option<Uuid>,
}

/// `POST /conversations/{id}/messages`：发送消息（HTTP 通道）。
pub async fn send_message(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
    Json(input): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<MessageDto>), AppError> {
    let user_id = user_id_of(&auth)?;
    // 群管理校验：禁言 / 仅管理员发言
    let ctx = repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    if ctx.conversation.r#type == "group" {
        if let Some(until) = ctx.member.muted_until {
            if until > state.now().fixed_offset() {
                return Err(AppError::forbidden("IM_MUTED", "你已被禁言，暂时无法发言"));
            }
        }
        let privileged = matches!(ctx.member.role.as_str(), "owner" | "admin");
        if ctx.conversation.only_admins_speak && !privileged && !ctx.member.can_speak {
            return Err(AppError::forbidden("IM_READONLY_GROUP", "当前群仅管理员可发言"));
        }
    }
    let (model, created) = crate::service::create_message(
        &state,
        user_id,
        conversation_id,
        &input.kind,
        &input.content,
        input.reply_to_id,
        input.client_msg_id,
    )
    .await?;
    if created {
        // 推送给在线的 WebSocket 客户端
        crate::realtime::broadcast_message(&state, conversation_id, &model).await;
    }
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(MessageDto::from(&model))))
}

/// 已读请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadRequest {
    /// 已读到该序号（含）。
    pub seq: i64,
}

/// `POST /conversations/{id}/read`：上报已读位点。
pub async fn mark_read(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
    Json(input): Json<ReadRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    let member = crate::service::apply_read(&state, user_id, conversation_id, input.seq).await?;
    Ok(Json(json!({ "lastReadSeq": member.last_read_seq })))
}


/// 群管理操作请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MuteRequest {
    /// 禁言到期时间（null 解除禁言）。
    pub muted_until: Option<DateTime<chrono::FixedOffset>>,
}

/// 发言白名单请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakRequest {
    /// 是否允许发言。
    pub can_speak: bool,
}

/// 群设置请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSettingsRequest {
    /// 仅管理员/白名单可发言。
    pub only_admins_speak: Option<bool>,
    /// 群公告（null 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub notice: Option<Option<String>>,
}

/// 角色请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleRequest {
    /// admin / member。
    pub role: String,
}

/// 转让群主请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRequest {
    /// 新群主。
    pub user_id: Uuid,
}

/// 三态 JSON。
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 校验当前用户为群主/管理员，返回（会话, 成员）。
async fn ensure_group_admin(
    state: &SharedState,
    conversation_id: Uuid,
    user_id: Uuid,
) -> Result<(conversation::Model, conversation_member::Model), AppError> {
    let ctx = repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    if ctx.conversation.r#type != "group" {
        return Err(AppError::unprocessable(
            "IM_VALIDATION",
            "仅群聊支持该操作",
            vec![],
        ));
    }
    if !matches!(ctx.member.role.as_str(), "owner" | "admin") {
        return Err(AppError::forbidden("IM_FORBIDDEN_GROUP", "仅群主/管理员可操作"));
    }
    Ok((ctx.conversation, ctx.member))
}

/// `POST /conversations/{id}/members/{uid}/mute`：禁言/解除禁言。
pub async fn mute_member(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path((conversation_id, target_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<MuteRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    let (conversation, operator) = ensure_group_admin(&state, conversation_id, user_id).await?;
    if operator.role == "admin" && conversation.owner_id == Some(target_id) {
        return Err(AppError::forbidden("IM_FORBIDDEN_GROUP", "不能禁言群主"));
    }
    let member = repo::set_member_mute(
        &state.db,
        conversation_id,
        target_id,
        input.muted_until.map(|value| value.with_timezone(&chrono::Utc)),
    )
    .await?;
    let event = serde_json::json!({
        "type": "member_muted",
        "payload": { "conversationId": conversation_id, "userId": target_id, "mutedUntil": member.muted_until }
    })
    .to_string();
    state.hub.send_to(target_id, &event);
    Ok(Json(json!({ "userId": target_id, "mutedUntil": member.muted_until })))
}

/// `POST /conversations/{id}/members/{uid}/speak`：发言白名单。
pub async fn set_member_speak(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path((conversation_id, target_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<SpeakRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    ensure_group_admin(&state, conversation_id, user_id).await?;
    let member = repo::set_member_speak(&state.db, conversation_id, target_id, input.can_speak).await?;
    Ok(Json(json!({ "userId": target_id, "canSpeak": member.can_speak })))
}

/// `POST /conversations/{id}/settings`：群设置。
pub async fn update_group_settings(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
    Json(input): Json<GroupSettingsRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    let (conversation, _) = ensure_group_admin(&state, conversation_id, user_id).await?;
    let updated = repo::update_group_settings(
        &state.db,
        &conversation,
        input.only_admins_speak,
        input.notice,
        state.now(),
    )
    .await?;
    Ok(Json(json!({
        "onlyAdminsSpeak": updated.only_admins_speak,
        "notice": updated.notice
    })))
}

/// `POST /conversations/{id}/members/{uid}/role`：设置/取消管理员（仅群主）。
pub async fn set_member_role(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path((conversation_id, target_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<RoleRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    let (conversation, operator) = ensure_group_admin(&state, conversation_id, user_id).await?;
    if operator.role != "owner" {
        return Err(AppError::forbidden("IM_FORBIDDEN_GROUP", "仅群主可设置管理员"));
    }
    if !["admin", "member"].contains(&input.role.as_str()) {
        return Err(AppError::unprocessable(
            "IM_VALIDATION",
            "角色不合法",
            vec![FieldError::new("role", "仅支持 admin/member")],
        ));
    }
    if conversation.owner_id == Some(target_id) {
        return Err(AppError::forbidden("IM_FORBIDDEN_GROUP", "不能修改群主角色"));
    }
    let member = repo::set_member_role(&state.db, conversation_id, target_id, &input.role).await?;
    let members = repo::list_members(&state.db, conversation_id).await?;
    let event = serde_json::json!({
        "type": "my_role_updated",
        "payload": { "conversationId": conversation_id, "role": member.role }
    })
    .to_string();
    state.hub.send_to(target_id, &event);
    let _ = members;
    Ok(Json(json!({ "userId": target_id, "role": member.role })))
}

/// `POST /conversations/{id}/transfer`：转让群主（仅群主）。
pub async fn transfer_owner(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
    Json(input): Json<TransferRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    let (conversation, operator) = ensure_group_admin(&state, conversation_id, user_id).await?;
    if operator.role != "owner" {
        return Err(AppError::forbidden("IM_FORBIDDEN_GROUP", "仅群主可转让群主"));
    }
    let updated = repo::transfer_owner(&state.db, &conversation, input.user_id, state.now()).await?;
    let event = serde_json::json!({
        "type": "owner_transferred",
        "payload": { "conversationId": conversation_id, "ownerId": input.user_id }
    })
    .to_string();
    let members = repo::list_members(&state.db, conversation_id).await?;
    for member in members {
        state.hub.send_to(member.user_id, &event);
    }
    Ok(Json(json!({ "ownerId": updated.owner_id })))
}

/// `DELETE /conversations/{id}`：解散群聊（仅群主）。
pub async fn dissolve_conversation(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let user_id = user_id_of(&auth)?;
    let (conversation, operator) = ensure_group_admin(&state, conversation_id, user_id).await?;
    if operator.role != "owner" {
        return Err(AppError::forbidden("IM_FORBIDDEN_GROUP", "仅群主可解散群聊"));
    }
    let members = repo::list_members(&state.db, conversation_id).await?;
    repo::dissolve_conversation(&state.db, conversation_id).await?;
    let _ = conversation;
    let event = serde_json::json!({
        "type": "conversation_dissolved",
        "payload": { "conversationId": conversation_id }
    })
    .to_string();
    for member in members {
        state.hub.send_to(member.user_id, &event);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /conversations/{id}/messages/{message_id}/recall`：撤回消息并广播。
pub async fn recall_message(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path((conversation_id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MessageDto>, AppError> {
    let user_id = user_id_of(&auth)?;
    let model =
        crate::service::recall_message(&state, user_id, conversation_id, message_id).await?;
    let members = repo::list_members(&state.db, conversation_id).await?;
    let event = serde_json::json!({
        "type": "message_recalled",
        "payload": { "conversationId": conversation_id, "messageId": message_id }
    })
    .to_string();
    for member in members {
        state.hub.send_to(member.user_id, &event);
    }
    Ok(Json(MessageDto::from(&model)))
}

/// `GET /conversations/{id}/messages/{message_id}/receipts`：已读回执。
pub async fn message_receipts(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path((conversation_id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    let receipts =
        crate::service::read_receipts(&state, user_id, conversation_id, message_id).await?;
    Ok(Json(json!(receipts)))
}

/// `GET /conversations/{id}/members`：成员列表。
pub async fn list_members(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    let members = repo::list_members(&state.db, conversation_id).await?;
    let items: Vec<Value> = members
        .iter()
        .map(|m| {
            json!({
                "userId": m.user_id,
                "role": m.role,
                "lastReadSeq": m.last_read_seq,
                "mutedUntil": m.muted_until,
                "canSpeak": m.can_speak,
                "joinedAt": m.joined_at
            })
        })
        .collect();
    Ok(Json(json!(items)))
}

/// 添加成员请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMembersRequest {
    /// 用户 ID 列表。
    pub user_ids: Vec<Uuid>,
}

/// `POST /conversations/{id}/members`：邀请成员（仅群聊，群主/管理员）。
pub async fn add_members(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path(conversation_id): Path<Uuid>,
    Json(input): Json<AddMembersRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = user_id_of(&auth)?;
    let ctx = repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    if ctx.conversation.r#type != conversation::TYPE_GROUP {
        return Err(AppError::bad_request("IM_NOT_GROUP", "单聊不能添加成员"));
    }
    if !matches!(
        ctx.member.role.as_str(),
        conversation_member::ROLE_OWNER | conversation_member::ROLE_ADMIN
    ) {
        return Err(AppError::forbidden(
            "IM_FORBIDDEN",
            "仅群主/管理员可邀请成员",
        ));
    }
    let now = state.now();
    let mut added = 0;
    for member_id in &input.user_ids {
        repo::insert_member(
            &state.db,
            conversation_id,
            *member_id,
            conversation_member::ROLE_MEMBER,
            now,
        )
        .await?;
        added += 1;
    }
    Ok(Json(json!({ "added": added })))
}

/// `DELETE /conversations/{id}/members/{userId}`：移出成员或主动退群。
pub async fn remove_member(
    State(state): State<SharedState>,
    auth: AuthUser,
    Path((conversation_id, target_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    let user_id = user_id_of(&auth)?;
    let ctx = repo::get_conversation_for_member(&state.db, conversation_id, user_id).await?;
    let is_self = user_id == target_id;
    let can_manage = matches!(
        ctx.member.role.as_str(),
        conversation_member::ROLE_OWNER | conversation_member::ROLE_ADMIN
    );
    if !is_self && !can_manage {
        return Err(AppError::forbidden(
            "IM_FORBIDDEN",
            "仅群主/管理员可移出成员",
        ));
    }
    repo::remove_member(&state.db, conversation_id, target_id, state.now()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/v1/im` 路由。
pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/conversations/{id}/messages",
            get(list_messages).post(send_message),
        )
        .route(
            "/conversations/{id}/messages/{message_id}/recall",
            post(recall_message),
        )
        .route(
            "/conversations/{id}/messages/{message_id}/receipts",
            get(message_receipts),
        )
        .route(
            "/conversations/{id}/members",
            get(list_members).post(add_members),
        )
        .route(
            "/conversations/{id}/members/{user_id}",
            delete(remove_member),
        )
        .route("/conversations/{id}/read", post(mark_read))
        .route(
            "/conversations/{id}/members/{user_id}/mute",
            post(mute_member),
        )
        .route(
            "/conversations/{id}/members/{user_id}/speak",
            post(set_member_speak),
        )
        .route("/conversations/{id}/settings", post(update_group_settings))
        .route(
            "/conversations/{id}/members/{user_id}/role",
            post(set_member_role),
        )
        .route("/conversations/{id}/transfer", post(transfer_owner))
        .route(
            "/conversations/{id}",
            axum::routing::delete(dissolve_conversation),
        )
}
