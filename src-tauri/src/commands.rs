use concord_media::MicrophoneSettings;
use concord_rtc::ScreenTier;
use tauri::{AppHandle, State};

use crate::client::{DesktopState, Snapshot};

#[tauri::command]
pub fn get_snapshot(state: State<'_, DesktopState>) -> Snapshot {
    state.snapshot()
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<Vec<concord_media::AudioDeviceInfo>, String> {
    tokio::task::spawn_blocking(concord_media::list_audio_devices)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn set_audio_devices(
    app: AppHandle,
    state: State<'_, DesktopState>,
    settings: concord_media::VoiceAudioConfig,
) -> Result<concord_media::VoiceAudioConfig, String> {
    state
        .inner()
        .clone()
        .set_audio_devices(&app, settings)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub(crate) async fn start_audio_test(
    app: AppHandle,
    state: State<'_, DesktopState>,
    request: crate::client::AudioTestRequest,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .start_audio_test(&app, request)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn stop_audio_test(state: State<'_, DesktopState>) -> Result<(), String> {
    state.inner().clone().stop_audio_test_requested().await;
    Ok(())
}
#[tauri::command]
pub async fn set_participant_volume(
    state: State<'_, DesktopState>,
    user_id: String,
    volume: u16,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .set_participant_volume(&user_id, volume)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_microphone_settings(
    app: AppHandle,
    state: State<'_, DesktopState>,
    settings: MicrophoneSettings,
) -> Result<MicrophoneSettings, String> {
    let state = state.inner().clone();
    state
        .set_microphone_settings(&app, settings)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    state: State<'_, DesktopState>,
    server_url: String,
    display_name: String,
    invite_token: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .connect(&app, &server_url, &display_name, &invite_token)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn disconnect(app: AppHandle, state: State<'_, DesktopState>) -> Result<(), String> {
    let state = state.inner().clone();
    state.disconnect(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn refresh_rooms(state: State<'_, DesktopState>) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .refresh_rooms()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn join_room(
    app: AppHandle,
    state: State<'_, DesktopState>,
    room_id: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .join_room(&app, &room_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn leave_room(app: AppHandle, state: State<'_, DesktopState>) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .leave_room(&app)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn send_chat(
    app: AppHandle,
    state: State<'_, DesktopState>,
    text: String,
    image: Option<concord_protocol::ImageUpload>,
    client_id: Option<String>,
    reply_to: Option<u64>,
) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .send_chat_content(&text, image, client_id, reply_to)
        .await
        .map_err(|error| error.to_string())?;
    state.publish(&app);
    Ok(())
}

#[tauri::command]
pub async fn get_chat_image(state: State<'_, DesktopState>, id: u64) -> Result<String, String> {
    state
        .inner()
        .clone()
        .get_chat_image(id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn load_chat_history(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .load_chat_history(&app)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn start_chat_attachment(
    app: AppHandle,
    state: State<'_, DesktopState>,
    input: concord_protocol::UploadStart,
) -> Result<concord_protocol::UploadReady, String> {
    state
        .inner()
        .clone()
        .start_chat_attachment(&app, input)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn inspect_chat_attachment_file(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<crate::client::DroppedAttachmentInfo, String> {
    state
        .inner()
        .clone()
        .inspect_chat_attachment_file(&path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_chat_attachment_path(
    app: AppHandle,
    state: State<'_, DesktopState>,
    path: String,
    request_id: String,
    text: String,
    reply_to: Option<u64>,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .send_chat_attachment_path(&app, &path, request_id, text, reply_to)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upload_chat_part(
    state: State<'_, DesktopState>,
    id: String,
    part: u64,
    data: String,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .upload_chat_part(&id, part, &data)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn finish_chat_attachment(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .finish_chat_attachment(&app, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn download_chat_attachment(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: u64,
    transfer_id: Option<String>,
) -> Result<String, String> {
    state
        .inner()
        .clone()
        .download_chat_attachment(
            &app,
            id,
            transfer_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_presence(
    state: State<'_, DesktopState>,
    muted: Option<bool>,
    deafened: Option<bool>,
) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .set_presence(muted, deafened)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_screen_sharing(
    state: State<'_, DesktopState>,
    enabled: bool,
) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .set_screen_sharing(enabled)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_screen_tier(state: State<'_, DesktopState>, tier: String) -> Result<(), String> {
    let tier = ScreenTier::parse(&tier).ok_or_else(|| "qualidade de tela inválida".to_owned())?;
    let state = state.inner().clone();
    state
        .set_screen_tier(tier)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn join_call(app: AppHandle, state: State<'_, DesktopState>) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .join_call(&app)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn leave_call(app: AppHandle, state: State<'_, DesktopState>) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .leave_call(&app)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn clear_error(app: AppHandle, state: State<'_, DesktopState>) {
    state.clear_error(&app);
}

#[tauri::command]
pub async fn edit_chat_message(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: u64,
    text: String,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .edit_chat_message(&app, id, text)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn delete_chat_message(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: u64,
) -> Result<(), String> {
    state
        .inner()
        .clone()
        .delete_chat_message(&app, id)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn search_chat(
    state: State<'_, DesktopState>,
    query: String,
    before: Option<u64>,
) -> Result<concord_protocol::ChatPage, String> {
    state
        .inner()
        .clone()
        .search_chat(query, before)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn get_chat_message(
    state: State<'_, DesktopState>,
    id: u64,
) -> Result<concord_protocol::ChatMessage, String> {
    state
        .inner()
        .clone()
        .get_chat_message(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_chat_download(state: State<'_, DesktopState>, transfer_id: String) {
    state.cancel_chat_download(&transfer_id);
}

#[tauri::command]
pub fn get_saved_account(
    state: State<'_, DesktopState>,
    server_url: String,
) -> Option<crate::client::SavedAccountInfo> {
    state.saved_account(&server_url)
}
#[tauri::command]
pub async fn authenticate_account(
    app: AppHandle,
    state: State<'_, DesktopState>,
    request: crate::client::AccountRequest,
) -> Result<Option<concord_protocol::AccountProfile>, String> {
    state
        .inner()
        .clone()
        .authenticate_account(&app, request)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn logout_account(app: AppHandle, state: State<'_, DesktopState>) -> Result<(), String> {
    state
        .inner()
        .clone()
        .logout_account(&app)
        .await
        .map_err(|e| e.to_string())
}
