mod hotkey;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

use crate::{DesktopState, Snapshot};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, Runtime};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DesktopSettings {
    pub notifications: bool,
    pub close_to_tray: bool,
    pub mute_shortcut: String,
}
impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            notifications: true,
            close_to_tray: true,
            mute_shortcut: "Ctrl+Shift+M".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesktopStatus {
    pub settings: DesktopSettings,
    pub tray_available: bool,
    pub global_shortcut_active: bool,
    pub shortcut_message: String,
}

pub(crate) struct DesktopServices {
    update: tokio::sync::Mutex<()>,
    settings: Mutex<DesktopSettings>,
    tray: AtomicBool,
    global: AtomicBool,
    message: Mutex<String>,
    worker: Mutex<Option<hotkey::HotkeyWorker>>,
    epoch: AtomicU64,
    seen: Mutex<HashMap<String, u64>>,
    last_toggle: Mutex<Option<Instant>>,
    last_mic: Mutex<Option<(bool, bool)>>,
}
impl Default for DesktopServices {
    fn default() -> Self {
        Self {
            update: tokio::sync::Mutex::new(()),
            settings: Mutex::new(DesktopSettings::default()),
            tray: AtomicBool::new(false),
            global: AtomicBool::new(false),
            message: Mutex::new("Preparando atalho…".into()),
            worker: Mutex::new(None),
            epoch: AtomicU64::new(0),
            seen: Mutex::new(HashMap::new()),
            last_toggle: Mutex::new(None),
            last_mic: Mutex::new(None),
        }
    }
}

impl DesktopServices {
    fn status(&self) -> DesktopStatus {
        DesktopStatus {
            settings: self.settings.lock().clone(),
            tray_available: self.tray.load(Ordering::Relaxed),
            global_shortcut_active: self.global.load(Ordering::Relaxed),
            shortcut_message: self.message.lock().clone(),
        }
    }
    pub(crate) fn initialize<R: Runtime>(&self, app: &AppHandle<R>) {
        if let Ok(dir) = app.path().app_config_dir()
            && let Ok(bytes) = std::fs::read(dir.join("desktop.json"))
            && let Ok(settings) = serde_json::from_slice::<DesktopSettings>(&bytes)
            && hotkey::Shortcut::parse(&settings.mute_shortcut).is_ok()
        {
            *self.settings.lock() = settings;
        }
        #[cfg(target_os = "linux")]
        if !linux::tray_available() {
            configure_shortcut(app);
            return;
        }
        match create_tray(app) {
            Ok(()) => {
                #[cfg(target_os = "linux")]
                let available = linux::tray_available();
                #[cfg(not(target_os = "linux"))]
                let available = true;
                self.tray.store(available, Ordering::Relaxed);
            }
            Err(error) => tracing::warn!(%error,"system tray unavailable"),
        }
        configure_shortcut(app);
    }
}

fn emit_status<R: Runtime>(app: &AppHandle<R>) {
    if let Some(service) = app.try_state::<DesktopServices>() {
        let _ = app.emit("concord:desktop-status", service.status());
    }
}
fn configure_shortcut<R: Runtime>(app: &AppHandle<R>) {
    let Some(service) = app.try_state::<DesktopServices>() else {
        return;
    };
    let epoch = service.epoch.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(previous) = service.worker.lock().take() {
        previous.stop();
    }
    service.global.store(false, Ordering::Relaxed);
    let value = service.settings.lock().mute_shortcut.clone();
    if value.is_empty() {
        *service.message.lock() = "Atalho desativado.".into();
        emit_status(app);
        return;
    }
    *service.message.lock() = "Preparando atalho global…".into();
    let app_for_status = app.clone();
    let app_for_toggle = app.clone();
    let worker = hotkey::start(
        value,
        move |active, message| {
            let Some(service) = app_for_status.try_state::<DesktopServices>() else {
                return;
            };
            if service.epoch.load(Ordering::SeqCst) != epoch {
                return;
            }
            service.global.store(active, Ordering::Relaxed);
            *service.message.lock() = message;
            emit_status(&app_for_status);
        },
        move || toggle_microphone(&app_for_toggle, true),
    );
    *service.worker.lock() = Some(worker);
    emit_status(app);
}

pub(crate) fn toggle_microphone<R: Runtime>(app: &AppHandle<R>, shortcut: bool) {
    if shortcut && let Some(service) = app.try_state::<DesktopServices>() {
        let mut last = service.last_toggle.lock();
        if last.is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
            return;
        }
        *last = Some(Instant::now());
    }
    let state = app.state::<DesktopState>().inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(call) = state.snapshot().call else {
            return;
        };
        if let Err(error) = state.set_presence(Some(!call.muted), None).await {
            tracing::debug!(%error,"microphone toggle failed");
        }
        state.publish(&app);
    });
}

