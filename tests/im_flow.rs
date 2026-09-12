//! IM 集成测试：会话/消息/已读/撤回/权限 + WebSocket 实时。

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use uuid::Uuid;

/// 创建单聊并返回会话 ID。
async fn create_direct(app: &TestApp, token: &str, other: Uuid) -> String {
    let response = request(
        &app.app,
        "POST",
        "/api/v1/im/conversations",
        Some(token),
        Some(&json!({ "type": "direct", "memberIds": [other] })),
    )
    .await;
    let body = response.expect(StatusCode::CREATED);
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn direct_conversation_message_read_recall_flow() {
    let app = spawn().await;
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let c = Uuid::now_v7();
    let token_a = issue_token(&app, a);
    let token_b = issue_token(&app, b);
    let token_c = issue_token(&app, c);

    // 单聊创建幂等
    let conversation_id = create_direct(&app, &token_a, b).await;
    let same = request(
        &app.app,
        "POST",
        "/api/v1/im/conversations",
        Some(&token_b),
        Some(&json!({ "type": "direct", "memberIds": [a] })),
    )
    .await;
    assert_eq!(same.expect(StatusCode::OK)["id"], conversation_id);

    // 发送消息 + clientMsgId 幂等
    let sent = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token_a),
        Some(&json!({ "type": "text", "content": { "text": "你好" }, "clientMsgId": "m-1" })),
    )
    .await;
    let sent = sent.expect(StatusCode::CREATED);
    assert_eq!(sent["seq"], 1);
    let message_id = sent["id"].as_str().unwrap().to_string();
    let resent = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token_a),
        Some(&json!({ "type": "text", "content": { "text": "你好" }, "clientMsgId": "m-1" })),
    )
    .await;
    assert_eq!(resent.expect(StatusCode::OK)["id"], message_id);

    // 对方未读 1
    let list_b = request(
        &app.app,
        "GET",
        "/api/v1/im/conversations",
        Some(&token_b),
        None,
    )
    .await;
    let list_b = list_b.expect(StatusCode::OK);
    assert_eq!(list_b[0]["unread"], 1);
    assert_eq!(list_b[0]["lastMessage"]["content"]["text"], "你好");

    // 历史消息 + 已读
    let history = request(
        &app.app,
        "GET",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(history.expect(StatusCode::OK)[0]["seq"], 1);
    let read = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/read"),
        Some(&token_b),
        Some(&json!({ "seq": 1 })),
    )
    .await;
    assert_eq!(read.expect(StatusCode::OK)["lastReadSeq"], 1);
    let list_b = request(
        &app.app,
        "GET",
        "/api/v1/im/conversations",
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(list_b.expect(StatusCode::OK)[0]["unread"], 0);

    // 已读回执
    let receipts = request(
        &app.app,
        "GET",
        &format!("/api/v1/im/conversations/{conversation_id}/messages/{message_id}/receipts"),
        Some(&token_a),
        None,
    )
    .await;
    let receipts = receipts.expect(StatusCode::OK);
    assert_eq!(receipts[0]["read"], true);

    // 撤回：本人可，他人不可
    let recalled = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/messages/{message_id}/recall"),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(recalled.expect(StatusCode::OK)["status"], "recalled");
    let forbidden = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/messages/{message_id}/recall"),
        Some(&token_b),
        None,
    )
    .await;
    forbidden.expect(StatusCode::FORBIDDEN);

    // 非成员不可读
    let denied = request(
        &app.app,
        "GET",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token_c),
        None,
    )
    .await;
    denied.expect(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn group_permissions_and_membership() {
    let app = spawn().await;
    let owner = Uuid::now_v7();
    let member = Uuid::now_v7();
    let newcomer = Uuid::now_v7();
    let token_owner = issue_token(&app, owner);
    let token_member = issue_token(&app, member);
    let token_new = issue_token(&app, newcomer);

    let created = request(
        &app.app,
        "POST",
        "/api/v1/im/conversations",
        Some(&token_owner),
        Some(&json!({ "type": "group", "name": "项目组", "memberIds": [member] })),
    )
    .await;
    let created = created.expect(StatusCode::CREATED);
    let conversation_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["type"], "group");

    // 普通成员邀请 → 403；群主邀请 → 200
    let denied = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/members"),
        Some(&token_member),
        Some(&json!({ "userIds": [newcomer] })),
    )
    .await;
    denied.expect(StatusCode::FORBIDDEN);
    let added = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/members"),
        Some(&token_owner),
        Some(&json!({ "userIds": [newcomer] })),
    )
    .await;
    assert_eq!(added.expect(StatusCode::OK)["added"], 1);

    // 移出成员后无权访问
    let removed = request(
        &app.app,
        "DELETE",
        &format!("/api/v1/im/conversations/{conversation_id}/members/{member}"),
        Some(&token_owner),
        None,
    )
    .await;
    removed.expect(StatusCode::NO_CONTENT);
    let denied = request(
        &app.app,
        "GET",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token_member),
        None,
    )
    .await;
    denied.expect(StatusCode::FORBIDDEN);

    // 新成员可以发言
    let sent = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token_new),
        Some(&json!({ "type": "text", "content": { "text": "大家好" } })),
    )
    .await;
    sent.expect(StatusCode::CREATED);
}

