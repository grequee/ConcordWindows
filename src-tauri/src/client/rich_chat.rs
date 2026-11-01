use super::chat::{client, read_json};
use super::*;
use concord_protocol::{ChatEdit, ChatPage, DeletedMessage};

impl DesktopState {
    pub async fn edit_chat_message<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        id: u64,
        text: String,
    ) -> Result<(), ClientError> {
        let access = self.chat_access()?;
        let mut url = access.endpoint.clone();
        url.set_path(&format!("{}/{id}", url.path()));
        let response = client()?
            .patch(url)
            .bearer_auth(&access.token)
            .header("x-concord-user", &access.user)
            .header("x-concord-author", &access.author)
            .json(&ChatEdit { text })
            .send()
            .await
            .map_err(|_| {
                ClientError::InvalidMessage(
                    "Não foi possível salvar a edição. Tente novamente.".into(),
                )
            })?;
        let message: ChatMessage = read_json(response).await?;
        self.update_for_generation(access.generation, app, |snapshot| {
            snapshot.update_chat(&message, false)
        });
        Ok(())
    }
    pub async fn delete_chat_message<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        id: u64,
    ) -> Result<(), ClientError> {
        let access = self.chat_access()?;
        let mut url = access.endpoint.clone();
        url.set_path(&format!("{}/{id}", url.path()));
        let response = client()?
            .delete(url)
            .bearer_auth(&access.token)
            .header("x-concord-user", &access.user)
            .header("x-concord-author", &access.author)
            .send()
            .await
            .map_err(|_| {
                ClientError::InvalidMessage(
                    "Não foi possível apagar a mensagem. Tente novamente.".into(),
                )
            })?;
        let deleted: Vec<DeletedMessage> = read_json(response).await?;
        self.update_for_generation(access.generation, app, |snapshot| {
            snapshot.forget_chat(&deleted)
        });
        Ok(())
    }
    pub async fn search_chat(
        &self,
        query: String,
        before: Option<u64>,
    ) -> Result<ChatPage, ClientError> {
        let access = self.chat_access()?;
        let mut url = access.endpoint.clone();
        url.set_path(&format!("{}/search", url.path()));
        let mut request = client()?
            .get(url)
            .bearer_auth(&access.token)
            .header("x-concord-user", &access.user)
            .query(&[("q", query)]);
        if let Some(before) = before {
            request = request.query(&[("before", before)]);
        }
        read_json(request.send().await.map_err(|_| {
            ClientError::InvalidMessage("Não foi possível buscar no histórico.".into())
        })?)
        .await
    }
    pub async fn get_chat_message(&self, id: u64) -> Result<ChatMessage, ClientError> {
        let access = self.chat_access()?;
        let mut url = access.endpoint.clone();
        url.set_path(&format!("{}/{id}", url.path()));
        read_json(
            client()?
                .get(url)
                .bearer_auth(&access.token)
                .header("x-concord-user", &access.user)
                .send()
                .await
                .map_err(|_| {
                    ClientError::InvalidMessage("Não foi possível abrir a mensagem citada.".into())
                })?,
        )
        .await
    }
}
