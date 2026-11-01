#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use parking_lot::Mutex as SyncMutex;
use std::collections::BTreeMap;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aVxkAAAAASUVORK5CYII=";

#[derive(Default, Clone)]
struct Data {
    rows: Vec<(Draft, ChatMessage)>,
    garbage: BTreeMap<String, u64>,
    staged: BTreeMap<String, u64>,
    next_id: u64,
    garbage_cursor: BTreeMap<String, u64>,
    evictions: usize,
    fail_append: Option<StorageError>,
}
#[derive(Default)]
struct MemoryDb(SyncMutex<Data>);

fn object_bytes(message: &ChatMessage) -> u64 {
    message.attachment.as_ref().map_or_else(
        || message.image.as_ref().map_or(0, |image| image.bytes),
        |file| file.bytes,
    )
}

fn removed(data: &mut Data, index: usize) -> DeletedMessage {
    let (draft, message) = data.rows.remove(index);
    if let Some(key) = draft.key {
        data.garbage.insert(key, object_bytes(&message));
    }
    DeletedMessage {
        room_id: message.room_id,
        id: message.id,
    }
}

#[async_trait]
impl Repository for MemoryDb {
    async fn find(&self, room: &RoomId, request: Uuid) -> Result<Option<Existing>, StorageError> {
        Ok(self
            .0
            .lock()
            .rows
            .iter()
            .find(|(draft, _)| &draft.room == room && draft.request == request)
            .map(|(draft, message)| Existing {
                message: message.clone(),
                hash: draft.hash.clone(),
            }))
    }
    async fn append(&self, draft: &Draft, limits: Limits) -> Result<Stored, StorageError> {
        let mut guard = self.0.lock();
        if let Some(error) = guard.fail_append.take() {
            return Err(error);
        }
        let mut data = guard.clone();
        let cost = |message: &ChatMessage| serde_json::to_vec(message).unwrap().len() as u64 + 4096;
        let mut deleted = vec![];
        // This repository double models logical admission. The PostgreSQL
        // function itself must also be validated against a real Neon branch.
        let mut planned_reclaimed = 0;
        loop {
            let bytes = data
                .rows
                .iter()
                .map(|(_, message)| cost(message))
                .sum::<u64>();
            let images = data
                .rows
                .iter()
                .map(|(_, message)| object_bytes(message))
                .sum::<u64>()
                + data.garbage.values().sum::<u64>()
                - planned_reclaimed
                + data
                    .staged
                    .iter()
                    .filter(|(key, _)| Some(key.as_str()) != draft.key.as_deref())
                    .map(|(_, size)| *size)
                    .sum::<u64>();
            if bytes + cost(&draft.message) <= limits.chat_bytes
                && images + object_bytes(&draft.message) <= limits.image_bytes
                && data.rows.len() < limits.messages as usize
            {
                break;
            }
            if !guard.garbage.is_empty() || data.rows.is_empty() {
                return Err(StorageError::Unavailable);
            }
            planned_reclaimed += object_bytes(&data.rows[0].1);
            deleted.push(removed(&mut data, 0));
        }
        data.next_id += 1;
        let mut message = draft.message.clone();
        message.id = data.next_id;
        if let Some(key) = &draft.key {
            data.staged.remove(key);
        }
        data.rows.push((draft.clone(), message.clone()));
        *guard = data;
        Ok(Stored {
            message,
            deleted,
            created: true,
        })
    }
    async fn evict_one(&self, images_only: bool) -> Result<Vec<DeletedMessage>, StorageError> {
        let mut data = self.0.lock();
        if !data.garbage.is_empty() {
            return Err(StorageError::Unavailable);
        }
        let index = data
            .rows
            .iter()
            .position(|(d, _)| !images_only || d.key.is_some());
        if let Some(index) = index {
            data.evictions += 1;
            Ok(vec![removed(&mut data, index)])
        } else {
            Ok(vec![])
        }
    }
    async fn page(&self, room: &RoomId, before: Option<u64>) -> Result<ChatPage, StorageError> {
        Ok(ChatPage {
            messages: self
                .0
                .lock()
                .rows
                .iter()
                .filter(|(_, m)| &m.room_id == room && before.is_none_or(|id| m.id < id))
                .map(|(_, m)| m.clone())
                .collect(),
            has_more: false,
        })
    }
    async fn image(&self, room: &RoomId, id: u64) -> Result<(String, ChatImage), StorageError> {
        self.0
            .lock()
            .rows
            .iter()
            .find(|(_, m)| &m.room_id == room && m.id == id)
            .and_then(|(d, m)| Some((d.key.clone()?, m.image.clone()?)))
            .ok_or(StorageError::NotFound)
    }
    async fn attachment(
        &self,
        room: &RoomId,
        id: u64,
    ) -> Result<(String, ChatAttachment), StorageError> {
        self.0
            .lock()
            .rows
            .iter()
            .find(|(_, m)| &m.room_id == room && m.id == id)
            .and_then(|(d, m)| Some((d.key.clone()?, m.attachment.clone()?)))
            .ok_or(StorageError::NotFound)
    }
    async fn advance_garbage(&self, key: &str, cursor: u64) -> Result<(), StorageError> {
        self.0.lock().garbage_cursor.insert(key.to_owned(), cursor);
        Ok(())
    }
    async fn stage(&self, blob: &Blob) -> Result<(), StorageError> {
        self.0.lock().staged.insert(blob.key.clone(), blob.bytes);
        Ok(())
    }
    async fn abandon(&self, key: &str) -> Result<(), StorageError> {
        let mut data = self.0.lock();
        if !data.rows.iter().any(|(d, _)| d.key.as_deref() == Some(key))
            && let Some(size) = data.staged.remove(key)
        {
            data.garbage.insert(key.to_owned(), size);
        }
        Ok(())
    }
    async fn reap_staged(&self) -> Result<(), StorageError> {
        Ok(())
    }
    async fn garbage(&self) -> Result<Vec<Blob>, StorageError> {
        let data = self.0.lock();
        Ok(data
            .garbage
            .iter()
            .map(|(key, bytes)| Blob {
                key: key.clone(),
                bytes: *bytes,
                cursor: data.garbage_cursor.get(key).copied().unwrap_or(0),
            })
            .collect())
    }

