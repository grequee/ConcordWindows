#![allow(unsafe_code)]
use super::*;
use ::windows::{
    Win32::UI::{
        Input::KeyboardAndMouse::{
            HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
            RegisterHotKey, UnregisterHotKey,
        },
        Shell::ShellExecuteW,
        WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, SW_SHOWNORMAL, WM_HOTKEY},
    },
    core::{HSTRING, PCWSTR},
};
pub(super) fn shortcut(
    key: hotkey::Shortcut,
    cancel: &AtomicBool,
    status: &impl Fn(bool, String),
    fire: &impl Fn(),
) -> Result<(), String> {
    let mut modifiers: HOT_KEY_MODIFIERS = MOD_NOREPEAT;
    if key.ctrl {
        modifiers |= MOD_CONTROL;
    }
    if key.alt {
        modifiers |= MOD_ALT;
    }
    if key.shift {
        modifiers |= MOD_SHIFT;
    }
    if key.meta {
        modifiers |= MOD_WIN;
    }
    let code = if key.key.len() == 1 {
        u32::from(key.key.as_bytes()[0])
    } else {
        0x6f + key.key[1..].parse::<u32>().map_err(|_| "Tecla inválida.")?
    };
    unsafe { RegisterHotKey(None, 1, modifiers, code) }
        .map_err(|_| "Atalho em uso por outro aplicativo.".to_owned())?;
    status(true, "Atalho global ativo.".into());
    while !cancel.load(Ordering::Relaxed) {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE) }.as_bool()
        {
            fire();
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let _ = unsafe { UnregisterHotKey(None, 1) };
    Ok(())
}
pub(super) fn open_url(url: &str) -> Result<(), String> {
    let verb = HSTRING::from("open");
    let url = HSTRING::from(url);
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(url.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err("Não foi possível abrir o navegador.".into())
    } else {
        Ok(())
    }
}
pub(super) fn notify(title: &str, body: &str) -> Result<(), String> {
    use ::windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    };
    fn escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
    let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
    let result = (|| -> ::windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(format!("<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",escape(title),escape(body))))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("chat.concord.desktop"))?
            .Show(&toast)
    })();
    if initialized {
        unsafe { RoUninitialize() };
    }
    result.map_err(|_| "Notificação não aceita pelo sistema.".into())
}
