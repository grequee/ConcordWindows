use super::*;
use glib::variant::ToVariant;
use gtk::{gio, glib};

fn connection() -> Result<gio::DBusConnection, String> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|_| "A sessão do desktop não está disponível.".into())
}
#[allow(unsafe_code)]
fn indicator_available() -> bool {
    // Probe before tray-icon's lazy loader, which panics if neither library exists.
    ["libayatana-appindicator3.so.1", "libappindicator3.so.1"]
        .iter()
        .any(|name| unsafe { libloading::Library::new(name) }.is_ok())
}
pub(super) fn tray_available() -> bool {
    if !indicator_available() {
        return false;
    }
    let Ok(bus) = connection() else { return false };
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|value| value.get::<(bool,)>())
    .is_some_and(|(owned,)| owned)
}
pub(super) fn notify(title: &str, body: &str) -> Result<(), String> {
    let bus = connection()?;
    let body = glib::markup_escape_text(body).to_string();
    let hints: HashMap<String, glib::Variant> = HashMap::new();
    let parameters = (
        "Concord",
        0u32,
        "",
        title,
        body,
        Vec::<String>::new(),
        hints,
        5000i32,
    )
        .to_variant();
    bus.call_sync(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "Notify",
        Some(&parameters),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
    )
    .map_err(|_| "Notificação não aceita pelo sistema.".to_owned())?;
    Ok(())
}

pub(super) fn shortcut(
    shortcut: hotkey::Shortcut,
    cancel: &AtomicBool,
    status: &impl Fn(bool, String),
    fire: &impl Fn(),
) -> Result<(), String> {
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
    {
        portal_shortcut(shortcut, cancel, status, fire)
    } else {
        x11_shortcut(shortcut, cancel, status, fire)
    }
}

