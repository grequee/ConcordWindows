use super::*;
use crate::{Server, chat_http::ApiError};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

impl From<AccountError> for ApiError {
    fn from(error: AccountError) -> Self {
        Self(
            match error {
                AccountError::Invalid(_) => StatusCode::BAD_REQUEST,
                AccountError::Denied => StatusCode::UNAUTHORIZED,
                AccountError::Conflict => StatusCode::CONFLICT,
                AccountError::Busy => StatusCode::TOO_MANY_REQUESTS,
                AccountError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            },
            error.to_string(),
        )
    }
}
fn service(server: &Server) -> Result<&Accounts, ApiError> {
    server.inner.accounts.as_deref().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "As contas precisam do banco de dados. Atualize o servidor e configure DATABASE_URL."
                .into(),
        )
    })
}
fn ip(value: Option<Extension<ConnectInfo<SocketAddr>>>) -> IpAddr {
    value.map_or(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        |Extension(ConnectInfo(addr))| addr.ip(),
    )
}
fn failure_key(ip: IpAddr, username: &str) -> String {
    format!(
        "{ip}:{}",
        username
            .trim()
            .to_ascii_lowercase()
            .chars()
            .take(64)
            .collect::<String>()
    )
}
fn authorize_attempt(server: &Server, ip: IpAddr, username: &str) -> Result<(), ApiError> {
    let key = failure_key(ip, username);
    if server
        .inner
        .account_failures
        .lock()
        .get(&key)
        .is_some_and(|entry| entry.is_blocked(std::time::Instant::now()))
    {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Muitas tentativas. Aguarde um minuto para tentar novamente.".into(),
        ));
    }
    let mut limits = server.inner.account_attempts.lock();
    if limits.len() >= 4096 && !limits.contains_key(&ip) {
        limits.retain(|_, entry| entry.0.elapsed() < std::time::Duration::from_secs(120));
        if limits.len() >= 4096 {
            return Err(AccountError::Busy.into());
        }
    }
    let entry = limits.entry(ip).or_insert_with(|| {
        (
            std::time::Instant::now(),
            crate::rate_limit::TokenBucket::new(1, 8),
        )
    });
    entry.0 = std::time::Instant::now();
    if !entry.1.allow() {
        return Err(AccountError::Busy.into());
    }
    Ok(())
}
fn bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| AccountError::Denied.into())
}
pub(crate) async fn register(
    State(server): State<Server>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(input): Json<AccountRegistration>,
) -> Result<impl IntoResponse, ApiError> {
    let ip = ip(peer);
    authorize_attempt(&server, ip, &input.username)?;
    if server.is_auth_blocked(ip) || !server.token_is_valid(&input.invite_token) {
        server.record_auth_failure(ip);
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "O convite para criar uma conta é inválido.".into(),
        ));
    }
    let profile = service(&server)?.register(input).await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(profile),
    ))
}
pub(crate) async fn login(
    State(server): State<Server>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(input): Json<AccountLogin>,
) -> Result<impl IntoResponse, ApiError> {
    let ip = ip(peer);
    authorize_attempt(&server, ip, &input.username)?;
    let key = failure_key(ip, &input.username);
    let session = service(&server)?.login(input).await.map_err(|error| {
        if matches!(error, AccountError::Denied) {
            let now = std::time::Instant::now();
            let mut entries = server.inner.account_failures.lock();
            if entries.len() >= 4096 {
                entries.retain(|_, value| !value.expired(now));
            }
            if entries.len() < 4096 || entries.contains_key(&key) {
                entries
                    .entry(key)
                    .or_insert_with(|| crate::rate_limit::AuthFailures::new(now))
                    .record_failure(now);
            }
        }
        ApiError::from(error)
    })?;
    // A successful password attempt does not reset failure throttles shared
    // with other usernames on this source address.
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(session)))
}
pub(crate) async fn me(
    State(server): State<Server>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let session = service(&server)?.resolve(bearer(&headers)?).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({"account":session.account,"expires_at_ms":session.expires_at_ms})),
    ))
}
pub(crate) async fn logout(
    State(server): State<Server>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = bearer(&headers)?;
    let account = service(&server)?.resolve(token).await?;
    service(&server)?.revoke(token).await?;
    let _gate = server.inner.session_gate.lock().await;
    let key = token_hash(token);
    let current = server
        .inner
        .sessions
        .lock()
        .get(&account.account.id)
        .filter(|s| s.account_session_hash.as_deref() == Some(key.as_str()))
        .map(|s| s.http_token.clone());
    if let Some(current) = current {
        server.cleanup_session_if(&account.account.id, &current);
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({"ok":true})),
    ))
}