    async fn deleted_blob(&self, key: &str) -> Result<(), StorageError> {
        self.0.lock().garbage.remove(key);
        Ok(())
    }
}

#[derive(Default)]
struct MemoryObjects {
    objects: SyncMutex<BTreeMap<String, Vec<u8>>>,
    fail_put: SyncMutex<Option<StorageError>>,
    fail_delete: SyncMutex<bool>,
    fail_delete_key: SyncMutex<Option<String>>,
    max_objects: SyncMutex<usize>,
    puts: SyncMutex<usize>,
}
#[async_trait]
impl Objects for MemoryObjects {
    async fn put(&self, key: &str, bytes: Vec<u8>, _: &str) -> Result<(), StorageError> {
        *self.puts.lock() += 1;
        if let Some(error) = self.fail_put.lock().clone() {
            return Err(error);
        }
        let mut objects = self.objects.lock();
        let max = *self.max_objects.lock();
        if max > 0 && objects.len() >= max && !objects.contains_key(key) {
            return Err(StorageError::Capacity);
        }
        objects.insert(key.to_owned(), bytes);
        Ok(())
    }
    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.objects
            .lock()
            .get(key)
            .cloned()
            .ok_or(StorageError::NotFound)
    }
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        if *self.fail_delete.lock() || self.fail_delete_key.lock().as_deref() == Some(key) {
            return Err(StorageError::Unavailable);
        }
        self.objects.lock().remove(key);
        Ok(())
    }
}

