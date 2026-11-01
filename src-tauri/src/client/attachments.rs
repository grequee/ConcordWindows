//! Bounded transfer requests; files never cross IPC or HTTP as one buffer.
use super::chat::{Access, client, read_bytes, read_json, response_error};
use super::{ClientError, DesktopState, ServerMessage};
use base64::{Engine, engine::general_purpose::STANDARD};
use concord_protocol::{CHAT_PART_BYTES, ChatReceipt, UploadReady, UploadStart, attachment_name};
use serde::Serialize;
use std::path::Path;
use tauri::{Emitter, Manager, Runtime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Serialize)]
pub struct DroppedAttachmentInfo {
    pub name: String,
    pub bytes: u64,
}

fn error(text: &str) -> ClientError {
    ClientError::InvalidMessage(text.to_owned())
}

fn upload_url(access: &Access, id: Option<&str>, suffix: &str) -> Result<url::Url, ClientError> {
    let mut url = access.endpoint.clone();
    let parent = url
        .path()
        .strip_suffix("/messages")
        .ok_or(ClientError::NotConnected)?;
    let path = if let Some(id) = id {
        let id =
            uuid::Uuid::parse_str(id).map_err(|_| error("Identificador de envio inválido."))?;
        format!("{parent}/uploads/{id}{suffix}")
    } else {
        format!("{parent}/uploads")
    };
    url.set_path(&path);
    Ok(url)
}