fn x11_shortcut(
    shortcut: hotkey::Shortcut,
    cancel: &AtomicBool,
    status: &impl Fn(bool, String),
    fire: &impl Fn(),
) -> Result<(), String> {
    use x11rb::{
        connection::Connection,
        protocol::{
            Event,
            xkb::{BoolCtrl, ConnectionExt as _, ID, PerClientFlag},
            xproto::{ConnectionExt, GrabMode, ModMask},
        },
    };
    let (conn, screen) = x11rb::connect(None)
        .map_err(|_| "Não foi possível acessar os atalhos do X11.".to_owned())?;
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let mapping = conn
        .get_keyboard_mapping(min, count)
        .map_err(|_| "Mapa de teclado indisponível.".to_owned())?
        .reply()
        .map_err(|_| "Mapa de teclado indisponível.".to_owned())?;
    let symbol = if shortcut.key.len() == 1 {
        u32::from(shortcut.key.as_bytes()[0])
    } else {
        0xffbd
            + shortcut.key[1..]
                .parse::<u32>()
                .map_err(|_| "Tecla inválida.".to_owned())?
    };
    let key = mapping
        .keysyms
        .chunks(mapping.keysyms_per_keycode as usize)
        .position(|syms| {
            syms.contains(&symbol)
                || symbol <= 127 && syms.contains(&u32::from((symbol as u8).to_ascii_lowercase()))
        })
        .map(|offset| min + offset as u8)
        .ok_or_else(|| "Tecla não encontrada no teclado atual.".to_owned())?;
    let mut mask = ModMask::default();
    if shortcut.ctrl {
        mask |= ModMask::CONTROL;
    }
    if shortcut.alt {
        mask |= ModMask::M1;
    }
    if shortcut.shift {
        mask |= ModMask::SHIFT;
    }
    if shortcut.meta {
        mask |= ModMask::M4;
    }
    let root = setup.roots[screen].root;
    for extra in [
        ModMask::default(),
        ModMask::LOCK,
        ModMask::M2,
        ModMask::LOCK | ModMask::M2,
    ] {
        conn.grab_key(
            false,
            root,
            mask | extra,
            key,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .map_err(|_| "Não foi possível registrar o atalho.".to_owned())?
        .check()
        .map_err(|_| "Esse atalho está em uso por outro aplicativo.".to_owned())?;
    }
    let detectable = conn
        .xkb_per_client_flags(
            ID::USE_CORE_KBD.into(),
            PerClientFlag::DETECTABLE_AUTO_REPEAT,
            PerClientFlag::DETECTABLE_AUTO_REPEAT,
            BoolCtrl::default(),
            BoolCtrl::default(),
            BoolCtrl::default(),
        )
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.supported.contains(PerClientFlag::DETECTABLE_AUTO_REPEAT));
    conn.flush()
        .map_err(|_| "Conexão com o teclado encerrada.".to_owned())?;
    status(true, "Atalho global ativo.".into());
    let mut pressed = false;
    let mut released_at = None;
    while !cancel.load(Ordering::Relaxed) {
        while let Some(event) = conn
            .poll_for_event()
            .map_err(|_| "Conexão com o teclado encerrada.".to_owned())?
        {
            match event {
                Event::KeyPress(event) if event.detail == key => {
                    released_at = None;
                    if !pressed {
                        pressed = true;
                        fire();
                    }
                }
                Event::KeyRelease(event) if event.detail == key => {
                    if detectable {
                        pressed = false;
                    } else {
                        released_at = Some(Instant::now());
                    }
                }
                _ => {}
            }
        }
        if released_at.is_some_and(|at| at.elapsed() > Duration::from_millis(35)) {
            pressed = false;
            released_at = None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn portal_request(
    bus: &gio::DBusConnection,
    context: &glib::MainContext,
    method: &str,
    params: &glib::Variant,
    cancel: &AtomicBool,
) -> Result<HashMap<String, glib::Variant>, String> {
    use std::{cell::RefCell, rc::Rc};
    type Responses = HashMap<String, (u32, HashMap<String, glib::Variant>)>;
    let responses = Rc::new(RefCell::new(Responses::new()));
    let captured = responses.clone();
    let subscription = bus.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        None,
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, path, _, _, args| {
            if let Some(response) = args.get::<(u32, HashMap<String, glib::Variant>)>() {
                captured.borrow_mut().insert(path.to_owned(), response);
            }
        },
    );
    let result = (|| {
        let response = bus
            .call_sync(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.GlobalShortcuts",
                method,
                Some(params),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                gio::Cancellable::NONE,
            )
            .map_err(|_| "O desktop não oferece o portal de atalhos globais.".to_owned())?;
        let path = response
            .child_value(0)
            .str()
            .ok_or_else(|| "Resposta inválida do sistema.".to_owned())?
            .to_owned();
        let began = Instant::now();
        loop {
            while context.pending() {
                context.iteration(false);
            }
            if let Some((code, result)) = responses.borrow_mut().remove(&path) {
                return if code == 0 {
                    Ok(result)
                } else {
                    Err("Atalho global não autorizado pelo sistema.".into())
                };
            }
            if cancel.load(Ordering::Relaxed) || began.elapsed() > Duration::from_secs(120) {
                let _ = bus.call_sync(
                    Some("org.freedesktop.portal.Desktop"),
                    &path,
                    "org.freedesktop.portal.Request",
                    "Close",
                    None,
                    None,
                    gio::DBusCallFlags::NONE,
                    1000,
                    gio::Cancellable::NONE,
                );
                return Err("Configuração de atalho cancelada.".into());
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    })();
    bus.signal_unsubscribe(subscription);
    result
}
fn portal_shortcut(
    shortcut: hotkey::Shortcut,
    cancel: &AtomicBool,
    status: &impl Fn(bool, String),
    fire: &impl Fn(),
) -> Result<(), String> {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let bus = connection()?;
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let mut options = HashMap::<String, glib::Variant>::new();
            options.insert(
                "handle_token".into(),
                format!("request_{nonce}").to_variant(),
            );
            options.insert(
                "session_handle_token".into(),
                format!("session_{nonce}").to_variant(),
            );
            let result = portal_request(
                &bus,
                &context,
                "CreateSession",
                &(options,).to_variant(),
                cancel,
            )?;
            let session = result
                .get("session_handle")
                .and_then(|v| v.str())
                .ok_or_else(|| "Sessão de atalho indisponível.".to_owned())?
                .to_owned();
            let session_path = glib::variant::ObjectPath::try_from(session.as_str())
                .map_err(|_| "Sessão de atalho inválida.".to_owned())?;
            let result = (|| {
                let mut properties = HashMap::<String, glib::Variant>::new();
                properties.insert(
                    "description".into(),
                    "Mutar ou desmutar o microfone no Concord".to_variant(),
                );
                properties.insert(
                    "preferred_trigger".into(),
                    shortcut.portal_trigger().to_variant(),
                );
                let shortcuts = vec![("toggle-microphone".to_owned(), properties)];
                let mut options = HashMap::<String, glib::Variant>::new();
                options.insert("handle_token".into(), format!("bind_{nonce}").to_variant());
                status(false, "Autorize o atalho na janela do sistema.".into());
                let result = portal_request(
                    &bus,
                    &context,
                    "BindShortcuts",
                    &(session_path, shortcuts, "", options).to_variant(),
                    cancel,
                )?;
                let bound = result
                    .get("shortcuts")
                    .and_then(|v| v.get::<Vec<(String, HashMap<String, glib::Variant>)>>())
                    .unwrap_or_default();
                if !bound.iter().any(|(id, _)| id == "toggle-microphone") {
                    return Err("O sistema não ativou esse atalho.".into());
                }
                let label = bound
                    .iter()
                    .find(|(id, _)| id == "toggle-microphone")
                    .and_then(|(_, v)| v.get("trigger_description"))
                    .and_then(|v| v.str())
                    .unwrap_or("atalho configurado");
                status(true, format!("Atalho global ativo: {label}."));
                let active = std::rc::Rc::new(std::cell::Cell::new(false));
                let active_down = active.clone();
                let session_down = session.clone();
                // The callback is local to this worker's context and borrows fire.
                let fires = std::rc::Rc::new(std::cell::Cell::new(0u64));
                let fired = fires.clone();
                let activated = bus.signal_subscribe(
                    Some("org.freedesktop.portal.Desktop"),
                    Some("org.freedesktop.portal.GlobalShortcuts"),
                    Some("Activated"),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_, _, _, _, _, args| {
                        if args.child_value(0).str() == Some(session_down.as_str())
                            && args.child_value(1).str() == Some("toggle-microphone")
                            && !active_down.replace(true)
                        {
                            fired.set(fired.get() + 1);
                        }
                    },
                );
                let active_up = active.clone();
                let session_up = session.clone();
                let deactivated = bus.signal_subscribe(
                    Some("org.freedesktop.portal.Desktop"),
                    Some("org.freedesktop.portal.GlobalShortcuts"),
                    Some("Deactivated"),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_, _, _, _, _, args| {
                        if args.child_value(0).str() == Some(session_up.as_str()) {
                            active_up.set(false);
                        }
                    },
                );
                let mut delivered = 0;
                while !cancel.load(Ordering::Relaxed) {
                    while context.pending() {
                        context.iteration(false);
                    }
                    if fires.get() != delivered {
                        delivered = fires.get();
                        fire();
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
                bus.signal_unsubscribe(activated);
                bus.signal_unsubscribe(deactivated);
                Ok(())
            })();
            let _ = bus.call_sync(
                Some("org.freedesktop.portal.Desktop"),
                &session,
                "org.freedesktop.portal.Session",
                "Close",
                None,
                None,
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
            );
            result
        })
        .map_err(|_| "Não foi possível iniciar o portal de atalhos.".to_owned())?
}