#[tokio::test]
async fn validation_and_errors() {
    let app = spawn().await;
    let a = Uuid::now_v7();
    let token = issue_token(&app, a);

    // 不能与自己单聊
    let self_chat = request(
        &app.app,
        "POST",
        "/api/v1/im/conversations",
        Some(&token),
        Some(&json!({ "type": "direct", "memberIds": [a] })),
    )
    .await;
    self_chat.expect(StatusCode::BAD_REQUEST);

    // 群聊必须有名字
    let no_name = request(
        &app.app,
        "POST",
        "/api/v1/im/conversations",
        Some(&token),
        Some(&json!({ "type": "group", "memberIds": [Uuid::now_v7()] })),
    )
    .await;
    no_name.expect(StatusCode::UNPROCESSABLE_ENTITY);

    // 系统消息类型不可发送
    let conversation_id = create_direct(&app, &token, Uuid::now_v7()).await;
    let system = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token),
        Some(&json!({ "type": "system", "content": {} })),
    )
    .await;
    system.expect(StatusCode::UNPROCESSABLE_ENTITY);

    // 引用不存在的消息
    let bad_reply = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{conversation_id}/messages"),
        Some(&token),
        Some(&json!({ "type": "text", "content": { "text": "hi" }, "replyToId": Uuid::now_v7() })),
    )
    .await;
    bad_reply.expect(StatusCode::UNPROCESSABLE_ENTITY);

    // 未登录 / 健康检查
    request(&app.app, "GET", "/api/v1/im/conversations", None, None)
        .await
        .expect(StatusCode::UNAUTHORIZED);
    let health = request(&app.app, "GET", "/healthz", None, None).await;
    assert_eq!(health.expect(StatusCode::OK)["status"], "ok");
    let ready = request(&app.app, "GET", "/readyz", None, None).await;
    assert_eq!(ready.expect(StatusCode::OK)["database"], "ok");
}

#[tokio::test]
async fn websocket_realtime_delivery() {
    let app = spawn().await;
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let token_a = issue_token(&app, a);
    let token_b = issue_token(&app, b);
    let conversation_id = create_direct(&app, &token_a, b).await;

    // 启动真实 HTTP 服务（WS 需要 TCP）
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = app.app.clone();
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });

    let (mut ws_a, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws/im?token={token_a}"))
            .await
            .expect("ws a");
    let (mut ws_b, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws/im?token={token_b}"))
            .await
            .expect("ws b");

    // ping → pong
    ws_a.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({ "type": "ping" }).to_string().into(),
    ))
    .await
    .expect("send ping");
    let pong = read_until(&mut ws_a, "pong").await;
    assert_eq!(pong["type"], "pong");

    // a 发送 → a 收到 ack，b 收到 message
    ws_a.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({
            "type": "send_message",
            "requestId": "r1",
            "payload": { "conversationId": conversation_id, "type": "text", "content": { "text": "WS 你好" }, "clientMsgId": "ws-1" }
        })
        .to_string()
        .into(),
    ))
    .await
    .expect("send message");

    let ack = read_until(&mut ws_a, "message_ack").await;
    assert_eq!(ack["requestId"], "r1");
    assert_eq!(ack["payload"]["message"]["seq"], 1);

    let delivered = read_until(&mut ws_b, "message").await;
    assert_eq!(delivered["payload"]["content"]["text"], "WS 你好");

    // 已读回执推送：b 上报已读，a 收到 read_update
    ws_b.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({ "type": "read", "payload": { "conversationId": conversation_id, "seq": 1 } })
            .to_string()
            .into(),
    ))
    .await
    .expect("send read");
    let read_update = read_until(&mut ws_a, "read_update").await;
    assert_eq!(read_update["payload"]["lastReadSeq"], 1);
}

/// 从 WebSocket 读取直到遇到目标类型的消息（最多 3 秒）。
async fn read_until<S>(socket: &mut S, kind: &str) -> serde_json::Value
where
    S: StreamExt<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(remaining, socket.next())
            .await
            .expect("等待消息超时")
            .expect("连接关闭")
            .expect("消息错误");
        if let tokio_tungstenite::tungstenite::Message::Text(text) = next {
            let value: serde_json::Value = serde_json::from_str(&text).expect("解析 WS 消息");
            if value["type"] == kind {
                return value;
            }
        }
    }
}

