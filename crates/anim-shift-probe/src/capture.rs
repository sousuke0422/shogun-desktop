//! Windows.Graphics.Capture of one window, one PNG per content update. WGC
//! raises `FrameArrived` as DWM composes the window — measured on a static
//! window that is every vsync (16.7 ms at 60 Hz) with identical pixels — so
//! a frame identical to the last kept one is dropped: what remains is one
//! PNG per change of the client area, stamped with `SystemRelativeTime`, the
//! QPC time DWM composed the first frame that showed it. No fixed sampling
//! clock is involved, so nothing beats against the app's paint clock.
//!
//! Desktop Duplication was the alternative; it is per-monitor (the whole
//! desktop, including other windows' updates), loses frames while the
//! window is occluded, and needs the window's screen position kept in sync.
//! WGC follows the one window and works while it is covered.
//!
//! The frame WGC hands over is the window's visual bounds (DWM extended
//! frame bounds), so it is cropped to the client area before saving: the
//! saved PNGs, the ROI and the masks all use client pixels. The process
//! opts into per-monitor DPI awareness (v2) first, so client rectangles are
//! physical pixels — the same pixels WGC captures.

use crate::seq;
use anyhow::{Context, Result, anyhow, bail};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowTextW, IsWindowVisible,
};
use windows::core::{BOOL, Interface};

pub enum Target {
    /// Substring of the window title (must match exactly one visible window).
    Title(String),
    Hwnd(isize),
}

pub struct Options {
    pub target: Target,
    pub duration: Duration,
    pub out: PathBuf,
}

pub struct Stats {
    pub frames: usize,
    pub client_w: u32,
    pub client_h: u32,
    pub dpi: u32,
    pub resized_frames: usize,
    /// Compositions identical to the previous kept frame (dropped).
    pub duplicates: usize,
}

/// Where the client area sits inside the captured frame.
#[derive(Clone, Copy)]
struct Crop {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

struct Raw {
    index: usize,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// Pixels of one frame before it is known to be new.
struct Pixels {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

struct State {
    ctx: ID3D11DeviceContext,
    device: ID3D11Device,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    crop: Crop,
    next: usize,
    t0: Option<i64>,
    /// The last kept frame, to drop identical re-compositions.
    last: Option<Pixels>,
    duplicates: usize,
    times: Vec<(usize, f64)>,
    resized: usize,
    tx: Option<mpsc::Sender<Raw>>,
    error: Option<String>,
}

fn find_window(target: &Target) -> Result<(HWND, String)> {
    match target {
        Target::Hwnd(h) => {
            let hwnd = HWND(*h as *mut _);
            Ok((hwnd, window_title(hwnd)))
        }
        Target::Title(needle) => {
            unsafe extern "system" fn collect(hwnd: HWND, lp: LPARAM) -> BOOL {
                let found = unsafe { &mut *(lp.0 as *mut Vec<(HWND, String)>) };
                if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                    let title = window_title(hwnd);
                    if !title.is_empty() {
                        found.push((hwnd, title));
                    }
                }
                BOOL(1)
            }
            let mut all: Vec<(HWND, String)> = Vec::new();
            unsafe { EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize)) }
                .context("EnumWindows")?;
            let hits: Vec<_> = all
                .into_iter()
                .filter(|(_, t)| t.contains(needle))
                .collect();
            match hits.len() {
                1 => Ok(hits.into_iter().next().expect("one hit")),
                0 => bail!("no visible window title contains {needle:?}"),
                n => {
                    let list: Vec<String> = hits
                        .iter()
                        .map(|(h, t)| format!("{:#x} {t:?}", h.0 as usize))
                        .collect();
                    bail!(
                        "{n} windows match {needle:?}; pass --hwnd: {}",
                        list.join(", ")
                    )
                }
            }
        }
    }
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Client rectangle relative to the captured frame (DWM extended bounds).
fn client_crop(hwnd: HWND) -> Result<Crop> {
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }.context("GetClientRect")?;
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        bail!("ClientToScreen failed");
    }
    let mut frame = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .context("DwmGetWindowAttribute(EXTENDED_FRAME_BOUNDS)")?;
    Ok(Crop {
        x: (origin.x - frame.left).max(0) as u32,
        y: (origin.y - frame.top).max(0) as u32,
        w: (client.right - client.left).max(0) as u32,
        h: (client.bottom - client.top).max(0) as u32,
    })
}

