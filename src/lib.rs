//! 社团 OA 即时通讯服务（im）。
//!
//! 当前实现：会话（单聊/群聊）、消息收发与序号、已读位点、成员管理、outbox 事件。
//! WebSocket 实时通道（`/ws/im`）在下一步补齐，当前客户端可用 HTTP 轮询/发送。

#![warn(missing_docs)]

/// 配置。
pub mod config {
    use std::collections::HashMap;

    use anyhow::anyhow;

    /// im 服务运行配置。
    #[derive(Debug, Clone)]
    pub struct Config {
        /// PostgreSQL 连接串。
        pub database_url: String,
        /// 监听地址。
        pub bind_addr: String,
        /// JWT issuer。
        pub issuer: String,
        /// 内部服务密钥（当前预留）。
        pub internal_secret: String,
        /// Redis 连接串（启用后投递 outbox 事件）。
        pub redis_url: Option<String>,
    }

    impl Config {
        /// 从环境变量加载。
        pub fn from_env() -> anyhow::Result<Self> {
            let map: HashMap<String, String> = std::env::vars().collect();
            Self::from_map(map)
        }

        /// 从键值映射加载（测试用）。
        pub fn from_env_from_map(map: HashMap<String, String>) -> Self {
            Self::from_map(map).expect("测试配置")
        }

        /// 从键值映射构建配置。
        fn from_map(map: HashMap<String, String>) -> anyhow::Result<Self> {
            Ok(Self {
                database_url: map
                    .get("DATABASE_URL")
                    .ok_or_else(|| anyhow!("缺少环境变量 DATABASE_URL"))?
                    .to_string(),
                bind_addr: map
                    .get("IM_BIND_ADDR")
                    .cloned()
                    .unwrap_or_else(|| "0.0.0.0:8082".to_string()),
                issuer: map
                    .get("AUTH_ISSUER")
                    .cloned()
                    .unwrap_or_else(|| "http://localhost:8081".to_string())
                    .trim_end_matches('/')
                    .to_string(),
                internal_secret: map
                    .get("INTERNAL_SERVICE_SECRET")
                    .cloned()
                    .unwrap_or_else(|| "dev-internal-secret".to_string()),
                redis_url: map.get("REDIS_URL").cloned(),
            })
        }
    }
}

/// 数据库连接。
pub mod db {
    use std::time::Duration;

    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr};

    /// 创建 schema 并以其为 search_path 连接。
    pub async fn connect_with_schema(url: &str, schema: &str) -> Result<DatabaseConnection, DbErr> {
        let bootstrap = Database::connect(url).await?;
        bootstrap
            .execute_unprepared(&format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\""))
            .await?;
        bootstrap.close().await?;
        let mut options = ConnectOptions::new(url.to_string());
        options.set_schema_search_path(schema);
        options.max_connections(10);
        options.acquire_timeout(Duration::from_secs(5));
        Database::connect(options).await
    }
}

/// 应用状态。
pub mod state {
    use std::sync::{Arc, RwLock};

    use anyhow::Context;
    use jsonwebtoken::DecodingKey;
    use sea_orm::DatabaseConnection;

    use club_auth_sdk::{decode_access_token, Claims, TokenVerifier};
    use club_bus::Bus;
    use club_common::AppError;

    use crate::config::Config;

    /// 状态。
    pub struct AppState {
        /// 数据库。
        pub db: DatabaseConnection,
        /// 配置。
        pub config: Config,
        /// 事件总线（未配置 Redis 时为空）。
        pub bus: Option<Bus>,
        /// JWKS 解码 key。
        pub signing_key: RwLock<Option<DecodingKey>>,
        /// 在线连接注册表。
        pub hub: crate::realtime::Hub,
    }

    impl AppState {
        /// 当前时间。
        pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::Utc::now()
        }

        /// 拉取并缓存 JWKS。
        pub async fn load_jwks(&self) -> anyhow::Result<()> {
            let url = format!("{}/.well-known/jwks.json", self.config.issuer);
            let jwks: club_auth_sdk::Jwks = reqwest::get(&url)
                .await
                .context("请求 JWKS 失败")?
                .error_for_status()?
                .json()
                .await
                .context("解析 JWKS 失败")?;
            let key = club_auth_sdk::jwks::decoding_key_from_jwks(&jwks, None)
                .map_err(|err| anyhow::anyhow!("JWKS 无可用公钥: {err}"))?;
            *self.signing_key.write().expect("lock") = Some(key);
            Ok(())
        }
    }

    impl TokenVerifier for AppState {
        fn verify_token(&self, token: &str) -> Result<Claims, AppError> {
            let guard = self.signing_key.read().expect("lock");
            let key = guard
                .as_ref()
                .ok_or_else(|| AppError::internal("JWKS 未加载"))?;
            decode_access_token(token, key, &self.config.issuer)
                .map_err(|_| AppError::unauthorized("AUTH_INVALID_TOKEN", "访问令牌无效或已过期"))
        }
    }

    /// 共享状态句柄。
    #[derive(Clone)]
    pub struct SharedState(Arc<AppState>);

    impl SharedState {
        /// 包装。
        pub fn new(state: AppState) -> Self {
            Self(Arc::new(state))
        }
    }

    impl std::ops::Deref for SharedState {
        type Target = AppState;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl TokenVerifier for SharedState {
        fn verify_token(&self, token: &str) -> Result<Claims, AppError> {
            self.0.verify_token(token)
        }
    }
}

pub mod domain;
pub mod entity;
pub mod migration;
/// WebSocket 实时通道。
pub mod realtime;
pub mod repo;
/// HTTP 路由。
pub mod routes;
/// 消息领域服务。
pub mod service;

use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::SharedState;

/// 构建路由。
pub fn build_router(state: SharedState) -> Router {
    Router::new()
        .route("/healthz", axum::routing::get(routes::healthz))
        .route("/readyz", axum::routing::get(routes::readyz))
        .route("/ws/im", axum::routing::get(realtime::ws_handler))
        .nest("/api/v1/im", routes::router())
        .layer(axum::middleware::from_fn(|req, next| {
            club_auth_sdk::guard::guard_bot_request(req, next, "im")
        }))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
