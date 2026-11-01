//! Persistent accounts and opaque revocable sessions. Passwords/tokens never
//! implement Debug and are never logged or broadcast through signaling.
mod http;
mod password;
#[cfg(test)]
mod tests;

use crate::storage::StorageError;
use async_trait::async_trait;
use concord_protocol::{AccountLogin, AccountProfile, AccountRegistration, AccountSession, UserId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const SESSION_LIFETIME_MS: u64 = 30 * 24 * 60 * 60 * 1000;
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredAccount {
    pub profile: AccountProfile,
    pub password_hash: String,
}
#[derive(Clone, Deserialize)]
pub(crate) struct StoredSession {
    pub account: AccountProfile,
    pub expires_at_ms: u64,
}

#[async_trait]
pub(crate) trait Repository: Send + Sync {
    async fn create_account(&self, account: &StoredAccount) -> Result<(), StorageError>;
    async fn find_account(&self, username: &str) -> Result<Option<StoredAccount>, StorageError>;
    async fn create_account_session(
        &self,
        id: &UserId,
        hash: &str,
        expires: u64,
    ) -> Result<(), StorageError>;
    async fn account_session(
        &self,
        hash: &str,
        now: u64,
    ) -> Result<Option<StoredSession>, StorageError>;
    async fn revoke_account_session(&self, hash: &str) -> Result<(), StorageError>;
}

pub(crate) struct Accounts {
    repository: Arc<dyn Repository>,
    slots: Arc<tokio::sync::Semaphore>,
}
impl Accounts {
    pub fn new(repository: Arc<dyn Repository>) -> Self {
        Self {
            repository,
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
    pub async fn register(
        &self,
        input: AccountRegistration,
    ) -> Result<AccountProfile, AccountError> {
        let username = normalize_username(&input.username)?;
        password::validate(&input.password)?;
        // Acquire before spawning, and retain the permit inside the blocking
        // worker even if its HTTP caller disconnects.
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| AccountError::Busy)?;
        let hash = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            password::hash(&input.password)
        })
        .await
        .map_err(|_| AccountError::Unavailable)??;
        let account = StoredAccount {
            profile: AccountProfile {
                id: UserId::parse(format!("u_{}", uuid::Uuid::new_v4().simple()))
                    .map_err(|_| AccountError::Unavailable)?,
                display_name: username.clone(),
                username,
            },
            password_hash: hash,
        };
        self.repository
            .create_account(&account)
            .await
            .map_err(AccountError::from)?;
        Ok(account.profile)
    }
    pub async fn login(&self, input: AccountLogin) -> Result<AccountSession, AccountError> {
        let username = normalize_username(&input.username).map_err(|_| AccountError::Denied)?;
        if input.password.is_empty()
            || input.password.chars().count() > 128
            || input.password.len() > 512
        {
            return Err(AccountError::Denied);
        }
        let stored = self.repository.find_account(&username).await?;
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| AccountError::Busy)?;
        let account = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            // Missing accounts still perform the full work to reduce timing
            // differences between an unknown username and a bad password.
            match stored {
                Some(account) => {
                    if password::verify(&input.password, &account.password_hash) {
                        Ok(account.profile)
                    } else {
                        Err(AccountError::Denied)
                    }
                }
                None => {
                    password::dummy_verify(&input.password);
                    Err(AccountError::Denied)
                }
            }
        })
        .await
        .map_err(|_| AccountError::Unavailable)??;
        let token = password::random_token()?;
        let expires_at_ms = concord_core::now_unix_ms() + SESSION_LIFETIME_MS;
        self.repository
            .create_account_session(&account.id, &token_hash(&token), expires_at_ms)
            .await?;
        Ok(AccountSession {
            account,
            token,
            expires_at_ms,
        })
    }
    pub async fn resolve(&self, token: &str) -> Result<StoredSession, AccountError> {
        if token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(AccountError::Denied);
        }
        self.repository
            .account_session(&token_hash(token), concord_core::now_unix_ms())
            .await?
            .ok_or(AccountError::Denied)
    }
    pub async fn revoke(&self, token: &str) -> Result<(), AccountError> {
        self.repository
            .revoke_account_session(&token_hash(token))
            .await?;
        Ok(())
    }
}

pub(crate) fn token_hash(token: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"concord-account-session-v1\0");
    hash.update(token.as_bytes());
    format!("{:x}", hash.finalize())
}
pub(crate) fn owner_id(account: &AccountProfile) -> String {
    format!("account:{}", account.id)
}
pub(crate) fn normalize_username(value: &str) -> Result<String, AccountError> {
    let value = value.trim().to_ascii_lowercase();
    if !(3..=32).contains(&value.len())
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err(AccountError::Invalid(
            "Use de 3 a 32 caracteres no usuário: letras, números, ponto, hífen ou sublinhado.",
        ));
    }
    Ok(value)
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum AccountError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("Usuário ou senha incorretos, ou sessão expirada.")]
    Denied,
    #[error("Este nome de usuário já está em uso.")]
    Conflict,
    #[error("Há muitas tentativas em andamento. Aguarde alguns segundos.")]
    Busy,
    #[error("Não foi possível acessar as contas. Tente novamente.")]
    Unavailable,
}
impl From<StorageError> for AccountError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Conflict => Self::Conflict,
            StorageError::Invalid(message) => Self::Invalid(message),
            _ => Self::Unavailable,
        }
    }
}
pub(crate) use http::{login, logout, me, register};