fn fixture(count: u64) -> (ChatStorage, Arc<MemoryDb>, Arc<MemoryObjects>) {
    let db = Arc::new(MemoryDb::default());
    let objects = Arc::new(MemoryObjects::default());
    let store = ChatStorage {
        db: db.clone(),
        objects: Some(objects.clone()),
        limits: Limits {
            chat_bytes: 128 * 1024 * 1024,
            image_bytes: 4_000_000_000,
            messages: count,
        },
        writes: Mutex::new(()),
        uploads: Mutex::new(Default::default()),
    };
    (store, db, objects)
}
fn input(text: &str, image: bool) -> ChatSubmission {
    ChatSubmission {
        reply_to: None,
        request_id: Uuid::new_v4().to_string(),
        text: text.to_owned(),
        image: image.then(|| concord_protocol::ImageUpload {
            data: PNG.to_owned(),
        }),
    }
}
async fn send(
    store: &ChatStorage,
    input: ChatSubmission,
) -> Result<Saved, (StorageError, Vec<DeletedMessage>)> {
    store
        .append(
            RoomId::parse("geral").unwrap(),
            UserId::parse("u_ana").unwrap(),
            "Ana".to_owned(),
            input,
        )
        .await
}

#[tokio::test]
async fn idempotent_retry_keeps_one_durable_message_and_one_image() {
    let (store, db, objects) = fixture(10);
    let request = input("foto", true);
    let first = send(&store, request.clone())
        .await
        .unwrap_or_else(|_| panic!("save"));
    let again = send(&store, request)
        .await
        .unwrap_or_else(|_| panic!("retry"));
    assert_eq!(first.message.id, again.message.id);
    assert!(!again.created);
    assert_eq!(db.0.lock().rows.len(), 1);
    assert_eq!(*objects.puts.lock(), 1);
    assert_eq!(
        store
            .get_image(&RoomId::parse("geral").unwrap(), first.message.id)
            .await
            .unwrap()
            .0,
        STANDARD.decode(PNG).unwrap()
    );
}

#[tokio::test]
async fn oldest_messages_and_their_images_are_removed_at_the_configured_limit() {
    let (store, db, objects) = fixture(2);
    let first = send(&store, input("antiga", true))
        .await
        .unwrap_or_else(|_| panic!("first"));
    send(&store, input("segunda", false))
        .await
        .unwrap_or_else(|_| panic!("second"));
    let saved = send(&store, input("nova", false))
        .await
        .unwrap_or_else(|_| panic!("new"));
    assert_eq!(saved.deleted[0].id, first.message.id);
    assert_eq!(db.0.lock().rows.len(), 2);
    assert!(objects.objects.lock().is_empty());
    assert!(db.0.lock().garbage.is_empty());
}

#[tokio::test]
async fn unavailable_services_do_not_purge_old_messages() {
    let (store, db, objects) = fixture(2);
    send(&store, input("preservar", false))
        .await
        .unwrap_or_else(|_| panic!("first"));
    *objects.fail_put.lock() = Some(StorageError::Unavailable);
    assert!(matches!(
        send(&store, input("foto", true)).await,
        Err((StorageError::Unavailable, _))
    ));
    db.0.lock().fail_append = Some(StorageError::Unavailable);
    assert!(matches!(
        send(&store, input("texto", false)).await,
        Err((StorageError::Unavailable, _))
    ));
    assert_eq!(db.0.lock().rows.len(), 1);
    assert_eq!(db.0.lock().evictions, 0);
}

#[tokio::test]
async fn actual_bucket_capacity_evicts_an_old_image_then_retries() {
    let (store, db, objects) = fixture(10);
    *objects.max_objects.lock() = 1;
    let old = send(&store, input("antiga", true))
        .await
        .unwrap_or_else(|_| panic!("old"));
    let saved = send(&store, input("nova", true))
        .await
        .unwrap_or_else(|_| panic!("new"));
    assert_eq!(saved.deleted[0].id, old.message.id);
    assert_eq!(db.0.lock().evictions, 1);
    assert_eq!(objects.objects.lock().len(), 1);
}

