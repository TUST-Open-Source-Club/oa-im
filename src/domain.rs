//! 领域逻辑（纯函数）。

use serde_json::Value;

use club_common::{AppError, FieldError};

/// 允许的消息类型（系统消息仅服务端产生）。
pub const USER_MESSAGE_TYPES: &[&str] = &["text", "image", "video", "file"];

/// 单条消息内容最大字节数。
pub const MAX_CONTENT_BYTES: usize = 16 * 1024;

/// 校验用户可发送的消息类型与内容大小。
pub fn validate_message(kind: &str, content: &Value) -> Result<(), AppError> {
    if !USER_MESSAGE_TYPES.contains(&kind) {
        return Err(AppError::unprocessable(
            "IM_VALIDATION",
            "消息类型不支持",
            vec![FieldError::new("type", "仅支持 text/image/video/file")],
        ));
    }
    if content.is_null() {
        return Err(AppError::unprocessable(
            "IM_VALIDATION",
            "消息内容不能为空",
            vec![FieldError::new("content", "不能为空")],
        ));
    }
    let size = serde_json::to_vec(content)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if size > MAX_CONTENT_BYTES {
        return Err(AppError::unprocessable(
            "IM_VALIDATION",
            "消息内容过大",
            vec![FieldError::new("content", "超过 16KB 限制")],
        ));
    }
    Ok(())
}

/// 生成会话列表预览文本（列表页展示，不含敏感细节）。
pub fn preview_of(kind: &str, content: &Value) -> String {
    match kind {
        "text" => content
            .get("text")
            .and_then(Value::as_str)
            .map(|text| text.chars().take(80).collect())
            .unwrap_or_default(),
        "image" => "[图片]".to_string(),
        "video" => "[视频]".to_string(),
        "file" => content
            .get("attachments")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("name"))
            .and_then(Value::as_str)
            .map(|name| format!("[文件] {name}"))
            .unwrap_or_else(|| "[文件]".to_string()),
        "system" => content
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or("系统消息")
            .to_string(),
        _ => String::new(),
    }
}

/// 计算未读数（max(0, next_seq - last_read_seq)）。
pub fn unread_of(next_seq: i64, last_read_seq: i64) -> i64 {
    (next_seq - last_read_seq).max(0)
}

/// 归一化单聊成员：去重、去自己、排序，保证 (a,b) 与 (b,a) 生成同一键。
pub fn direct_member_key(self_id: uuid::Uuid, other_id: uuid::Uuid) -> (uuid::Uuid, uuid::Uuid) {
    if self_id <= other_id {
        (self_id, other_id)
    } else {
        (other_id, self_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_type_and_size() {
        assert!(validate_message("text", &json!({"text":"hi"})).is_ok());
        assert!(
            validate_message("system", &json!({})).is_err(),
            "用户不可发系统消息"
        );
        assert!(validate_message("text", &Value::Null).is_err());
        let big = json!({ "text": "x".repeat(MAX_CONTENT_BYTES + 1) });
        assert!(validate_message("text", &big).is_err());
    }

    #[test]
    fn builds_previews() {
        assert_eq!(preview_of("text", &json!({"text":"你好"})), "你好");
        assert_eq!(preview_of("image", &json!({})), "[图片]");
        assert_eq!(preview_of("video", &json!({})), "[视频]");
        assert_eq!(
            preview_of("file", &json!({"attachments":[{"name":"a.pdf"}]})),
            "[文件] a.pdf"
        );
        assert_eq!(preview_of("unknown", &json!({})), "");
        let long = json!({"text": "x".repeat(200)});
        assert_eq!(preview_of("text", &long).chars().count(), 80);
    }

    #[test]
    fn computes_unread() {
        assert_eq!(unread_of(10, 7), 3);
        assert_eq!(unread_of(7, 7), 0);
        assert_eq!(unread_of(5, 9), 0, "已读位点超前时不应为负");
    }

    #[test]
    fn direct_key_is_order_independent() {
        let a = uuid::Uuid::now_v7();
        let b = uuid::Uuid::now_v7();
        assert_eq!(direct_member_key(a, b), direct_member_key(b, a));
    }
}
