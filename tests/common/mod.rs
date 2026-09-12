//! 集成测试脚手架：真实 PostgreSQL + 独立 schema + 测试密钥。

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::RwLock;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

use im_service::config::Config;
use im_service::migration::Migrator;
use im_service::state::{AppState, SharedState};
use im_service::{build_router, db};

/// 测试数据库连接串。
pub fn test_database_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:55432/club_oa".to_string())
}

/// 测试应用。
pub struct TestApp {
    /// 状态。
    pub state: SharedState,
    /// 路由。
    pub app: Router,
    /// 签名私钥。
    pub private_pem: Vec<u8>,
    /// 公钥。
    pub public_pem: Vec<u8>,
}

/// 启动测试应用。
pub async fn spawn() -> TestApp {
    let url = test_database_url();
    let schema = format!("test_{}", Uuid::now_v7().simple());
    let database = db::connect_with_schema(&url, &schema)
        .await
        .expect("连接测试数据库失败");
    Migrator::up(&database, None).await.expect("迁移失败");

    let mut rng = rand_core::OsRng;
    let private = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("keygen");
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    let private_pem = private
        .to_pkcs8_pem(LineEnding::LF)
        .expect("private pem")
        .as_bytes()
        .to_vec();
    let public_pem = rsa::RsaPublicKey::from(&private)
        .to_public_key_pem(LineEnding::LF)
        .expect("public pem")
        .into_bytes();
    let decoding = club_auth_sdk::decoding_key_from_rsa_pem(&public_pem).expect("decoding key");

    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("DATABASE_URL".to_string(), url);
    env.insert("AUTH_ISSUER".to_string(), "https://oa.test".to_string());
    let config = Config::from_env_from_map(env);

    let state = SharedState::new(AppState {
        db: database,
        config,
        bus: None,
        signing_key: RwLock::new(Some(decoding)),
        hub: Default::default(),
    });
    let app = build_router(state.clone());
    TestApp {
        state,
        app,
        private_pem,
        public_pem,
    }
}

/// 为指定用户签发 Access Token。
pub fn issue_token(app: &TestApp, user_id: Uuid) -> String {
    let claims = club_auth_sdk::Claims {
        sub: user_id.to_string(),
        name: "测试用户".into(),
        avatar: None,
        roles: vec!["member".into()],
        scopes: vec!["im".into()],
        guest: false,
        iss: "https://oa.test".into(),
        iat: chrono::Utc::now().timestamp(),
        exp: chrono::Utc::now().timestamp() + 3600,
        jti: Uuid::now_v7().to_string(),
    };
    let kid = club_auth_sdk::jwks::key_id_from_pem(&app.public_pem);
    club_auth_sdk::encode_access_token(&claims, &app.private_pem, &kid).expect("签发令牌")
}

/// HTTP 响应快照。
pub struct TestResponse {
    /// 状态码。
    pub status: StatusCode,
    /// JSON。
    pub body: Value,
}

impl TestResponse {
    /// 断言状态码并返回 JSON。
    pub fn expect(self, status: StatusCode) -> Value {
        assert_eq!(self.status, status, "响应体: {}", self.body);
        self.body
    }
}

/// 发送 JSON 请求。
pub async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> TestResponse {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let bytes = body
        .map(|value| serde_json::to_vec(value).expect("序列化"))
        .unwrap_or_default();
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(bytes)).expect("请求"))
        .await
        .expect("执行请求");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("响应体")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    TestResponse { status, body }
}
