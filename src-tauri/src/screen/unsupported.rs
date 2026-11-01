//! Unsupported screen-sharing backend for platforms whose capture work has not
//! landed yet. The API stays identical so the call actor compiles everywhere.

use concord_media::VideoProfile;
use tauri::{AppHandle, Runtime};

use super::{DecodedFrame, FrameSender, PreviewFrame};

pub struct ShareSession;

impl ShareSession {
    pub fn instance(&self) -> u64 {
        0
    }

    pub fn source_name(&self) -> &str {
        ""
    }

    pub fn force_keyframe(&self) {}

    pub fn take_preview(&self) -> Option<PreviewFrame> {
        None
    }

    pub async fn reconfigure(&self, _profile: VideoProfile) -> Result<(), String> {
        Err("compartilhamento de tela ainda não está disponível nesta plataforma".to_owned())
    }

    pub fn stop(&mut self) {}
}

pub async fn start_share(
    _profile: VideoProfile,
    _instance: u64,
    _events: FrameSender,
) -> Result<ShareSession, String> {
    Err("compartilhamento de tela ainda não está disponível nesta plataforma".to_owned())
}

pub struct RemoteDecoder;

impl RemoteDecoder {
    pub fn new() -> Self {
        Self
    }

    pub fn decode(&mut self, _data: &[u8]) -> Result<Option<DecodedFrame>, String> {
        Err("decodificação de tela ainda não está disponível nesta plataforma".to_owned())
    }
}

pub struct Viewer;

impl Viewer {
    pub fn open<R: Runtime>(
        _app: &AppHandle<R>,
        _title: &str,
        _width: u32,
        _height: u32,
    ) -> Result<Self, String> {
        Err("visualizador nativo ainda não está disponível nesta plataforma".to_owned())
    }

    pub fn push_frame(&self, _data: Vec<u8>, _width: u32, _height: u32) {}

    pub fn is_open(&self) -> bool {
        false
    }

    pub fn close(&mut self) {}
}
