//! WebSocket 实时通道（单实例内存 Hub；多实例广播后续接 Redis PubSub）。
//!
//! 鉴权：`GET /ws/im?token=<Access Token>`（一次性 WS 票据为后续优化）。
//! 协议：JSON envelope `{ "type": "...", "requestId": "...", "payload": {...} }`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use club_auth_sdk::{Claims, TokenVerifier};
use club_common::AppError;

use crate::entity::message;
use crate::repo;
use crate::routes::MessageDto;
use crate::service;
use crate::state::SharedState;

/// 用户 → (连接 ID → 发送端) 两级映射。
type Connections = HashMap<Uuid, HashMap<Uuid, mpsc::Sender<String>>>;

/// 在线连接注册表。
#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<Connections>>,
}

impl Hub {
    /// 注册连接，返回连接 ID 与消息接收端。
    pub fn register(&self, user_id: Uuid) -> (Uuid, mpsc::Receiver<String>) {
        let connection_id = Uuid::now_v7();
        let (sender, receiver) = mpsc::channel(64);
        self.inner
            .lock()
            .expect("hub lock")
            .entry(user_id)
            .or_default()
            .insert(connection_id, sender);
        (connection_id, receiver)
    }

    /// 注销连接。
    pub fn unregister(&self, user_id: Uuid, connection_id: Uuid) {
        let mut guard = self.inner.lock().expect("hub lock");
        if let Some(connections) = guard.get_mut(&user_id) {
            connections.remove(&connection_id);
            if connections.is_empty() {
                guard.remove(&user_id);
            }
        }
    }

    /// 向单个用户的全部连接推送文本（返回送达连接数）。
    pub fn send_to(&self, user_id: Uuid, text: &str) -> usize {
        let guard = self.inner.lock().expect("hub lock");
        let Some(connections) = guard.get(&user_id) else {
            return 0;
        };
        connections
            .values()
            .filter(|sender| sender.try_send(text.to_string()).is_ok())
            .count()
    }

    /// 向多个用户广播。
    pub fn broadcast(&self, user_ids: &[Uuid], text: &str) {
        for user_id in user_ids {
            let _ = self.send_to(*user_id, text);
        }
    }

    /// 用户是否在线。
    pub fn online(&self, user_id: Uuid) -> bool {
        self.inner
            .lock()
            .expect("hub lock")
            .get(&user_id)
            .map(|connections| !connections.is_empty())
            .unwrap_or(false)
    }
}

/// 连接查询参数。
#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// Access Token。
    pub token: String,
}

/// WebSocket 升级入口。
pub async fn ws_handler(
    State(state): State<SharedState>,
    Query(query): Query<WsQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let claims = state.verify_token(&query.token)?;
    Ok(upgrade.on_upgrade(move |socket| handle_socket(state, claims, socket)))
}

