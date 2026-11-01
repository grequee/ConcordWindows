//! Local microphone preferences. No audio, invitation tokens or server data
//! are stored here. Save atomically before publishing a new live setting.

use std::io::Write;
use std::path::Path;

use concord_media::MicrophoneSettings;

pub(crate) fn load_microphone(path: &Path) -> Result<MicrophoneSettings, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MicrophoneSettings::default());
        }
        Err(error) => {
            return Err(format!(
                "não foi possível ler os ajustes do microfone: {error}"
            ));
        }
    };
    let mut settings: MicrophoneSettings = serde_json::from_slice(&bytes)
        .map_err(|error| format!("ajustes de microfone inválidos: {error}"))?;
    if !concord_media::NOISE_SUPPRESSION_AVAILABLE {
        settings.noise_suppression = false;
    }
    settings.validate().map_err(|error| error.to_string())
}

pub(crate) fn save_microphone(path: &Path, settings: MicrophoneSettings) -> Result<(), String> {
    settings.validate().map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "pasta de configurações inválida".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("não foi possível criar a pasta de configurações: {error}"))?;
    let serial = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let temporary = parent.join(format!("microphone-{}-{serial}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(&settings).map_err(|error| error.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "não foi possível salvar os ajustes do microfone: {error}"
        ));
    }
    Ok(())
}

pub(crate) fn load_audio_devices(
    path: &Path,
) -> Result<Option<concord_media::VoiceAudioConfig>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let config: concord_media::VoiceAudioConfig =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    config.validate().map_err(|e| e.to_string())?;
    Ok(Some(config))
}

pub(crate) fn save_audio_devices(
    path: &Path,
    config: &concord_media::VoiceAudioConfig,
) -> Result<(), String> {
    config.validate().map_err(|e| e.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "pasta de configuração inválida".to_owned())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let nonce = uuid::Uuid::new_v4();
    let temporary = parent.join(format!("audio-{nonce}.tmp"));
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn device_preferences_roundtrip_and_invalid_choices_preserve_previous_file() {
        let dir = std::env::temp_dir().join(format!("concord-devices-{}", uuid::Uuid::new_v4()));
        let path = dir.join("audio-devices.json");
        assert_eq!(load_audio_devices(&path).expect("missing"), None);
        let settings = concord_media::VoiceAudioConfig {
            input_device: Some("test-input".into()),
            output_device: Some("test-output".into()),
        };
        save_audio_devices(&path, &settings).expect("save");
        assert_eq!(
            load_audio_devices(&path).expect("reload"),
            Some(settings.clone())
        );
        let invalid = concord_media::VoiceAudioConfig {
            input_device: Some(String::new()),
            output_device: None,
        };
        assert!(save_audio_devices(&path, &invalid).is_err());
        assert_eq!(
            load_audio_devices(&path).expect("preserved"),
            Some(settings)
        );
        save_audio_devices(&path, &concord_media::VoiceAudioConfig::default())
            .expect("system defaults");
        assert_eq!(
            load_audio_devices(&path).expect("reload defaults"),
            Some(concord_media::VoiceAudioConfig::default())
        );
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn preferences_roundtrip_and_invalid_updates_preserve_the_file() {
        let dir = std::env::temp_dir().join(format!(
            "concord-mic-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("microphone.json");
        assert_eq!(
            load_microphone(&path).expect("missing file"),
            MicrophoneSettings::default()
        );
        let settings = MicrophoneSettings {
            volume_percent: 140,
            noise_suppression: concord_media::NOISE_SUPPRESSION_AVAILABLE,
        };
        save_microphone(&path, settings).expect("save");
        assert_eq!(load_microphone(&path).expect("load"), settings);
        assert!(
            save_microphone(
                &path,
                MicrophoneSettings {
                    volume_percent: 201,
                    ..settings
                }
            )
            .is_err()
        );
        assert_eq!(load_microphone(&path).expect("unchanged"), settings);
        std::fs::write(&path, b"invalid json").expect("malformed file");
        assert!(load_microphone(&path).is_err());
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