fn tray_image(open: bool, in_call: bool) -> tauri::image::Image<'static> {
    let mut rgba = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let p = (y * 32 + x) * 4;
            let inside = (x as i32 - 16).pow(2) + (y as i32 - 16).pow(2) < 225;
            if inside {
                rgba[p..p + 4].copy_from_slice(&[28, 34, 29, 255]);
            }
            let microphone = (12..20).contains(&x) && (7..19).contains(&y)
                || ((9..23).contains(&x) && (18..22).contains(&y))
                || ((15..17).contains(&x) && (21..27).contains(&y));
            if inside && microphone {
                let color = if !in_call {
                    [150, 153, 146, 255]
                } else if open {
                    [217, 242, 107, 255]
                } else {
                    [255, 159, 139, 255]
                };
                rgba[p..p + 4].copy_from_slice(&color);
            }
        }
    }
    tauri::image::Image::new_owned(rgba, 32, 32)
}
fn create_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    use tauri::{
        menu::{Menu, MenuItem},
        tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    };
    let open = MenuItem::with_id(app, "open", "Abrir Concord", true, None::<&str>)?;
    let mute = MenuItem::with_id(
        app,
        "mute",
        "Mutar / desmutar microfone",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Sair do Concord", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &mute, &quit])?;
    TrayIconBuilder::with_id("concord-tray")
        .icon(tray_image(false, false))
        .tooltip("Concord · fora da chamada")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_window(app),
            "mute" => toggle_microphone(app, false),
            "quit" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<DesktopState>().inner().clone();
                    state.stop_audio_test_requested().await;
                    state.disconnect(&app).await;
                    app.exit(0);
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
pub(crate) fn show_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
pub(crate) fn close_to_tray<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<DesktopServices>()
        .is_some_and(|s| s.tray.load(Ordering::Relaxed) && s.settings.lock().close_to_tray)
}
pub(crate) fn update_tray<R: Runtime>(app: &AppHandle<R>, snapshot: &Snapshot) {
    let Some(service) = app.try_state::<DesktopServices>() else {
        return;
    };
    let state = (
        snapshot.call.is_some(),
        snapshot.call.as_ref().is_some_and(|c| !c.muted),
    );
    let mut old = service.last_mic.lock();
    if *old == Some(state) {
        return;
    }
    *old = Some(state);
    drop(old);
    if let Some(tray) = app.tray_by_id("concord-tray") {
        let _ = tray.set_icon(Some(tray_image(state.1, state.0)));
        let text = if !state.0 {
            "Concord · fora da chamada"
        } else if state.1 {
            "Concord · microfone aberto"
        } else {
            "Concord · microfone fechado"
        };
        let _ = tray.set_tooltip(Some(text));
    }
}

pub(crate) fn reset_seen<R: Runtime>(app: &AppHandle<R>) {
    if let Some(service) = app.try_state::<DesktopServices>() {
        service.seen.lock().clear();
    }
}

pub(crate) fn observe_history<R: Runtime>(
    app: &AppHandle<R>,
    room: &str,
    messages: &[concord_protocol::ChatMessage],
) {
    if let Some(service) = app.try_state::<DesktopServices>() {
        let max = messages.iter().map(|m| m.id).max().unwrap_or(0);
        service.seen.lock().insert(room.to_owned(), max);
    }
}
pub(crate) fn chat_received<R: Runtime>(
    app: &AppHandle<R>,
    message: &concord_protocol::ChatMessage,
) {
    let Some(service) = app.try_state::<DesktopServices>() else {
        return;
    };
    let mut seen = service.seen.lock();
    let last = seen.entry(message.room_id.as_str().to_owned()).or_default();
    if message.id <= *last {
        return;
    }
    *last = message.id;
    drop(seen);
    if !service.settings.lock().notifications {
        return;
    }
    let snapshot = app.state::<DesktopState>().snapshot();
    if message.author_id.as_deref() == Some(snapshot.self_author_id.as_str())
        || snapshot.self_id.as_ref() == Some(&message.user_id)
    {
        return;
    }
    if app
        .get_webview_window("main")
        .is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
    {
        return;
    }
    let title = format!("{} · #{}", message.display_name, message.room_id);
    let body = if message.text.is_empty() {
        message
            .attachment
            .as_ref()
            .map(|f| format!("Anexo: {}", f.name))
            .unwrap_or_else(|| "Enviou uma imagem".into())
    } else {
        message.text.chars().take(240).collect()
    };
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "linux")]
        let _ = linux::notify(&title, &body);
        #[cfg(target_os = "windows")]
        let _ = windows::notify(&title, &body);
    });
}

#[tauri::command]
pub(crate) fn get_desktop_settings(state: tauri::State<'_, DesktopServices>) -> DesktopStatus {
    state.status()
}
#[tauri::command]
pub(crate) async fn set_desktop_settings(
    app: AppHandle,
    state: tauri::State<'_, DesktopServices>,
    settings: DesktopSettings,
) -> Result<DesktopStatus, String> {
    let _update = state.update.lock().await;
    hotkey::Shortcut::parse(&settings.mute_shortcut)?;
    let folder = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let saved = settings.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let temporary = folder.join(format!("desktop-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?)
                .and_then(|()| file.sync_all())
                .map_err(|e| e.to_string())?;
            drop(file);
            std::fs::rename(&temporary, folder.join("desktop.json")).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    })
    .await
    .map_err(|e| e.to_string())??;
    let changed = state.settings.lock().mute_shortcut != settings.mute_shortcut;
    *state.settings.lock() = settings;
    if changed {
        configure_shortcut(&app);
    }
    emit_status(&app);
    Ok(state.status())
}
#[tauri::command]
pub(crate) fn toggle_microphone_shortcut(app: AppHandle) {
    toggle_microphone(&app, true);
}
#[tauri::command]
pub(crate) fn minimize_to_tray(app: AppHandle) -> Result<(), String> {
    if !app
        .try_state::<DesktopServices>()
        .is_some_and(|s| s.tray.load(Ordering::Relaxed))
    {
        return Err("Bandeja indisponível neste ambiente.".into());
    }
    app.get_webview_window("main")
        .ok_or_else(|| "Janela indisponível.".to_owned())?
        .hide()
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub(crate) async fn open_external(url: String) -> Result<(), String> {
    let url = url::Url::parse(&url).map_err(|_| "Link inválido.".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Só é permitido abrir links HTTP ou HTTPS.".into());
    }
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open")
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(|_| ())
                .map_err(|_| "Não foi possível abrir o navegador.".to_owned())
        }
        #[cfg(target_os = "windows")]
        {
            windows::open_url(&url)
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = url;
            Err("Plataforma não suportada.".to_owned())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