fn d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice)> {
    let mut device = None;
    let mut ctx = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
    }
    .context("D3D11CreateDevice")?;
    let device: ID3D11Device = device.ok_or_else(|| anyhow!("no D3D11 device"))?;
    let ctx = ctx.ok_or_else(|| anyhow!("no D3D11 context"))?;
    let dxgi: IDXGIDevice = device.cast()?;
    let winrt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?.cast()?;
    Ok((device, ctx, winrt))
}

/// Copy one captured texture to CPU memory as tightly packed RGBA, cropped.
fn read_frame(
    st: &mut State,
    tex: &ID3D11Texture2D,
    content_w: u32,
    content_h: u32,
) -> Result<Pixels> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    let fresh = !matches!(&st.staging, Some((_, w, h)) if *w == desc.Width && *h == desc.Height);
    if fresh {
        let mut sd = desc;
        sd.Usage = D3D11_USAGE_STAGING;
        sd.BindFlags = 0;
        sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        sd.MiscFlags = 0;
        let mut staging = None;
        unsafe { st.device.CreateTexture2D(&sd, None, Some(&mut staging)) }
            .context("CreateTexture2D(staging)")?;
        st.staging = Some((
            staging.ok_or_else(|| anyhow!("no staging texture"))?,
            desc.Width,
            desc.Height,
        ));
    }
    let staging = st.staging.as_ref().expect("staging set").0.clone();
    unsafe { st.ctx.CopyResource(&staging, tex) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        st.ctx
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
    }
    .context("Map")?;
    let c = st.crop;
    let w = c.w.min(content_w.saturating_sub(c.x));
    let h = c.h.min(content_h.saturating_sub(c.y));
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let pitch = mapped.RowPitch as usize;
    let src = mapped.pData as *const u8;
    for row in 0..h as usize {
        let line = unsafe {
            std::slice::from_raw_parts(
                src.add((c.y as usize + row) * pitch + c.x as usize * 4),
                w as usize * 4,
            )
        };
        let dst = &mut rgba[row * w as usize * 4..(row + 1) * w as usize * 4];
        for (d, s) in dst.chunks_exact_mut(4).zip(line.chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], 255]); // BGRA -> RGBA, opaque
        }
    }
    unsafe { st.ctx.Unmap(&staging, 0) };
    if (w, h) != (c.w, c.h) {
        st.resized += 1;
    }
    Ok(Pixels { w, h, rgba })
}

fn on_frame(pool: &Direct3D11CaptureFramePool, state: &Mutex<State>) -> Result<()> {
    let mut st = state.lock().map_err(|_| anyhow!("state poisoned"))?;
    let frame = pool.TryGetNextFrame()?;
    let t = frame.SystemRelativeTime()?.Duration; // 100 ns QPC units
    let size = frame.ContentSize()?;
    let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
    let tex: ID3D11Texture2D = unsafe { access.GetInterface() }?;
    let px = read_frame(
        &mut st,
        &tex,
        size.Width.max(0) as u32,
        size.Height.max(0) as u32,
    )?;
    let same = st
        .last
        .as_ref()
        .is_some_and(|l| (l.w, l.h) == (px.w, px.h) && l.rgba == px.rgba);
    if same {
        st.duplicates += 1;
        return Ok(());
    }
    let t0 = *st.t0.get_or_insert(t);
    let t_ms = (t - t0) as f64 / 10_000.0;
    let index = st.next;
    st.next += 1;
    st.times.push((index, t_ms));
    let raw = Raw {
        index,
        w: px.w,
        h: px.h,
        rgba: px.rgba.clone(),
    };
    st.last = Some(px);
    if let Some(tx) = &st.tx {
        let _ = tx.send(raw);
    }
    Ok(())
}

fn encode(out: &Path, raw: &Raw) -> Result<()> {
    let path = out.join(seq::frame_file_name(raw.index));
    let file =
        std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
    PngEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        CompressionType::Fast,
        FilterType::Sub,
    )
    .write_image(&raw.rgba, raw.w, raw.h, ExtendedColorType::Rgba8)
    .with_context(|| format!("encode {}", path.display()))
}

