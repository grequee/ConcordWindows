//! Authenticated HTTP for durable text, attachments and paged history. The
//! WebView receives image data URLs, never database or bucket credentials.

use super::{ClientError, DesktopState, LinkStatus, ServerMessage};
use base64::{Engine, engine::general_purpose::STANDARD};
use concord_protocol::{
    ChatPage, ChatReceipt, ChatSubmission, ImageUpload, MAX_CHAT_IMAGE_BYTES, RoomId,
};
use serde::de::DeserializeOwned;

pub(super) struct Access {
    pub(super) author: String,
    pub(super) endpoint: url::Url,
    pub(super) token: String,
    pub(super) user: String,
    pub(super) room: RoomId,
    pub(super) generation: u64,
}

impl DesktopState {
    pub(super) fn chat_access(&self) -> Result<Access, ClientError> {
        let snapshot = self.shared.snapshot.lock();
        if snapshot.status != LinkStatus::Connected {
            return Err(ClientError::NotConnected);
        }
        let chat = snapshot.chat_access.as_ref().ok_or_else(|| {
            ClientError::InvalidMessage(
                "Atualize o servidor para acessar anexos e histórico.".to_owned(),
            )
        })?;
        let room = snapshot
            .active_room
            .as_ref()
            .ok_or_else(|| ClientError::InvalidMessage("Entre em uma sala primeiro.".to_owned()))?
            .room
            .id
            .clone();
        let mut endpoint = url::Url::parse(
            snapshot
                .server_url
                .as_deref()
                .ok_or(ClientError::NotConnected)?,
        )
        .map_err(|_| ClientError::NotConnected)?;
        let scheme = if endpoint.scheme() == "wss" {
            "https"
        } else {
            "http"
        };
        endpoint
            .set_scheme(scheme)
            .map_err(|_| ClientError::NotConnected)?;
        let prefix = endpoint.path().strip_suffix("/ws").unwrap_or("");
        endpoint.set_path(&format!("{prefix}/api/rooms/{room}/messages"));
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        Ok(Access {
            author: self.shared.identity.lock().key.clone(),
            endpoint,
            token: chat.token.clone(),
            user: snapshot
                .self_id
                .as_ref()
                .ok_or(ClientError::NotConnected)?
                .as_str()
                .to_owned(),
            room,
            generation: self
                .shared
                .generation
                .load(std::sync::atomic::Ordering::SeqCst),
        })
    }

    pub(super) async fn post_chat(
        &self,
        text: &str,
        image: Option<ImageUpload>,
        client_id: Option<String>,
        reply_to: Option<u64>,
    ) -> Result<(), ClientError> {
        let access = self.chat_access()?;
        if image
            .as_ref()
            .is_some_and(|image| image.data.len() > MAX_CHAT_IMAGE_BYTES.div_ceil(3) * 4)
        {
            return Err(ClientError::InvalidMessage(
                "A imagem deve ter no máximo 2 MiB.".to_owned(),
            ));
        }
        let input = ChatSubmission {
            reply_to,
            text: text.to_owned(),
            image,
            request_id: client_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        };
        let response = client()?.post(access.endpoint.clone()).bearer_auth(&access.token)
            .header("x-concord-user",&access.user).header("x-concord-author",&access.author).json(&input).send().await
            .map_err(|_| ClientError::InvalidMessage("Não foi possível confirmar o envio. Sua mensagem foi mantida; tente novamente.".to_owned()))?;
        let receipt: ChatReceipt = read_json(response).await?;
        // The server broadcasts only after commit. Its echo and this receipt
        // may arrive in either order; Snapshot deduplicates by durable id.
        let mut snapshot = self.shared.snapshot.lock();
        if self.is_current_generation(access.generation)
            && snapshot
                .active_room
                .as_ref()
                .is_some_and(|room| room.room.id == access.room)
        {
            snapshot.apply(ServerMessage::Chat {
                message: receipt.message,
            });
        }
        Ok(())
    }

    pub async fn load_chat_history<R: tauri::Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
    ) -> Result<(), ClientError> {
        let access = self.chat_access()?;
        let before = self
            .shared
            .snapshot
            .lock()
            .active_room
            .as_ref()
            .and_then(|room| room.messages.first())
            .map(|message| message.id);
        let Some(before) = before else {
            return Ok(());
        };
        let response = client()?
            .get(access.endpoint.clone())
            .bearer_auth(&access.token)
            .header("x-concord-user", &access.user)
            .header("x-concord-author", &access.author)
            .query(&[("before", before)])
            .send()
            .await
            .map_err(|_| {
                ClientError::InvalidMessage(
                    "Não foi possível carregar o histórico. Tente novamente.".to_owned(),
                )
            })?;
        let page: ChatPage = read_json(response).await?;
        self.update_for_generation(access.generation, app, |snapshot| {
            if let Some(room) = &mut snapshot.active_room
                && room.room.id == access.room
            {
                for message in page.messages {
                    if !room.deleted_ids.contains(&message.id)
                        && !room
                            .messages
                            .iter()
                            .any(|existing| existing.id == message.id)
                    {
                        room.messages.push(message);
                    }
                }
                room.messages.sort_by_key(|message| message.id);
                room.history_expanded = true;
                room.has_more = page.has_more;
            }
            snapshot.forget_chat(&[]);
        });
        Ok(())
    }

    pub async fn get_chat_image(&self, id: u64) -> Result<String, ClientError> {
        let access = self.chat_access()?;
        let mut endpoint = access.endpoint.clone();
        endpoint.set_path(&format!("{}/{id}/image", endpoint.path()));
        let response = client()?
            .get(endpoint)
            .bearer_auth(&access.token)
            .header("x-concord-user", &access.user)
            .header("x-concord-author", &access.author)
            .send()
            .await
            .map_err(|_| {
                ClientError::InvalidMessage("Não foi possível carregar a imagem.".to_owned())
            })?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if !matches!(
            content_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp"
        ) {
            return Err(ClientError::InvalidMessage(
                "Formato de imagem inválido.".to_owned(),
            ));
        }
        let bytes = read_bytes(response, MAX_CHAT_IMAGE_BYTES).await?;
        Ok(format!(
            "data:{content_type};base64,{}",
            STANDARD.encode(bytes)
        ))
    }
}

pub(super) fn client() -> Result<reqwest::Client, ClientError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ClientError::ConnectionClosed)
}

pub(super) async fn read_bytes(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ClientError> {
    let mut bytes = vec![];
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ClientError::ConnectionClosed)?
    {
        if bytes.len() + chunk.len() > limit {
            return Err(ClientError::InvalidMessage(
                "Resposta maior que o permitido.".to_owned(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) async fn read_json<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ClientError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    serde_json::from_slice(&read_bytes(response, 1024 * 1024).await?)
        .map_err(|_| ClientError::InvalidMessage("Resposta de chat inválida.".to_owned()))
}

pub(super) async fn response_error(response: reqwest::Response) -> ClientError {
    let message = read_bytes(response, 16 * 1024)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "Não foi possível concluir a operação. Tente novamente.".to_owned());
    ClientError::InvalidMessage(message)
}
