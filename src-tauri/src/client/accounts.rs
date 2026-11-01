//! Account passwords are transient inputs. Only an opaque account session is
//! saved in the native config directory, never in WebView storage or snapshots.
use super::*;
use concord_protocol::{AccountLogin, AccountProfile, AccountRegistration, AccountSession};
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub(super) struct AccountState {
    pub(super) pending: Mutex<Option<Arc<tokio::sync::Notify>>>,
    saved: Mutex<Option<SavedLogin>>,
    lifecycle: AsyncMutex<()>,
    persistence: Arc<AsyncMutex<()>>,
}
#[derive(Clone, Serialize, Deserialize)]
struct SavedLogin {
    server_url: String,
    session: AccountSessionCopy,
}
#[derive(Clone, Serialize, Deserialize)]
struct AccountSessionCopy {
    account: AccountProfile,
    token: String,
    expires_at_ms: u64,
}
#[derive(Serialize)]
pub(crate) struct SavedAccountInfo {
    pub account: AccountProfile,
    pub expires_at_ms: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccountAction {
    Register,
    Login,
    Resume,
}
#[derive(Deserialize)]
pub(crate) struct AccountRequest {
    pub server_url: String,
    pub action: AccountAction,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub invite_token: String,
}
#[derive(Deserialize)]
struct SessionInfo {
    account: AccountProfile,
    expires_at_ms: u64,
}

fn error(message: &str) -> ClientError {
    ClientError::InvalidMessage(message.into())
}
fn endpoint(server: &str, action: &str) -> Result<Url, ClientError> {
    let mut url = Url::parse(server).map_err(|_| error("Endereço inválido."))?;
    let tls = url.scheme() == "wss";
    if !tls {
        let host = url.host_str().unwrap_or_default();
        let local = host == "localhost"
            || !host.contains('.') && !host.contains(':')
            || host.ends_with(".local")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| match ip {
                    std::net::IpAddr::V4(ip) => {
                        ip.is_loopback() || ip.is_private() || ip.is_link_local()
                    }
                    std::net::IpAddr::V6(ip) => {
                        ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
                    }
                });
        if !local {
            return Err(error(
                "Use HTTPS para entrar com uma conta em um servidor na internet.",
            ));
        }
    }
    url.set_scheme(if tls { "https" } else { "http" })
        .map_err(|_| error("Endereço inválido."))?;
    let prefix = url.path().strip_suffix("/ws").unwrap_or("");
    url.set_path(&format!("{prefix}/api/accounts/{action}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}
fn client() -> Result<reqwest::Client, ClientError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| error("Não foi possível preparar o acesso à conta."))
}
async fn response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ClientError> {
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(error(
            "Atualize o ConcordServer no Render para ativar cadastro e login.",
        ));
    }
    let bytes = chat::read_bytes(response, 16 * 1024).await?;
    if !status.is_success() {
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "Não foi possível acessar a conta. Tente novamente.".into());
        return Err(ClientError::InvalidMessage(message));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| error("O servidor enviou uma resposta de conta inválida."))
}
fn saved_path<R: Runtime>(app: &AppHandle<R>) -> Result<std::path::PathBuf, ClientError> {
    app.path()
        .app_config_dir()
        .map(|p| p.join("account-session.json"))
        .map_err(|_| error("Pasta de configuração indisponível."))
}
fn save_file(path: &std::path::Path, login: &SavedLogin) -> Result<(), ClientError> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| error("Não foi possível salvar sua sessão neste computador."))?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| error("Não foi possível salvar sua sessão neste computador."))?;
        let bytes = serde_json::to_vec(login).map_err(|_| error("Sessão inválida."))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| error("Não foi possível salvar sua sessão neste computador."))?;
        drop(file);
        std::fs::rename(&temporary, path)
            .map_err(|_| error("Não foi possível salvar sua sessão neste computador."))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