#[tokio::test]
async fn members_leave_pagination_and_recall_expiry() {
    use sea_orm::ConnectionTrait;

    let app = spawn().await;
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let c = Uuid::now_v7();
    let token_a = issue_token(&app, a);
    let token_b = issue_token(&app, b);
    let _token_c = issue_token(&app, c);

    // 群成员列表
    let group = request(
        &app.app,
        "POST",
        "/api/v1/im/conversations",
        Some(&token_a),
        Some(&json!({ "type": "group", "name": "测试群", "memberIds": [b] })),
    )
    .await;
    let group_id = group.expect(StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .to_string();
    let members = request(
        &app.app,
        "GET",
        &format!("/api/v1/im/conversations/{group_id}/members"),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(members.expect(StatusCode::OK).as_array().unwrap().len(), 2);

    // 单聊不能加人
    let direct_id = create_direct(&app, &token_a, c).await;
    let cannot_add = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{direct_id}/members"),
        Some(&token_a),
        Some(&json!({ "userIds": [b] })),
    )
    .await;
    cannot_add.expect(StatusCode::BAD_REQUEST);

    // 成员主动退群
    let leave = request(
        &app.app,
        "DELETE",
        &format!("/api/v1/im/conversations/{group_id}/members/{b}"),
        Some(&token_b),
        None,
    )
    .await;
    leave.expect(StatusCode::NO_CONTENT);

    // 分页：发送 3 条后 beforeSeq=4 取 2 条
    for index in 1..=3 {
        request(
            &app.app,
            "POST",
            &format!("/api/v1/im/conversations/{group_id}/messages"),
            Some(&token_a),
            Some(&json!({ "type": "text", "content": { "text": format!("m{index}") } })),
        )
        .await
        .expect(StatusCode::CREATED);
    }
    let page = request(
        &app.app,
        "GET",
        &format!("/api/v1/im/conversations/{group_id}/messages?beforeSeq=4&limit=2"),
        Some(&token_a),
        None,
    )
    .await;
    let page = page.expect(StatusCode::OK);
    assert_eq!(page.as_array().unwrap().len(), 2);
    assert_eq!(page[0]["seq"], 2);

    // 超时撤回 → 403（回拨 created_at 模拟 10 分钟前）
    let old = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{group_id}/messages"),
        Some(&token_a),
        Some(&json!({ "type": "text", "content": { "text": "旧消息" } })),
    )
    .await;
    let old_id = old.expect(StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.state
        .db
        .execute_unprepared(&format!(
            "UPDATE messages SET created_at = now() - interval '10 minutes' WHERE id = '{}'",
            old_id.replace('\'', "")
        ))
        .await
        .expect("回拨时间");
    let expired = request(
        &app.app,
        "POST",
        &format!("/api/v1/im/conversations/{group_id}/messages/{old_id}/recall"),
        Some(&token_a),
        None,
    )
    .await;
    expired.expect(StatusCode::FORBIDDEN);

    // 不存在的消息回执 → 404
    let missing = request(
        &app.app,
        "GET",
        &format!(
            "/api/v1/im/conversations/{group_id}/messages/{}/receipts",
            Uuid::now_v7()
        ),
        Some(&token_a),
        None,
    )
    .await;
    missing.expect(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn websocket_typing_and_error_paths() {
    let app = spawn().await;
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let token_a = issue_token(&app, a);
    let token_b = issue_token(&app, b);
    let conversation_id = create_direct(&app, &token_a, b).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.app.clone();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let (mut ws_a, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws/im?token={token_a}"))
            .await
            .expect("ws a");
    let (mut ws_b, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws/im?token={token_b}"))
            .await
            .expect("ws b");

    // 非法 JSON → error
    ws_a.send(tokio_tungstenite::tungstenite::Message::Text(
        "not-json".into(),
    ))
    .await
    .unwrap();
    assert_eq!(
        read_until(&mut ws_a, "error").await["payload"]["code"],
        "IM_INVALID_JSON"
    );

    // 未知类型 → error
    ws_a.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({ "type": "nope" }).to_string().into(),
    ))
    .await
    .unwrap();
    assert_eq!(
        read_until(&mut ws_a, "error").await["payload"]["code"],
        "IM_UNKNOWN_TYPE"
    );

    // typing → b 收到
    ws_a.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({ "type": "typing", "payload": { "conversationId": conversation_id, "state": "start" } })
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(
        read_until(&mut ws_b, "typing").await["payload"]["state"],
        "start"
    );
}
