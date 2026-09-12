//! im schema 实体。

/// 会话。
pub mod conversation {
    use sea_orm::entity::prelude::*;

    /// 会话类型：单聊。
    pub const TYPE_DIRECT: &str = "direct";
    /// 会话类型：群聊。
    pub const TYPE_GROUP: &str = "group";

    /// 会话模型。
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "conversations")]
    pub struct Model {
        /// 会话 ID。
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        /// direct / group。
        pub r#type: String,
        /// 群名（单聊为空）。
        #[sea_orm(nullable)]
        pub name: Option<String>,
        /// 群主（单聊为空）。
        #[sea_orm(nullable)]
        pub owner_id: Option<Uuid>,
        /// 群公告。
        #[sea_orm(nullable, column_type = "Text")]
        pub notice: Option<String>,
        /// 已分配的最大消息序号（unread 计算基准）。
        pub next_seq: i64,
        /// 创建时间。
        pub created_at: DateTimeWithTimeZone,
        /// 更新时间（用于会话排序）。
        pub updated_at: DateTimeWithTimeZone,
    }

    /// 关系。
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// 会话成员。
pub mod conversation_member {
    use sea_orm::entity::prelude::*;

    /// 成员角色：群主。
    pub const ROLE_OWNER: &str = "owner";
    /// 成员角色：管理员。
    pub const ROLE_ADMIN: &str = "admin";
    /// 成员角色：普通成员。
    pub const ROLE_MEMBER: &str = "member";

    /// 会话成员模型（会话 + 用户 联合主键）。
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "conversation_members")]
    pub struct Model {
        /// 会话 ID。
        #[sea_orm(primary_key, auto_increment = false)]
        pub conversation_id: Uuid,
        /// 用户 ID。
        #[sea_orm(primary_key, auto_increment = false)]
        pub user_id: Uuid,
        /// 角色。
        pub role: String,
        /// 已读位点（此序号之前的消息均已读）。
        pub last_read_seq: i64,
        /// 消息免打扰。
        pub muted: bool,
        /// 置顶。
        pub pinned: bool,
        /// 加入时间。
        pub joined_at: DateTimeWithTimeZone,
        /// 退群/被移出时间。
        #[sea_orm(nullable)]
        pub left_at: Option<DateTimeWithTimeZone>,
    }

    /// 关系。
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// 消息。
pub mod message {
    use sea_orm::entity::prelude::*;

    /// 消息类型：文本。
    pub const TYPE_TEXT: &str = "text";
    /// 消息类型：图片。
    pub const TYPE_IMAGE: &str = "image";
    /// 消息类型：视频。
    pub const TYPE_VIDEO: &str = "video";
    /// 消息类型：文件。
    pub const TYPE_FILE: &str = "file";
    /// 消息类型：系统消息。
    pub const TYPE_SYSTEM: &str = "system";

    /// 消息模型。
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "messages")]
    pub struct Model {
        /// 消息 ID（UUIDv7）。
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        /// 会话 ID。
        pub conversation_id: Uuid,
        /// 会话内单调序号。
        pub seq: i64,
        /// 发送者（系统消息为空）。
        #[sea_orm(nullable)]
        pub sender_id: Option<Uuid>,
        /// 消息类型。
        pub r#type: String,
        /// 内容（按类型约定的 JSON）。
        #[sea_orm(column_type = "JsonBinary")]
        pub content: Json,
        /// 引用的消息 ID。
        #[sea_orm(nullable)]
        pub reply_to_id: Option<Uuid>,
        /// 客户端幂等 ID。
        #[sea_orm(nullable)]
        pub client_msg_id: Option<String>,
        /// 状态：normal / recalled。
        pub status: String,
        /// 撤回时间。
        #[sea_orm(nullable)]
        pub recalled_at: Option<DateTimeWithTimeZone>,
        /// 发送时间。
        pub created_at: DateTimeWithTimeZone,
    }

    /// 关系。
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
