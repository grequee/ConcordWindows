mod call;
mod client;
mod commands;
mod desktop;
mod preferences;
mod screen;

pub use client::{DesktopState, LinkStatus, Snapshot};

use anyhow::Context;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

pub fn run() -> anyhow::Result<()> {
    init_tracing();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        protocol = concord_protocol::PROTOCOL_VERSION,
        "starting Concord desktop"
    );

    tauri::Builder::default()
        .manage(DesktopState::default())
        .manage(desktop::DesktopServices::default())
        .setup(|app| {
            app.state::<DesktopState>()
                .initialize_microphone(app.handle());
            app.state::<DesktopState>().initialize_audio(app.handle());
            app.state::<DesktopState>()
                .initialize_identity(app.handle());
            app.state::<DesktopState>()
                .initialize_accounts(app.handle());
            app.state::<desktop::DesktopServices>()
                .initialize(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && desktop::close_to_tray(window.app_handle())
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            desktop::get_desktop_settings,
            desktop::set_desktop_settings,
            desktop::toggle_microphone_shortcut,
            desktop::minimize_to_tray,
            desktop::open_external,
            commands::cancel_chat_download,
            commands::get_saved_account,
            commands::authenticate_account,
            commands::logout_account,
            commands::get_snapshot,
            commands::list_audio_devices,
            commands::set_audio_devices,
            commands::start_audio_test,
            commands::stop_audio_test,
            commands::set_participant_volume,
            commands::set_microphone_settings,
            commands::connect,
            commands::disconnect,
            commands::refresh_rooms,
            commands::join_room,
            commands::leave_room,
            commands::send_chat,
            commands::edit_chat_message,
            commands::delete_chat_message,
            commands::search_chat,
            commands::get_chat_message,
            commands::get_chat_image,
            commands::load_chat_history,
            commands::start_chat_attachment,
            commands::inspect_chat_attachment_file,
            commands::send_chat_attachment_path,
            commands::upload_chat_part,
            commands::finish_chat_attachment,
            commands::download_chat_attachment,
            commands::set_presence,
            commands::set_screen_sharing,
            commands::set_screen_tier,
            commands::join_call,
            commands::leave_call,
            commands::clear_error,
        ])
        .run(tauri::generate_context!())
        .context("Tauri runtime failed")
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,concord_app_lib=debug"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init();
}