#[tokio::test]
async fn failed_blob_deletion_stays_in_outbox_and_blocks_further_eviction() {
    let (store, db, objects) = fixture(1);
    send(&store, input("antiga", true))
        .await
        .unwrap_or_else(|_| panic!("old"));
    *objects.fail_delete.lock() = true;
    send(&store, input("nova", false))
        .await
        .unwrap_or_else(|_| panic!("new"));
    assert_eq!(db.0.lock().garbage.len(), 1);
    assert!(send(&store, input("outra", false)).await.is_err());
    assert_eq!(db.0.lock().rows[0].1.text, "nova");
    *objects.fail_delete.lock() = false;
    store.maintenance().await;
    assert!(db.0.lock().garbage.is_empty());
    assert!(objects.objects.lock().is_empty());
}

#[tokio::test]
async fn a_provider_that_does_not_reclaim_space_cannot_empty_the_entire_history() {
    let (store, db, objects) = fixture(20);
    for _ in 0..8 {
        send(&store, input("antiga", true))
            .await
            .unwrap_or_else(|_| panic!("seed"));
    }
    *objects.fail_put.lock() = Some(StorageError::Capacity);
    assert!(matches!(
        send(&store, input("nova", true)).await,
        Err((StorageError::Capacity, _))
    ));
    assert_eq!(db.0.lock().evictions, 3);
    assert_eq!(db.0.lock().rows.len(), 5);
}

#[tokio::test]
async fn confirmed_database_capacity_allows_a_bounded_cleanup_retry() {
    let (store, db, _) = fixture(10);
    send(&store, input("antiga", false))
        .await
        .unwrap_or_else(|_| panic!("old"));
    db.0.lock().fail_append = Some(StorageError::Capacity);
    let saved = send(&store, input("nova", false))
        .await
        .unwrap_or_else(|_| panic!("new"));
    assert_eq!(saved.deleted.len(), 1);
    assert_eq!(db.0.lock().evictions, 1);
    assert_eq!(db.0.lock().rows.len(), 1);
}

#[tokio::test]
async fn malformed_and_oversized_uploads_do_not_delete_history() {
    let (store, db, _) = fixture(1);
    send(&store, input("preservar", false))
        .await
        .unwrap_or_else(|_| panic!("old"));
    for bytes in [
        b"<svg onload='alert(1)'/>".to_vec(),
        vec![0; MAX_CHAT_IMAGE_BYTES + 1],
    ] {
        let mut request = input("", true);
        request.image.as_mut().unwrap().data = STANDARD.encode(bytes);
        assert!(matches!(
            send(&store, request).await,
            Err((StorageError::Invalid(_), _))
        ));
    }
    assert_eq!(db.0.lock().rows.len(), 1);
    assert_eq!(db.0.lock().evictions, 0);
}

#[test]
fn free_tier_settings_validate_without_exposing_credentials() {
    let url = "postgresql://owner:very-private-password@ep-unit-pooler.us-east-2.aws.neon.tech/neondb?sslmode=require";
    let settings = StorageSettings::read(|key| (key == "DATABASE_URL").then(|| url.to_owned()))
        .unwrap()
        .unwrap();
    assert_eq!(settings.limits.image_bytes, 4_000_000_000);
    assert_eq!(
        neon::endpoint(url).unwrap().as_str(),
        "https://api.us-east-2.aws.neon.tech/sql"
    );
    let error = StorageSettings::read(|key| {
        (key == "DATABASE_URL")
            .then(|| "postgresql://owner:very-private-password@evil.example/db".to_owned())
    })
    .err()
    .unwrap();
    assert!(!error.to_string().contains("very-private-password"));
}

fn file_input(name: &str, bytes: u64) -> concord_protocol::UploadStart {
    concord_protocol::UploadStart {
        reply_to: None,
        request_id: Uuid::new_v4().to_string(),
        text: String::new(),
        name: name.to_owned(),
        bytes,
    }
}

