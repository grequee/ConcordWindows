use super::*;
use concord_protocol::ReplyPreview;

impl ChatStorage {
    pub async fn message(&self, room: &RoomId, id: u64) -> Result<ChatMessage, StorageError> {
        self.db.message(room, id).await
    }
    pub(super) async fn reply_preview(
        &self,
        room: &RoomId,
        id: u64,
    ) -> Result<ReplyPreview, StorageError> {
        if id == 0 || id > i64::MAX as u64 {
            return Err(StorageError::Invalid("Mensagem citada inválida."));
        }
        let message = self.db.message(room, id).await?;
        Ok(ReplyPreview {
            id,
            display_name: message.display_name,
            text: message.text.chars().take(180).collect(),
        })
    }
    pub async fn search(
        &self,
        room: &RoomId,
        query: &str,
        before: Option<u64>,
    ) -> Result<ChatPage, StorageError> {
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 100 {
            return Err(StorageError::Invalid(
                "Digite de 1 a 100 caracteres para buscar.",
            ));
        }
        self.db.search(room, query, before).await
    }
    pub async fn edit(
        &self,
        room: &RoomId,
        id: u64,
        user: &UserId,
        author: Option<&str>,
        text: &str,
    ) -> Result<ChatMessage, StorageError> {
        let _write = self.writes.lock().await;
        let text = if text.trim().is_empty() {
            String::new()
        } else {
            normalize_chat_text(text)
                .map_err(|_| StorageError::Invalid("A mensagem deve ter de 1 a 2000 caracteres."))?
        };
        self.db
            .edit(room, id, user, author, &text, self.limits.chat_bytes)
            .await
    }
    pub async fn delete(
        &self,
        room: &RoomId,
        id: u64,
        user: &UserId,
        author: Option<&str>,
    ) -> Result<Vec<DeletedMessage>, StorageError> {
        let _write = self.writes.lock().await;
        let deleted = self.db.delete(room, id, user, author).await?;
        self.flush_garbage().await;
        Ok(deleted)
    }
}
