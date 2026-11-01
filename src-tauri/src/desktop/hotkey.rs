use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: String,
}
impl Shortcut {
    pub fn parse(value: &str) -> Result<Option<Self>, String> {
        if value.is_empty() {
            return Ok(None);
        }
        if value.len() > 80 {
            return Err("Atalho inválido.".into());
        }
        let mut shortcut = Self {
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
            key: String::new(),
        };
        for token in value.split('+') {
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => shortcut.ctrl = true,
                "alt" => shortcut.alt = true,
                "shift" => shortcut.shift = true,
                "meta" | "super" | "win" => shortcut.meta = true,
                _ => {
                    if !shortcut.key.is_empty() {
                        return Err("Use uma única tecla junto aos modificadores.".into());
                    }
                    shortcut.key = token.to_ascii_uppercase();
                }
            }
        }
        let valid = shortcut.key.len() == 1
            && shortcut.key.bytes().all(|c| c.is_ascii_alphanumeric())
            || shortcut
                .key
                .strip_prefix('F')
                .and_then(|v| v.parse::<u8>().ok())
                .is_some_and(|v| (1..=12).contains(&v));
        if !valid || !(shortcut.ctrl || shortcut.alt || shortcut.meta) {
            return Err("Use Ctrl, Alt ou Meta com uma letra, número ou F1 a F12.".into());
        }
        Ok(Some(shortcut))
    }
    #[cfg(target_os = "linux")]
    pub fn portal_trigger(&self) -> String {
        let mut fields = vec![];
        if self.ctrl {
            fields.push("CTRL");
        }
        if self.alt {
            fields.push("ALT");
        }
        if self.shift {
            fields.push("SHIFT");
        }
        if self.meta {
            fields.push("LOGO");
        }
        let key = if self.key.len() == 1 {
            self.key.to_ascii_lowercase()
        } else {
            self.key.clone()
        };
        fields.push(&key);
        fields.join("+")
    }
}

pub(super) struct HotkeyWorker {
    cancel: Arc<AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl HotkeyWorker {
    pub fn stop(mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
impl Drop for HotkeyWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub(super) fn start(
    value: String,
    status: impl Fn(bool, String) + Send + 'static,
    fire: impl Fn() + Send + 'static,
) -> HotkeyWorker {
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = cancel.clone();
    let task = std::thread::spawn(move || {
        let shortcut = match Shortcut::parse(&value) {
            Ok(Some(value)) => value,
            _ => return,
        };
        #[cfg(target_os = "linux")]
        let result = super::linux::shortcut(shortcut, &worker, &status, &fire);
        #[cfg(target_os = "windows")]
        let result = super::windows::shortcut(shortcut, &worker, &status, &fire);
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let result: Result<(), String> = Err("Atalho global indisponível nesta plataforma.".into());
        if let Err(error) = result
            && !worker.load(Ordering::Relaxed)
        {
            status(
                false,
                format!("{error} O atalho continua disponível com o app em foco."),
            );
        }
    });
    HotkeyWorker {
        cancel,
        task: Some(task),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcuts_require_modifiers_and_one_supported_key() {
        assert!(matches!(
            Shortcut::parse("Ctrl+Shift+M"),
            Ok(Some(Shortcut {
                ctrl: true,
                shift: true,
                ..
            }))
        ));
        for invalid in ["M", "Shift+M", "Ctrl+M+N", "Alt+F13", "Meta+", "Ctrl+Enter"] {
            assert!(Shortcut::parse(invalid).is_err(), "{invalid}");
        }
        assert_eq!(Shortcut::parse(""), Ok(None));
    }
}