#[tokio::test]
async fn arbitrary_file_larger_than_ten_mib_uploads_resumes_and_downloads_in_parts() {
    let (store, db, objects) = fixture(20);
    let room = RoomId::parse("geral").unwrap();
    let user = UserId::parse("u_ana").unwrap();
    let input = file_input("documento.exe", 12 * CHAT_PART_BYTES as u64 + 17);
    let (ready, _) = store
        .begin_upload(room.clone(), user.clone(), "Ana".into(), input.clone())
        .await
        .unwrap_or_else(|_| panic!("start"));
    let chunk = vec![0x86; CHAT_PART_BYTES];
    store
        .upload_part(&room, &user, &ready.upload_id, 0, chunk.clone())
        .await
        .unwrap_or_else(|_| panic!("part"));
    store
        .upload_part(&room, &user, &ready.upload_id, 0, chunk.clone())
        .await
        .unwrap_or_else(|_| panic!("retry part"));
    assert_eq!(*objects.puts.lock(), 1);
    assert!(matches!(
        store
            .upload_part(&room, &user, &ready.upload_id, 0, vec![0; CHAT_PART_BYTES])
            .await,
        Err((StorageError::Conflict, _))
    ));
    let (resumed, _) = store
        .begin_upload(room.clone(), user.clone(), "Ana".into(), input.clone())
        .await
        .unwrap_or_else(|_| panic!("resume"));
    assert_eq!(resumed.next_part, 1);
    for part in 1..13 {
        let bytes = if part == 12 {
            vec![7; 17]
        } else {
            chunk.clone()
        };
        store
            .upload_part(&room, &user, &ready.upload_id, part, bytes)
            .await
            .unwrap_or_else(|_| panic!("part"));
    }
    let saved = store
        .finish_upload(&room, &user, &ready.upload_id)
        .await
        .unwrap_or_else(|_| panic!("finish"));
    assert_eq!(
        saved.message.attachment.as_ref().unwrap().name,
        "documento.exe"
    );
    assert_eq!(
        saved.message.attachment.as_ref().unwrap().bytes,
        input.bytes
    );
    assert!(saved.message.image.is_none());
    assert_eq!(
        store
            .attachment_part(&room, saved.message.id, 0)
            .await
            .unwrap(),
        chunk
    );
    assert_eq!(
        store
            .attachment_part(&room, saved.message.id, 12)
            .await
            .unwrap(),
        vec![7; 17]
    );
    assert!(matches!(
        store
            .attachment_part(&RoomId::parse("outra").unwrap(), saved.message.id, 0)
            .await,
        Err(StorageError::NotFound)
    ));
    let (again, _) = store
        .begin_upload(room, user, "Ana".into(), input)
        .await
        .unwrap_or_else(|_| panic!("confirmed retry"));
    assert_eq!(again.receipt.unwrap().message.id, saved.message.id);
    assert_eq!(db.0.lock().rows.len(), 1);
    assert!(db.0.lock().staged.is_empty());
    assert_eq!(objects.objects.lock().len(), 13);
}

#[tokio::test]
async fn every_file_type_and_empty_files_are_allowed_and_parts_are_validated() {
    let (store, db, _) = fixture(20);
    let room = RoomId::parse("geral").unwrap();
    let user = UserId::parse("u_ana").unwrap();
    for name in [
        "documento.pdf",
        "pasta.zip",
        "arquivo.svg",
        "programa.exe",
        "sem-extensao",
        ".gitignore",
    ] {
        let (ready, _) = store
            .begin_upload(
                room.clone(),
                user.clone(),
                "Ana".into(),
                file_input(name, 0),
            )
            .await
            .unwrap_or_else(|_| panic!("start"));
        assert!(
            store
                .upload_part(&room, &user, &ready.upload_id, 1, vec![])
                .await
                .is_err()
        );
        assert!(
            store
                .upload_part(
                    &room,
                    &UserId::parse("u_outro").unwrap(),
                    &ready.upload_id,
                    0,
                    vec![]
                )
                .await
                .is_err()
        );
        assert!(
            store
                .finish_upload(&room, &user, &ready.upload_id)
                .await
                .is_err()
        );
        store
            .upload_part(&room, &user, &ready.upload_id, 0, vec![])
            .await
            .unwrap_or_else(|_| panic!("empty part"));
        let saved = store
            .finish_upload(&room, &user, &ready.upload_id)
            .await
            .unwrap_or_else(|_| panic!("finish"));
        assert_eq!(saved.message.attachment.unwrap().name, name);
    }
    assert_eq!(db.0.lock().rows.len(), 6);
}

