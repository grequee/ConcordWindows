use super::*;

pub(super) struct ChatIdentity {
    pub key: String,
    pub public: String,
}
impl Default for ChatIdentity {
    fn default() -> Self {
        let key = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let public = concord_protocol::author_id_from_key(&key).unwrap_or_default();
        Self { key, public }
    }
}
impl DesktopState {
    pub(crate) fn initialize_identity<R: Runtime>(&self, app: &AppHandle<R>) {
        let result = (|| -> Result<ChatIdentity, String> {
            let folder = app
                .path()
                .app_config_dir()
                .map_err(|_| "Pasta de configuração indisponível.".to_owned())?;
            std::fs::create_dir_all(&folder)
                .map_err(|_| "Não foi possível criar a identidade local.".to_owned())?;
            let path = folder.join("chat-identity.json");
            if !path.exists() {
                let identity = ChatIdentity::default();
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                match options.open(&path) {
                    Ok(mut file) => {
                        use std::io::Write;
                        let bytes = serde_json::to_vec(&serde_json::json!({"key":identity.key}))
                            .map_err(|_| "Identidade local inválida.".to_owned())?;
                        file.write_all(&bytes)
                            .and_then(|()| file.sync_all())
                            .map_err(|_| {
                                "Não foi possível salvar a identidade local.".to_owned()
                            })?;
                        return Ok(identity);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err("Não foi possível salvar a identidade local.".to_owned()),
                }
            }
            let bytes = std::fs::read(&path)
                .map_err(|_| "Não foi possível ler a identidade local.".to_owned())?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|_| "Identidade local inválida.".to_owned())?;
            let key = value["key"]
                .as_str()
                .ok_or_else(|| "Identidade local inválida.".to_owned())?
                .to_owned();
            let public = concord_protocol::author_id_from_key(&key)
                .ok_or_else(|| "Identidade local inválida.".to_owned())?;
            Ok(ChatIdentity { key, public })
        })();
        match result {
            Ok(identity) => *self.shared.identity.lock() = identity,
            Err(error) => warn!(%error,"using temporary chat identity"),
        }
    }
}
