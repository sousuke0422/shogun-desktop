//! Per-tab shell icons, wt-style.
//!
//! Two layers, most accurate first:
//! 1. **Program resource icon** (Windows): the real icon embedded in the
//!    shell's `.exe`/`.dll` (cmd, pwsh, powershell, any custom exe), extracted
//!    with the same Win32 path Explorer uses. Full-colour and exact, and it
//!    needs nothing bundled — the program ships its own icon.
//! 2. **Bundled glyph** (`font-logos`, public domain): a distro logo tinted
//!    with the distro's brand colour. This is what carries WSL tabs (whose
//!    program is the generic `wsl.exe`, so its own icon wouldn't say *which*
//!    distro) and everything on non-Windows platforms, where the font is the
//!    portable common denominator (no system-font dependency).
//!
//! `resolve` picks the layer; the tab strip renders `Image` as a raster and
//! `Glyph` as tinted text in the `font-logos` family (registered at startup).

use std::path::PathBuf;
use std::sync::Arc;

/// The bundled icon font's family name (see `assets/fonts/font-logos.ttf`,
/// registered via `add_fonts` in `main`). Confirmed against the TTF name table.
pub const FONT_LOGOS: &str = "font-logos";

/// A resolved tab icon.
#[derive(Clone)]
pub enum TabIcon {
    /// A raster icon lifted from a program's resources (Windows exe/dll).
    Image(Arc<gpui::RenderImage>),
    /// A `font-logos` glyph tinted with a brand colour (`0xRRGGBB`).
    Glyph { text: gpui::SharedString, tint: u32 },
}

/// A distro's `font-logos` glyph and brand tint, matched from a free-text hint
/// (the profile/tab name or the `wsl -d <name>` argument).
fn distro_glyph(hint: &str) -> Option<TabIcon> {
    let h = hint.to_ascii_lowercase();
    // (substring, codepoint, brand colour). Order matters: more specific first
    // (e.g. "kubuntu"/"linuxmint" before "ubuntu"/"mint" would-be prefixes).
    const TABLE: &[(&str, char, u32)] = &[
        ("kubuntu", '\u{f333}', 0x1D99F3),
        ("ubuntu", '\u{f31b}', 0xE95420),
        ("debian", '\u{f306}', 0xA80030),
        ("arch", '\u{f303}', 0x1793D1),
        ("fedora", '\u{f30a}', 0x3C6EB4),
        ("kali", '\u{f327}', 0x367BF0),
        ("alpine", '\u{f300}', 0x0D597F),
        ("opensuse", '\u{f314}', 0x73BA25),
        ("suse", '\u{f314}', 0x73BA25),
        ("centos", '\u{f304}', 0x932279),
        ("rocky", '\u{f32b}', 0x10B981),
        ("almalinux", '\u{f31d}', 0x0F4266),
        ("alma", '\u{f31d}', 0x0F4266),
        ("nixos", '\u{f313}', 0x5277C3),
        ("nix", '\u{f313}', 0x5277C3),
        ("manjaro", '\u{f312}', 0x35BF5C),
        ("gentoo", '\u{f30d}', 0x54487A),
        ("linuxmint", '\u{f30e}', 0x87CF3E),
        ("mint", '\u{f30e}', 0x87CF3E),
        ("raspberry", '\u{f315}', 0xC51A4A),
        ("void", '\u{f32e}', 0x478061),
        ("elementary", '\u{f309}', 0x64BAFF),
        ("freebsd", '\u{f30c}', 0xAB2B28),
    ];
    for (needle, cp, tint) in TABLE {
        if h.contains(needle) {
            return Some(TabIcon::Glyph {
                text: cp.to_string().into(),
                tint: *tint,
            });
        }
    }
    None
}

/// The generic Tux glyph — a Linux shell we can't pin to a distro.
fn tux() -> TabIcon {
    TabIcon::Glyph {
        text: '\u{f31a}'.to_string().into(),
        tint: 0xDCDCDC,
    }
}

/// Is `program` a bare Unix shell (so on non-Windows it gets Tux, not an exe
/// icon)?
fn is_unix_shell(program: &str) -> bool {
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        "bash" | "sh" | "zsh" | "fish" | "dash" | "ash" | "ksh" | "tcsh" | "csh" | "nu" | "elvish"
    )
}

/// The `wsl -d <name>` distribution, if this is a WSL launch.
fn wsl_distro<'a>(program: &str, argv: &'a [String]) -> Option<&'a str> {
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    if base != "wsl.exe" && base != "wsl" {
        return None;
    }
    // `-d NAME` / `--distribution NAME`.
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == "-d" || a == "--distribution" {
            return it.next().map(|s| s.as_str());
        }
    }
    None
}