pub fn run(opts: &Options) -> Result<Stats> {
    unsafe {
        // Physical pixels for GetClientRect/ClientToScreen, matching WGC.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        bail!("Windows.Graphics.Capture is not supported on this system");
    }
    std::fs::create_dir_all(&opts.out).with_context(|| format!("create {}", opts.out.display()))?;
    let (hwnd, title) = find_window(&opts.target)?;
    let crop = client_crop(hwnd)?;
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let (device, ctx, winrt_device) = d3d_device()?;

    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForWindow(hwnd) }.context("CreateForWindow")?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        3,
        item.Size()?,
    )?;
    let session = pool.CreateCaptureSession(&item)?;
    // Best effort: neither is essential and both are newer than WGC itself.
    let _ = session.SetIsCursorCaptureEnabled(false);
    let _ = session.SetIsBorderRequired(false);

    // PNG encoding runs on worker threads so the frame callback only copies.
    let (tx, rx) = mpsc::channel::<Raw>();
    let rx = Arc::new(Mutex::new(rx));
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let rx = Arc::clone(&rx);
            let out = opts.out.clone();
            std::thread::spawn(move || -> Result<()> {
                loop {
                    let raw = match rx.lock().map_err(|_| anyhow!("rx poisoned"))?.recv() {
                        Ok(raw) => raw,
                        Err(_) => return Ok(()),
                    };
                    encode(&out, &raw)?;
                }
            })
        })
        .collect();

    let state = Arc::new(Mutex::new(State {
        ctx,
        device,
        staging: None,
        crop,
        next: 0,
        t0: None,
        last: None,
        duplicates: 0,
        times: Vec::new(),
        resized: 0,
        tx: Some(tx),
        error: None,
    }));
    let cb_state = Arc::clone(&state);
    let token = pool.FrameArrived(&TypedEventHandler::new(
        move |sender: windows::core::Ref<'_, Direct3D11CaptureFramePool>, _| {
            if let Some(pool) = sender.as_ref()
                && let Err(e) = on_frame(pool, &cb_state)
                && let Ok(mut st) = cb_state.lock()
            {
                st.error.get_or_insert_with(|| format!("{e:#}"));
            }
            Ok(())
        },
    ))?;
    session.StartCapture()?;
    std::thread::sleep(opts.duration);
    pool.RemoveFrameArrived(token)?;
    session.Close()?;
    pool.Close()?;

    let (times, resized, duplicates, error) = {
        let mut st = state.lock().map_err(|_| anyhow!("state poisoned"))?;
        st.tx = None; // close the channel so the workers drain and exit
        (
            std::mem::take(&mut st.times),
            st.resized,
            st.duplicates,
            st.error.take(),
        )
    };
    for w in workers {
        w.join().map_err(|_| anyhow!("encoder thread panicked"))??;
    }
    if let Some(e) = error {
        bail!("capture failed: {e}");
    }
    seq::write_index(&opts.out, &times)?;

    let mut note = String::new();
    let _ = writeln!(note, "hwnd: {:#x}", hwnd.0 as usize);
    let _ = writeln!(note, "title: {title}");
    let _ = writeln!(
        note,
        "api: Windows.Graphics.Capture (FrameArrived per DWM composition)"
    );
    let _ = writeln!(
        note,
        "clock: SystemRelativeTime (QPC), ms since first frame"
    );
    let _ = writeln!(
        note,
        "client_px: {}x{} at +{}+{} in the captured frame",
        crop.w, crop.h, crop.x, crop.y
    );
    let _ = writeln!(
        note,
        "dpi: {dpi} (scale {:.2}); all coordinates are physical client pixels",
        f64::from(dpi) / 96.0
    );
    let _ = writeln!(note, "frames: {} (one per content change)", times.len());
    let _ = writeln!(note, "dropped_identical: {duplicates}");
    let _ = writeln!(note, "frames_with_size_change: {resized}");
    std::fs::write(opts.out.join("capture.txt"), note).context("write capture.txt")?;

    Ok(Stats {
        frames: times.len(),
        client_w: crop.w,
        client_h: crop.h,
        dpi,
        resized_frames: resized,
        duplicates,
    })
}
