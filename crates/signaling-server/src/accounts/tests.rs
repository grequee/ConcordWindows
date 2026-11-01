#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::{
    Config, Invite, Outbound, RoomConfig, Server, Session, build_router, rate_limit::TokenBucket,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use std::{collections::HashMap, time::Instant};
use tower::ServiceExt;

#[derive(Default)]
struct MemoryRepository {
    accounts: Mutex<HashMap<String, StoredAccount>>,
    sessions: Mutex<HashMap<String, (UserId, u64)>>,
}
#[async_trait]
impl Repository for MemoryRepository {
    async fn create_account(&self, account: &StoredAccount) -> Result<(), StorageError> {
        let mut rows = self.accounts.lock();
        if rows.contains_key(&account.profile.username) {
            return Err(StorageError::Conflict);
        }
        rows.insert(account.profile.username.clone(), account.clone());
        Ok(())
    }
    async fn find_account(&self, username: &str) -> Result<Option<StoredAccount>, StorageError> {
        Ok(self.accounts.lock().get(username).cloned())
    }
    async fn create_account_session(
        &self,
        id: &UserId,
        hash: &str,
        expires: u64,
    ) -> Result<(), StorageError> {
        self.sessions
            .lock()
            .insert(hash.to_owned(), (id.clone(), expires));
        Ok(())
    }
    async fn account_session(
        &self,
        hash: &str,
        now: u64,
    ) -> Result<Option<StoredSession>, StorageError> {
        let Some((id, expires_at_ms)) = self
            .sessions
            .lock()
            .get(hash)
            .cloned()
            .filter(|(_, expires)| *expires > now)
        else {
            return Ok(None);
        };
        Ok(self
            .accounts
            .lock()
            .values()
            .find(|account| account.profile.id == id)
            .map(|account| StoredSession {
                account: account.profile.clone(),
                expires_at_ms,
            }))
    }
    async fn revoke_account_session(&self, hash: &str) -> Result<(), StorageError> {
        self.sessions.lock().remove(hash);
        Ok(())
    }
}
const PASSWORD: &str = "uma frase de senha 123";
const INVITE: &str = "test-account-invite-private";
fn fixture() -> (Server, Arc<MemoryRepository>) {
    let mut server = Server::new(Config {
        invites: vec![Invite {
            token: INVITE.into(),
            label: "teste".into(),
        }],
        rooms: vec![RoomConfig {
            id: "geral".into(),
            name: "Geral".into(),
            description: None,
        }],
        ..Config::default()
    })
    .unwrap();
    let repository = Arc::new(MemoryRepository::default());
    Arc::get_mut(&mut server.inner).unwrap().accounts =
        Some(Arc::new(Accounts::new(repository.clone())));
    (server, repository)
}
async fn request(
    server: &Server,
    path: &str,
    method: &str,
    body: serde_json::Value,
    token: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = build_router(server.clone())
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    (
        status,
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap(),
    )
}
#[tokio::test]
async fn registration_does_not_login_and_password_login_reuses_a_permanent_identity() {
    let (server, repository) = fixture();
    let (status, profile) = request(
        &server,
        "/api/accounts/register",
        "POST",
        serde_json::json!({"username":"Ana.Teste","password":PASSWORD,"invite_token":INVITE}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(profile["username"], "ana.teste");
    assert!(profile.get("token").is_none());
    assert!(repository.sessions.lock().is_empty());
    assert!(
        !repository
            .accounts
            .lock()
            .values()
            .next()
            .unwrap()
            .password_hash
            .contains(PASSWORD)
    );
    let credentials = serde_json::json!({"username":"ANA.TESTE","password":PASSWORD});
    let (status, first) = request(
        &server,
        "/api/accounts/login",
        "POST",
        credentials.clone(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["account"], profile);
    let token = first["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(!repository.sessions.lock().contains_key(token));
    assert!(repository.sessions.lock().contains_key(&token_hash(token)));
    let (status, me) = request(
        &server,
        "/api/accounts/me",
        "GET",
        serde_json::Value::Null,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["account"], profile);
    assert!(me.get("password_hash").is_none());
    assert!(me.get("token").is_none());
    // A new Accounts service backed by the same database resolves saved sessions.
    assert_eq!(
        Accounts::new(repository.clone())
            .resolve(token)
            .await
            .unwrap()
            .account
            .id
            .as_str(),
        profile["id"].as_str().unwrap()
    );
    assert_eq!(
        request(
            &server,
            "/api/accounts/logout",
            "POST",
            serde_json::Value::Null,
            Some(token)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &server,
            "/api/accounts/me",
            "GET",
            serde_json::Value::Null,
            Some(token)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, again) = request(&server, "/api/accounts/login", "POST", credentials, None).await;
    assert_eq!(again["account"]["id"], profile["id"]);
    assert_ne!(again["token"], first["token"]);
}
#[tokio::test]
async fn duplicate_names_invalid_invites_and_wrong_passwords_do_not_grant_a_session() {
    let (server, repository) = fixture();
    let registration =
        serde_json::json!({"username":"ana","password":PASSWORD,"invite_token":INVITE});
    assert_eq!(
        request(
            &server,
            "/api/accounts/register",
            "POST",
            serde_json::json!({"username":"ana","password":PASSWORD,"invite_token":"wrong"}),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(repository.accounts.lock().is_empty());
    assert_eq!(
        request(
            &server,
            "/api/accounts/register",
            "POST",
            registration.clone(),
            None
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(
            &server,
            "/api/accounts/register",
            "POST",
            registration,
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let bad = request(
        &server,
        "/api/accounts/login",
        "POST",
        serde_json::json!({"username":"ana","password":"senha errada"}),
        None,
    )
    .await;
    let unknown = request(
        &server,
        "/api/accounts/login",
        "POST",
        serde_json::json!({"username":"ninguem","password":"senha errada"}),
        None,
    )
    .await;
    assert_eq!(bad, unknown);
    assert_eq!(bad.0, StatusCode::UNAUTHORIZED);
    assert!(repository.sessions.lock().is_empty());
}
#[test]
fn cleanup_removes_call_presence_and_a_late_old_guard_cannot_remove_the_new_session() {
    let (server, _) = fixture();
    let user = UserId::parse("u_permanent").unwrap();
    let room = concord_protocol::RoomId::parse("geral").unwrap();
    let insert = |token: &str| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        server.inner.sessions.lock().insert(
            user.clone(),
            Session {
                account: None,
                account_session_hash: None,
                http_token: token.into(),
                http_bucket: TokenBucket::new(5, 20),
                tx,
                display_name: "Ana".into(),
                addr: "127.0.0.1".parse().unwrap(),
                connected_at: Instant::now(),
            },
        );
        rx
    };
    let mut first = insert("first-transport");
    server
        .inner
        .registry
        .lock()
        .join(&room, user.clone(), "Ana", 1)
        .unwrap();
    server
        .inner
        .registry
        .lock()
        .set_presence(&user, Some(false), Some(false), Some(false), Some(true))
        .unwrap();
    assert!(server.inner.registry.lock().participants(&room).unwrap()[0].in_call);
    server.cleanup_session_if(&user, "first-transport");
    assert_eq!(server.connected_clients(), 0);
    assert_eq!(server.participants_in_rooms(), 0);
    assert!(matches!(first.try_recv(), Ok(Outbound::Close)));
    let _second = insert("second-transport");
    server
        .inner
        .registry
        .lock()
        .join(&room, user.clone(), "Ana", 2)
        .unwrap();
    // A dropped reader from the prior transport must not erase this login.
    drop(crate::SessionGuard {
        server: server.clone(),
        user: user.clone(),
        http_token: "first-transport".into(),
    });
    assert!(server.session_is_current(&user, "second-transport"));
    assert_eq!(server.participants_in_rooms(), 1);
    drop(crate::SessionGuard {
        server: server.clone(),
        user,
        http_token: "second-transport".into(),
    });
    assert_eq!(server.connected_clients(), 0);
    assert_eq!(server.participants_in_rooms(), 0);
}
#[tokio::test]
async fn expired_sessions_and_known_hashes_are_not_credentials() {
    let (_, repository) = fixture();
    let accounts = Accounts::new(repository.clone());
    let profile = accounts
        .register(AccountRegistration {
            username: "ana".into(),
            password: PASSWORD.into(),
            invite_token: INVITE.into(),
        })
        .await
        .unwrap();
    let token = "aa".repeat(32);
    repository
        .create_account_session(&profile.id, &token_hash(&token), 0)
        .await
        .unwrap();
    assert!(matches!(
        accounts.resolve(&token).await,
        Err(AccountError::Denied)
    ));
    repository
        .create_account_session(&profile.id, &token_hash(&token), u64::MAX)
        .await
        .unwrap();
    assert!(accounts.resolve(&token).await.is_ok());
    assert!(matches!(
        accounts.resolve(&token_hash(&token)).await,
        Err(AccountError::Denied)
    ));
}
#[test]
fn passwords_are_salted_and_validate_exact_unicode_without_trimming() {
    let password = "  senha com café 🦀  ";
    assert!(password::validate(password).is_ok());
    let first = password::hash(password).unwrap();
    let second = password::hash(password).unwrap();
    assert_ne!(first, second);
    assert!(password::verify(password, &first));
    assert!(!password::verify(password.trim(), &first));
    assert!(password::validate("curta").is_err());
    assert!(password::validate(&"x".repeat(129)).is_err());
    for invalid in ["Ana Com Espaço", "a", "ana@teste"] {
        assert!(normalize_username(invalid).is_err());
    }
}

#[tokio::test]
async fn authenticated_account_owns_messages_across_transports_and_installation_keys() {
    let (server, _) = fixture();
    let profile = AccountProfile {
        id: UserId::parse("u_account_ana").unwrap(),
        username: "ana".into(),
        display_name: "Ana".into(),
    };
    let room = concord_protocol::RoomId::parse("geral").unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    server.inner.sessions.lock().insert(
        profile.id.clone(),
        Session {
            account: Some(profile.clone()),
            account_session_hash: None,
            http_token: "first".into(),
            http_bucket: TokenBucket::new(5, 20),
            tx,
            display_name: profile.display_name.clone(),
            addr: "127.0.0.1".parse().unwrap(),
            connected_at: Instant::now(),
        },
    );
    server
        .inner
        .registry
        .lock()
        .join(&room, profile.id.clone(), "Ana", 1)
        .unwrap();
    let req=Request::builder().method("POST").uri("/api/rooms/geral/messages").header("content-type","application/json").header("authorization","Bearer first").header("x-concord-user",profile.id.as_str()).header("x-concord-author","aa".repeat(32)).body(Body::from(serde_json::json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"mensagem da conta"}).to_string())).unwrap();
    let response = build_router(server.clone()).oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(receipt["message"]["author_id"], owner_id(&profile));
    let id = receipt["message"]["id"].as_u64().unwrap();
    server
        .inner
        .sessions
        .lock()
        .get_mut(&profile.id)
        .unwrap()
        .http_token = "fresh".into();
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/rooms/geral/messages/{id}"))
        .header("content-type", "application/json")
        .header("authorization", "Bearer fresh")
        .header("x-concord-user", profile.id.as_str())
        .header("x-concord-author", "bb".repeat(32))
        .body(Body::from(r#"{"text":"editada de outro computador"}"#))
        .unwrap();
    assert_eq!(
        build_router(server.clone())
            .oneshot(req)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let other = UserId::parse("u_account_other").unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    server.inner.sessions.lock().insert(
        other.clone(),
        Session {
            account: Some(AccountProfile {
                id: other.clone(),
                username: "other".into(),
                display_name: "Ana".into(),
            }),
            account_session_hash: None,
            http_token: "other".into(),
            http_bucket: TokenBucket::new(5, 20),
            tx,
            display_name: "Ana".into(),
            addr: "127.0.0.1".parse().unwrap(),
            connected_at: Instant::now(),
        },
    );
    server
        .inner
        .registry
        .lock()
        .join(&room, other.clone(), "Ana", 1)
        .unwrap();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/rooms/geral/messages/{id}"))
        .header("authorization", "Bearer other")
        .header("x-concord-user", other.as_str())
        .header("x-concord-author", owner_id(&profile))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        build_router(server).oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn failed_login_throttling_is_scoped_to_username_and_source() {
    let (server, _) = fixture();
    let now = Instant::now();
    let mut failures = crate::rate_limit::AuthFailures::new(now);
    for _ in 0..5 {
        failures.record_failure(now);
    }
    server
        .inner
        .account_failures
        .lock()
        .insert("127.0.0.1:ana".into(), failures);
    assert_eq!(
        request(
            &server,
            "/api/accounts/login",
            "POST",
            serde_json::json!({"username":"ANA","password":PASSWORD}),
            None
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        request(
            &server,
            "/api/accounts/login",
            "POST",
            serde_json::json!({"username":"outro","password":PASSWORD}),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_closed_old_socket_cannot_take_over_an_account_after_slow_authentication() {
    let mut closed = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(
        axum::extract::ws::Message::Close(None),
    )]);
    assert_eq!(
        crate::authenticate_while_open(std::future::ready(7), &mut closed).await,
        None
    );
    let mut controls_then_close = futures_util::stream::iter(vec![
        Ok::<_, std::io::Error>(axum::extract::ws::Message::Ping(vec![].into())),
        Ok(axum::extract::ws::Message::Close(None)),
    ]);
    assert_eq!(
        crate::authenticate_while_open(std::future::ready(8), &mut controls_then_close).await,
        None
    );
    let mut healthy =
        futures_util::stream::pending::<Result<axum::extract::ws::Message, std::io::Error>>();
    assert_eq!(
        crate::authenticate_while_open(std::future::ready(9), &mut healthy).await,
        Some(9)
    );
}