async fn transfer(
    access: &Access,
    method: reqwest::Method,
    url: url::Url,
    bytes: Vec<u8>,
) -> Result<reqwest::Response, ClientError> {
    let client = client()?;
    for attempt in 0..8 {
        let result = client
            .request(method.clone(), url.clone())
            .bearer_auth(&access.token)
            .header("x-concord-user", &access.user)
            .header("x-concord-author", &access.author)
            .body(bytes.clone())
            .send()
            .await;
        match result {
            Ok(response) if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {}
            Ok(response) => {
                return if response.status().is_success() {
                    Ok(response)
                } else {
                    Err(response_error(response).await)
                };
            }
            Err(_) if attempt == 7 => {
                return Err(error(
                    "A transferência foi interrompida. Tente novamente para continuar o envio.",
                ));
            }
            Err(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(500 * (attempt + 1))).await;
    }
    Err(error("O servidor está ocupado. Aguarde e tente novamente."))
}

impl DesktopState {
    pub async fn inspect_chat_attachment_file(
        &self,
        path: &str,
    ) -> Result<DroppedAttachmentInfo, ClientError> {
        let path = Path::new(path);
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|_| error("Não foi possível acessar esse arquivo."))?;
        if !metadata.is_file() {
            return Err(error("Solte um arquivo, não uma pasta."));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| error("Não foi possível ler o nome desse arquivo."))?;
        Ok(DroppedAttachmentInfo {
            name: name.to_owned(),
            bytes: metadata.len(),
        })
    }

    pub async fn send_chat_attachment_path<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        path: &str,
        request_id: String,
        text: String,
        reply_to: Option<u64>,
    ) -> Result<(), ClientError> {
        let info = self.inspect_chat_attachment_file(path).await?;
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|_| error("Não foi possível abrir esse arquivo para envio."))?;
        let ready = self
            .start_chat_attachment(
                app,
                UploadStart {
                    reply_to,
                    request_id: request_id.clone(),
                    text,
                    name: info.name,
                    bytes: info.bytes,
                },
            )
            .await?;
        if ready.receipt.is_some() {
            let _ = app.emit(
                "concord:upload-progress",
                serde_json::json!({"id":request_id,"percent":100}),
            );
            return Ok(());
        }
        if ready.part_bytes != CHAT_PART_BYTES {
            return Err(error("O servidor usa uma versão incompatível de anexos."));
        }

        let total_parts = info.bytes.div_ceil(ready.part_bytes as u64).max(1);
        if ready.next_part > total_parts {
            return Err(error("O servidor retornou uma posição de envio inválida."));
        }
        let offset = ready.next_part.saturating_mul(ready.part_bytes as u64);
        tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| error("Não foi possível continuar a leitura do arquivo."))?;
        for part in ready.next_part..total_parts {
            let part_offset = part * ready.part_bytes as u64;
            let expected = (info.bytes - part_offset).min(ready.part_bytes as u64) as usize;
            let mut bytes = vec![0; expected];
            file.read_exact(&mut bytes)
                .await
                .map_err(|_| error("O arquivo mudou ou não pôde ser lido durante o envio."))?;
            let access = self.chat_access()?;
            transfer(
                &access,
                reqwest::Method::PUT,
                upload_url(&access, Some(&ready.upload_id), &format!("/parts/{part}"))?,
                bytes,
            )
            .await?;
            let percent = ((part + 1) * 100 / total_parts) as u8;
            let _ = app.emit(
                "concord:upload-progress",
                serde_json::json!({"id":request_id,"percent":percent}),
            );
        }
        self.finish_chat_attachment(app, &ready.upload_id).await?;
        Ok(())
    }

    fn attachment_receipt<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        access: &Access,
        receipt: ChatReceipt,
    ) {
        self.update_for_generation(access.generation, app, |snapshot| {
            if snapshot
                .active_room
                .as_ref()
                .is_some_and(|room| room.room.id == access.room)
            {
                snapshot.apply(ServerMessage::Chat {
                    message: receipt.message,
                });
            }
        });
    }

    pub async fn start_chat_attachment<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        input: UploadStart,
    ) -> Result<UploadReady, ClientError> {
        let access = self.chat_access()?;
        if !self.shared.snapshot.lock().attachment_uploads {
            return Err(error(
                "Atualize o servidor para enviar qualquer tipo de anexo.",
            ));
        }
        let response = client()?
            .post(upload_url(&access, None, "")?)
            .bearer_auth(&access.token)
            .header("x-concord-user", &access.user)
            .header("x-concord-author", &access.author)
            .json(&input)
            .send()
            .await
            .map_err(|_| error("Não foi possível iniciar o envio. Tente novamente."))?;
        let ready: UploadReady = read_json(response).await?;
        if ready.part_bytes != CHAT_PART_BYTES {
            return Err(error("O servidor usa uma versão incompatível de anexos."));
        }
        if let Some(receipt) = &ready.receipt {
            self.attachment_receipt(app, &access, receipt.clone());
        }
        Ok(ready)
    }

    pub async fn upload_chat_part(
        &self,
        id: &str,
        part: u64,
        data: &str,
    ) -> Result<(), ClientError> {
        let access = self.chat_access()?;
        if data.len() > CHAT_PART_BYTES.div_ceil(3) * 4 {
            return Err(error("Parte de arquivo inválida."));
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| error("Parte de arquivo inválida."))?;
        if bytes.len() > CHAT_PART_BYTES {
            return Err(error("Parte de arquivo inválida."));
        }
        transfer(
            &access,
            reqwest::Method::PUT,
            upload_url(&access, Some(id), &format!("/parts/{part}"))?,
            bytes,
        )
        .await?;
        Ok(())
    }

    pub async fn finish_chat_attachment<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        id: &str,
    ) -> Result<(), ClientError> {
        let access = self.chat_access()?;
        let response = transfer(
            &access,
            reqwest::Method::POST,
            upload_url(&access, Some(id), "/finish")?,
            vec![],
        )
        .await?;
        self.attachment_receipt(app, &access, read_json(response).await?);
        Ok(())
    }

    pub fn cancel_chat_download(&self, transfer_id: &str) {
        if let Some(cancel) = self.shared.downloads.lock().get(transfer_id) {
            cancel.notify_one();
        }
    }
    pub async fn download_chat_attachment<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        id: u64,
        transfer_id: String,
    ) -> Result<String, ClientError> {
        uuid::Uuid::parse_str(&transfer_id)
            .map_err(|_| error("Identificador de download inválido."))?;
        let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
        {
            let mut downloads = self.shared.downloads.lock();
            if downloads.len() >= 8 || downloads.contains_key(&transfer_id) {
                return Err(error(
                    "Já existem downloads em andamento. Aguarde ou cancele um deles.",
                ));
            }
            downloads.insert(transfer_id.clone(), cancel.clone());
        }
        let result = self
            .download_attachment_inner(app, id, &transfer_id, &cancel)
            .await;
        self.shared.downloads.lock().remove(&transfer_id);
        let (state, path, message) = match &result {
            Ok(path) => ("completed", Some(path.clone()), None),
            Err(cause) => ("failed", None, Some(cause.to_string())),
        };
        let _ = app.emit(
            "concord:download-progress",
            serde_json::json!({"id":transfer_id,"state":state,"path":path,"error":message}),
        );
        result
    }
    async fn download_attachment_inner<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        id: u64,
        transfer_id: &str,
        cancel: &tokio::sync::Notify,
    ) -> Result<String, ClientError> {
        let access = self.chat_access()?;
        let attachment = self
            .shared
            .snapshot
            .lock()
            .active_room
            .as_ref()
            .and_then(|room| room.messages.iter().find(|message| message.id == id))
            .and_then(|message| message.attachment.clone())
            .ok_or_else(|| error("Anexo não encontrado nesta sala."))?;
        let _=app.emit("concord:download-progress",serde_json::json!({"id":transfer_id,"state":"downloading","name":attachment.name,"received":0,"total":attachment.bytes}));
        let folder = app
            .path()
            .download_dir()
            .map_err(|_| error("Não foi possível localizar a pasta Downloads."))?;
        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|_| error("Não foi possível acessar a pasta Downloads."))?;
        let name = attachment_name(&attachment.name);
        let mut destination = None;
        for index in 0..1000 {
            let filename = if index == 0 {
                name.clone()
            } else {
                let path = std::path::Path::new(&name);
                let stem = path.file_stem().and_then(|v| v.to_str()).unwrap_or("anexo");
                let extension = path
                    .extension()
                    .and_then(|v| v.to_str())
                    .map(|v| format!(".{v}"))
                    .unwrap_or_default();
                format!("{stem} ({index}){extension}")
            };
            let path = folder.join(filename);
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&path).await {
                Ok(file) => {
                    destination = Some((path, file));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(error("Não foi possível salvar na pasta Downloads.")),
            }
        }
        let (path, mut file) = destination
            .ok_or_else(|| error("Não foi possível escolher um nome livre em Downloads."))?;
        let result = async {
            for part in 0..attachment.bytes.div_ceil(CHAT_PART_BYTES as u64).max(1) {
                let mut url = access.endpoint.clone();
                url.set_path(&format!("{}/{id}/attachment/parts/{part}", url.path()));
                let bytes = tokio::select! {
                    biased;
                    _=cancel.notified()=>return Err(error("Download cancelado.")),
                    result=async {let response=transfer(&access,reqwest::Method::GET,url,vec![]).await?;read_bytes(response,CHAT_PART_BYTES).await}=>result?,
                };
                if bytes.len() as u64
                    != (attachment.bytes - part * CHAT_PART_BYTES as u64)
                        .min(CHAT_PART_BYTES as u64)
                {
                    return Err(error("O arquivo recebido está incompleto."));
                }
                file.write_all(&bytes).await.map_err(|_| {
                    error("Não foi possível gravar o arquivo. Confira o espaço em disco.")
                })?;
                let received=((part+1)*CHAT_PART_BYTES as u64).min(attachment.bytes);
                let _=app.emit("concord:download-progress",serde_json::json!({"id":transfer_id,"state":"downloading","name":attachment.name,"received":received,"total":attachment.bytes}));
            }
            file.flush()
                .await
                .map_err(|_| error("Não foi possível concluir a gravação do arquivo."))?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(cause) = result {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(cause);
        }
        Ok(path.to_string_lossy().into_owned())
    }
}