/// Resolve a tab's icon from how its shell was launched. `title` is the
/// profile/tab name (wt WSL profiles are named after the distro, the best
/// distro hint we have).
pub fn resolve(program: &str, argv: &[String], title: Option<&str>) -> Option<TabIcon> {
    // 1) WSL: pin the distro from `-d NAME` or the profile name — wsl.exe's own
    //    icon is the generic penguin and wouldn't distinguish distros.
    if let Some(distro) = wsl_distro(program, argv) {
        return distro_glyph(distro).or_else(|| Some(tux()));
    }
    if let Some(t) = title
        && let Some(g) = distro_glyph(t)
    {
        return Some(g);
    }

    // 2) Windows: the program's own resource icon (cmd/pwsh/powershell/custom).
    #[cfg(windows)]
    if let Some(path) = resolve_program_path(program)
        && let Some(img) = extract_exe_icon(&path)
    {
        return Some(TabIcon::Image(img));
    }

    // 3) A plain Unix shell → Tux; otherwise no icon (Windows shells already
    //    handled above; unknown programs stay unadorned rather than guess).
    if is_unix_shell(program) {
        return Some(tux());
    }
    None
}

/// Resolve a program name to a full path: absolute/relative as-is, else search
/// `PATH` (appending `.exe` on Windows). Pure Rust so no extra Win32 surface.
fn resolve_program_path(program: &str) -> Option<PathBuf> {
    let p = std::path::Path::new(program);
    if p.is_absolute() || program.contains(['/', '\\']) {
        return p.exists().then(|| p.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) { &["", ".exe"] } else { &[""] };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let cand = dir.join(format!("{program}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Extract a program's large icon (Windows) into a gpui render image.
#[cfg(windows)]
fn extract_exe_icon(path: &std::path::Path) -> Option<Arc<gpui::RenderImage>> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut large = HICON::default();
    let n = unsafe { ExtractIconExW(PCWSTR(wide.as_ptr()), 0, Some(&mut large), None, 1) };
    if n == 0 || large.is_invalid() {
        return None;
    }
    let out = hicon_to_image(large);
    unsafe {
        let _ = DestroyIcon(large);
    }
    out
}

/// HICON → BGRA render image (Windows DIBs are already BGRA — gpui's order).
#[cfg(windows)]
fn hicon_to_image(
    hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
) -> Option<Arc<gpui::RenderImage>> {
    use std::ffi::c_void;
    use windows::Win32::Graphics::Gdi::{
        BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(hicon, &mut info) }.ok()?;
    let hbm = info.hbmColor;
    let cleanup = |info: &ICONINFO| unsafe {
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
    };
    if hbm.is_invalid() {
        cleanup(&info);
        return None;
    }

    let mut bm = BITMAP::default();
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut c_void),
        )
    };
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    if got == 0 || w <= 0 || h <= 0 || w > 512 || h > 512 {
        cleanup(&info);
        return None;
    }

    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = 0; // BI_RGB

    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    let hdc = unsafe { CreateCompatibleDC(None) };
    let lines = unsafe {
        GetDIBits(
            hdc,
            hbm,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    unsafe {
        let _ = DeleteDC(hdc);
    }
    cleanup(&info);
    if lines == 0 {
        return None;
    }

    // Some legacy icons carry no per-pixel alpha (all zero) — treat as opaque.
    if !buf.chunks_exact(4).any(|p| p[3] != 0) {
        for p in buf.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }

    let img = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(w as u32, h as u32, buf)?;
    Some(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
        img,
    )])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distro_matches_specific_before_generic() {
        // kubuntu must win over the "ubuntu" substring.
        let TabIcon::Glyph { text, .. } = distro_glyph("Kubuntu 24.04").unwrap() else {
            panic!("expected glyph")
        };
        assert_eq!(text.as_ref(), "\u{f333}");
        let TabIcon::Glyph { text, .. } = distro_glyph("Ubuntu").unwrap() else {
            panic!("expected glyph")
        };
        assert_eq!(text.as_ref(), "\u{f31b}");
        assert!(distro_glyph("PowerShell").is_none());
    }

    #[test]
    fn wsl_distro_from_args_and_title() {
        let argv = vec!["-d".to_string(), "Debian".to_string()];
        assert_eq!(wsl_distro("wsl.exe", &argv), Some("Debian"));
        assert_eq!(
            wsl_distro("C:\\Windows\\System32\\wsl.exe", &argv),
            Some("Debian")
        );
        assert_eq!(wsl_distro("pwsh.exe", &argv), None);
        // Resolve prefers the distro glyph over any exe icon for WSL.
        assert!(matches!(
            resolve("wsl.exe", &argv, None),
            Some(TabIcon::Glyph { .. })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn extracts_a_real_system_exe_icon() {
        // cmd.exe always exists and carries a 32-bit icon.
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let cmd = std::path::Path::new(&root).join("System32").join("cmd.exe");
        let img = extract_exe_icon(&cmd).expect("cmd.exe icon should extract");
        let sz = img.size(0);
        assert!(
            sz.width.0 >= 8 && sz.width.0 <= 512 && sz.height.0 >= 8,
            "unexpected icon size {:?}",
            sz
        );
    }
}