async fn erase_file(
    persistence: Arc<AsyncMutex<()>>,
    path: std::path::PathBuf,
) -> Result<(), ClientError> {
    let permit = persistence.lock_owned().await;
    // A blocking filesystem task cannot be cancelled once it starts. Keep
    // the permit inside that task, so a late save cannot undo a later logout.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(error("Não foi possível apagar a sessão salva. Confira as permissões da pasta de configuração.")),
        }
    }).await.map_err(|_| error("Não foi possível apagar a sessão salva."))?
}
impl DesktopState {
    pub(crate) fn initialize_accounts<R: Runtime>(&self, app: &AppHandle<R>) {
        let saved = (|| -> Option<SavedLogin> {
            let path = saved_path(app).ok()?;
            if std::fs::metadata(&path).ok()?.len() > 16 * 1024 {
                return None;
            }
            let saved: SavedLogin = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            if normalize_signaling_url(&saved.server_url).ok()?.as_str() != saved.server_url
                || saved.session.token.len() != 64
                || !saved.session.token.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return None;
            }
            Some(saved)
        })();
        *self.shared.accounts.saved.lock() = saved;
    }
    pub(crate) fn saved_account(&self, server_url: &str) -> Option<SavedAccountInfo> {
        let url = if server_url.trim().is_empty() {
            None
        } else {
            Some(normalize_signaling_url(server_url).ok()?)
        };
        let saved = self.shared.accounts.saved.lock();
        let saved = saved
            .as_ref()
            .filter(|saved| url.as_ref().is_none_or(|url| &saved.server_url == url))?;
        Some(SavedAccountInfo {
            account: saved.session.account.clone(),
            expires_at_ms: saved.session.expires_at_ms,
        })
    }
    pub(crate) async fn authenticate_account<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        input: AccountRequest,
    ) -> Result<Option<AccountProfile>, ClientError> {
        let _lifecycle = self.shared.accounts.lifecycle.lock().await;
        let server = if matches!(input.action, AccountAction::Resume)
            && input.server_url.trim().is_empty()
        {
            self.shared
                .accounts
                .saved
                .lock()
                .as_ref()
                .map(|saved| saved.server_url.clone())
                .ok_or_else(|| error("Entre com seu usuário e senha."))?
        } else {
            normalize_signaling_url(&input.server_url)?
        };
        self.disconnect(app).await;
        let generation = self.shared.generation.load(Ordering::SeqCst);
        let cancel = Arc::new(tokio::sync::Notify::new());
        *self.shared.accounts.pending.lock() = Some(cancel.clone());
        let registering = matches!(input.action, AccountAction::Register);
        self.update_for_generation(generation, app, |snapshot| {
            snapshot.server_url = Some(server.clone());
            snapshot.status = LinkStatus::Connecting;
            snapshot.connection_message = Some(
                if registering {
                    "Criando sua conta…"
                } else {
                    "Entrando na sua conta…"
                }
                .into(),
            );
            snapshot.connection_attempt = 1;
        });
        let result = tokio::select! {
            biased;
            _=cancel.notified()=>Err(error("Entrada cancelada.")),
            result=self.authenticate_inner(app,&server,input,generation)=>result,
        };
        *self.shared.accounts.pending.lock() = None;
        if result.is_err() || registering {
            self.update_for_generation(generation, app, |snapshot| {
                snapshot.status = LinkStatus::Disconnected;
                snapshot.connection_message = None;
                snapshot.connection_attempt = 0;
            });
        }
        result
    }
    async fn account_request<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        generation: u64,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ClientError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        let mut attempt = 0u32;
        let mut last_error = None;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(last_error.unwrap_or_else(|| {
                    error("O servidor continua indisponível. Tente novamente em alguns instantes.")
                }));
            }
            if !self.is_current_generation(generation) {
                return Err(error("Entrada cancelada."));
            }
            attempt = attempt.saturating_add(1);
            if attempt > 1 {
                self.update_for_generation(generation, app, |snapshot| {
                    snapshot.status = LinkStatus::WaitingForServer;
                    snapshot.connection_attempt = attempt;
                    snapshot.connection_message =
                        Some("Aguardando o servidor para entrar na sua conta…".into());
                });
            }
            let next = request
                .try_clone()
                .ok_or_else(|| error("Não foi possível preparar a entrada."))?;
            match tokio::time::timeout(remaining, next.send()).await {
                Ok(Ok(response))
                    if !matches!(response.status().as_u16(), 408 | 429 | 502 | 503 | 504) =>
                {
                    return Ok(response);
                }
                Ok(Ok(response)) => {
                    let cause = chat::response_error(response).await;
                    if cause.to_string().contains("configure DATABASE_URL") {
                        return Err(cause);
                    }
                    last_error = Some(cause);
                }
                _ => {
                    last_error = Some(error(
                        "Não foi possível alcançar o servidor. Confira a conexão e tente novamente.",
                    ))
                }
            }
            let wait = Duration::from_secs((1u64 << attempt.saturating_sub(1).min(4)).min(15))
                .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
            tokio::time::sleep(wait).await;
        }
    }
    async fn authenticate_inner<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        server: &str,
        input: AccountRequest,
        generation: u64,
    ) -> Result<Option<AccountProfile>, ClientError> {
        let client = client()?;
        let session = match input.action {
            AccountAction::Register => {
                // Wake Render with a read. Never retry an ambiguous signup write.
                let ready = self
                    .account_request(app, generation, client.get(endpoint(server, "me")?))
                    .await?;
                drop(ready);
                let request = AccountRegistration {
                    username: input.username,
                    password: input.password,
                    invite_token: input.invite_token,
                };
                let sent=client.post(endpoint(server,"register")?).json(&request).send().await.map_err(|_|error("Não foi possível criar a conta. O servidor pode estar iniciando; tente novamente."))?;
                let account: AccountProfile = response(sent).await?;
                return Ok(Some(account));
            }
            AccountAction::Login => {
                let sent = self
                    .account_request(
                        app,
                        generation,
                        client.post(endpoint(server, "login")?).json(&AccountLogin {
                            username: input.username,
                            password: input.password,
                        }),
                    )
                    .await?;
                let session: AccountSession = response(sent).await?;
                AccountSessionCopy {
                    account: session.account,
                    token: session.token,
                    expires_at_ms: session.expires_at_ms,
                }
            }
            AccountAction::Resume => {
                let saved = self
                    .shared
                    .accounts
                    .saved
                    .lock()
                    .as_ref()
                    .filter(|saved| saved.server_url == server)
                    .cloned()
                    .ok_or_else(|| error("Entre com seu usuário e senha."))?;
                let sent = self
                    .account_request(
                        app,
                        generation,
                        client
                            .get(endpoint(server, "me")?)
                            .bearer_auth(&saved.session.token),
                    )
                    .await?;
                if sent.status() == reqwest::StatusCode::UNAUTHORIZED {
                    *self.shared.accounts.saved.lock() = None;
                    if let Ok(path) = saved_path(app) {
                        erase_file(self.shared.accounts.persistence.clone(), path).await?;
                    }
                    return Err(error(
                        "Sua sessão expirou. Entre novamente com seu usuário e senha.",
                    ));
                }
                let info: SessionInfo = response(sent).await?;
                if info.account.id != saved.session.account.id {
                    return Err(error(
                        "A identidade salva não corresponde à conta do servidor.",
                    ));
                }
                AccountSessionCopy {
                    account: info.account,
                    expires_at_ms: info.expires_at_ms,
                    ..saved.session
                }
            }
        };
        if !self.is_current_generation(generation) {
            return Err(error("Entrada cancelada."));
        }
        let saved = SavedLogin {
            server_url: server.to_owned(),
            session: session.clone(),
        };
        let path = saved_path(app)?;
        let to_save = saved.clone();
        let permit = self.shared.accounts.persistence.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            save_file(&path, &to_save)
        })
        .await
        .map_err(|_| error("Não foi possível salvar sua sessão."))??;
        *self.shared.accounts.saved.lock() = Some(saved);
        let credentials = connection::Credentials::account(
            server.to_owned(),
            session.account.display_name,
            session.token,
            generation,
        );
        self.connect_transport(
            app,
            credentials,
            connection::WebSocketConnector,
            connection::RetryPolicy::default(),
        )
        .await?;
        Ok(None)
    }
    pub(crate) async fn logout_account<R: Runtime>(
        &self,
        app: &AppHandle<R>,
    ) -> Result<(), ClientError> {
        self.disconnect(app).await;
        let _lifecycle = self.shared.accounts.lifecycle.lock().await;
        let saved = self.shared.accounts.saved.lock().take();
        let path = saved_path(app)?;
        erase_file(self.shared.accounts.persistence.clone(), path).await?;
        if let Some(saved) = saved {
            // Local sign-out is complete even if a dead network prevents the
            // best-effort revocation; the server token still has a fixed expiry.
            if let Ok(client) = reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                && let Ok(url) = endpoint(&saved.server_url, "logout")
            {
                let _ = client
                    .post(url)
                    .bearer_auth(saved.session.token)
                    .send()
                    .await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    fn login() -> SavedLogin {
        SavedLogin {
            server_url: "wss://example.test/ws".into(),
            session: AccountSessionCopy {
                account: AccountProfile {
                    id: UserId::parse("u_account_ana").unwrap(),
                    username: "ana".into(),
                    display_name: "ana".into(),
                },
                token: "aa".repeat(32),
                expires_at_ms: 1_900_000_000_000,
            },
        }
    }
    #[test]
    fn account_session_file_is_private_replaces_safely_and_is_not_a_password_store() {
        let folder =
            std::env::temp_dir().join(format!("concord-account-test-{}", uuid::Uuid::new_v4()));
        let path = folder.join("account-session.json");
        let first = login();
        save_file(&path, &first).unwrap();
        let mut second = login();
        second.session.token = "bb".repeat(32);
        save_file(&path, &second).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let saved: SavedLogin = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved.session.token, second.session.token);
        assert!(!String::from_utf8(bytes).unwrap().contains("password"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn saved_login_is_bound_to_its_server_and_secret_never_enters_public_state() {
        let state = DesktopState::default();
        let saved = login();
        let secret = saved.session.token.clone();
        *state.shared.accounts.saved.lock() = Some(saved);
        assert!(state.saved_account("https://example.test").is_some());
        assert!(state.saved_account("https://other.test").is_none());
        assert!(
            !serde_json::to_string(&state.snapshot())
                .unwrap()
                .contains(&secret)
        );
        assert!(
            !serde_json::to_string(&state.saved_account("https://example.test"))
                .unwrap()
                .contains(&secret)
        );
    }
    #[test]
    fn passwords_and_tokens_require_tls_outside_the_local_network() {
        assert!(endpoint("ws://public.example/ws", "login").is_err());
        assert_eq!(
            endpoint("wss://public.example/base/ws", "login")
                .unwrap()
                .as_str(),
            "https://public.example/base/api/accounts/login"
        );
        assert_eq!(
            endpoint("ws://127.0.0.1:8787/ws", "login")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8787/api/accounts/login"
        );
    }
    #[tokio::test]
    async fn late_uncancellable_save_cannot_recreate_the_file_after_sign_out() {
        let folder = std::env::temp_dir().join(format!(
            "concord-account-logout-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = folder.join("account-session.json");
        let persistence = Arc::new(AsyncMutex::new(()));
        let permit = persistence.clone().lock_owned().await;
        let writing = path.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _ = started_tx.send(());
            release_rx.recv().unwrap();
            save_file(&writing, &login()).unwrap();
        });
        started_rx.await.unwrap();
        worker.abort();
        // Logout queues behind the uncancellable save, then removes its file.
        let deleting = tokio::spawn(erase_file(persistence.clone(), path.clone()));
        tokio::task::yield_now().await;
        assert!(!deleting.is_finished());
        release_tx.send(()).unwrap();
        let _ = worker.await;
        deleting.await.unwrap().unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
