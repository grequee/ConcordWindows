//! Windows Graphics Capture (Windows 10 1903+) and D3D11 CPU readback.
//!
//! The caller initializes a WinRT MTA on its dedicated capture thread before
//! calling `new`, and keeps it alive until the capturer has been dropped.
//! No graphics resource or frame is passed to JavaScript.

#![allow(unsafe_code)] // COM/D3D boundary; all resource access stays on the capture thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{E_POINTER, HMODULE, POINT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface, factory};

use crate::{CaptureSource, CapturedFrame, MediaError, PixelFormat, ScreenCapturer};

pub struct WindowsCapturer {
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    item: GraphicsCaptureItem,
    closed_token: i64,
    closed: Arc<AtomicBool>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    staging: Option<ID3D11Texture2D>,
    size: SizeInt32,
    running: Arc<AtomicBool>,
    source: CaptureSource,
    last: Option<CapturedFrame>,
}

impl WindowsCapturer {
    /// Share the complete primary monitor only after an explicit share action.
    /// WGC's system capture border remains enabled.
    pub fn new(running: Arc<AtomicBool>) -> Result<Self, MediaError> {
        Self::create(running).map_err(capture_error)
    }

    fn create(running: Arc<AtomicBool>) -> windows::core::Result<Self> {
        if !GraphicsCaptureSession::IsSupported()? {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_NOTIMPL,
                "Windows Graphics Capture indisponível; use Windows 10 1903 ou posterior",
            ));
        }
        // SAFETY: no pointers supplied by the caller; output COM interfaces
        // are reference counted and remain on this dedicated capture thread.
        let (device, context, item) = unsafe {
            let mut device = None;
            let mut context = None;
            let mut result = Err(windows::core::Error::empty());
            for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
                result = D3D11CreateDevice(
                    None::<&IDXGIAdapter>,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                );
                if result.is_ok() {
                    break;
                }
            }
            result?;
            let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(monitor)?;
            (
                device.ok_or_else(windows::core::Error::empty)?,
                context.ok_or_else(windows::core::Error::empty)?,
                item,
            )
        };
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: dxgi is a live ID3D11Device queried for IDXGIDevice.
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()? };
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        let closed = Arc::new(AtomicBool::new(false));
        let closed_flag = closed.clone();
        let closed_token = item.Closed(
            &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                closed_flag.store(true, Ordering::SeqCst);
                Ok(())
            }),
        )?;
        let source = CaptureSource::Monitor {
            id: 0,
            name: format!("Monitor principal — {}", item.DisplayName()?),
        };
        let capturer = Self {
            session,
            pool,
            item,
            closed_token,
            closed,
            device,
            context,
            winrt_device,
            staging: None,
            size,
            running,
            source,
            last: None,
        };
        // Construct the owner before starting so failure also closes the pool.
        capturer.session.StartCapture()?;
        Ok(capturer)
    }

    fn read_frame(&mut self) -> Result<Option<CapturedFrame>, MediaError> {
        let frame = match self.pool.TryGetNextFrame() {
            Ok(frame) => frame,
            // WinRT returns a null interface when no frame is ready. The Rust
            // projection represents that successful null return as E_POINTER.
            Err(error) if error.code() == E_POINTER => return Ok(None),
            Err(error) => return Err(capture_error(error)),
        };
        let result = (|| {
            let size = frame.ContentSize().map_err(capture_error)?;
            if size.Width <= 0 || size.Height <= 0 {
                return Ok(None);
            }
            if size.Width != self.size.Width || size.Height != self.size.Height {
                // Release the old frame before resizing the pool. The next
                // frame will match the new dimensions (resolution/rotation).
                frame.Close().map_err(capture_error)?;
                self.pool
                    .Recreate(
                        &self.winrt_device,
                        DirectXPixelFormat::B8G8R8A8UIntNormalized,
                        2,
                        size,
                    )
                    .map_err(capture_error)?;
                self.size = size;
                self.staging = None;
                self.last = None;
                return Ok(None);
            }
            let surface = frame.Surface().map_err(capture_error)?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(capture_error)?;
            // SAFETY: WGC supplies an ID3D11Texture2D; the staging copy is used
            // only by this thread, with an identical texture description.
            unsafe {
                let texture: ID3D11Texture2D = access.GetInterface().map_err(capture_error)?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);
                if desc.Width != size.Width as u32 || desc.Height != size.Height as u32 {
                    return Ok(None);
                }
                if self.staging.is_none() {
                    desc.Usage = D3D11_USAGE_STAGING;
                    desc.BindFlags = 0;
                    desc.MiscFlags = 0;
                    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                    self.device
                        .CreateTexture2D(&desc, None, Some(&mut self.staging))
                        .map_err(capture_error)?;
                }
                let staging = self.staging.as_ref().ok_or(MediaError::SourceLost)?;
                self.context.CopyResource(staging, &texture);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(capture_error)?;
                let width = desc.Width;
                let height = desc.Height;
                let row_bytes = width as usize * 4;
                let result = if mapped.pData.is_null() || (mapped.RowPitch as usize) < row_bytes {
                    Err(MediaError::Capture("buffer D3D11 inválido".to_owned()))
                } else {
                    let mut data = vec![0; row_bytes * height as usize];
                    for row in 0..height as usize {
                        // RowPitch can include GPU padding; never encode it.
                        std::ptr::copy_nonoverlapping(
                            mapped
                                .pData
                                .cast::<u8>()
                                .add(row * mapped.RowPitch as usize),
                            data.as_mut_ptr().add(row * row_bytes),
                            row_bytes,
                        );
                    }
                    Ok(Some(CapturedFrame {
                        width,
                        height,
                        stride: width * 4,
                        format: PixelFormat::Bgra8,
                        data,
                        captured_at: Instant::now(),
                    }))
                };
                self.context.Unmap(staging, 0);
                result
            }
        })();
        let _ = frame.Close();
        result
    }
}

impl ScreenCapturer for WindowsCapturer {
    fn source(&self) -> &CaptureSource {
        &self.source
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, MediaError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.running.load(Ordering::SeqCst) && !self.closed.load(Ordering::SeqCst) {
            if let Some(frame) = self.read_frame()? {
                self.last = Some(frame);
            }
            // Static desktops need not produce WGC frames. Repeat the last
            // image at the profile cadence, rather than failing idle captures.
            if let Some(last) = &self.last {
                return Ok(CapturedFrame {
                    width: last.width,
                    height: last.height,
                    stride: last.stride,
                    format: last.format,
                    data: last.data.clone(),
                    captured_at: Instant::now(),
                });
            }
            if Instant::now() >= deadline {
                return Err(MediaError::Capture(
                    "o Windows não forneceu o primeiro quadro em 5 segundos".to_owned(),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(MediaError::SourceLost)
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = self.session.Close();
        let _ = self.pool.Close();
        self.last = None;
    }
}

impl Drop for WindowsCapturer {
    fn drop(&mut self) {
        let _ = self.item.RemoveClosed(self.closed_token);
        self.stop();
    }
}

fn capture_error(error: windows::core::Error) -> MediaError {
    MediaError::Capture(format!("Windows Graphics Capture: {error}"))
}