/// 单连接处理循环。
async fn handle_socket(state: SharedState, claims: Claims, socket: WebSocket) {
    let user_id = match claims.sub.parse::<Uuid>() {
        Ok(user_id) => user_id,
        Err(_) => return,
    };
    let (connection_id, mut receiver) = state.hub.register(user_id);
    let (mut sink, mut stream) = socket.split();

    // 推送任务：注册表 → 客户端
    let send_task = tokio::spawn(async move {
        while let Some(text) = receiver.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // 接收循环
    while let Some(Ok(incoming)) = stream.next().await {
        let text = match incoming {
            Message::Text(text) => text.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        if let Some(reply) = handle_client_text(&state, user_id, &text).await {
            let _ = state.hub.send_to(user_id, &reply);
        }
    }

    state.hub.unregister(user_id, connection_id);
    send_task.abort();
    tracing::info!(user = %user_id, "WS 连接关闭");
}

/// 处理客户端单条消息，返回需要回给发送者的响应（可选）。
async fn handle_client_text(state: &SharedState, user_id: Uuid, text: &str) -> Option<String> {
    let envelope: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return Some(error_message(None, "IM_INVALID_JSON", "消息不是合法 JSON")),
    };
    let kind = envelope.get("type").and_then(Value::as_str).unwrap_or("");
    let request_id = envelope.get("requestId").and_then(Value::as_str);
    let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);

    match kind {
        "ping" => Some(json!({ "type": "pong" }).to_string()),
        "send_message" => {
            let conversation_id = payload
                .get("conversationId")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok());
            let Some(conversation_id) = conversation_id else {
                return Some(error_message(
                    request_id,
                    "IM_VALIDATION",
                    "缺少 conversationId",
                ));
            };
            let msg_type = payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("text");
            let content = payload.get("content").cloned().unwrap_or(Value::Null);
            let reply_to_id = payload
                .get("replyToId")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok());
            let client_msg_id = payload
                .get("clientMsgId")
                .and_then(Value::as_str)
                .map(str::to_string);
            match service::create_message(
                state,
                user_id,
                conversation_id,
                msg_type,
                &content,
                reply_to_id,
                client_msg_id.clone(),
            )
            .await
            {
                Ok((model, _created)) => {
                    broadcast_message(state, conversation_id, &model).await;
                    Some(json!({
                        "type": "message_ack",
                        "requestId": request_id,
                        "payload": { "clientMsgId": client_msg_id, "message": MessageDto::from(&model) }
                    })
                    .to_string())
                }
                Err(err) => Some(error_message(request_id, err.code(), &err.to_string())),
            }
        }
        "read" => {
            let conversation_id = payload
                .get("conversationId")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok());
            let seq = payload.get("seq").and_then(Value::as_i64);
            if let (Some(conversation_id), Some(seq)) = (conversation_id, seq) {
                if service::apply_read(state, user_id, conversation_id, seq)
                    .await
                    .is_ok()
                {
                    if let Ok(members) = repo::list_members(&state.db, conversation_id).await {
                        let targets: Vec<String> = members
                            .iter()
                            .filter(|member| member.user_id != user_id)
                            .map(|member| member.user_id.to_string())
                            .collect();
                        let event = json!({
                            "type": "read_update",
                            "payload": { "conversationId": conversation_id, "userId": user_id, "lastReadSeq": seq }
                        })
                        .to_string();
                        let _ = targets; // 在线用户由 Hub 广播（下方按 ID 过滤）
                        for member in members.iter().filter(|m| m.user_id != user_id) {
                            state.hub.send_to(member.user_id, &event);
                        }
                    }
                }
            }
            None
        }
        "typing" => {
            let conversation_id = payload
                .get("conversationId")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok());
            let typing = payload
                .get("state")
                .and_then(Value::as_str)
                .map(|state| state == "start")
                .unwrap_or(false);
            if let Some(conversation_id) = conversation_id {
                if let Ok(members) = repo::list_members(&state.db, conversation_id).await {
                    let event = json!({
                        "type": "typing",
                        "payload": { "conversationId": conversation_id, "userId": user_id, "state": if typing { "start" } else { "stop" } }
                    })
                    .to_string();
                    for member in members.iter().filter(|m| m.user_id != user_id) {
                        state.hub.send_to(member.user_id, &event);
                    }
                }
            }
            None
        }
        _ => Some(error_message(request_id, "IM_UNKNOWN_TYPE", "未知消息类型")),
    }
}

/// 广播新消息给会话成员（含发送者其他端）。
pub async fn broadcast_message(state: &SharedState, conversation_id: Uuid, model: &message::Model) {
    let Ok(members) = repo::list_members(&state.db, conversation_id).await else {
        return;
    };
    let event = json!({
        "type": "message",
        "payload": MessageDto::from(model)
    })
    .to_string();
    for member in members {
        state.hub.send_to(member.user_id, &event);
    }
}

/// 构造错误 envelope。
fn error_message(request_id: Option<&str>, code: &str, message: &str) -> String {
    json!({
        "type": "error",
        "requestId": request_id,
        "payload": { "code": code, "message": message }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_tracks_multiple_connections_and_presence() {
        let hub = Hub::default();
        let user = Uuid::now_v7();
        assert!(!hub.online(user));

        let (conn1, mut rx1) = hub.register(user);
        let (_conn2, mut rx2) = hub.register(user);
        assert!(hub.online(user));

        let sent = hub.send_to(user, "hello");
        assert_eq!(sent, 2, "多端均应收到");
        assert_eq!(rx1.try_recv().expect("rx1"), "hello");
        assert_eq!(rx2.try_recv().expect("rx2"), "hello");

        hub.unregister(user, conn1);
        assert!(hub.online(user), "仍有另一连接");
        let _ = rx1;
    }

    #[test]
    fn hub_ignores_offline_users_and_bad_envelope() {
        let hub = Hub::default();
        let user = Uuid::now_v7();
        assert_eq!(hub.send_to(user, "x"), 0);
        hub.broadcast(&[user], "y");
        assert!(error_message(None, "E", "msg").contains("\"type\":\"error\""));
    }
}