#[tokio::test]
async fn retention_removes_all_parts_of_a_file_and_small_images_still_have_preview() {
    let (store, db, objects) = fixture(1);
    let room = RoomId::parse("geral").unwrap();
    let user = UserId::parse("u_ana").unwrap();
    let bytes = STANDARD.decode(PNG).unwrap();
    let (ready, _) = store
        .begin_upload(
            room.clone(),
            user.clone(),
            "Ana".into(),
            file_input("foto.png", bytes.len() as u64),
        )
        .await
        .unwrap_or_else(|_| panic!("start"));
    store
        .upload_part(&room, &user, &ready.upload_id, 0, bytes.clone())
        .await
        .unwrap_or_else(|_| panic!("part"));
    let saved = store
        .finish_upload(&room, &user, &ready.upload_id)
        .await
        .unwrap_or_else(|_| panic!("finish"));
    assert!(saved.message.image.is_some());
    assert_eq!(
        store.get_image(&room, saved.message.id).await.unwrap().0,
        bytes
    );
    send(&store, input("substituir", false))
        .await
        .unwrap_or_else(|_| panic!("replace"));
    assert!(db.0.lock().garbage.is_empty());
    assert!(objects.objects.lock().is_empty());
}

#[tokio::test]
async fn deletion_of_large_files_checkpoints_progress_and_resumes_after_failure() {
    let (store, db, objects) = fixture(10);
    let key = "concord-images/test-file/";
    db.0.lock()
        .garbage
        .insert(key.to_owned(), 35 * CHAT_PART_BYTES as u64);
    for part in 0..35 {
        objects
            .objects
            .lock()
            .insert(format!("{key}{part}"), vec![1]);
    }
    *objects.fail_delete_key.lock() = Some(format!("{key}32"));
    store.maintenance().await;
    assert_eq!(db.0.lock().garbage_cursor.get(key), Some(&32));
    assert!(objects.objects.lock().contains_key(&format!("{key}32")));
    *objects.fail_delete_key.lock() = None;
    store.maintenance().await;
    assert!(db.0.lock().garbage.is_empty());
    assert!(objects.objects.lock().is_empty());
}

#[tokio::test]
async fn storage_budget_is_the_only_file_size_constraint_and_impossible_uploads_do_not_purge() {
    let (store, db, objects) = fixture(20);
    send(&store, input("preservar", false))
        .await
        .unwrap_or_else(|_| panic!("seed"));
    let room = RoomId::parse("geral").unwrap();
    let user = UserId::parse("u_ana").unwrap();
    assert!(
        store
            .begin_upload(
                room.clone(),
                user.clone(),
                "Ana".into(),
                file_input("grande.bin", 500_000_000)
            )
            .await
            .is_ok()
    );
    assert!(matches!(
        store
            .begin_upload(
                room,
                user,
                "Ana".into(),
                file_input("nao-cabe.bin", 4_000_000_001)
            )
            .await,
        Err((StorageError::Invalid(_), _))
    ));
    assert_eq!(db.0.lock().rows.len(), 1);
    assert_eq!(db.0.lock().evictions, 0);
    assert_eq!(*objects.puts.lock(), 0);
}
