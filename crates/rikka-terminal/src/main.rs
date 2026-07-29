//! RikkaTerminal — standalone tabbed terminal on rikka-terminal-core.
//!
//! Thin by design: local shells over ConPTY, engine sessions, one gpui window
//! per tab group. Tabs are window-independent sessions (see `hub`), so
//! detach/merge are synchronous Vec moves — the failure class where Windows
//! Terminal crashes (live-control migration racing output) cannot occur.
//!
//! Keys: Ctrl+Shift+T new tab / W close / D detach to a new window /
//! A merge every window into this one (M where the OS delivers it);
//! Ctrl+PageDown/PageUp (or Ctrl+Tab where delivered) cycles;
//! Ctrl+Shift+C/V (and Ctrl/Shift+Insert) copy/paste;
//! Ctrl+Shift+L toggles session logging (● in the tab; see session_log);
//! Shift+PageUp/PageDown pages the scrollback.
//! The Ctrl+Shift chords are reassignable via `[keys]` (keymap.rs).

// Release builds are GUI-subsystem: no console window tags along (and
// closing it can no longer kill the app with it). Debug builds keep the
// console for printf-style work; release diagnostics go to the panic log.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod attach;
mod cli;
mod config;
mod hub;
mod keymap;
#[cfg(windows)]
mod pty_local;
mod session_log;
mod settings_window;
mod tab_icon;
#[cfg(windows)]
mod tab_move;
mod taskbar_progress;
mod tsf;
mod wt_profiles;
mod wt_schemes;

use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, Application, Bounds, ClickEvent, Context, Entity, FocusHandle, KeyDownEvent, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, TitlebarOptions, Window, WindowBounds, WindowControlArea,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};
use parking_lot::FairMutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use gpui_component::menu::ContextMenuExt as _;
use hub::TabEntry;
use rikka_terminal_core::ime::{ImeHost, TerminalIme};
use rikka_terminal_core::keys::{key_delivery_plans, printable_text};
use rikka_terminal_core::renderer::{measure_cell_metrics, render_grid};
use rikka_terminal_core::selection::{self, SelectionHost, SelectionState};
use rikka_terminal_core::{PtyResizer, ReportMods, TerminalSession, xtversion};
use rikka_terminal_ipc as ipc;

/// Default font: always present on Windows; CJK falls through DirectWrite's
/// system fallback. Bundled fonts are a P1 item.
const MONO_FONT: &str = "Consolas";
/// Horizontal pane padding (logical px); half on each side via `.px_1()`.
/// No vertical padding — the grid sits flush against the tab strip (a top
/// band of pane background reads as an ugly gap below the tabs).
const PAD: f32 = 8.0;
/// Tab strip height (logical px): WinUI TabView geometry — 8px breathing room
/// on top (TabViewHeaderPadding) + 32px tab zone (TabViewItemMinHeight).
const TAB_STRIP_H: f32 = 40.0;
const TAB_H: f32 = 32.0;
// ── chrome palette: Files (files.community) = WinUI TabView restyled ─────────
// Tokens lifted from TabView_themeresources.xaml / Common_themeresources_any
// (both MIT), dark theme — including the dark-gray surface ladder:
// SolidBackgroundFillColorBase for the window chrome and
// SolidBackgroundFillColorTertiary for the content layer, which is exactly
// the brush WinUI points TabViewItemHeaderBackgroundSelected at, so the
// selected tab merges with the pane by construction.
const CHROME_BG: u32 = 0x202020;
/// Pane surface = SolidBackgroundFillColorTertiary; the selected tab shares
/// it (the WinUI merge).
const PANE_BG: u32 = 0x282828;
/// LayerOnMicaBaseAltFillColorSecondary — unselected tab hover.
const TAB_HOVER: u32 = 0xFFFFFF0F;
/// SubtleFillColorSecondary — small button (close / add / caption) hover.
const SUBTLE_HOVER: u32 = 0xFFFFFF0F;
/// TextFillColorPrimary / TextFillColorSecondary.
const TEXT_PRIMARY: u32 = 0xFFFFFF;
const TEXT_SECONDARY: u32 = 0xFFFFFFC5;
/// DividerStrokeColorDefault — the 1px separators between unselected tabs.
const DIVIDER: u32 = 0xFFFFFF15;
/// PTY-burst coalescing window (same rationale/value as shogun-desktop).
pub(crate) const FRAME_COALESCE: Duration = Duration::from_millis(8);

gpui::actions!(
    rikka_terminal,
    [
        TerminalCopy,
        TerminalPaste,
        PaneToTab,
        PaneToWindow,
        TogglePaneBroadcast
    ]
);

/// Appearance/terminal settings resolved once at startup from
/// `%APPDATA%/rikka-terminal/config.toml` (see `config.rs`).
static FONT_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static ACRYLIC_CFG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static SCROLLBACK_CFG: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
/// The `[theme]` palette (inline + `wt_scheme`), the base a tab with no
/// per-profile scheme falls back to. `None` = no global theme configured.
static CONFIG_PALETTE: std::sync::OnceLock<rikka_terminal_core::theme::Palette> =
    std::sync::OnceLock::new();

fn apply_appearance(cfg: &config::Config) {
    if let Some(size) = cfg.appearance.font_size {
        rikka_terminal_core::typography::set_font_size(size);
    }
    if let Some(mult) = cfg.appearance.line_height {
        rikka_terminal_core::typography::set_line_height(mult);
    }
    if let Some(font) = &cfg.appearance.font {
        let _ = FONT_OVERRIDE.set(font.clone());
    }
    if let Some(features) = &cfg.appearance.font_features {
        rikka_terminal_core::renderer::set_font_features(config::parse_font_features(features));
    }
    match cfg.appearance.search_style.as_deref() {
        Some("vscode") => rikka_terminal_core::search_bar::set_sheet(
            rikka_terminal_core::search_bar::SearchColors::vscode(),
        ),
        None | Some("winui") => {}
        Some(other) => log::warn!("[config] search_style {other:?} unknown (winui|vscode)"),
    }
    let _ = ACRYLIC_CFG.set(cfg.appearance.acrylic.unwrap_or(false));
    let _ = SCROLLBACK_CFG.set(cfg.terminal.scrollback.map(|n| n as usize));
}

/// The environment/identity a spawned shell is launched with. Resolved once
/// from `[terminal]` at startup; every spawn reads it (`pty_local`).
struct SpawnIdentity {
    /// `TERM` — capability/terminfo name (default `xterm-256color`).
    term: String,
    /// XTVERSION reply body (`DCS >| <this> ST`).
    xtversion: String,
    /// `(TERM_PROGRAM, TERM_PROGRAM_VERSION)`.
    term_program: (String, String),
}

impl Default for SpawnIdentity {
    fn default() -> Self {
        Self {
            term: "xterm-256color".into(),
            xtversion: rikka_terminal_core::xtversion::engine_identity(),
            term_program: (
                rikka_terminal_core::xtversion::TERM_PROGRAM.into(),
                rikka_terminal_core::xtversion::TERM_PROGRAM_VERSION.into(),
            ),
        }
    }
}

static SPAWN_IDENTITY: std::sync::OnceLock<SpawnIdentity> = std::sync::OnceLock::new();

/// Resolve `[terminal] term`/`identity` and stash it for spawns. Default is
/// honest (`rikka-terminal` / `xterm-256color`); `identity = "ghostty"`
/// masquerades so emulator-sniffing apps enable kitty features — reserved for
/// non-ConPTY paths (SSH/Unix), where advertising them doesn't invite
/// conhost-stripped protocols.
fn apply_identity(cfg: &config::Config) {
    let _ = SPAWN_IDENTITY.set(resolve_spawn_identity(&cfg.terminal));
}

/// Pure mapping of `[terminal]` into a [`SpawnIdentity`] (default honest /
/// `xterm-256color`; `identity = "ghostty"` masquerades).
fn resolve_spawn_identity(t: &config::TerminalSection) -> SpawnIdentity {
    let mut id = SpawnIdentity::default();
    if let Some(term) = &t.term {
        id.term = term.clone();
    }
    match t.identity.as_deref() {
        Some("ghostty") => {
            id.xtversion = "ghostty 1.3.1".into();
            id.term_program = ("ghostty".into(), "1.3.1".into());
        }
        Some("honest") | None => {}
        Some(other) => log::warn!("[terminal] unknown identity {other:?} — using honest"),
    }
    id
}

fn spawn_identity() -> &'static SpawnIdentity {
    SPAWN_IDENTITY.get_or_init(SpawnIdentity::default)
}

/// `TERM` for spawned shells.
pub(crate) fn spawn_term() -> &'static str {
    &spawn_identity().term
}

/// XTVERSION identity string for spawned sessions.
pub(crate) fn spawn_xtversion() -> &'static str {
    &spawn_identity().xtversion
}

/// `(TERM_PROGRAM, TERM_PROGRAM_VERSION)` for spawned shells.
pub(crate) fn spawn_term_program() -> (&'static str, &'static str) {
    let tp = &spawn_identity().term_program;
    (&tp.0, &tp.1)
}

/// Resolve `[theme]` into a palette and install it engine-wide. Order:
/// start from the built-in default, fold a `wt_scheme` import over it (compat
/// mode), then apply inline `#RRGGBB` overrides last so they always win. A
/// section with none of these set leaves the built-in palette untouched.
fn apply_theme(cfg: &config::Config) {
    use rikka_terminal_core::theme::{self, Rgb};
    let t = &cfg.theme;
    if t.wt_scheme.is_none()
        && t.background.is_none()
        && t.foreground.is_none()
        && t.selection.is_none()
        && t.ansi.is_none()
    {
        return;
    }
    let parse = |s: &str| -> Option<Rgb> {
        let h = s.strip_prefix('#').unwrap_or(s);
        (h.len() == 6)
            .then(|| u32::from_str_radix(h, 16).ok())
            .flatten()
            .map(|v| Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
    };
    let mut palette = theme::DEFAULT;
    if let Some(name) = &t.wt_scheme {
        match wt_schemes::palette_for(name, palette.clone()) {
            Some(p) => palette = p,
            None => log::warn!("[theme] wt_scheme {name:?} not found in wt settings/fragments"),
        }
    }
    let apply = |slot: &mut Rgb, v: &Option<String>| {
        if let Some(rgb) = v.as_deref().and_then(parse) {
            *slot = rgb;
        }
    };
    apply(&mut palette.background, &t.background);
    apply(&mut palette.foreground, &t.foreground);
    apply(&mut palette.selection, &t.selection);
    if let Some(list) = &t.ansi {
        if list.len() == 16 {
            for (slot, v) in palette.ansi.iter_mut().zip(list) {
                if let Some(rgb) = parse(v) {
                    *slot = rgb;
                }
            }
        } else {
            log::warn!(
                "[theme] ansi must have exactly 16 entries, got {}",
                list.len()
            );
        }
    }
    // Remember the base for per-tab resolution and install it as the
    // initial palette (pre-first-tab paint); after_tab_change then keeps the
    // global in step with the active tab.
    let _ = CONFIG_PALETTE.set(palette.clone());
    theme::set_palette(palette);
}

/// Resolve a profile's color-scheme name into a palette, folded onto the
/// `[theme]` base (or the built-in default). `None` for an unknown name (the
/// tab then follows the global theme) — logged so a typo is visible.
fn resolve_tab_palette(scheme: &str) -> Option<rikka_terminal_core::theme::Palette> {
    let base = CONFIG_PALETTE
        .get()
        .cloned()
        .unwrap_or(rikka_terminal_core::theme::DEFAULT);
    match wt_schemes::palette_for(scheme, base) {
        Some(p) => Some(p),
        None => {
            log::warn!("[profile] color scheme {scheme:?} not found in wt settings/fragments");
            None
        }
    }
}

/// Install the active tab's palette (its profile scheme, else the global
/// `[theme]`, else the built-in default) into the engine, so the visible tab
/// wears its own colors. Called from `after_tab_change`.
fn apply_active_theme(palette: Option<rikka_terminal_core::theme::Palette>) {
    use rikka_terminal_core::theme;
    match palette.or_else(|| CONFIG_PALETTE.get().cloned()) {
        Some(p) => theme::set_palette(p),
        None => theme::clear_palette(),
    }
}

/// The grid font: configured, or the classic default.
fn mono_font() -> &'static str {
    FONT_OVERRIDE.get().map(String::as_str).unwrap_or(MONO_FONT)
}

/// Configured scrollback capacity, applied to every new session by
/// [`hub::new_tab`]. `None` = keep the engine default.
pub(crate) fn configured_scrollback() -> Option<usize> {
    SCROLLBACK_CFG.get().copied().flatten()
}

/// `[appearance] acrylic = true` (or RIKKA_ACRYLIC=1) → system acrylic
/// blur behind the window, with the chrome and pane surround going
/// translucent. The grid itself stays opaque (the engine paints cell
/// backgrounds) — blur belongs to the chrome, not under the text.
fn acrylic() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("RIKKA_ACRYLIC").is_ok_and(|v| v != "0")
            || ACRYLIC_CFG.get().copied().unwrap_or(false)
    })
}

/// Strip fill: solid chrome, or a 72% tint over acrylic.
fn chrome_fill() -> gpui::Rgba {
    if acrylic() {
        gpui::rgba(0x202020B8)
    } else {
        rgb(CHROME_BG)
    }
}

/// Pane surround fill (also the selected tab, which merges with it): solid,
/// or a 78% tint over acrylic.
fn pane_fill() -> gpui::Rgba {
    // With a theme active, the pane adopts the palette background so the grid
    // sits on the scheme's color (and the selected tab, sharing this fill,
    // stays merged); unthemed keeps the WinUI tertiary fill. is_overridden
    // tracks the ACTIVE tab's theme (after_tab_change swaps it per tab).
    let base = if rikka_terminal_core::theme::is_overridden() {
        let c = rikka_terminal_core::theme::background();
        u32::from_be_bytes([0, c.r, c.g, c.b])
    } else {
        PANE_BG
    };
    if acrylic() {
        // Same 0xC8 (78%) tint over acrylic, on the themed (or default) base.
        gpui::rgba((base << 8) | 0xC8)
    } else {
        rgb(base)
    }
}

// ── local PTY plumbing ───────────────────────────────────────────────────────

/// Newtype asserting `Box<dyn MasterPty>` is `Send + Sync` — same reasoning as
/// shogun-desktop's pty_spawn: ConPTY's HPCON is thread-safe for resize, and
/// the FairMutex serializes access anyway.
struct SendMaster(Box<dyn portable_pty::MasterPty>);
unsafe impl Send for SendMaster {}
unsafe impl Sync for SendMaster {}

struct LocalResizer {
    master: FairMutex<SendMaster>,
}

impl PtyResizer for LocalResizer {
    fn resize(&self, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> Result<()> {
        self.master.lock().0.resize(PtySize {
            rows,
            cols,
            pixel_width,
            pixel_height,
        })?;
        Ok(())
    }
}

/// Spawn `program args…` on a local PTY and wire it into an engine session.
///
/// Windows first tries the handoff-shaped direct ConPTY drive (pty_local) —
/// the session then owns the same handle set an OS handoff delivers, which
/// is what cross-window tab moves ride. portable-pty stays as the fallback
/// (and the `RIKKA_LEGACY_PTY` escape hatch) so a missing/mismatched
/// sideload pair degrades to the old path instead of a dead tab.
fn spawn_local_shell(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    cols: u16,
    rows: u16,
) -> Result<TerminalSession> {
    #[cfg(windows)]
    if std::env::var_os("RIKKA_LEGACY_PTY").is_none() {
        match pty_local::spawn_local(program, args, cwd, cols, rows) {
            Ok(session) => return Ok(session),
            Err(e) => {
                log::warn!("handoff-shaped local spawn failed, using portable-pty: {e:#}");
            }
        }
    }
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }
    cmd.env("TERM_PROGRAM", xtversion::TERM_PROGRAM);
    cmd.env("TERM_PROGRAM_VERSION", xtversion::TERM_PROGRAM_VERSION);
    let _child = pair.slave.spawn_command(cmd)?;
    let writer: Box<dyn Write + Send> = pair.master.take_writer()?;
    let reader: Box<dyn Read + Send> = Box::new(pair.master.try_clone_reader()?);
    let resizer: Arc<dyn PtyResizer> = Arc::new(LocalResizer {
        master: FairMutex::new(SendMaster(pair.master)),
    });
    let session = rikka_terminal_core::pty_session::build_terminal_session(
        cols,
        rows,
        reader,
        Arc::new(FairMutex::new(writer)),
        resizer,
        spawn_xtversion(),
    )?;
    // portable-pty on Windows is ConPTY underneath — same conhost reflow
    // and no kitty-keyboard advertisement (mark_conpty docs).
    #[cfg(windows)]
    session.mark_conpty();
    Ok(session)
}

/// The shells to try, most preferred first.
fn shell_candidates() -> Vec<String> {
    if cfg!(windows) {
        let mut v = vec!["pwsh.exe".to_string(), "powershell.exe".to_string()];
        v.push(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()));
        v
    } else {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
    }
}

/// A wt profile as a CLI spec: its resolved argv replaces the shell, its
/// name seeds the tab title (until the app's OSC 0/2 overrides it).
fn profile_to_spec(p: &wt_profiles::WtProfile) -> cli::TabSpec {
    cli::TabSpec {
        profile: None,
        dir: p.dir.clone(),
        title: Some(p.name.clone()),
        cmdline: p.argv.clone(),
        hold: false,
        color_scheme: p.color_scheme.clone(),
    }
}

/// The spec a plain new-tab opens: the configured default wt profile, or the
/// built-in shell search when no profile menu is active.
fn default_spec(cx: &App) -> cli::TabSpec {
    let menu = &cx.global::<hub::ProfileMenu>().0;
    match menu.default.and_then(|i| menu.profiles.get(i)) {
        Some(p) => profile_to_spec(p),
        None => cli::TabSpec::default(),
    }
}

/// Progress for a tab: an explicit OSC 9;4 report when the app sent one,
/// otherwise inferred from a window-title spinner. The fallback covers agents
/// that drop OSC 9;4 on some surfaces — Claude Code animates a Braille spinner
/// in the title and emits no OSC 9;4 inside tmux, so its activity would
/// otherwise never show. (Same layering as shogun-desktop; the heuristic lives
/// in `rikka-terminal-agent-integration`. Needs tmux `set-titles on`.)
fn tab_progress(
    session: &rikka_terminal_core::TerminalSession,
) -> Option<(rikka_terminal_core::progress::ProgressState, u8)> {
    if let Some(explicit) = session.progress.get() {
        return Some(explicit);
    }
    let title = session.title.lock();
    rikka_terminal_agent_integration::progress_from_title(title.as_deref().unwrap_or("")).map(
        |_| {
            (
                rikka_terminal_core::progress::ProgressState::Indeterminate,
                0,
            )
        },
    )
}

/// Pick the more urgent of two progress reports for the shared taskbar button:
/// Error > Normal (higher percent wins) > Warning > Indeterminate.
fn taskbar_aggregate(
    a: Option<(rikka_terminal_core::progress::ProgressState, u8)>,
    b: Option<(rikka_terminal_core::progress::ProgressState, u8)>,
) -> Option<(rikka_terminal_core::progress::ProgressState, u8)> {
    use rikka_terminal_core::progress::ProgressState as S;
    fn rank(s: S) -> u8 {
        match s {
            S::Error => 3,
            S::Normal => 2,
            S::Warning => 1,
            S::Indeterminate => 0,
        }
    }
    match (a, b) {
        (Some(x), Some(y)) => Some(match rank(x.0).cmp(&rank(y.0)) {
            std::cmp::Ordering::Greater => x,
            std::cmp::Ordering::Less => y,
            std::cmp::Ordering::Equal => {
                if x.1 >= y.1 {
                    x
                } else {
                    y
                }
            }
        }),
        (v, None) | (None, v) => v,
    }
}

/// wt-style circular progress in a tab's icon slot (shown in place of the
/// shell icon while progress is active). Normal = a green arc sweeping
/// clockwise from 12 o'clock; error / warning = a full red / gold ring;
/// indeterminate = a rotating arc.
fn progress_ring(
    id: impl Into<gpui::ElementId>,
    (state, percent): (rikka_terminal_core::progress::ProgressState, u8),
) -> gpui::AnyElement {
    use gpui::AnimationExt as _;
    use rikka_terminal_core::progress::ProgressState;
    // Slot-sized like the 16px icon, so icon↔ring swaps don't shift the title.
    const D: f32 = 16.0;
    let slot = || div().w(px(D)).h(px(D)).mr(px(6.)).flex_shrink_0();
    match state {
        // Full static rings: a rounded-full border IS the ring — no path math.
        ProgressState::Error => slot()
            .rounded_full()
            .border_2()
            .border_color(rgb(0xBE5A50))
            .into_any_element(),
        ProgressState::Warning => slot()
            .rounded_full()
            .border_2()
            .border_color(rgb(0xC9A94E))
            .into_any_element(),
        ProgressState::Normal => slot()
            .child(ring_arc(percent as f32 / 100.0, 0.0, rgb(0x16C60C)))
            .into_any_element(),
        ProgressState::Indeterminate => slot()
            .with_animation(
                id,
                gpui::Animation::new(std::time::Duration::from_millis(1200)).repeat(),
                |el, delta| el.child(ring_arc(0.30, delta, rgb(0x60CDFF))),
            )
            .into_any_element(),
    }
}

/// A circular track plus an arc of `fraction` turns starting `start_turns`
/// past 12 o'clock, painted clockwise with gpui's path API inside a canvas.
fn ring_arc(fraction: f32, start_turns: f32, color: gpui::Rgba) -> gpui::AnyElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            use std::f32::consts::{FRAC_PI_2, PI, TAU};
            let c = bounds.center();
            let r = (bounds.size.width / px(1.)).min(bounds.size.height / px(1.)) * 0.5 - 1.5;
            if r <= 0.0 {
                return;
            }
            let at = |a: f32| gpui::point(c.x + px(r * a.cos()), c.y + px(r * a.sin()));
            let radii = gpui::point(px(r), px(r));
            // Track: a full circle from two half arcs (a single arc with
            // start == end would collapse).
            let mut pb = gpui::PathBuilder::stroke(px(1.5));
            pb.move_to(at(-FRAC_PI_2));
            pb.arc_to(radii, px(0.), false, true, at(FRAC_PI_2));
            pb.arc_to(radii, px(0.), false, true, at(-FRAC_PI_2));
            if let Ok(p) = pb.build() {
                window.paint_path(p, gpui::rgba(0xFFFFFF30));
            }
            // The progress arc (screen y grows downward, so sweep=true is
            // clockwise).
            let frac = fraction.clamp(0.0, 1.0);
            if frac < 0.004 {
                return;
            }
            let a0 = start_turns * TAU - FRAC_PI_2;
            let mut pb = gpui::PathBuilder::stroke(px(2.));
            pb.move_to(at(a0));
            if frac >= 0.999 {
                pb.arc_to(radii, px(0.), false, true, at(a0 + PI));
                pb.arc_to(radii, px(0.), false, true, at(a0));
            } else {
                pb.arc_to(radii, px(0.), frac > 0.5, true, at(a0 + frac * TAU));
            }
            if let Ok(p) = pb.build() {
                window.paint_path(p, color);
            }
        },
    )
    .size_full()
    .into_any_element()
}

/// Rainbow segments of the animated progress fill (a seamless scrolling
/// spectrum — each segment is a gradient to the next segment's hue).
const PROGRESS_RAINBOW_SEGMENTS: usize = 16;

/// A 3px full-width progress bar (the SD/ghostty-style strip the active tab
/// shows across the top of the pane).
///
/// Normal fills to the percentage with a scrolling rainbow (static green was
/// too easy to miss — same ゲーミング仕様 as shogun-desktop); indeterminate is
/// the full-width rainbow. Error = red and warning = gold stay static so their
/// semantics read at a glance. `with_animation` only requests frames while the
/// element renders, so idle windows stay at zero CPU.
fn render_progress_bar(
    id: impl Into<gpui::ElementId>,
    (state, percent): (rikka_terminal_core::progress::ProgressState, u8),
) -> gpui::AnyElement {
    use gpui::AnimationExt as _;
    use rikka_terminal_core::progress::ProgressState;

    let fraction = match state {
        ProgressState::Normal => percent as f32 / 100.0,
        ProgressState::Indeterminate => 1.0,
        // Keep a visible sliver even at 0% so the state itself shows.
        ProgressState::Error | ProgressState::Warning => (percent as f32 / 100.0).max(0.05),
    };
    let fill = div().h_full().w(gpui::relative(fraction.clamp(0.0, 1.0)));
    let fill: gpui::AnyElement = match state {
        ProgressState::Error => fill.bg(rgb(0xBE5A50)).into_any_element(),
        ProgressState::Warning => fill.bg(rgb(0xC9A94E)).into_any_element(),
        ProgressState::Normal | ProgressState::Indeterminate => fill
            .with_animation(
                id,
                gpui::Animation::new(std::time::Duration::from_secs(2)).repeat(),
                |bar, delta| {
                    let seg_hue = move |i: usize| {
                        let hue =
                            (i as f32 / PROGRESS_RAINBOW_SEGMENTS as f32 - delta).rem_euclid(1.0);
                        gpui::hsla(hue, 0.75, 0.42, 1.0)
                    };
                    bar.child(div().flex().flex_row().size_full().children(
                        (0..PROGRESS_RAINBOW_SEGMENTS).map(move |i| {
                            div().flex_1().h_full().bg(gpui::linear_gradient(
                                90.,
                                gpui::linear_color_stop(seg_hue(i), 0.),
                                gpui::linear_color_stop(seg_hue(i + 1), 1.),
                            ))
                        }),
                    ))
                },
            )
            .into_any_element(),
    };
    div()
        .w_full()
        .h(px(3.))
        .bg(gpui::rgba(0x00000059))
        .child(fill)
        .into_any_element()
}

/// Render a resolved icon (a raster program icon or a tinted distro glyph) with
/// a trailing margin — shared by the tab strip and the new-tab dropdown.
fn icon_element(icon: tab_icon::TabIcon, margin_right: f32) -> gpui::AnyElement {
    let inner: gpui::AnyElement = match icon {
        tab_icon::TabIcon::Image(data) => gpui::img(data).w(px(16.)).h(px(16.)).into_any_element(),
        tab_icon::TabIcon::Glyph { text, tint } => div()
            .font_family(tab_icon::FONT_LOGOS)
            .text_size(px(14.))
            .text_color(rgb(tint))
            .child(text)
            .into_any_element(),
    };
    div()
        .flex_shrink_0()
        .mr(px(margin_right))
        .child(inner)
        .into_any_element()
}

/// Starting directory for a tab that didn't specify one: the user's home,
/// wt-style (wt's default `startingDirectory` is `%USERPROFILE%`). A shell
/// must never open wherever the host process happened to have its cwd — the
/// default-terminal and forwarded-monarch paths regularly sit in System32.
/// Explicit locations (`rt <dir>` / `rt .`, a profile's `dir`) still win.
fn default_shell_dir() -> Option<String> {
    let home = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())?;
    std::path::Path::new(&home).is_dir().then_some(home)
}

/// Tab from a CLI spec (wt semantics): an explicit commandline replaces the
/// shell entirely; `-p` narrows the shell to one candidate; `--title` seeds
/// the tab title until the application's OSC 0/2 takes over.
/// One tab: a pane tree (a single leaf until the first split) plus which
/// pane holds the focus. Pane ids are tab-local and never reused.
struct Tab {
    root: PaneNode,
    active_pane: usize,
    next_pane_id: usize,
    /// Split-tab zoom: the focused pane temporarily fills the whole tab
    /// area. The tree keeps its shape — only rendering collapses.
    zoomed: bool,
    /// Broadcast input: keystrokes, pastes and IME commits go to EVERY
    /// pane of this tab (multi-server fan-out, wt/iTerm2's shape).
    broadcast: bool,
}

/// One pane of a tab. `measured` is the painted size sink (from the pane
/// overlay canvas) driving this pane's own PTY fit once the tab is split;
/// `origin` (window coords) completes the painted bounds so window-level
/// wheel events can be routed to the pane under the cursor.
struct Leaf {
    id: usize,
    entry: TabEntry,
    measured: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    origin: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
}

impl Leaf {
    fn new(id: usize, entry: TabEntry) -> Self {
        Leaf {
            id,
            entry,
            measured: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            origin: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
        }
    }
}

enum PaneNode {
    Leaf(Leaf),
    Split {
        /// `true` = children sit side by side (a vertical divider).
        horizontal: bool,
        ratio: f32,
        a: Box<PaneNode>,
        b: Box<PaneNode>,
    },
    /// Transient tombstone for in-place tree surgery ([`PaneNode::remove`])
    /// — never survives a call.
    Empty,
}

impl PaneNode {
    fn first_leaf(&self) -> Option<&Leaf> {
        match self {
            PaneNode::Leaf(l) => Some(l),
            PaneNode::Split { a, b, .. } => a.first_leaf().or_else(|| b.first_leaf()),
            PaneNode::Empty => None,
        }
    }

    fn find(&self, id: usize) -> Option<&Leaf> {
        match self {
            PaneNode::Leaf(l) => (l.id == id).then_some(l),
            PaneNode::Split { a, b, .. } => a.find(id).or_else(|| b.find(id)),
            PaneNode::Empty => None,
        }
    }

    fn for_each(&self, f: &mut dyn FnMut(&Leaf)) {
        match self {
            PaneNode::Leaf(l) => f(l),
            PaneNode::Split { a, b, .. } => {
                a.for_each(f);
                b.for_each(f);
            }
            PaneNode::Empty => {}
        }
    }

    /// Collect leaf references (unlike [`Self::for_each`], the borrows
    /// escape — needed to gather input-fan-out targets).
    fn leaves<'a>(&'a self, out: &mut Vec<&'a Leaf>) {
        match self {
            PaneNode::Leaf(l) => out.push(l),
            PaneNode::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
            PaneNode::Empty => {}
        }
    }

    /// Split leaf `id` in place: it becomes `Split{old, new}` (or
    /// `{new, old}` with `new_first` — a drop on the left/top edge).
    /// Returns the leaf back when `id` is not in this subtree.
    fn split(
        &mut self,
        id: usize,
        horizontal: bool,
        new_leaf: Leaf,
        new_first: bool,
    ) -> Option<Leaf> {
        match self {
            PaneNode::Leaf(l) if l.id == id => {
                let old = std::mem::replace(self, PaneNode::Empty);
                let (a, b) = if new_first {
                    (PaneNode::Leaf(new_leaf), old)
                } else {
                    (old, PaneNode::Leaf(new_leaf))
                };
                *self = PaneNode::Split {
                    horizontal,
                    ratio: 0.5,
                    a: Box::new(a),
                    b: Box::new(b),
                };
                None
            }
            PaneNode::Leaf(..) | PaneNode::Empty => Some(new_leaf),
            PaneNode::Split { a, b, .. } => {
                let leftover = a.split(id, horizontal, new_leaf, new_first)?;
                b.split(id, horizontal, leftover, new_first)
            }
        }
    }

    /// Remove leaf `id`, promoting its sibling into the parent's slot.
    /// Returns the removed entry. Only meaningful under a Split — a lone
    /// root leaf is a tab close, handled by the caller.
    fn remove(&mut self, id: usize) -> Option<TabEntry> {
        let PaneNode::Split { a, b, .. } = self else {
            return None;
        };
        let a_hit = matches!(a.as_ref(), PaneNode::Leaf(l) if l.id == id);
        let b_hit = matches!(b.as_ref(), PaneNode::Leaf(l) if l.id == id);
        if a_hit || b_hit {
            let (victim, sibling) = if a_hit { (a, b) } else { (b, a) };
            let PaneNode::Leaf(leaf) = std::mem::replace(victim.as_mut(), PaneNode::Empty) else {
                return None;
            };
            *self = std::mem::replace(sibling.as_mut(), PaneNode::Empty);
            return Some(leaf.entry);
        }
        a.remove(id).or_else(|| b.remove(id))
    }

    /// Normalized layout: every leaf's `(id, x, y, w, h)` in 0..1 space —
    /// directional focus navigation works on these rects.
    fn rects(&self, x: f32, y: f32, w: f32, h: f32, out: &mut Vec<(usize, f32, f32, f32, f32)>) {
        match self {
            PaneNode::Leaf(l) => out.push((l.id, x, y, w, h)),
            PaneNode::Split {
                horizontal,
                ratio,
                a,
                b,
            } => {
                if *horizontal {
                    a.rects(x, y, w * ratio, h, out);
                    b.rects(x + w * ratio, y, w * (1.0 - ratio), h, out);
                } else {
                    a.rects(x, y, w, h * ratio, out);
                    b.rects(x, y + h * ratio, w, h * (1.0 - ratio), out);
                }
            }
            PaneNode::Empty => {}
        }
    }

    /// The normalized (0..1) rect of the Split at `path` (`false` = a,
    /// `true` = b at each hop). `None` when the path no longer leads to a
    /// Split — the tree changed under a drag, which then no-ops.
    fn split_rect(&self, path: &[bool]) -> Option<(f32, f32, f32, f32)> {
        let mut node = self;
        let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 1.0, 1.0);
        for &go_b in path {
            let PaneNode::Split {
                horizontal,
                ratio,
                a,
                b,
            } = node
            else {
                return None;
            };
            if *horizontal {
                if go_b {
                    x += w * ratio;
                    w *= 1.0 - ratio;
                } else {
                    w *= ratio;
                }
            } else if go_b {
                y += h * ratio;
                h *= 1.0 - ratio;
            } else {
                h *= ratio;
            }
            node = if go_b { b } else { a };
        }
        matches!(node, PaneNode::Split { .. }).then_some((x, y, w, h))
    }

    /// Set the ratio of the Split at `path`; a stale path no-ops.
    fn set_ratio(&mut self, path: &[bool], new_ratio: f32) {
        let mut node = self;
        for &go_b in path {
            let PaneNode::Split { a, b, .. } = node else {
                return;
            };
            node = if go_b { b } else { a };
        }
        if let PaneNode::Split { ratio, .. } = node {
            *ratio = new_ratio;
        }
    }
}

impl Tab {
    fn single(entry: TabEntry) -> Self {
        Tab {
            root: PaneNode::Leaf(Leaf::new(0, entry)),
            active_pane: 0,
            next_pane_id: 1,
            zoomed: false,
            broadcast: false,
        }
    }

    /// The top-left pane: the tab strip's face (title, icon, theme) and the
    /// unit tab moves operate on while splits don't travel.
    fn primary(&self) -> &TabEntry {
        &self
            .root
            .first_leaf()
            .expect("a tab always holds at least one pane")
            .entry
    }

    /// The focused pane: where input, search, copy/paste and the IME go.
    fn active_entry(&self) -> &TabEntry {
        self.root
            .find(self.active_pane)
            .or_else(|| self.root.first_leaf())
            .map(|l| &l.entry)
            .expect("a tab always holds at least one pane")
    }

    fn for_each_entry(&self, mut f: impl FnMut(&TabEntry)) {
        self.root.for_each(&mut |l| f(&l.entry));
    }

    /// More than one pane. Gates tab moves — splits don't travel.
    fn is_split(&self) -> bool {
        !matches!(self.root, PaneNode::Leaf(..))
    }

    /// Any pane of this tab individually marked as a broadcast recipient.
    fn has_broadcast_member(&self) -> bool {
        let mut found = false;
        self.root
            .for_each(&mut |l| found |= l.entry.0.broadcast_target());
        found
    }

    /// The sole pane of an unsplit tab; `None` (untouched drop would leak
    /// drivers, so the caller must guard with [`Self::is_split`]) otherwise.
    fn take_single(self) -> Option<TabEntry> {
        match self.root {
            PaneNode::Leaf(l) => Some(l.entry),
            _ => None,
        }
    }

    /// Split the focused pane; the new pane takes the focus. Splitting
    /// while zoomed unzooms — silently growing panes behind a zoom would
    /// disorient.
    fn split_active(&mut self, horizontal: bool, entry: TabEntry) {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        if self
            .root
            .split(self.active_pane, horizontal, Leaf::new(id, entry), false)
            .is_none()
        {
            self.active_pane = id;
        }
        self.zoomed = false;
    }

    /// Split pane `target` with `entry` on the `new_first` side (a tab
    /// dropped onto a pane edge); the new pane takes the focus.
    fn split_at(&mut self, target: usize, horizontal: bool, new_first: bool, entry: TabEntry) {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        if self
            .root
            .split(target, horizontal, Leaf::new(id, entry), new_first)
            .is_none()
        {
            self.active_pane = id;
        }
        self.zoomed = false;
    }

    /// Close the focused pane of a SPLIT tab (callers close the whole tab
    /// when it isn't). Focus falls to the first remaining leaf.
    fn close_active_pane(&mut self) -> Option<TabEntry> {
        let removed = self.root.remove(self.active_pane)?;
        self.active_pane = self.root.first_leaf().map(|l| l.id).unwrap_or(0);
        if !self.is_split() {
            self.zoomed = false;
            self.broadcast = false;
        }
        Some(removed)
    }

    fn shutdown_all(&self) {
        self.for_each_entry(|e| e.0.shutdown());
    }
}

fn create_tab_spec(cx: &mut App, spec: &cli::TabSpec) -> Option<TabEntry> {
    let candidates: Vec<String> = if !spec.cmdline.is_empty() {
        vec![spec.cmdline[0].clone()]
    } else if let Some(p) = &spec.profile {
        vec![p.clone()]
    } else {
        shell_candidates()
    };
    let args: &[String] = if spec.cmdline.len() > 1 {
        &spec.cmdline[1..]
    } else {
        &[]
    };
    // No explicit directory → the user's home, never the host process's cwd.
    let dir = spec.dir.clone().or_else(default_shell_dir);
    // Keep the program that actually spawned (the shell search may fall past
    // pwsh to cmd), so its icon matches the running shell.
    let (program, session) = candidates.iter().find_map(|program| {
        spawn_local_shell(program, args, dir.as_deref(), 80, 24)
            .ok()
            .map(|s| (program.clone(), s))
    })?;
    if let Some(t) = &spec.title {
        *session.title.lock() = Some(t.clone());
    }
    let entry = hub::new_tab(cx, session);
    // Tab icon: the program's own exe icon (Windows) or a bundled distro glyph
    // (WSL / cross-platform). Resolved once at creation like the palette below.
    entry
        .0
        .set_icon(tab_icon::resolve(&program, args, spec.title.as_deref()));
    // Resolve this profile's color scheme once, at creation (no file IO on
    // the per-tab-switch path); after_tab_change installs it when the tab
    // becomes active.
    if let Some(scheme) = &spec.color_scheme {
        entry.0.set_theme(resolve_tab_palette(scheme));
    }
    Some(entry)
}

// ── the tabbed window ────────────────────────────────────────────────────────

/// The OTHER process owning the top-level window under the mouse cursor —
/// the drag-merge target probe, called on a release outside our window.
/// `None`: nothing there, or it is ours. Whether that pid is actually a
/// rikka window is settled by `resolve_window` (unknown pids fail).
#[cfg(windows)]
fn other_process_under_cursor() -> Option<(u32, (i32, i32))> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetCursorPos, GetWindowThreadProcessId, WindowFromPoint,
    };
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }.ok()?;
    let hwnd = unsafe { WindowFromPoint(pt) };
    if hwnd.0.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(root, Some(&mut pid)) };
    (pid != 0 && pid != std::process::id()).then_some((pid, (pt.x, pt.y)))
}

/// Strip content-x → insertion index: the gap nearest the drop point.
/// Dropping on a tab's left half inserts before it, right half after;
/// anything past the last tab appends.
fn strip_insert_index(content_x: f32, tab_w: f32, len: usize) -> usize {
    if tab_w <= 0.0 {
        return len;
    }
    (((content_x / tab_w) + 0.5).floor().max(0.0) as usize).min(len)
}

/// Drag payload of a tab being moved: its strip index (`title` rides along
/// for the ghost). Dropping on another tab reorders; dropping on the pane
/// detaches into a fresh window.
#[derive(Clone)]
struct TabDrag {
    ix: usize,
    title: String,
}

/// A pane grabbed by its hover handle (split tabs) — dropped on the strip
/// it becomes a tab, on another pane's edge it rearranges, outside the
/// window it detaches.
#[derive(Clone)]
struct PaneDrag {
    pane: usize,
    title: String,
}

/// The floating preview under the pointer while a tab is dragged.
struct TabDragGhost {
    title: String,
    /// True when this is the root of the follower window rather than an
    /// overlay inside the terminal. The chip is identical either way — what
    /// you are holding must not change appearance at the window edge — so
    /// this only wraps it in a transparent margin, which keeps any edge
    /// artefact of the popup off the chip itself.
    windowed: bool,
}

impl Render for TabDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let chip = div()
            .h(px(TAB_H))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(8.))
            .bg(pane_fill())
            .border_1()
            .border_color(gpui::rgba(DIVIDER))
            .text_size(px(12.))
            .text_color(rgb(TEXT_PRIMARY))
            .child(self.title.clone());
        if self.windowed {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(chip)
                .into_any_element()
        } else {
            chip.into_any_element()
        }
    }
}

pub struct TabsWindow {
    tabs: Vec<Tab>,
    active: usize,
    terminal_focus: FocusHandle,
    ime: Entity<TerminalIme<Self>>,
    selection: SelectionState,
    /// Fractional wheel accumulators (trackpads deliver sub-line deltas).
    scroll_accum: f32,
    hwheel_accum: f32,
    /// Session identity (Arc pointer) the accumulators belong to — a wheel
    /// event for a different pane/tab starts from zero instead of inheriting
    /// the previous target's fractional residue.
    wheel_target: Option<usize>,
    /// Last OS activation state seen by the activation observer, for the
    /// paths that need it without a `&Window` (focus reporting).
    window_active: bool,
    /// Last PTY size applied to the ACTIVE tab (0 forces a re-apply, e.g.
    /// after a tab switch or an adoption from another window).
    cols: u16,
    rows: u16,
    /// Last OSC title applied to the OS window (dedup).
    applied_title: Option<String>,
    /// The new-tab profile dropdown is open (rendered below the strip).
    profile_menu: bool,
    /// Open tab context menu: (tab index, menu-left in logical px).
    tab_menu: Option<(usize, f32)>,
    /// Scrollback search bar (Ctrl+Shift+F) — shared engine widget.
    search: rikka_terminal_core::search_bar::SearchBar,
    /// Horizontal scroll of the tab viewport (Firefox-style overflow: when
    /// tabs would push the caption buttons, they scroll here instead).
    strip_scroll: ScrollHandle,
    /// The tab index riding an active drag — the tear-off handler needs it
    /// on a mouse up OUTSIDE the window, where the hitbox-based drop
    /// system cannot see anything. Cleared by every in-window resolution
    /// (the drop handlers and the root mouse-up).
    dragging_tab: Option<usize>,
    dragging_pane: Option<usize>,
    /// Title of the tab riding the active drag, kept so the follower window
    /// below can be built without reaching into the drag payload.
    drag_title: Option<String>,
    /// The preview that carries a tab drag across the desktop. A window
    /// cannot draw outside itself, so once the pointer leaves our frame the
    /// in-window ghost is replaced by this borderless, unfocused, top-most
    /// window moving under the cursor — the Chrome gesture. Alive only while
    /// the pointer is outside; back inside, the normal ghost takes over.
    drag_follower: Option<gpui::WindowHandle<TabDragGhost>>,
    /// A live divider resize: the Split's path in the active tab's tree
    /// (`false` = a, `true` = b) plus its orientation. Mouse moves on the
    /// window root steer the ratio while this is set.
    divider_drag: Option<(Vec<bool>, bool)>,
    /// Window-wide broadcast: input fans out to every pane of EVERY tab
    /// of this window (iTerm2's "all panes in all tabs"). Independent of
    /// the per-tab toggle; either one being on broadcasts.
    broadcast_all: bool,
    /// The active tab changed with the TSF store possibly mid-composition;
    /// the next render recycles the store's document so that composition
    /// cannot commit into the newly active tab's PTY.
    ime_reset_pending: bool,
}

impl ImeHost for TabsWindow {
    fn ime_session(&self) -> Option<&TerminalSession> {
        self.active_session()
    }

    fn ime_font(&self) -> &str {
        mono_font()
    }

    // Typed text rides the broadcast fan-out (candidate-window geometry
    // stays on the focused pane via ime_session).
    fn ime_commit(&self, text: &str) {
        self.send_input(text.as_bytes());
    }
}

impl SelectionHost for TabsWindow {
    fn selection_state(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    fn pane_session(&self, pane: usize) -> Option<&TerminalSession> {
        self.tabs
            .get(self.active)
            .and_then(|t| t.root.find(pane))
            .map(|l| &l.entry.0.session)
            .or_else(|| self.active_session())
    }
}

impl TabsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>, initial: Vec<TabEntry>) -> Self {
        let weak = cx.weak_entity();
        let ime = cx.new(|_| TerminalIme::new(weak));
        cx.observe(&ime, |_, _, cx| cx.notify()).detach();
        let terminal_focus = cx.focus_handle();
        window.focus(&terminal_focus);
        // TSF (always on — see `tsf`): bind the store while our terminal
        // input owns focus. The waker only schedules a notify; render then
        // drains the queued IME events.
        let tsf_view = cx.weak_entity();
        let tsf_async = cx.to_async();
        window
            .on_focus_in(&terminal_focus, cx, move |_, _| {
                let view = tsf_view.clone();
                let async_cx = tsf_async.clone();
                tsf::on_input_focus(Box::new(move || {
                    let view = view.clone();
                    async_cx
                        .spawn(async move |cx| {
                            let _ = view.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                }));
            })
            .detach();
        let blur_ime = ime.clone();
        window
            .on_focus_out(&terminal_focus, cx, move |_, _, cx| {
                tsf::on_input_blur();
                // The backend just dropped any in-flight composition; drop
                // its inline echo too, or a cancelled preedit stays on
                // screen after focus loss.
                blur_ime.update(cx, |ime, cx| {
                    if ime.marked.take().is_some() {
                        cx.notify();
                    }
                });
            })
            .detach();
        // Re-assert TSF focus once the OS window is FOREGROUND. The store's
        // SetFocus must run while our window is active for the active TIP to
        // bind the taskbar IME indicator to us and for stricter IMEs (Google
        // 日本語入力) to engage — basic composition works even bound in the
        // wrong context, but the indicator and those IMEs do not. The
        // window.focus() above fires on_focus_in during construction, before
        // the window is foreground, so this activation hook corrects it (and
        // re-binds after an alt-tab away and back).
        cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            // ?1004 focus reporting follows the OS activation.
            this.sync_focus_reports();
            if this.window_active && this.terminal_focus.is_focused(window) {
                let view = cx.weak_entity();
                let async_cx = cx.to_async();
                tsf::on_input_focus(Box::new(move || {
                    let view = view.clone();
                    async_cx
                        .spawn(async move |cx| {
                            let _ = view.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                }));
            }
        })
        .detach();
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            terminal_focus,
            ime,
            selection: SelectionState::default(),
            scroll_accum: 0.0,
            hwheel_accum: 0.0,
            wheel_target: None,
            window_active: true,
            cols: 0,
            rows: 0,
            applied_title: None,
            profile_menu: false,
            tab_menu: None,
            search: Default::default(),
            strip_scroll: ScrollHandle::default(),
            dragging_tab: None,
            dragging_pane: None,
            drag_title: None,
            drag_follower: None,
            divider_drag: None,
            broadcast_all: false,
            ime_reset_pending: false,
        };
        for entry in initial {
            this.adopt(entry, cx);
        }
        // Prime the ?1004 focus state (nothing is sent while the mode is
        // off; the first real transition then reports correctly).
        this.sync_focus_reports();
        this
    }

    fn active_session(&self) -> Option<&TerminalSession> {
        self.tabs
            .get(self.active)
            .map(|t| &t.active_entry().0.session)
    }

    /// User input (encoded keystrokes, IME commits) to the focused pane —
    /// or to EVERY pane of the active tab while its broadcast toggle is
    /// on. Broadcast reuses the active pane's encoding: per-pane terminal
    /// modes (DECCKM etc.) are not consulted for the copies. Each target
    /// snaps back to the live bottom first — typing while scrolled into
    /// the scrollback returns you to the prompt, like every terminal.
    fn send_input(&self, bytes: &[u8]) {
        for s in self.input_sessions() {
            s.scroll_display_to_bottom();
            s.send_bytes(bytes);
        }
    }

    /// Keystroke delivery, encoded PER RECIPIENT: each target session's
    /// own terminal mode picks the byte spelling (kitty CSI u vs legacy),
    /// so a mixed-mode broadcast never receives another pane's encoding
    /// (the adversarial-review finding). Returns true when consumed — the
    /// caller must stop propagation, which suppresses WM_CHAR for every
    /// target, so targets whose mode said "plain text" get the printable
    /// character inline right here instead of being orphaned. When no
    /// target consumes the key, it keeps propagating and the WM_CHAR →
    /// ime_commit path fans the text out as before.
    fn send_key_input(&self, ks: &gpui::Keystroke) -> bool {
        let targets = self.input_sessions();
        let modes: Vec<_> = targets.iter().map(|s| *s.term.lock().mode()).collect();
        let Some(plans) = key_delivery_plans(ks, &modes) else {
            return false;
        };
        for (s, plan) in targets.iter().zip(plans) {
            match plan {
                Some(bytes) => {
                    s.scroll_display_to_bottom();
                    s.send_bytes(&bytes);
                }
                None => {
                    if let Some(text) = printable_text(ks) {
                        s.scroll_display_to_bottom();
                        s.send_bytes(text.as_bytes());
                    }
                }
            }
        }
        true
    }

    /// [`Self::send_input`] for pastes (bracketed-paste aware per pane).
    fn paste_input(&self, text: &str) {
        for s in self.input_sessions() {
            s.paste(text);
        }
    }

    /// Window-root wheel: route to the pane under the cursor (split tabs)
    /// with cell coordinates measured from that pane's own painted origin —
    /// the active pane is the fallback for the gutters, and the only target
    /// while zoomed (hidden panes' painted bounds are stale). Fractional
    /// deltas accumulate per target; switching targets drops the residue.
    fn wheel_scroll(&mut self, event: &ScrollWheelEvent, cw: f32, ch: f32) {
        use std::sync::atomic::Ordering;
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let (x, y) = (event.position.x / px(1.), event.position.y / px(1.));
        let mut under: Option<usize> = None;
        if !(tab.zoomed && tab.is_split()) {
            tab.root.for_each(&mut |leaf| {
                if under.is_none() {
                    let (ox, oy) = leaf.origin.get();
                    let (mw, mh) = leaf.measured.get();
                    if mw > 0.0 && (ox..ox + mw).contains(&x) && (oy..oy + mh).contains(&y) {
                        under = Some(leaf.id);
                    }
                }
            });
        }
        let Some(leaf) = under
            .and_then(|id| tab.root.find(id))
            .or_else(|| tab.root.find(tab.active_pane))
        else {
            return;
        };
        let s = &leaf.entry.0.session;
        let target = std::sync::Arc::as_ptr(&leaf.entry.0) as usize;
        if self.wheel_target != Some(target) {
            self.wheel_target = Some(target);
            self.scroll_accum = 0.0;
            self.hwheel_accum = 0.0;
        }
        let (ox, oy) = leaf.origin.get();
        let cols = s.cols.load(Ordering::Relaxed).max(1) as usize;
        let rows = s.rows.load(Ordering::Relaxed).max(1) as usize;
        let col = (((x - ox) / cw).max(0.0) as usize).min(cols - 1);
        let row = (((y - oy) / ch).max(0.0) as usize).min(rows - 1);
        let mods = ReportMods {
            alt: event.modifiers.alt,
            ctrl: event.modifiers.control,
        };
        // Vertical: PTY first (mouse reporting / alternate scroll), local
        // scrollback otherwise.
        self.scroll_accum += match &event.delta {
            ScrollDelta::Pixels(p) => (p.y / px(1.)) / ch,
            ScrollDelta::Lines(l) => l.y,
        };
        let whole = self.scroll_accum.trunc() as i32;
        if whole != 0 {
            self.scroll_accum -= whole as f32;
            if !s.wheel_to_pty(whole, col, row, mods) {
                s.scroll_display(whole);
            }
        }
        // Horizontal: reporting-only (buttons 66/67).
        self.hwheel_accum += match &event.delta {
            ScrollDelta::Pixels(p) => (p.x / px(1.)) / cw,
            ScrollDelta::Lines(l) => l.x,
        };
        let whole_x = self.hwheel_accum.trunc() as i32;
        if whole_x != 0 {
            self.hwheel_accum -= whole_x as f32;
            s.hwheel_to_pty(whole_x, col, row, mods);
        }
    }

    /// Focus reporting (DECSET ?1004): the active pane of the active tab is
    /// "focused" while the OS window is; every other session of this window
    /// is not. Sessions de-duplicate and check the mode themselves, so
    /// calling this on every activation / tab / pane change is idempotent.
    fn sync_focus_reports(&self) {
        for (ix, tab) in self.tabs.iter().enumerate() {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            for leaf in leaves {
                let focused = self.window_active && ix == self.active && leaf.id == tab.active_pane;
                leaf.entry.0.session.report_focus(focused);
            }
        }
    }

    /// Every session user input goes to right now, deduplicated: the
    /// focused pane always, plus the broadcast recipients — the whole
    /// window (`broadcast_all`), the active tab (its `broadcast` toggle),
    /// and every individually marked pane (選択ブロードキャスト), which
    /// receives copies no matter which tab is focused.
    fn input_sessions(&self) -> Vec<&TerminalSession> {
        let mut seen: Vec<*const hub::TabSession> = Vec::new();
        let mut out: Vec<&TerminalSession> = Vec::new();
        let Some(active_tab) = self.tabs.get(self.active) else {
            return out;
        };
        let active_entry = active_tab.active_entry();
        seen.push(std::sync::Arc::as_ptr(&active_entry.0));
        out.push(&active_entry.0.session);
        for (ix, tab) in self.tabs.iter().enumerate() {
            let whole_tab = self.broadcast_all || (ix == self.active && tab.broadcast);
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            for leaf in leaves {
                if !(whole_tab || leaf.entry.0.broadcast_target()) {
                    continue;
                }
                let p = std::sync::Arc::as_ptr(&leaf.entry.0);
                if !seen.contains(&p) {
                    seen.push(p);
                    out.push(&leaf.entry.0.session);
                }
            }
        }
        out
    }

    /// Run one `[keys]` action — the single dispatch point for the
    /// configurable chords (keymap.rs).
    fn perform(&mut self, action: keymap::Action, window: &mut Window, cx: &mut Context<Self>) {
        use keymap::Action::*;
        match action {
            NewTab => self.new_tab(cx),
            CloseTab => {
                // With a split, W closes the focused PANE (wt's shape); the
                // whole tab only goes when a single pane remains.
                if self.tabs.get(self.active).is_some_and(Tab::is_split) {
                    self.close_active_pane(cx);
                } else {
                    self.close_active(window, cx);
                }
            }
            DetachTab => self.detach_active(cx),
            #[cfg(windows)]
            EjectTab => self.eject_active(window, cx),
            #[cfg(windows)]
            MoveTab => self.move_active_to_other_window(window, cx),
            // No cross-process moves off Windows (yet) — degrade to the
            // in-process split.
            #[cfg(not(windows))]
            EjectTab => self.detach_active(cx),
            #[cfg(not(windows))]
            MoveTab => {}
            MergeAll => self.merge_all(cx),
            // Session logging toggle — the ● in the tab is the feedback,
            // so redraw right away.
            ToggleLogging => {
                if let Some(s) = self.active_session() {
                    session_log::toggle(s);
                }
                cx.notify();
            }
            Copy => selection::copy_to_clipboard(&self.selection, self.active_session(), cx),
            Paste => {
                if let Some(item) = cx.read_from_clipboard()
                    && let Some(text) = item.text()
                {
                    self.paste_input(&text);
                }
            }
            CycleBack => self.cycle(false, cx),
            // Shell integration: hop between OSC 133 prompt marks.
            JumpPromptPrev => {
                if let Some(s) = self.active_session() {
                    s.jump_prompt(-1);
                }
            }
            JumpPromptNext => {
                if let Some(s) = self.active_session() {
                    s.jump_prompt(1);
                }
            }
            OpenSettings => settings_window::open(cx),
            SplitRight => self.split_active_pane(true, cx),
            SplitDown => self.split_active_pane(false, cx),
            // Zoom: the focused pane fills the tab area; the tree stays.
            ZoomPane => {
                if let Some(tab) = self.tabs.get_mut(self.active)
                    && tab.is_split()
                {
                    tab.zoomed = !tab.zoomed;
                    cx.notify();
                }
            }
            Search => {
                let sess = self
                    .tabs
                    .get(self.active)
                    .map(|t| &t.active_entry().0.session);
                self.search.toggle(sess);
                cx.notify();
            }
        }
    }

    /// Step the open search (↑/↓ buttons, Enter): shared helper for the
    /// button listeners.
    fn search_nav(&mut self, dir: i32, cx: &mut Context<Self>) {
        let sess = self
            .tabs
            .get(self.active)
            .map(|t| &t.active_entry().0.session);
        self.search.nav(dir, sess);
        cx.notify();
    }

    /// Take ownership of a tab: point its driver's waker at this window and
    /// make it the active tab. This is the whole "attach" operation — the
    /// session itself never moves threads.
    /// Split the focused pane of the active tab; the new pane runs the
    /// default profile in the inherited cwd and takes the focus.
    fn split_active_pane(&mut self, horizontal: bool, cx: &mut Context<Self>) {
        if self.tabs.get(self.active).is_none() {
            return;
        }
        let mut spec = default_spec(cx);
        self.inherit_cwd(&mut spec);
        let Some(entry) = create_tab_spec(cx, &spec) else {
            return;
        };
        let weak = cx.weak_entity();
        *entry.0.waker.lock() = Some(Box::new(move |acx| {
            let _ = weak.update(acx, |_, cx| cx.notify());
        }));
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.split_active(horizontal, entry);
        }
        cx.notify();
    }

    /// Pull the focused pane out of a split tab. `None` when the tab is
    /// not split (nothing to pull) — shared by the menu detach paths and
    /// pane drags.
    fn take_active_pane(&mut self) -> Option<TabEntry> {
        let tab = self.tabs.get_mut(self.active)?;
        if !tab.is_split() {
            return None;
        }
        let entry = tab.close_active_pane()?;
        Some(entry)
    }

    /// Right-click: the focused pane leaves the split and becomes its own
    /// tab right after the current one (which keeps the rest).
    fn pane_to_tab(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.take_active_pane() else {
            return;
        };
        let weak = cx.weak_entity();
        *entry.0.waker.lock() = Some(Box::new(move |acx| {
            let _ = weak.update(acx, |_, cx| cx.notify());
        }));
        let ix = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(ix, Tab::single(entry));
        self.active = ix;
        self.after_tab_change(cx);
        cx.notify();
    }

    /// Right-click: the focused pane leaves the split into a fresh window
    /// of this process (same shape as the in-process tab detach).
    fn pane_to_window(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.take_active_pane() else {
            return;
        };
        self.after_tab_change(cx);
        cx.notify();
        open_tabs_window(cx, vec![entry]);
    }

    /// A pane dropped on the tab strip: it leaves the split and becomes a
    /// tab at `ix`.
    fn pane_drop_to_tab_slot(&mut self, pane: usize, ix: usize, cx: &mut Context<Self>) {
        self.dragging_pane = None;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if !tab.is_split() {
            return;
        }
        tab.active_pane = pane;
        let Some(entry) = tab.close_active_pane() else {
            return;
        };
        let weak = cx.weak_entity();
        *entry.0.waker.lock() = Some(Box::new(move |acx| {
            let _ = weak.update(acx, |_, cx| cx.notify());
        }));
        let ix = ix.min(self.tabs.len());
        self.tabs.insert(ix, Tab::single(entry));
        self.active = ix;
        self.after_tab_change(cx);
        cx.notify();
    }

    /// A pane dropped on another pane's edge: pull it out of its slot and
    /// split-join it there.
    fn pane_rearrange(
        &mut self,
        src: usize,
        target: usize,
        horizontal: bool,
        new_first: bool,
        cx: &mut Context<Self>,
    ) {
        self.dragging_pane = None;
        if src == target {
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if !tab.is_split() {
            return;
        }
        let Some(entry) = tab.root.remove(src) else {
            return;
        };
        tab.split_at(target, horizontal, new_first, entry);
        cx.notify();
    }

    /// Close the focused pane of a split tab; its sibling takes the space.
    fn close_active_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active)
            && let Some(entry) = tab.close_active_pane()
        {
            entry.0.shutdown();
        }
        cx.notify();
    }

    /// Move pane focus geometrically (Alt+arrows): pick the leaf whose
    /// center is nearest in the pressed direction, in normalized tree
    /// space.
    fn focus_pane_direction(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let mut rects = Vec::new();
        tab.root.rects(0.0, 0.0, 1.0, 1.0, &mut rects);
        let Some(&(_, cx0, cy0, cw0, chh0)) = rects.iter().find(|(id, ..)| *id == tab.active_pane)
        else {
            return;
        };
        let (mx, my) = (cx0 + cw0 / 2.0, cy0 + chh0 / 2.0);
        let best = rects
            .iter()
            .filter(|(id, ..)| *id != tab.active_pane)
            .filter_map(|&(id, x, y, w, h)| {
                let (px_, py_) = (x + w / 2.0, y + h / 2.0);
                let ok = match key {
                    "left" => px_ < mx - 1e-3,
                    "right" => px_ > mx + 1e-3,
                    "up" => py_ < my - 1e-3,
                    "down" => py_ > my + 1e-3,
                    _ => false,
                };
                ok.then(|| {
                    let d = (px_ - mx).powi(2) + (py_ - my).powi(2);
                    (id, d)
                })
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id);
        if let Some(id) = best {
            tab.active_pane = id;
            cx.notify();
        }
    }

    /// Steer a live divider drag: the mouse position maps to the grabbed
    /// Split's ratio (clamped so neither side can collapse away). The pane
    /// area mirrors render's layout — the strip above, `px_1` at the sides.
    fn drag_divider_to(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some((path, horizontal)) = self.divider_drag.clone() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Some((nx, ny, nw, nh)) = tab.root.split_rect(&path) else {
            return;
        };
        let vp = window.viewport_size();
        let ratio = if horizontal {
            let area_w = (vp.width / px(1.) - 8.0).max(1.0);
            let sx = 4.0 + nx * area_w;
            (pos.x / px(1.) - sx) / (nw * area_w).max(1.0)
        } else {
            let area_h = (vp.height / px(1.) - TAB_STRIP_H).max(1.0);
            let sy = TAB_STRIP_H + ny * area_h;
            (pos.y / px(1.) - sy) / (nh * area_h).max(1.0)
        }
        .clamp(0.1, 0.9);
        tab.root.set_ratio(&path, ratio);
        cx.notify();
    }

    /// Keep the drag preview under the pointer even past our own frame.
    ///
    /// The pointer is captured for the whole gesture, so positions keep
    /// arriving after it leaves the window — but nothing we draw can appear
    /// out there. While it is outside, stand a borderless unfocused top-most
    /// window under it and move that instead; back inside, tear it down and
    /// let gpui's in-window ghost resume. Purely visual: the drop targets and
    /// the outside-release tear-off are untouched.
    fn sync_drag_follower(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vp = window.viewport_size();
        let outside = pos.x < px(0.) || pos.y < px(0.) || pos.x > vp.width || pos.y > vp.height;
        let Some(title) = self.drag_title.clone() else {
            self.close_drag_follower(cx);
            return;
        };
        if !cx.has_active_drag() || !outside {
            self.close_drag_follower(cx);
            return;
        }

        // Screen space, offset down-right so the preview trails the cursor
        // instead of sitting under it (and swallowing its own hit-testing).
        let root = window.bounds().origin;
        let origin = point(root.x + pos.x + px(14.), root.y + pos.y + px(10.));

        if let Some(handle) = self.drag_follower {
            let _ = handle.update(cx, |_, win, _| win.set_position(origin));
            return;
        }
        let bounds = Bounds {
            origin,
            // Roomier than the chip so the chip floats inside a transparent
            // margin: whatever the popup does at its own edges stays off it.
            size: size(px(220.), px(TAB_H + 16.)),
        };
        self.drag_follower = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: None,
                    // Never take activation: focus leaving the dragging
                    // window would end the drag.
                    focus: false,
                    show: true,
                    kind: gpui::WindowKind::PopUp,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    window_background: gpui::WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| TabDragGhost {
                        title,
                        windowed: true,
                    })
                },
            )
            .ok();
        // The popup exists now; take DWM's shadow off it before it is seen.
        strip_tool_window_shadows();
    }

    /// Retire the follower window, if one is up.
    fn close_drag_follower(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.drag_follower.take() {
            let _ = handle.update(cx, |_, win, _| win.remove_window());
        }
    }

    /// A zero-size canvas that exists only to register window-global mouse
    /// listeners: outside the frame no element is hit, so element handlers
    /// stop firing — and `on_mouse_event` is legal only during paint, which
    /// a canvas paint closure is.
    fn drag_follower_hook(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity();
        gpui::canvas(
            |_, _, _| (),
            move |_bounds, _, window, _cx| {
                let moved = view.clone();
                window.on_mouse_event(move |ev: &gpui::MouseMoveEvent, phase, window, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    let pos = ev.position;
                    moved.update(cx, |this, cx| this.sync_drag_follower(pos, window, cx));
                });
                let released = view.clone();
                window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, _window, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    // The release resolves the gesture wherever it lands, so
                    // the preview goes with it — a move event may never come.
                    released.update(cx, |this, cx| {
                        this.close_drag_follower(cx);
                        this.drag_title = None;
                    });
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element()
    }

    /// A strip tab dropped onto a pane edge: the tab leaves the strip and
    /// joins the active tab as a new pane in that direction (wt/VSCode's
    /// editor-drop). Same-window, unsplit source tabs only (v1).
    fn drop_tab_into_pane(
        &mut self,
        drag_ix: usize,
        target_pane: usize,
        horizontal: bool,
        new_first: bool,
        cx: &mut Context<Self>,
    ) {
        self.dragging_tab = None;
        if drag_ix >= self.tabs.len() || drag_ix == self.active {
            return;
        }
        if self.tabs[drag_ix].is_split() {
            log::warn!("pane drop: split tabs can't join as a pane yet");
            return;
        }
        let Some(entry) = self.tabs.remove(drag_ix).take_single() else {
            return;
        };
        if drag_ix < self.active {
            self.active -= 1;
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.split_at(target_pane, horizontal, new_first, entry);
        }
        cx.notify();
    }

    /// The five drop zones over a pane while a tab drag is live: edge
    /// quarters split-join in that direction (steel-blue wash advertises
    /// the side), the center keeps the classic detach. Mounted ONLY during
    /// a drag, so normal hit-testing never sees these.
    fn pane_drop_zones(
        &self,
        pane_id: usize,
        can_join_tab: bool,
        can_join_pane: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let zone = |top: f32, left: f32, w: f32, h: f32| {
            div()
                .absolute()
                .top(gpui::relative(top))
                .left(gpui::relative(left))
                .w(gpui::relative(w))
                .h(gpui::relative(h))
        };
        let wash = |d: gpui::Div| {
            d.drag_over::<TabDrag>(|s, _, _, _| s.bg(gpui::rgba(0x3465A455)))
                .drag_over::<PaneDrag>(|s, _, _, _| s.bg(gpui::rgba(0x3465A455)))
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            // Left / right / top / bottom quarters → split-join (a dragged
            // tab joins as a pane; a dragged pane rearranges). Only shown
            // when the drag can actually land — a wash that would do
            // nothing misleads.
            .when(can_join_tab || can_join_pane, |d| {
                d.child(
                    wash(zone(0.0, 0.0, 0.25, 1.0))
                        .on_drop(cx.listener(move |this: &mut Self, drag: &TabDrag, _w, cx| {
                            this.drop_tab_into_pane(drag.ix, pane_id, true, true, cx);
                        }))
                        .on_drop(
                            cx.listener(move |this: &mut Self, drag: &PaneDrag, _w, cx| {
                                this.pane_rearrange(drag.pane, pane_id, true, true, cx);
                            }),
                        ),
                )
                .child(
                    wash(zone(0.0, 0.75, 0.25, 1.0))
                        .on_drop(cx.listener(move |this: &mut Self, drag: &TabDrag, _w, cx| {
                            this.drop_tab_into_pane(drag.ix, pane_id, true, false, cx);
                        }))
                        .on_drop(
                            cx.listener(move |this: &mut Self, drag: &PaneDrag, _w, cx| {
                                this.pane_rearrange(drag.pane, pane_id, true, false, cx);
                            }),
                        ),
                )
                .child(
                    wash(zone(0.0, 0.25, 0.5, 0.25))
                        .on_drop(cx.listener(move |this: &mut Self, drag: &TabDrag, _w, cx| {
                            this.drop_tab_into_pane(drag.ix, pane_id, false, true, cx);
                        }))
                        .on_drop(
                            cx.listener(move |this: &mut Self, drag: &PaneDrag, _w, cx| {
                                this.pane_rearrange(drag.pane, pane_id, false, true, cx);
                            }),
                        ),
                )
                .child(
                    wash(zone(0.75, 0.25, 0.5, 0.25))
                        .on_drop(cx.listener(move |this: &mut Self, drag: &TabDrag, _w, cx| {
                            this.drop_tab_into_pane(drag.ix, pane_id, false, false, cx);
                        }))
                        .on_drop(
                            cx.listener(move |this: &mut Self, drag: &PaneDrag, _w, cx| {
                                this.pane_rearrange(drag.pane, pane_id, false, false, cx);
                            }),
                        ),
                )
            })
            // Center → the classic detach-into-a-window, dashed like the
            // old whole-pane affordance.
            .child(
                zone(0.25, 0.25, 0.5, 0.5)
                    .drag_over::<TabDrag>(|s, _, _, _| {
                        s.border_2()
                            .border_dashed()
                            .border_color(gpui::rgba(0x8A9CC880))
                    })
                    .on_drop(cx.listener(|this: &mut Self, drag: &TabDrag, window, cx| {
                        this.detach_at(drag.ix, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// Render the pane tree: splits become ratio-sized flex children with a
    /// 1px divider; leaves paint their own grid + overlay.
    fn render_pane_node(
        &self,
        node: &PaneNode,
        path: Vec<bool>,
        active_pane: usize,
        tab_is_split: bool,
        cw: f32,
        ch: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            PaneNode::Empty => div().into_any_element(),
            PaneNode::Leaf(leaf) => {
                self.render_leaf(leaf, active_pane, tab_is_split, tab_is_split, cw, ch, cx)
            }
            PaneNode::Split {
                horizontal,
                ratio,
                a,
                b,
            } => {
                let horizontal = *horizontal;
                let ratio = *ratio;
                let (mut path_a, mut path_b) = (path.clone(), path.clone());
                path_a.push(false);
                path_b.push(true);
                // Both children are absolutely placed by PERCENT INSETS,
                // never flex or percent sizes: a percent height inside a
                // nested split is indefinite to taffy and collapses to
                // content size (the "bottom pane one line tall" bug), and
                // flex-grow weights lose to the grid's intrinsic size the
                // same way. Insets resolve against the laid-out containing
                // block — definite in every nesting.
                let first = div()
                    .absolute()
                    .map(|d| {
                        if horizontal {
                            d.left_0()
                                .top_0()
                                .bottom_0()
                                .right(gpui::relative(1.0 - ratio))
                        } else {
                            d.top_0()
                                .left_0()
                                .right_0()
                                .bottom(gpui::relative(1.0 - ratio))
                        }
                    })
                    .child(self.render_pane_node(a, path_a, active_pane, tab_is_split, cw, ch, cx));
                let second = div()
                    .absolute()
                    .map(|d| {
                        if horizontal {
                            d.right_0().top_0().bottom_0().left(gpui::relative(ratio))
                        } else {
                            d.bottom_0().left_0().right_0().top(gpui::relative(ratio))
                        }
                    })
                    .child(self.render_pane_node(b, path_b, active_pane, tab_is_split, cw, ch, cx));
                let divider = div()
                    .absolute()
                    .map(|d| {
                        if horizontal {
                            d.left(gpui::relative(ratio)).top_0().bottom_0().w(px(1.))
                        } else {
                            d.top(gpui::relative(ratio)).left_0().right_0().h(px(1.))
                        }
                    })
                    .bg(gpui::rgba(DIVIDER));
                // The grab strip: an invisible 7px band centered on the 1px
                // divider, painted last so it wins hit-testing over both
                // children. Dragging it steers this Split's ratio (mouse
                // moves land on the window root — see drag_divider_to).
                let grab = div()
                    .absolute()
                    .map(|d| {
                        if horizontal {
                            d.left(gpui::relative(ratio))
                                .ml(px(-3.))
                                .top_0()
                                .bottom_0()
                                .w(px(7.))
                                .cursor(gpui::CursorStyle::ResizeLeftRight)
                        } else {
                            d.top(gpui::relative(ratio))
                                .mt(px(-3.))
                                .left_0()
                                .right_0()
                                .h(px(7.))
                                .cursor(gpui::CursorStyle::ResizeUpDown)
                        }
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, _w, cx| {
                            this.divider_drag = Some((path.clone(), horizontal));
                            cx.stop_propagation();
                        }),
                    );
                div()
                    .size_full()
                    .relative()
                    .child(first)
                    .child(second)
                    .child(divider)
                    .child(grab)
                    .into_any_element()
            }
        }
    }

    /// One pane: grid + shared overlay, with its own painted-size PTY fit.
    /// The focused pane owns the caret, IME preedit and search highlight;
    /// unfocused panes of a split get a subtle dim wash. `show_handle`
    /// gates the top-center drag grip separately from the fit (a zoomed
    /// pane keeps the split fit but hides the grip — drop-zone geometry
    /// would lie while the tree is visually collapsed).
    fn render_leaf(
        &self,
        leaf: &Leaf,
        active_pane: usize,
        tab_is_split: bool,
        show_handle: bool,
        cw: f32,
        ch: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use std::sync::atomic::Ordering;
        let session = &leaf.entry.0.session;
        // Per-pane PTY fit from the size the overlay canvas painted last
        // frame — the only truth once a tab is split. Cell-quantized and
        // only on change, so this never thrashes.
        let (mw, mh) = leaf.measured.get();
        if tab_is_split && mw > cw && mh > ch {
            let cols = ((mw / cw) as u16).max(2);
            let rows = ((mh / ch) as u16).max(2);
            if (
                session.cols.load(Ordering::Relaxed),
                session.rows.load(Ordering::Relaxed),
            ) != (cols, rows)
            {
                session.resize(cols, rows, (cw, ch));
            }
        }
        let snap = session.snapshot.lock().clone();
        let focused = leaf.id == active_pane;
        let pane_id = leaf.id;
        let ime_preedit = if focused {
            self.ime.read(cx).marked.clone()
        } else {
            None
        };
        let focus_handle = self.terminal_focus.clone();
        let ime = self.ime.clone();
        let view = cx.entity();
        let (grid_rows, grid_cols) = (snap.rows, snap.cols);
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            // Click moves the pane focus (split tabs). Right-click too —
            // the context menu's pane actions must hit the pane that was
            // clicked, not whichever held the focus before.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _w, cx| {
                    if let Some(tab) = this.tabs.get_mut(this.active)
                        && tab.active_pane != pane_id
                    {
                        tab.active_pane = pane_id;
                        // The composition in flight belongs to the pane we
                        // just left — recycle the TSF document exactly like
                        // a tab switch would, and move the ?1004 focus.
                        this.ime_reset_pending = true;
                        this.sync_focus_reports();
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _w, cx| {
                    if let Some(tab) = this.tabs.get_mut(this.active)
                        && tab.active_pane != pane_id
                    {
                        tab.active_pane = pane_id;
                        // The composition in flight belongs to the pane we
                        // just left — recycle the TSF document exactly like
                        // a tab switch would, and move the ?1004 focus.
                        this.ime_reset_pending = true;
                        this.sync_focus_reports();
                        cx.notify();
                    }
                }),
            )
            .child(render_grid(
                &snap,
                mono_font(),
                cw,
                ch,
                snap.selection,
                self.selection.hover_link_for(pane_id),
                Some(&session.images),
                ime_preedit,
                if focused {
                    session.search_render_state()
                } else {
                    None
                },
            ))
            // Shared pane overlay (IME handler + selection listeners +
            // caret), pane-addressed so hit-testing lands on THIS leaf.
            .child(rikka_terminal_core::pane::pane_overlay(
                rikka_terminal_core::pane::PaneOverlay {
                    focus_handle,
                    ime,
                    view,
                    pane: pane_id,
                    cw,
                    ch,
                    grid_rows,
                    grid_cols,
                    inset: 0.0,
                    caret_enabled: focused,
                    measured: Some(leaf.measured.clone()),
                    origin: Some(leaf.origin.clone()),
                },
                // Pipe the caret rect to TSF so the IME candidate window
                // opens at the terminal cursor (focused pane only).
                move |caret| {
                    if focused {
                        tsf::set_caret(caret.map(|(left, top, right, bottom)| {
                            rikka_terminal_gpui_ime::CaretRect {
                                left,
                                top,
                                right,
                                bottom,
                            }
                        }));
                    }
                },
            ))
            // Split-tab affordances: unfocused panes get a subtle wash.
            .when(tab_is_split && !focused, |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .bg(gpui::rgba(0x0000002E)),
                )
            })
            // Broadcast input: every receiving pane wears a rust-red frame
            // — typing fanning out must never be a surprise. Individually
            // marked panes (selection broadcast) show it whichever tab or
            // pane holds the focus.
            .when(
                self.broadcast_all
                    || self.tabs.get(self.active).is_some_and(|t| t.broadcast)
                    || leaf.entry.0.broadcast_target(),
                |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .border_2()
                            .border_color(gpui::rgba(0xBE5A5070)),
                    )
                },
            )
            // The grabbed pane's original fades, like a dragged tab's.
            .when(self.dragging_pane == Some(pane_id), |d| d.opacity(0.5))
            // Split tabs: a hover handle at the top center grabs this pane
            // (drag to the strip = tab, to another pane's edge = rearrange,
            // out of the window = new window).
            .when(show_handle, |d| {
                let title = session
                    .title
                    .lock()
                    .clone()
                    .unwrap_or_else(|| "ペイン".to_string());
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w_full()
                        .h(px(12.))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id(("pane-handle", pane_id))
                                .w(px(48.))
                                .h(px(12.))
                                .rounded_b(px(5.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(9.))
                                .text_color(gpui::rgba(0xFFFFFF00))
                                .hover(|s| {
                                    s.bg(gpui::rgba(0x3465A4CC))
                                        .text_color(gpui::rgba(0xFFFFFFD0))
                                })
                                .cursor_grab()
                                .child("⋯")
                                .on_drag(
                                    PaneDrag {
                                        pane: pane_id,
                                        title,
                                    },
                                    |drag, _offset, _window, cx| {
                                        let title = drag.title.clone();
                                        cx.new(|_| TabDragGhost {
                                            title,
                                            windowed: false,
                                        })
                                    },
                                ),
                        ),
                )
            })
            // Drop zones appear only while a tab or pane drag is live, so
            // they never sit in normal hit-testing.
            .when(
                self.dragging_tab.is_some() || self.dragging_pane.is_some(),
                |d| {
                    let can_join_tab = self.dragging_tab.is_some_and(|ix| {
                        ix != self.active && self.tabs.get(ix).is_some_and(|t| !t.is_split())
                    });
                    let can_join_pane = self.dragging_pane.is_some_and(|p| p != pane_id);
                    d.child(self.pane_drop_zones(pane_id, can_join_tab, can_join_pane, cx))
                },
            )
            .into_any_element()
    }

    fn adopt_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let ix = self.tabs.len();
        let weak = cx.weak_entity();
        tab.for_each_entry(|entry| {
            let weak = weak.clone();
            *entry.0.waker.lock() = Some(Box::new(move |acx| {
                let _ = weak.update(acx, |_, cx| cx.notify());
            }));
        });
        self.tabs.insert(ix, tab);
        self.switch_to(ix, cx);
        cx.notify();
    }

    fn adopt(&mut self, entry: TabEntry, cx: &mut Context<Self>) {
        self.adopt_at(entry, None, cx);
    }

    /// [`Self::adopt`] at a strip position; `None`/out-of-range appends.
    fn adopt_at(&mut self, entry: TabEntry, ix: Option<usize>, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        *entry.0.waker.lock() = Some(Box::new(move |acx| {
            let _ = weak.update(acx, |_, cx| cx.notify());
        }));
        let ix = ix.unwrap_or(usize::MAX).min(self.tabs.len());
        self.tabs.insert(ix, Tab::single(entry));
        self.active = ix;
        self.after_tab_change(cx);
    }

    /// [`Self::adopt`], but a drag-merge lands at the strip position under
    /// the drop point instead of always appending. The geometry runs on
    /// Win32 — the adopt path arrives from the IPC pump with no
    /// `&mut Window` — recreating `render`'s strip math from the client
    /// rect and DPI of the window under the point. Anything that doesn't
    /// line up (point over a different window of this process, stale
    /// coordinates) falls back to append.
    #[cfg(windows)]
    fn adopt_dropped(
        &mut self,
        entry: TabEntry,
        drop_at: Option<(i32, i32)>,
        cx: &mut Context<Self>,
    ) {
        let ix = drop_at.and_then(|pt| self.drop_index_at(pt));
        self.adopt_at(entry, ix, cx);
    }

    /// Map a screen-pixel drop point to the tab-strip insertion index —
    /// `render`'s layout math (equal-width tabs, WinUI paddings) replayed
    /// against Win32 window metrics. `None` = not on this window, append.
    #[cfg(windows)]
    fn drop_index_at(&self, pt: (i32, i32)) -> Option<usize> {
        use windows::Win32::Foundation::{POINT, RECT};
        use windows::Win32::Graphics::Gdi::ScreenToClient;
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        use windows::Win32::UI::WindowsAndMessaging::{
            GA_ROOT, GetAncestor, GetClientRect, GetWindowThreadProcessId, WindowFromPoint,
        };
        let mut p = POINT { x: pt.0, y: pt.1 };
        let hwnd = unsafe { WindowFromPoint(p) };
        if hwnd.0.is_null() {
            return None;
        }
        let hwnd = unsafe { GetAncestor(hwnd, GA_ROOT) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // A window process hosts one window, so ours-by-pid is ours. (An
        // in-process second window would need per-window addressing first —
        // see TODO "窓単位 addressing".)
        if pid != std::process::id() {
            return None;
        }
        let mut rc = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rc) }.ok()?;
        if !unsafe { ScreenToClient(hwnd, &mut p) }.as_bool() {
            return None;
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let width = (rc.right - rc.left) as f32 / scale;
        let x = p.x as f32 / scale;
        // Mirror render(): avail / plus_w / needs_scroll / tab_w, then the
        // strip origin (pl_2 = 8, plus the left arrow when scrolling).
        let plus_w = 32.0 + 18.0;
        let avail = width - 8.0 - (3.0 * 46.0) - (2.0 * 24.0);
        let needs_scroll = (self.tabs.len() as f32 * 100.0 + plus_w) > avail;
        let tab_w = ((avail - plus_w) / self.tabs.len().max(1) as f32).clamp(100.0, 240.0);
        let origin = 8.0 + if needs_scroll { 22.0 } else { 0.0 };
        let content_x = x - origin - (self.strip_scroll.offset().x / px(1.));
        Some(strip_insert_index(content_x, tab_w, self.tabs.len()))
    }

    fn switch_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            self.after_tab_change(cx);
        }
    }

    fn cycle(&mut self, forward: bool, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        if n > 1 {
            self.active = (self.active + if forward { 1 } else { n - 1 }) % n;
            self.after_tab_change(cx);
        }
    }

    fn after_tab_change(&mut self, cx: &mut Context<Self>) {
        // Force the viewport size onto the newly shown session (it may have
        // been sized by another window) and re-sync the OS title.
        self.cols = 0;
        self.rows = 0;
        self.applied_title = None;
        // An IME composition in flight belongs to the tab we just left; the
        // next render (which has the Window needed to check focus and
        // activation) cancels it so its commit cannot land in this tab's PTY.
        self.ime_reset_pending = true;
        // The ?1004 focus moved with the active pane.
        self.sync_focus_reports();
        // The search bar is per-session state — a tab switch closes it and
        // sweeps the highlight off every tab.
        if self.search.open {
            self.search.open = false;
            for t in &self.tabs {
                t.for_each_entry(|e| e.0.session.search_close());
            }
        }
        // Install the now-active tab's palette so the visible tab wears its
        // own colors (per-profile theming). Only the active tab renders, so
        // one global palette is enough.
        apply_active_theme(
            self.tabs
                .get(self.active)
                .and_then(|t| t.primary().0.theme()),
        );
        // The global palette just swapped: this tab's snapshots may have
        // baked their ANSI colors under another tab's theme (snapshots
        // refresh on PTY output, whenever that last happened — a background
        // tab that printed then wears the wrong palette). Rebuild them now
        // that the right palette is installed.
        if let Some(tab) = self.tabs.get(self.active) {
            tab.for_each_entry(|e| e.0.session.rebuild_snapshot());
        }
        cx.notify();
    }

    /// Run the TSF focus cycle again for this window: the backend pops the
    /// current document (the TIP abandons any in-flight composition with
    /// it, and queued events are discarded) and pushes a fresh focused one.
    fn tsf_refocus(&self, cx: &mut Context<Self>) {
        let view = cx.weak_entity();
        let async_cx = cx.to_async();
        tsf::on_input_focus(Box::new(move || {
            let view = view.clone();
            async_cx
                .spawn(async move |cx| {
                    let _ = view.update(cx, |_, cx| cx.notify());
                })
                .detach();
        }));
    }

    /// Close the active tab; closing the last one closes the window.
    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_at(self.active, window, cx);
    }

    /// Close the tab at `ix` (tab-strip ✕); closing the last one closes the
    /// window.
    fn close_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            if self.tabs.is_empty() {
                window.remove_window();
            }
            return;
        }
        let tab = self.tabs.remove(ix);
        tab.shutdown_all();
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        if ix < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.after_tab_change(cx);
    }

    /// Detach the active tab into a fresh window (no-op with a single tab —
    /// that would just be a window move).
    fn detach_active(&mut self, cx: &mut Context<Self>) {
        self.split_off_in_process(self.active, cx);
    }

    /// The in-process half of a detach: move the tab's Arc into a fresh
    /// window of THIS process (crash fate shared, scrollback kept intact).
    fn split_off_in_process(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 || ix >= self.tabs.len() {
            return;
        }
        // Splits don't travel (Phase B): moving one pane of a split would
        // orphan the rest, so a split tab simply stays put.
        if self.tabs[ix].is_split() {
            return;
        }
        let Some(entry) = self.tabs.remove(ix).take_single() else {
            return;
        };
        if ix < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.after_tab_change(cx);
        open_tabs_window(cx, vec![entry]);
    }

    /// Detach the tab at `ix` into a fresh window, preferring full crash
    /// isolation (its own OS process) when the session can leave this one;
    /// a non-transferable session (legacy PTY) splits in-process instead,
    /// which never risks it. Single tab = a window move, all cost no gain —
    /// no-op. Drag-to-pane and Ctrl+Shift+E both land here.
    fn detach_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.dragging_tab = None;
        if self.tabs.len() < 2 || ix >= self.tabs.len() {
            return;
        }
        if self.tabs[ix].is_split() {
            return;
        }
        #[cfg(windows)]
        {
            let entry = self.tabs[ix].primary();
            if tab_move::is_transferable(&entry.0.session) {
                let palette = entry.0.theme().map(|p| p.to_wire());
                match tab_move::send_tab(
                    &entry.0.session,
                    palette,
                    tab_move::Destination::NewProcess,
                ) {
                    Ok(()) => self.close_at(ix, window, cx),
                    // Quiesce is irreversible — the tab stays, honestly
                    // disconnected; splitting it off would just relocate
                    // the corpse.
                    Err(e) => {
                        log::warn!("cross-process detach failed: {e:#}");
                        cx.notify();
                    }
                }
                return;
            }
        }
        let _ = window;
        self.split_off_in_process(ix, cx);
    }

    /// Reorder: the dragged tab leaves `from` and lands at the strip
    /// position of the drop target. The moved tab stays the user's focus.
    fn reorder_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.dragging_tab = None;
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let entry = self.tabs.remove(from);
        self.tabs.insert(to, entry);
        self.active = to;
        self.after_tab_change(cx);
    }

    /// Cross-process detach of the active tab (Ctrl+Shift+E) — see
    /// [`Self::detach_at`].
    #[cfg(windows)]
    fn eject_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detach_at(self.active, window, cx);
    }

    /// A tab released over ANOTHER rikka window (the drag-merge gesture):
    /// move it there via that window's own socket. Works from a single-tab
    /// window too — that is a window merge, and moving the last tab closes
    /// this one. Returns false when `pid` is not a reachable rikka window
    /// (the caller falls back to the tear-off); a non-transferable session
    /// (legacy PTY — an in-process Arc cannot cross processes) cancels.
    #[cfg(windows)]
    fn drop_tab_on_window_process(
        &mut self,
        ix: usize,
        pid: u32,
        drop_at: (i32, i32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tabs.get(ix) else {
            return true;
        };
        if tab.is_split() {
            log::warn!("drag-merge: split tabs don't travel yet — cancelled");
            return true;
        }
        let entry = tab.primary();
        if !tab_move::is_transferable(&entry.0.session) {
            log::warn!("drag-merge: session is not transferable (legacy PTY) — cancelled");
            return true;
        }
        let Ok(endpoint) = tab_move::resolve_window(u64::from(pid)) else {
            return false; // not a (reachable) rikka window
        };
        match tab_move::send_tab(
            &entry.0.session,
            entry.0.theme().map(|p| p.to_wire()),
            tab_move::Destination::Window {
                id: u64::from(pid),
                endpoint,
                drop_at: Some(drop_at),
            },
        ) {
            Ok(()) => self.close_at(ix, window, cx),
            // connect-first ordering means the tab usually survives this.
            Err(e) => {
                log::warn!("drag-merge failed: {e:#}");
                cx.notify();
            }
        }
        true
    }

    /// Move the active tab into another window PROCESS (resolve through the
    /// monarch, attach on that window's own socket). Moving the last tab
    /// closes this window — that is a window merge, and the point.
    #[cfg(windows)]
    fn move_active_to_other_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.is_split() {
            log::warn!("tab move: split tabs don't travel yet — cancelled");
            return;
        }
        let entry = tab.primary();
        match tab_move::move_to_any_other_window(
            &entry.0.session,
            entry.0.theme().map(|p| p.to_wire()),
        ) {
            Ok(()) => self.close_at(self.active, window, cx),
            Err(e) => {
                log::warn!("tab move failed: {e:#}");
                cx.notify();
            }
        }
    }

    /// Pull every other window's tabs into this one, then close them. Each
    /// step is a synchronous Vec move on this thread — mash it as hard as
    /// you like.
    fn merge_all(&mut self, cx: &mut Context<Self>) {
        let my_id = cx.entity_id();
        let others = hub::other_windows(cx, my_id);
        for (handle, entity) in others {
            let Some(moved) = entity.update(cx, |other, _| hub::take_for_merge(&mut other.tabs))
            else {
                // Never close a source from which no tab was retained. This
                // is the load-bearing guard against a stale/closing window
                // snapshot turning merge-all into close-all.
                continue;
            };
            for tab in moved {
                self.adopt_tab(tab, cx);
            }
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn new_tab(&mut self, cx: &mut Context<Self>) {
        let mut spec = default_spec(cx);
        self.inherit_cwd(&mut spec);
        if let Some(entry) = create_tab_spec(cx, &spec) {
            self.adopt(entry, cx);
        }
        self.profile_menu = false;
    }

    /// Open a new tab from the profile at `idx` in the shared menu.
    fn new_tab_profile(&mut self, idx: usize, cx: &mut Context<Self>) {
        let spec = cx
            .global::<hub::ProfileMenu>()
            .0
            .profiles
            .get(idx)
            .map(profile_to_spec);
        if let Some(mut spec) = spec {
            self.inherit_cwd(&mut spec);
            if let Some(entry) = create_tab_spec(cx, &spec) {
                self.adopt(entry, cx);
            }
        }
        self.profile_menu = false;
    }

    /// Shell integration: a new tab without an explicit directory opens in
    /// the ACTIVE tab's shell-reported cwd (OSC 9;9 / OSC 7), wt-style —
    /// falling through to the home default when the shell never reported
    /// one or the path no longer exists.
    fn inherit_cwd(&self, spec: &mut cli::TabSpec) {
        if spec.dir.is_some() {
            return;
        }
        spec.dir = self
            .active_session()
            .and_then(|s| s.current_cwd())
            .filter(|d| std::path::Path::new(d).is_dir());
    }
}

/// Native caption button (min/max/close), Files-style. The
/// `window_control_area` hitbox makes WM_NCHITTEST return
/// HTMINBUTTON/HTMAXBUTTON/HTCLOSE and gpui's NC handlers run the native
/// action (ShowWindowAsync / WM_CLOSE) — no click listener wanted, or the
/// strip would have to reimplement press/release semantics.
fn caption_button(glyph: &'static str, area: WindowControlArea) -> impl IntoElement {
    let close = matches!(area, WindowControlArea::Close);
    div()
        .w(px(46.))
        .h_full()
        // Never shrink: the caption group stays pinned right and full-size no
        // matter how many tabs crowd the strip (they scroll instead).
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        // Segoe MDL2 Assets ships on Windows 10+; its E92x/E8BB glyphs are
        // the system caption icons (what Files/wt render).
        .font_family("Segoe MDL2 Assets")
        .text_size(px(10.))
        .text_color(gpui::rgba(TEXT_SECONDARY))
        .hover(move |t| {
            if close {
                // The one non-monochrome hover in the chrome: the standard
                // Windows close red.
                t.bg(gpui::rgba(0xC42B1CFF)).text_color(rgb(TEXT_PRIMARY))
            } else {
                t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY))
            }
        })
        .window_control_area(area)
        .child(glyph)
}

impl Render for TabsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A tab change may have left a composition in flight: recycle the
        // TSF document (as a focus cycle would) so the TIP abandons it and
        // queued events are discarded — both belong to the tab we left. The
        // recycle only makes sense while the store is actually bound to us
        // (OS-active window, terminal focused); the stale inline preedit is
        // cleared either way.
        if self.ime_reset_pending {
            self.ime_reset_pending = false;
            if window.is_window_active() && self.terminal_focus.is_focused(window) {
                self.tsf_refocus(cx);
            }
            self.ime.update(cx, |ime, cx| {
                if ime.marked.take().is_some() {
                    cx.notify();
                }
            });
        }

        // TSF: apply queued IME events while our terminal input owns focus —
        // preedit renders inline via ime.marked, commits go to the active
        // tab's PTY. Also gated on OS-window activation: the store is
        // thread-global, and a background window of this process (whose own
        // focus tree still reports its terminal focused) would otherwise
        // drain — and steal — the active window's events.
        if window.is_window_active() && self.terminal_focus.is_focused(window) {
            for ev in tsf::drain() {
                match ev {
                    rikka_terminal_gpui_ime::ImeEvent::Preedit(s) => {
                        let marked = (!s.is_empty()).then_some(s);
                        self.ime.update(cx, |ime, cx| {
                            ime.marked = marked;
                            cx.notify();
                        });
                    }
                    rikka_terminal_gpui_ime::ImeEvent::Commit(s) => {
                        self.send_input(s.as_bytes());
                        self.ime.update(cx, |ime, cx| {
                            ime.marked = None;
                            cx.notify();
                        });
                    }
                }
            }
        }

        // OSC 0/2 of the active tab → OS window title (deduped).
        if let Some(s) = self.active_session() {
            let title = s.title.lock().clone();
            if title != self.applied_title {
                window.set_window_title(title.as_deref().unwrap_or("RikkaTerminal"));
                self.applied_title = title;
            }
        }

        // Every tab's progress, worst-wins, onto this window's taskbar button
        // (visible while minimized/behind). Deduped inside `update`.
        let agg = self
            .tabs
            .iter()
            .map(|t| tab_progress(&t.primary().0.session))
            .fold(None, taskbar_aggregate);
        taskbar_progress::update(
            self.applied_title.as_deref().unwrap_or("RikkaTerminal"),
            agg,
        );

        let (cw, ch) = measure_cell_metrics(&cx.text_system(), mono_font(), window.scale_factor());

        // Fit the ACTIVE tab's PTY to the pane (viewport minus strip/padding).
        let vp = window.viewport_size();
        let content_w = (vp.width / px(1.)) - PAD;
        let content_h = (vp.height / px(1.)) - TAB_STRIP_H;
        if content_w > cw && content_h > ch {
            let new_cols = ((content_w / cw) as u16).max(2);
            let new_rows = ((content_h / ch) as u16).max(2);
            if (new_cols, new_rows) != (self.cols, self.rows) {
                self.cols = new_cols;
                self.rows = new_rows;
                // Every tab, not just the active one: the guard above is
                // window state, so a background tab that misses this moment
                // would never be re-fit — by the time it's activated the
                // dims already "match" and it stays at its stale PTY size
                // (wrong wrap column) until the next window resize.
                for tab in &self.tabs {
                    // Split tabs fit per pane from the painted size (see
                    // render_leaf) — the window-derived fit would fight it.
                    if tab.is_split() {
                        continue;
                    }
                    tab.for_each_entry(|entry| {
                        entry.0.session.resize(new_cols, new_rows, (cw, ch));
                    });
                }
            }
        }

        // ── tab strip = the titlebar (appears_transparent hides the native
        // one; Files integrates tabs the same way). Geometry and states are
        // WinUI TabView's: 32px tabs bottom-aligned under 8px of breathing
        // room, top-rounded 8px, the selected tab merging seamlessly into
        // the pane surface below (no accent, no bottom border), and 1px
        // separators between unselected neighbors. ─────────────────────────
        let maximized = window.is_maximized();
        let active_ix = self.active;
        // New-tab profile menu (wt profiles, config-filtered). >1 profile
        // shows the dropdown chevron next to [+]; the list is rendered below
        // the strip when open.
        let profiles: Vec<(usize, String, Option<tab_icon::TabIcon>)> = {
            let menu = &cx.global::<hub::ProfileMenu>().0;
            let icons = &cx.global::<hub::ProfileIcons>().0;
            menu.profiles
                .iter()
                .enumerate()
                .map(|(i, p)| (i, p.name.clone(), icons.get(i).cloned().flatten()))
                .collect()
        };
        let has_profiles = !profiles.is_empty();
        // Firefox-style tab overflow: tabs shrink to a 100px floor, then
        // scroll (caption buttons stay pinned) once even the floored tabs
        // plus [+]/⌄ can't fit left of the caption group and the arrows.
        // The ⌄ is always shown: its menu carries the "設定..." entry, so a
        // single-profile setup must not lose it.
        let plus_w = 32.0 + 18.0;
        let avail = (vp.width / px(1.)) - 8.0 - (3.0 * 46.0) - (2.0 * 24.0);
        let needs_scroll = (self.tabs.len() as f32 * 100.0 + plus_w) > avail;
        // WinUI TabView (Files/wt) sizing: EQUAL widths regardless of the
        // title — the standard 240px, shrinking uniformly to the 100px
        // floor as tabs crowd the strip, then they scroll. Computed here
        // instead of leaning on flex shrink: a (potential) scroll
        // container measures its children against infinite space, so
        // taffy would never shrink them.
        let tab_w = ((avail - plus_w) / self.tabs.len().max(1) as f32).clamp(100.0, 240.0);
        // Left edge of the profile ⌄ (it sits after the tabs and the 32px
        // [+]), for dropping the menu under it instead of the strip's left
        // corner. Clamped so the 200px menu stays on-screen; the scroll case
        // (⌄ scrolled to the right end) lands near the clamp, close enough.
        let chevron_x = 8.0 + self.tabs.len() as f32 * tab_w + 32.0;
        let menu_left = chevron_x.clamp(8.0, (vp.width / px(1.) - 208.0).max(8.0));
        let tab_viewport = div()
            .id("tab-viewport")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_end()
            .map(|v| {
                if needs_scroll {
                    v.overflow_x_scroll().track_scroll(&self.strip_scroll)
                } else {
                    v.overflow_hidden()
                }
            })
            // A drop on the strip's empty space (past the last tab) moves
            // the dragged tab to the end — drops ON a tab are consumed by
            // that tab's own handler and never bubble here.
            .drag_over::<TabDrag>(|style, _, _, _| style.bg(gpui::rgba(TAB_HOVER)))
            .drag_over::<PaneDrag>(|style, _, _, _| style.bg(gpui::rgba(TAB_HOVER)))
            .on_drop(cx.listener(|this, drag: &PaneDrag, _window, cx| {
                let end = this.tabs.len();
                this.pane_drop_to_tab_slot(drag.pane, end, cx);
            }))
            .on_drop(cx.listener(|this, drag: &TabDrag, _window, cx| {
                let last = this.tabs.len().saturating_sub(1);
                this.reorder_tab(drag.ix, last, cx);
            }))
            .children(self.tabs.iter().enumerate().flat_map(|(ix, tab)| {
                let entry = tab.primary();
                let title = entry
                    .0
                    .session
                    .title
                    .lock()
                    .clone()
                    .unwrap_or_else(|| format!("シェル {}", ix + 1));
                let title: String = title.chars().take(20).collect();
                let drag_title = title.clone();
                let active = ix == active_ix;
                // Each tab always wears ITS profile's background color, so
                // tabs stay distinguishable by color even when inactive.
                // Active = full (merges with the pane, which carries the same
                // palette); inactive = the same color recessed; unthemed tabs
                // fall back to the chrome behavior.
                let prof_rgb = entry
                    .0
                    .theme()
                    .map(|p| (p.background.r, p.background.g, p.background.b));
                // Recording indicator: session logging is on (Ctrl+Shift+L).
                let rec_dot = entry.0.session.logging_active().then(|| {
                    div()
                        .mr(px(4.))
                        .flex_shrink_0()
                        .text_color(rgb(0xBE5A50))
                        .child("●")
                });
                // Broadcast indicator, paired with the panes' rust frame.
                let bc_mark = (tab.broadcast || self.broadcast_all || tab.has_broadcast_member())
                    .then(|| {
                        div()
                            .mr(px(4.))
                            .flex_shrink_0()
                            .text_color(rgb(0xBE5A50))
                            .child("»")
                    });
                // Icon slot: while OSC 9;4 (or title-spinner) progress is
                // active, a wt-style circular indicator takes the shell icon's
                // place — but only on INACTIVE tabs. The active tab already
                // shows its progress as the bar atop the pane, so it keeps its
                // icon (a ring there would say the same thing twice).
                let icon_el = match tab_progress(&entry.0.session) {
                    Some(p) if !active => Some(progress_ring(("tab-ring", ix), p)),
                    _ => entry.0.icon().map(|ic| icon_element(ic, 6.)),
                };
                // Separator to the left of this tab — hidden next to the
                // selected tab, whose silhouette does the separating.
                let sep = (ix > 0 && !active && ix - 1 != active_ix).then(|| {
                    div()
                        .w(px(1.))
                        .h(px(16.))
                        .mb(px((TAB_H - 16.0) / 2.0))
                        .bg(gpui::rgba(DIVIDER))
                        .into_any_element()
                });
                let tab = div()
                    .id(("tab", ix))
                    .h(px(TAB_H))
                    // Equal WinUI TabView width — see `tab_w` above.
                    .w(px(tab_w))
                    .flex_shrink_0()
                    .pl(px(8.))
                    .pr(px(4.))
                    .py(px(3.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded_tl(px(8.))
                    .rounded_tr(px(8.))
                    .text_size(px(12.))
                    .map(|t| match (active, prof_rgb) {
                        (true, Some((r, g, b))) => t
                            .bg(rgb(u32::from_be_bytes([0, r, g, b])))
                            .text_color(rgb(TEXT_PRIMARY)),
                        (true, None) => t.bg(pane_fill()).text_color(rgb(TEXT_PRIMARY)),
                        (false, Some((r, g, b))) => t
                            .bg(gpui::rgba(u32::from_be_bytes([r, g, b, 0x99])))
                            .text_color(rgb(TEXT_PRIMARY))
                            .hover(|t| t.bg(gpui::rgba(u32::from_be_bytes([r, g, b, 0xCC])))),
                        (false, None) => t
                            .text_color(gpui::rgba(TEXT_SECONDARY))
                            .hover(|t| t.bg(gpui::rgba(TAB_HOVER))),
                    })
                    // The dragged tab's strip original fades — the ghost is
                    // what you're holding, this is where it came from.
                    .when(self.dragging_tab == Some(ix), |t| t.opacity(0.35))
                    .children(icon_el)
                    .children(rec_dot)
                    .children(bc_mark)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(title),
                    )
                    .child(
                        // WinUI tab close: 32×24 hit target, ControlCornerRadius,
                        // E711 glyph at 12px.
                        div()
                            .id(("tab-close", ix))
                            .w(px(32.))
                            .h(px(24.))
                            .ml(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .font_family("Segoe MDL2 Assets")
                            .text_size(px(12.))
                            .text_color(gpui::rgba(TEXT_SECONDARY))
                            .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                            .child("\u{E711}")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.close_at(ix, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                        this.switch_to(ix, cx);
                    }))
                    // Right-click: the tab context menu (close / logging).
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, ev: &gpui::MouseDownEvent, _win, cx| {
                            cx.stop_propagation();
                            this.tab_menu = Some((ix, ev.position.x / px(1.)));
                            cx.notify();
                        }),
                    )
                    // Tab DnD: drop on a tab = reorder there; drop on the
                    // pane below = detach into a fresh window (see the pane).
                    .on_drag(
                        TabDrag {
                            ix,
                            title: drag_title,
                        },
                        |drag, _offset, _window, cx| {
                            let title = drag.title.clone();
                            cx.new(|_| TabDragGhost {
                                title,
                                windowed: false,
                            })
                        },
                    )
                    .drag_over::<TabDrag>(|style, _, _, _| {
                        // The drop lands AT this tab's slot — the steel-blue
                        // left edge says exactly that.
                        style
                            .bg(gpui::rgba(TAB_HOVER))
                            .border_l_2()
                            .border_color(rgb(0x3465A4))
                    })
                    .drag_over::<PaneDrag>(|style, _, _, _| {
                        style
                            .bg(gpui::rgba(TAB_HOVER))
                            .border_l_2()
                            .border_color(rgb(0x3465A4))
                    })
                    .on_drop(cx.listener(move |this, drag: &TabDrag, _window, cx| {
                        this.reorder_tab(drag.ix, ix, cx);
                    }))
                    .on_drop(cx.listener(move |this, drag: &PaneDrag, _window, cx| {
                        this.pane_drop_to_tab_slot(drag.pane, ix, cx);
                    }))
                    .into_any_element();
                sep.into_iter().chain(std::iter::once(tab))
            }))
            .child(
                // WinUI add-tab button: 32×24, E710 at 12px, centered in the
                // tab zone.
                div()
                    .id("tab-new")
                    .w(px(32.))
                    .h(px(24.))
                    .ml(px(4.))
                    .mb(px((TAB_H - 24.0) / 2.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .font_family("Segoe MDL2 Assets")
                    .text_size(px(12.))
                    .text_color(gpui::rgba(TEXT_SECONDARY))
                    .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                    .child("\u{E710}")
                    .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                        this.new_tab(cx);
                    })),
            )
            .child(
                // ChevronDown next to [+]: opens the profile list plus the
                // "設定..." entry (wt-style split new-tab button). Always
                // shown — with a single profile it is still the mouse path
                // to settings.
                div()
                    .id("tab-new-menu")
                    .w(px(18.))
                    .h(px(24.))
                    // Breathing room from [+]: they are distinct actions
                    // (new default tab vs. pick a profile), not a merged
                    // split button.
                    .ml(px(6.))
                    .mb(px((TAB_H - 24.0) / 2.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .font_family("Segoe MDL2 Assets")
                    .text_size(px(8.))
                    .text_color(gpui::rgba(TEXT_SECONDARY))
                    .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                    .child("\u{E70D}")
                    .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                        this.profile_menu = !this.profile_menu;
                        cx.notify();
                    })),
            )
            .child(
                // Trailing drag filler INSIDE the viewport: draggable empty
                // space when tabs are few; collapses to zero (and the tabs
                // scroll) when they overflow. HTCAPTION buys native drag,
                // double-click maximize, snap and the system menu.
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            );

        // ChevronLeft / ChevronRight, shown only when the tabs overflow. Each
        // click nudges the viewport by ~2 tabs, clamped to the scroll range.
        let scroll_arrow = |id: &'static str, glyph: &'static str, dir: f32| {
            div()
                .id(id)
                .flex_shrink_0()
                .w(px(22.))
                .h(px(24.))
                .mb(px((TAB_H - 24.0) / 2.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .font_family("Segoe MDL2 Assets")
                .text_size(px(10.))
                .text_color(gpui::rgba(TEXT_SECONDARY))
                .hover(|t| t.bg(gpui::rgba(SUBTLE_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                .child(glyph)
                .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                    let cur = this.strip_scroll.offset().x / px(1.);
                    let maxw = this.strip_scroll.max_offset().width / px(1.);
                    let nx = (cur + dir * 220.0).clamp(-maxw, 0.0);
                    this.strip_scroll.set_offset(point(px(nx), px(0.)));
                    cx.notify();
                }))
        };

        let strip = div()
            .w_full()
            .h(px(TAB_STRIP_H))
            .flex()
            .flex_row()
            .items_end()
            .pl_2()
            .bg(chrome_fill())
            // Wheel anywhere over the strip scrolls the tabs horizontally
            // (both axes fold into horizontal, browser-style), and never
            // reaches the terminal below. No-op when the tabs already fit.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _win, cx| {
                // Swallow FIRST: wheel over the strip must never fall through
                // to the terminal scroll handler, tabs overflowing or not.
                cx.stop_propagation();
                let maxw = this.strip_scroll.max_offset().width / px(1.);
                if maxw <= 0.0 {
                    return;
                }
                let step = match ev.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.x) + f32::from(p.y),
                    ScrollDelta::Lines(l) => (l.x + l.y) * 40.0,
                };
                if step == 0.0 {
                    return;
                }
                let cur = this.strip_scroll.offset().x / px(1.);
                let nx = (cur + step).clamp(-maxw, 0.0);
                this.strip_scroll.set_offset(point(px(nx), px(0.)));
                cx.notify();
            }))
            .when(needs_scroll, |s| {
                s.child(scroll_arrow("tab-scroll-left", "\u{E76B}", 1.0))
            })
            .child(tab_viewport)
            .when(needs_scroll, |s| {
                s.child(scroll_arrow("tab-scroll-right", "\u{E76C}", -1.0))
            })
            .child(caption_button("\u{E921}", WindowControlArea::Min))
            .child(caption_button(
                if maximized { "\u{E923}" } else { "\u{E922}" },
                WindowControlArea::Max,
            ))
            .child(caption_button("\u{E8BB}", WindowControlArea::Close));

        // ── terminal pane (active tab) ───────────────────────────────────
        let pane = if self.tabs.get(self.active).is_some() {
            div()
                .flex_1()
                .w_full()
                .px_1()
                // Focus-on-click lives on the PANE, not the window root: gpui's
                // focus listener calls prevent_default on every mouse down over
                // a focusable hitbox, and gpui-Windows reads that as "the app
                // consumed this click" — which would swallow the caption
                // buttons' and drag area's non-client handling up in the strip.
                .track_focus(&self.terminal_focus.clone())
                // Dropping a tab on the pane detaches it into a fresh window
                // (its own OS process when the session is transferable) —
                // the "tear a tab off" gesture without OS-level DnD. The
                // dashed outline advertises it while a tab hovers.
                .drag_over::<TabDrag>(|style, _, _, _| {
                    style
                        .border_2()
                        .border_dashed()
                        .border_color(gpui::rgba(0x8A9CC880))
                })
                .on_drop(cx.listener(|this, drag: &TabDrag, window, cx| {
                    this.detach_at(drag.ix, window, cx);
                }))
                .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                    selection::copy_to_clipboard(&this.selection, this.active_session(), cx);
                }))
                .on_action(cx.listener(|this, _: &TerminalPaste, _window, cx| {
                    if let Some(item) = cx.read_from_clipboard()
                        && let Some(text) = item.text()
                    {
                        this.paste_input(&text);
                    }
                }))
                .on_action(cx.listener(|this, _: &PaneToTab, _window, cx| {
                    this.pane_to_tab(cx);
                }))
                .on_action(cx.listener(|this, _: &PaneToWindow, _window, cx| {
                    this.pane_to_window(cx);
                }))
                .on_action(cx.listener(|this, _: &TogglePaneBroadcast, _window, cx| {
                    if let Some(tab) = this.tabs.get(this.active) {
                        tab.active_entry().0.toggle_broadcast_target();
                        cx.notify();
                    }
                }))
                .child({
                    let tab = &self.tabs[self.active];
                    let zoom_leaf = (tab.zoomed && tab.is_split())
                        .then(|| tab.root.find(tab.active_pane))
                        .flatten();
                    if let Some(leaf) = zoom_leaf {
                        // Zoomed: the focused pane alone fills the tab area
                        // (fit stays per-pane/measured; the grip hides).
                        div()
                            .relative()
                            .size_full()
                            .child(self.render_leaf(leaf, tab.active_pane, true, false, cw, ch, cx))
                            .child(
                                div()
                                    .absolute()
                                    .top(px(6.))
                                    .right(px(10.))
                                    .px(px(7.))
                                    .py(px(2.))
                                    .rounded(px(4.))
                                    .bg(gpui::rgba(0x1F1E1CB0))
                                    .border_1()
                                    .border_color(gpui::rgba(0x45403A90))
                                    .text_size(px(11.))
                                    .text_color(gpui::rgba(0xC9A94EC0))
                                    .child("ズーム中"),
                            )
                            .into_any_element()
                    } else {
                        self.render_pane_node(
                            &tab.root,
                            Vec::new(),
                            tab.active_pane,
                            tab.is_split(),
                            cw,
                            ch,
                            cx,
                        )
                    }
                })
                // Right-click menu, same actions as the Ctrl+Shift chords.
                // Attached to the pane — a plain flex div, NEVER a scroll
                // container: the open menu injects a window-sized absolute
                // subtree as a child, and taffy counts absolute children
                // toward a scroll container's content size, which scrolled
                // the grid clean out of view in shogun-desktop (the
                // "right-click blanks the alt screen" bug, fixed 2026-07-10).
                .context_menu({
                    let menu_focus = self.terminal_focus.clone();
                    let split = self.tabs.get(self.active).is_some_and(Tab::is_split);
                    move |menu, _window, _cx| {
                        let menu = menu
                            .action_context(menu_focus.clone())
                            .menu("コピー", Box::new(TerminalCopy))
                            .menu("ペースト", Box::new(TerminalPaste))
                            .separator()
                            // Marks THIS pane as a broadcast recipient
                            // (selection broadcast) — the rust frame is
                            // the state readout, so the label stays
                            // stateless (render-time capture would go
                            // stale on the right-click focus hop).
                            .menu("ブロードキャスト対象を切替", Box::new(TogglePaneBroadcast));
                        if split {
                            menu.separator()
                                .menu("ペインをタブへ分離", Box::new(PaneToTab))
                                .menu("ペインを新しい窓へ分離", Box::new(PaneToWindow))
                        } else {
                            menu
                        }
                    }
                })
                .into_any_element()
        } else {
            div()
                .flex_1()
                .text_color(rgb(0xE8DCC8))
                .child("シェルを起動できなかった (pwsh / cmd が見つからない)")
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(pane_fill())
            // Carries the drag preview past our own frame (see the method).
            .child(self.drag_follower_hook(cx))
            // ── tab tear-off ─────────────────────────────────────────────
            // Track which tab rides the active drag; on a mouse up OUTSIDE
            // the window (SetCapture routes it here) no drop target can
            // fire — that release IS the browser-style tear-off gesture,
            // so detach the tab into its own window. An in-window release
            // that no drop target consumed just cancels.
            .on_drag_move::<TabDrag>(cx.listener(
                |this, ev: &gpui::DragMoveEvent<TabDrag>, _w, cx| {
                    let drag = ev.drag(cx);
                    this.dragging_tab = Some(drag.ix);
                    this.drag_title = Some(drag.title.clone());
                },
            ))
            .on_drag_move::<PaneDrag>(cx.listener(
                |this, ev: &gpui::DragMoveEvent<PaneDrag>, _w, cx| {
                    this.dragging_pane = Some(ev.drag(cx).pane);
                },
            ))
            // Divider resize: moves steer the grabbed Split's ratio, any
            // release drops it (the ratio is already applied live).
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, cx| {
                this.drag_divider_to(ev.position, window, cx);
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _ev, _w, cx| {
                    this.divider_drag = None;
                    if this.dragging_tab.take().is_some() | this.dragging_pane.take().is_some() {
                        // A cancelled drag must repaint NOW — the faded
                        // source and the drop zones would linger otherwise.
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _ev: &gpui::MouseUpEvent, window, cx| {
                    this.divider_drag = None;
                    // A pane released outside the window detaches into a
                    // fresh window of this process.
                    if let Some(pane) = this.dragging_pane.take() {
                        if let Some(tab) = this.tabs.get_mut(this.active)
                            && tab.is_split()
                        {
                            tab.active_pane = pane;
                            if let Some(entry) = tab.close_active_pane() {
                                this.after_tab_change(cx);
                                cx.notify();
                                open_tabs_window(cx, vec![entry]);
                            }
                        }
                        return;
                    }
                    let Some(ix) = this.dragging_tab.take() else {
                        return;
                    };
                    // Released over another rikka window = drag-merge (the
                    // tab moves there, even from a single-tab window);
                    // anywhere else = tear-off into a fresh window.
                    #[cfg(windows)]
                    if let Some((pid, drop_at)) = other_process_under_cursor()
                        && this.drop_tab_on_window_process(ix, pid, drop_at, window, cx)
                    {
                        return;
                    }
                    this.detach_at(ix, window, cx);
                }),
            )
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let ks = &event.keystroke;
                let m = &ks.modifiers;
                // The search bar swallows the keyboard while open.
                if this.search.open {
                    let close_chord =
                        keymap::resolve(m, ks.key.as_str()) == Some(keymap::Action::Search);
                    let sess = this
                        .tabs
                        .get(this.active)
                        .map(|t| &t.active_entry().0.session);
                    if this.search.key(ks, close_chord, sess, cx) {
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                }
                // Alt+arrows: directional pane focus — only while the tab
                // is actually split. Unsplit tabs pass the chord to the
                // application (ESC-prefixed arrow via the encoder below).
                if m.alt
                    && !m.control
                    && !m.shift
                    && matches!(ks.key.as_str(), "left" | "right" | "up" | "down")
                    && this.tabs.get(this.active).is_some_and(Tab::is_split)
                {
                    this.focus_pane_direction(ks.key.as_str(), cx);
                    cx.stop_propagation();
                    return;
                }
                // ── tab management chords (defaults Ctrl+Shift+…, each
                // reassignable through `[keys]` — see keymap.rs) ────────
                if let Some(action) = keymap::resolve(m, ks.key.as_str()) {
                    this.perform(action, window, cx);
                    cx.stop_propagation();
                    return;
                }
                // Hardwired merge synonym: gpui-Windows never delivers
                // Ctrl+M (the ^M = CR legacy swallows it) — "a" is the real
                // binding, "m" kept for platforms where it arrives.
                if m.control && m.shift && ks.key == "m" {
                    this.merge_all(cx);
                    cx.stop_propagation();
                    return;
                }
                if m.control && !m.shift && (ks.key == "tab" || ks.key == "pagedown") {
                    this.cycle(true, cx);
                    cx.stop_propagation();
                    return;
                }
                if m.control && !m.shift && ks.key == "pageup" {
                    this.cycle(false, cx);
                    cx.stop_propagation();
                    return;
                }
                if m.control && !m.shift && ks.key == "insert" {
                    selection::copy_to_clipboard(&this.selection, this.active_session(), cx);
                    cx.stop_propagation();
                    return;
                }
                if !m.control && m.shift && ks.key == "insert" {
                    if let Some(item) = cx.read_from_clipboard()
                        && let Some(text) = item.text()
                    {
                        this.paste_input(&text);
                    }
                    cx.stop_propagation();
                    return;
                }
                // Shift+PageUp/PageDown: page through the scrollback.
                if m.shift && (ks.key == "pageup" || ks.key == "pagedown") {
                    if let Some(s) = this.active_session() {
                        let page = s.rows.load(Ordering::Relaxed).saturating_sub(1) as i32;
                        s.scroll_display(if ks.key == "pageup" { page } else { -page });
                    }
                    cx.stop_propagation();
                    return;
                }
                // Everything else through the engine's encoder, per
                // recipient. A key nobody consumes keeps propagating so
                // WM_CHAR feeds the IME input handler (otherwise chars
                // would double).
                if this.send_key_input(ks) {
                    cx.stop_propagation();
                }
            }))
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, _win, _cx| {
                    this.wheel_scroll(event, cw, ch);
                }),
            )
            .child(strip)
            .child(pane)
            // Active tab's progress: an SD/ghostty-style bar across the top of
            // the pane. An overlay, so the grid never reflows when it appears.
            .children(self.active_session().and_then(tab_progress).map(|p| {
                div()
                    .absolute()
                    .top(px(TAB_STRIP_H))
                    .left_0()
                    .right_0()
                    .child(render_progress_bar("pane-progress", p))
            }))
            // Scrollback search bar (Ctrl+Shift+F): VSCode/wt-style, top
            // right — counter from the session, buttons via host listeners.
            .children({
                let status = self
                    .tabs
                    .get(self.active)
                    .and_then(|t| t.active_entry().0.session.search_status());
                let handlers = rikka_terminal_core::search_bar::SearchHandlers {
                    prev: Box::new(cx.listener(|this: &mut TabsWindow, _, _, cx| {
                        this.search_nav(-1, cx);
                    })),
                    next: Box::new(cx.listener(|this: &mut TabsWindow, _, _, cx| {
                        this.search_nav(1, cx);
                    })),
                    close: Box::new(cx.listener(|this: &mut TabsWindow, _, _, cx| {
                        let sess = this
                            .tabs
                            .get(this.active)
                            .map(|t| &t.active_entry().0.session);
                        this.search.close(sess);
                        cx.notify();
                    })),
                    case: Box::new(cx.listener(|this: &mut TabsWindow, _, _, cx| {
                        let sess = this
                            .tabs
                            .get(this.active)
                            .map(|t| &t.active_entry().0.session);
                        this.search.toggle_case(sess);
                        cx.notify();
                    })),
                    regex: Box::new(cx.listener(|this: &mut TabsWindow, _, _, cx| {
                        let sess = this
                            .tabs
                            .get(this.active)
                            .map(|t| &t.active_entry().0.session);
                        this.search.toggle_regex(sess);
                        cx.notify();
                    })),
                };
                let colors = rikka_terminal_core::search_bar::sheet();
                self.search.render(status, handlers, &colors).map(|bar| {
                    div()
                        .absolute()
                        .top(px(TAB_STRIP_H + 10.))
                        .right(px(14.))
                        .child(bar)
                })
            })
            .when(self.profile_menu, |root| {
                // Click-away scrim behind the list; a click anywhere else
                // closes the menu. Rendered before the list so the list wins
                // the overlap.
                root.child(
                    div()
                        .id("profile-scrim")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                            this.profile_menu = false;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(TAB_STRIP_H))
                        .left(px(menu_left))
                        .flex()
                        .flex_col()
                        .min_w(px(200.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(rgb(CHROME_BG))
                        .border_1()
                        .border_color(gpui::rgba(DIVIDER))
                        .child(
                            div()
                                .id("menu-settings")
                                .flex()
                                .flex_row()
                                .items_center()
                                .px(px(12.))
                                .py(px(6.))
                                .text_size(px(13.))
                                .text_color(gpui::rgba(TEXT_SECONDARY))
                                .hover(|t| {
                                    t.bg(gpui::rgba(TAB_HOVER)).text_color(rgb(TEXT_PRIMARY))
                                })
                                .child("設定...")
                                .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                                    cx.stop_propagation();
                                    this.profile_menu = false;
                                    settings_window::open(cx);
                                    cx.notify();
                                })),
                        )
                        .when(has_profiles, |menu| {
                            // Divider between "設定..." and the profile list —
                            // dropped when the list is empty (default config)
                            // or it dangles under the lone settings entry.
                            menu.child(
                                div()
                                    .h(px(1.))
                                    .mx(px(8.))
                                    .my(px(3.))
                                    .bg(gpui::rgba(DIVIDER)),
                            )
                        })
                        .children(profiles.into_iter().map(|(idx, name, icon)| {
                            div()
                                .id(("profile", idx))
                                .flex()
                                .flex_row()
                                .items_center()
                                .px(px(12.))
                                .py(px(6.))
                                .text_size(px(13.))
                                .text_color(gpui::rgba(TEXT_SECONDARY))
                                .hover(|t| {
                                    t.bg(gpui::rgba(TAB_HOVER)).text_color(rgb(TEXT_PRIMARY))
                                })
                                .children(icon.map(|ic| icon_element(ic, 8.)))
                                .child(name)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                                    cx.stop_propagation();
                                    this.new_tab_profile(idx, cx);
                                    cx.notify();
                                }))
                        })),
                )
            })
            .when_some(self.tab_menu, |root, (menu_ix, at_x)| {
                // Tab context menu, profile-menu-shaped: click-away scrim
                // behind an absolutely placed list under the strip.
                let logging = self
                    .tabs
                    .get(menu_ix)
                    .map(|t| t.primary().0.session.logging_active())
                    .unwrap_or(false);
                // Broadcast toggle: split tabs only — on a single pane the
                // fan-out is meaningless and the item would just confuse.
                let broadcast = self
                    .tabs
                    .get(menu_ix)
                    .filter(|t| t.is_split())
                    .map(|t| t.broadcast);
                let vw = window.viewport_size().width / px(1.);
                let left = at_x.min(vw - 200.).max(0.);
                let item = |id: &'static str, label: &'static str| {
                    div()
                        .id(id)
                        .flex()
                        .flex_row()
                        .items_center()
                        .px(px(12.))
                        .py(px(6.))
                        .text_size(px(13.))
                        .text_color(gpui::rgba(TEXT_SECONDARY))
                        .hover(|t| t.bg(gpui::rgba(TAB_HOVER)).text_color(rgb(TEXT_PRIMARY)))
                        .child(label)
                };
                root.child(
                    div()
                        .id("tab-menu-scrim")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                            this.tab_menu = None;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(TAB_STRIP_H))
                        .left(px(left))
                        .flex()
                        .flex_col()
                        .min_w(px(180.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(rgb(CHROME_BG))
                        .border_1()
                        .border_color(gpui::rgba(DIVIDER))
                        .shadow_lg()
                        .child(
                            item(
                                "tab-menu-log",
                                if logging {
                                    "ログ停止"
                                } else {
                                    "ログ開始"
                                },
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _win, cx| {
                                    cx.stop_propagation();
                                    if let Some(tab) = this.tabs.get(menu_ix) {
                                        session_log::toggle(&tab.primary().0.session);
                                    }
                                    this.tab_menu = None;
                                    cx.notify();
                                },
                            )),
                        )
                        .when_some(broadcast, |menu, on| {
                            menu.child(
                                item(
                                    "tab-menu-broadcast",
                                    if on {
                                        "ブロードキャスト入力を停止"
                                    } else {
                                        "ブロードキャスト入力を開始"
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _win, cx| {
                                        cx.stop_propagation();
                                        if let Some(tab) = this.tabs.get_mut(menu_ix) {
                                            tab.broadcast = !tab.broadcast;
                                        }
                                        this.tab_menu = None;
                                        cx.notify();
                                    },
                                )),
                            )
                        })
                        // Cross-tab fan-out needs a second tab to mean
                        // anything; single-pane tabs still qualify.
                        .when(self.tabs.len() > 1, |menu| {
                            let on = self.broadcast_all;
                            menu.child(
                                item(
                                    "tab-menu-broadcast-all",
                                    if on {
                                        "全タブへのブロードキャストを停止"
                                    } else {
                                        "全タブへブロードキャスト"
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _win, cx| {
                                        cx.stop_propagation();
                                        this.broadcast_all = !this.broadcast_all;
                                        this.tab_menu = None;
                                        cx.notify();
                                    },
                                )),
                            )
                        })
                        .child(
                            div()
                                .h(px(1.))
                                .mx(px(8.))
                                .my(px(3.))
                                .bg(gpui::rgba(DIVIDER)),
                        )
                        .child(item("tab-menu-close", "閉じる").on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.tab_menu = None;
                                this.close_at(menu_ix, window, cx);
                            },
                        ))),
                )
            })
    }
}

/// Dark-mode DWM frames for every window of this process. The titlebar
/// itself is hidden (appears_transparent), but the attribute still colors
/// the remaining 1px window border and any pre-first-paint frame flash.
/// DWM attribute only — no gpui changes.
/// Strip the DWM drop shadow from our tool windows — today only the tab-drag
/// follower, which is the sole `WindowKind::PopUp` we open.
///
/// The follower is borderless by style (`WS_EX_TOOLWINDOW`, no caption), but
/// DWM still frames a top-level window with a shadow. Floating over the
/// desktop mid-drag that reads as a stray border around the preview rather
/// than as depth. `DWMNCRP_DISABLED` turns off non-client rendering for that
/// window only. DWM attribute only — no gpui changes, same as the dark
/// titlebar pass below.
#[cfg(windows)]
fn strip_tool_window_shadows() {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::Graphics::Dwm::{
        DWMNCRP_DISABLED, DWMWA_NCRENDERING_POLICY, DwmSetWindowAttribute,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetWindowLongPtrW, GetWindowThreadProcessId, WS_EX_TOOLWINDOW,
    };
    use windows::core::BOOL;
    unsafe extern "system" fn apply(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == lparam.0 as u32 {
            let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            if ex & WS_EX_TOOLWINDOW.0 != 0 {
                let policy = DWMNCRP_DISABLED;
                unsafe {
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_NCRENDERING_POLICY,
                        &raw const policy as *const _,
                        std::mem::size_of_val(&policy) as u32,
                    );
                }
            }
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(apply), LPARAM(GetCurrentProcessId() as isize));
    }
}

#[cfg(not(windows))]
fn strip_tool_window_shadows() {}

#[cfg(windows)]
fn apply_dark_titlebars() {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};
    use windows::core::BOOL;
    unsafe extern "system" fn apply(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == lparam.0 as u32 {
            let dark: i32 = 1;
            unsafe {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_USE_IMMERSIVE_DARK_MODE,
                    &raw const dark as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(apply), LPARAM(GetCurrentProcessId() as isize));
    }
}

#[cfg(not(windows))]
fn apply_dark_titlebars() {}

/// Open a tab-group window hosting `initial` and register it for merge-all
/// and release cleanup.
fn open_tabs_window(cx: &mut App, initial: Vec<TabEntry>) {
    open_tabs_window_opts(cx, initial, &cli::Launch::default());
}

/// Open a tab-group window with wt-style launch geometry. `--size` uses cell
/// ESTIMATES (real metrics need a live window; the PTY refits to the painted
/// pane on the first frame, so only the window's outer size is approximate).
fn open_tabs_window_opts(cx: &mut App, initial: Vec<TabEntry>, launch: &cli::Launch) {
    const EST_CW: f32 = 8.5;
    const EST_CH: f32 = 21.0;
    let win_size = match launch.size_cells {
        Some((c, r)) => size(
            px(c as f32 * EST_CW + PAD * 2.0),
            px(r as f32 * EST_CH + TAB_STRIP_H + PAD * 2.0),
        ),
        None => size(px(1000.), px(640.)),
    };
    let bounds = match launch.pos {
        Some((x, y)) => Bounds {
            origin: point(px(x), px(y)),
            size: win_size,
        },
        None => Bounds::centered(None, win_size, cx),
    };
    let window_bounds = if launch.fullscreen {
        WindowBounds::Fullscreen(bounds)
    } else if launch.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some("RikkaTerminal".into()),
                    // Hides the native titlebar (gpui: hide_title_bar) — the
                    // tab strip takes over via window_control_area hitboxes.
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                // gpui maps Blurred to ACCENT_ENABLE_ACRYLICBLURBEHIND
                // (SetWindowCompositionAttribute, Win10 1809+) — the same
                // Win10-era acrylic Files' setting uses. Opt-in until the
                // known drag-latency cost of that API is judged acceptable;
                // a settings file is a P1 item.
                window_background: if acrylic() {
                    gpui::WindowBackgroundAppearance::Blurred
                } else {
                    gpui::WindowBackgroundAppearance::Opaque
                },
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TabsWindow::new(window, cx, initial)),
        )
        .expect("open window");
    let Ok(entity) = handle.update(cx, |_, _, cx| cx.entity()) else {
        return;
    };
    hub::register_window(
        cx,
        hub::alloc_window_id(),
        handle.into(),
        entity.downgrade(),
    );
    // Window closed (caption ✕ or last-tab close): stop the surviving tabs'
    // drivers; the sessions drop with the entity and close their PTYs. Quit
    // once no window is left — with the titlebar integrated, the caption ✕
    // is the product's real close button and must end the process.
    cx.observe_release(&entity, |win: &mut TabsWindow, cx| {
        for tab in &win.tabs {
            tab.shutdown_all();
        }
        if hub::live_windows(cx) == 0 {
            cx.quit();
        }
    })
    .detach();
    apply_dark_titlebars();
}

/// Single-instance role after the socket election.
enum Role {
    /// This process owns the socket — it hosts windows and serves forwards.
    Monarch(ipc::transport::Monarch),
    /// Spawned by the monarch to host one window (crash isolation): no
    /// socket, no forwarding — just register back with the spawner.
    WindowProcess,
    /// Couldn't bind and couldn't forward (a rare race) — run a lone window.
    Standalone,
}

/// Bind the per-user socket (become monarch), or forward this launch to the
/// running monarch. Returns `None` when the launch was forwarded (this process
/// should exit); `Some(role)` when this process should run.
fn elect(endpoint: &str, launch: &cli::Launch) -> Option<Role> {
    for _ in 0..5 {
        match ipc::transport::Monarch::bind(endpoint) {
            Ok(monarch) => return Some(Role::Monarch(monarch)),
            // Someone holds the socket — forward our launch to them and exit.
            // If they vanished mid-race the connect fails; loop and re-bind.
            Err(_) if forward_launch(endpoint, launch).is_ok() => return None,
            Err(_) => continue,
        }
    }
    Some(Role::Standalone)
}

/// The wire form of a cold-start handoff: the same message the shim would
/// have sent warm, except the handles now live in THIS process (inherited
/// through CreateProcess), so the pid is ours and a monarch pulls from us.
fn attach_request(a: &cli::AttachSpec, launch: &cli::Launch) -> ipc::AttachArgs {
    let [input, output, signal, reference, server, client] = a.handles;
    // A tab-move relay parent left the screen state in a temp file (bulk
    // bytes cannot ride handle inheritance) — consume it exactly once. The
    // relay writes the full state JSON (`{vt_b64, images}`); a raw-VT file
    // from an older sibling still parses via the fallback.
    let state = a
        .state_path
        .as_ref()
        .and_then(|p| {
            let bytes = std::fs::metadata(p)
                .ok()
                .filter(|m| m.len() <= ipc::MAX_FRAME_BYTES as u64)
                .and_then(|_| std::fs::read(p).ok());
            let _ = std::fs::remove_file(p);
            bytes
        })
        .map(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .filter(|v| v.get("vt_b64").is_some())
                .unwrap_or_else(|| ipc::state_from_vt(&bytes))
        });
    ipc::AttachArgs {
        pid: std::process::id(),
        handles: ipc::Handles {
            input,
            output,
            signal,
            reference,
            server,
            client,
            ..Default::default()
        },
        startup: ipc::StartupInfo {
            title: a.title.clone(),
            x: 0,
            y: 0,
            cols: launch.size_cells.map_or(0, |s| s.0),
            rows: launch.size_cells.map_or(0, |s| s.1),
        },
        state,
        elevated: false,
        target: ipc::Target::New,
        drop_at: None,
        palette: a.palette.clone(),
    }
}

/// Forward this launch to the running monarch and exit: a plain CLI goes as
/// a `spawn` (raw argv + cwd, re-parsed there); a cold-start handoff that
/// LOST the bind race goes as a regular `attach` — the winner pulls the
/// inherited handles straight out of this process (IPC.md's race rule).
fn forward_launch(endpoint: &str, launch: &cli::Launch) -> std::io::Result<()> {
    let mut conn = ipc::transport::connect(endpoint)?;
    let req = match &launch.attach {
        Some(a) => ipc::Request::Attach(attach_request(a, launch)),
        None => {
            let cwd = std::env::current_dir()
                .ok()
                .and_then(|p| p.to_str().map(str::to_owned));
            let argv: Vec<String> = std::env::args().skip(1).collect();
            ipc::Request::Spawn(ipc::SpawnArgs {
                cwd,
                argv,
                window: launch.window,
                ..Default::default()
            })
        }
    };
    conn.send_request(&req)?;
    if conn.recv_response()?.ok {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "monarch rejected the forwarded launch",
        ))
    }
}

/// Open the cold-start handoff window when this launch carries one. Returns
/// false when there is nothing to adopt or the adoption failed.
#[cfg(windows)]
fn open_inherited(cx: &mut App, launch: &cli::Launch) -> bool {
    let Some(a) = &launch.attach else {
        return false;
    };
    let args = attach_request(a, launch);
    match attach::local_attach(&args).and_then(|pulled| {
        let palette = pulled.palette();
        pulled.into_session().map(|s| (s, palette))
    }) {
        Ok((session, palette)) => {
            open_attached(cx, session, args.startup, palette);
            true
        }
        Err(e) => {
            log::warn!("cold-start attach failed, opening a normal window: {e:#}");
            false
        }
    }
}

#[cfg(not(windows))]
fn open_inherited(_cx: &mut App, launch: &cli::Launch) -> bool {
    if launch.attach.is_some() {
        log::warn!("--attach is Windows-only (OS default-terminal handoff)");
    }
    false
}

/// A request accepted on the IPC thread, handed to the main (gpui) thread to
/// open its window. An `Attach` session is already live (its PTY threads are
/// pumping) — only the window wrapping needs the UI thread.
enum Forwarded {
    /// A forwarded launch: re-parse the CLI and open a window for it.
    Spawn(Vec<String>),
    /// A targeted launch (`rt -w`) routed to this process's window socket:
    /// open its tabs in the addressed window instead of a fresh one.
    SpawnInWindow(ipc::SpawnArgs),
    /// An adopted OS handoff: wrap the session in a fresh window.
    #[cfg(windows)]
    Attach(Box<TerminalSession>, ipc::StartupInfo, Option<Vec<u32>>),
    /// A tab-move arriving on this window's OWN socket: adopt the session as
    /// a tab of a window this process hosts (never a new window). Fields
    /// after the startup info: the drop point, the tab's wire palette, and
    /// the addressed target window id (per-window addressing) when the
    /// sender named one.
    #[cfg(windows)]
    AdoptTab(
        Box<TerminalSession>,
        ipc::StartupInfo,
        Option<(i32, i32)>,
        Option<Vec<u32>>,
        Option<u64>,
    ),
}

/// Decode a wire palette (19 packed 0xRRGGBB) into the engine's type; a
/// malformed payload fails open (no theme).
fn wire_theme(p: Option<Vec<u32>>) -> Option<rikka_terminal_core::theme::Palette> {
    p.as_deref()
        .and_then(rikka_terminal_core::theme::Palette::from_wire)
}

/// Apply one IPC-thread message on the gpui thread — shared by the monarch's
/// main-socket pump and every window's own-socket pump.
fn pump_forwarded(cx: &mut App, msg: Forwarded) {
    match msg {
        Forwarded::Spawn(argv) => open_forwarded(cx, argv),
        Forwarded::SpawnInWindow(s) => spawn_in_window(cx, s),
        #[cfg(windows)]
        Forwarded::Attach(session, startup, palette) => {
            open_attached(cx, *session, startup, palette)
        }
        #[cfg(windows)]
        Forwarded::AdoptTab(session, startup, drop_at, palette, target) => {
            adopt_forwarded(cx, *session, startup, drop_at, palette, target)
        }
    }
}

/// Adopt an `attach` on the IPC thread — the handle pull must finish while
/// the sender still waits on our response. The session then gets its own
/// window process (crash isolation); only if that launch fails is it hosted
/// in this process as a fallback.
#[cfg(windows)]
fn handle_attach(
    tx: &futures::channel::mpsc::UnboundedSender<Forwarded>,
    args: ipc::AttachArgs,
    peer_pid: u32,
) -> Result<()> {
    if args.target != ipc::Target::New {
        // Tab moves route directly: resolve_window, then attach on the
        // window's own socket. The monarch never proxies handles.
        anyhow::bail!("attach target window:<id>: resolve_window and attach its own socket");
    }
    let pulled = attach::pull_attach(&args, peer_pid)?;
    match pulled.relay_to_window_process() {
        // Dropping `pulled` closes our copies; the child keeps its
        // inherited ones.
        Ok(()) => Ok(()),
        Err(e) => {
            log::warn!("attach relay failed, adopting in-process: {e:#}");
            let startup = pulled.startup.clone();
            let palette = pulled.palette();
            let session = pulled.into_session()?;
            tx.unbounded_send(Forwarded::Attach(Box::new(session), startup, palette))
                .map_err(|_| anyhow::anyhow!("monarch is shutting down"))?;
            Ok(())
        }
    }
}

#[cfg(not(windows))]
fn handle_attach(
    _tx: &futures::channel::mpsc::UnboundedSender<Forwarded>,
    _args: ipc::AttachArgs,
    _peer_pid: u32,
) -> Result<()> {
    anyhow::bail!("attach is Windows-only (OS default-terminal handoff)")
}

/// The monarch's window bookkeeping (IPC.md `register_window` /
/// `list_windows`). v1: window ids are pids and there is no liveness
/// pruning — id-targeted routing lands with inc6's tab moves.
#[derive(Default)]
struct WindowDirectory(Vec<ipc::RegisterWindow>);

impl WindowDirectory {
    /// Single-window upsert, keyed by window id (the legacy heartbeat form;
    /// per-window ids made the pid no longer unique per entry).
    fn register(&mut self, r: ipc::RegisterWindow) {
        match self.0.iter_mut().find(|w| w.window_id == r.window_id) {
            Some(slot) => *slot = r,
            None => self.0.push(r),
        }
    }

    /// Replace ALL of `pid`'s entries — the per-window heartbeat: every live
    /// window in one swap, closed ones disappearing with it.
    fn register_all(&mut self, pid: u32, windows: Vec<ipc::RegisterWindow>) {
        self.0.retain(|w| w.pid != pid);
        self.0.extend(windows.into_iter().filter(|w| w.pid == pid));
    }

    fn list(&self) -> Vec<ipc::WindowInfo> {
        self.0
            .iter()
            .map(|w| ipc::WindowInfo {
                id: w.window_id,
                title: None,
                pid: w.pid,
            })
            .collect()
    }

    /// Resolve a target query to a concrete reachable window: `0` = any
    /// live window (`rt -w last`); otherwise an exact window-id match, then
    /// the bare-pid fallback (a drag-merge sender only knows the pid under
    /// the cursor — that process's windows share one socket, and the
    /// receiver routes by drop point). Returns the CONCRETE window id with
    /// the endpoint so a forwarded spawn addresses a real window. `None`
    /// when nothing reachable matches (endpointless windows cannot receive).
    fn resolve_target(&self, query: u64) -> Option<(u64, String)> {
        let hit = if query == 0 {
            self.0.iter().find(|w| !w.endpoint.is_empty())
        } else {
            self.0
                .iter()
                .find(|w| w.window_id == query && !w.endpoint.is_empty())
                .or_else(|| {
                    u32::try_from(query).ok().and_then(|pid| {
                        self.0
                            .iter()
                            .find(|w| w.pid == pid && !w.endpoint.is_empty())
                    })
                })
        };
        hit.map(|w| (w.window_id, w.endpoint.clone()))
    }

    /// The window's own socket endpoint, for direct tab-move routing.
    /// `None` when the id is unknown or the window registered without a
    /// socket (its bind failed — it cannot receive moves).
    fn resolve(&self, window_id: u64) -> Option<String> {
        // 0 means "any" only for -w spawns, never for a tab move.
        if window_id == 0 {
            return None;
        }
        self.resolve_target(window_id).map(|(_, ep)| ep)
    }
}

fn validate_registration(peer_pid: u32, r: &ipc::RegisterWindow) -> Result<()> {
    anyhow::ensure!(
        r.pid == peer_pid,
        "registration pid {} does not match connected peer {peer_pid}",
        r.pid
    );
    let owns_id = r.window_id == u64::from(peer_pid) || (r.window_id >> 20) == u64::from(peer_pid);
    anyhow::ensure!(owns_id, "window id does not belong to the registering pid");
    anyhow::ensure!(
        r.endpoint.is_empty() || r.endpoint == ipc::transport::window_endpoint_name(peer_pid),
        "window endpoint is not canonical for the registering pid"
    );
    Ok(())
}

fn validate_registrations(peer_pid: u32, pid: u32, windows: &[ipc::RegisterWindow]) -> Result<()> {
    anyhow::ensure!(
        pid == peer_pid,
        "registration pid {pid} does not match connected peer {peer_pid}"
    );
    windows
        .iter()
        .try_for_each(|r| validate_registration(peer_pid, r))
}

const MAX_IPC_WORKERS: usize = 32;

struct IpcWorkerPermit(Arc<AtomicUsize>);

impl Drop for IpcWorkerPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

fn acquire_ipc_worker(active: &Arc<AtomicUsize>) -> Option<IpcWorkerPermit> {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_IPC_WORKERS).then_some(n + 1)
        })
        .ok()
        .map(|_| IpcWorkerPermit(Arc::clone(active)))
}

/// Forward a targeted spawn (`rt -w`) to a window process's own socket.
/// `Ok` only when the window answered ok — anything else lets the caller
/// fall open to the new-window path.
fn forward_spawn_to_window(endpoint: &str, s: &ipc::SpawnArgs) -> std::io::Result<()> {
    let mut conn = ipc::transport::connect(endpoint)?;
    conn.send_request(&ipc::Request::Spawn(s.clone()))?;
    let resp = conn.recv_response()?;
    if resp.ok {
        Ok(())
    } else {
        Err(std::io::Error::other(resp.error.unwrap_or_default()))
    }
}

/// Launch a forwarded spawn as its own OS process (`--window-process` + the
/// original argv, in the original cwd) — the crash-isolation core: one
/// window dying can never take the others with it.
fn spawn_window_process(s: &ipc::SpawnArgs) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--window-process").args(&s.argv);
    if let Some(cwd) = &s.cwd {
        cmd.current_dir(cwd);
    }
    cmd.spawn().map(drop)
}

/// Monarch side: accept forwarded launches on a background thread. Spawns
/// and handoffs become their own window processes; the channel to the main
/// (gpui) thread only carries the in-process fallbacks.
fn spawn_ipc_accept(
    cx: &mut App,
    monarch: ipc::transport::Monarch,
    window_endpoint: Option<String>,
) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
    std::thread::Builder::new()
        .name("rikka-ipc".into())
        .spawn(move || {
            // The monarch hosts a window itself, so it is in the directory
            // too (same id scheme: pid), reachable through its own window
            // socket like everyone else.
            let directory = Arc::new(FairMutex::new(WindowDirectory::default()));
            directory.lock().register(ipc::RegisterWindow {
                pid: std::process::id(),
                window_id: u64::from(std::process::id()),
                endpoint: window_endpoint.unwrap_or_default(),
            });
            monarch_accept_loop(monarch, tx, directory);
        })
        .ok();
    let async_cx = cx.to_async();
    async_cx
        .spawn(async move |cx| {
            use futures::StreamExt as _;
            while let Some(msg) = rx.next().await {
                let _ = cx.update(|cx| pump_forwarded(cx, msg));
            }
        })
        .detach();
}

/// The monarch's service loop: forwarded launches, handle transfers, and
/// window-directory queries, until the listener dies. Shared by the original
/// monarch ([`spawn_ipc_accept`]) and a re-elected one
/// ([`spawn_monarch_watcher`]).
fn monarch_accept_loop(
    monarch: ipc::transport::Monarch,
    tx: futures::channel::mpsc::UnboundedSender<Forwarded>,
    directory: Arc<FairMutex<WindowDirectory>>,
) {
    let active_workers = Arc::new(AtomicUsize::new(0));
    {
        {
            loop {
                let Ok(mut conn) = monarch.accept() else {
                    break;
                };
                let Some(permit) = acquire_ipc_worker(&active_workers) else {
                    continue;
                };
                // One worker per connection: a client that connects but
                // never sends its frame (dying process, wedged pipe) must
                // not stall every later launch and tab move behind it.
                let tx = tx.clone();
                let directory = Arc::clone(&directory);
                std::thread::Builder::new()
                    .name("rikka-ipc-conn".into())
                    .spawn(move || {
                        match {
                            let _permit = &permit;
                            conn.recv_request()
                        } {
                            Ok(ipc::Request::Spawn(mut s)) => {
                                // Targeted spawn (`rt -w`): resolve the addressed
                                // window and forward to its own socket, rewriting
                                // 0/pid forms to the concrete per-window id. Any
                                // failure falls open to the normal new-window
                                // path — a launch must never be lost.
                                let routed = match s.window {
                                    Some(q) => match directory.lock().resolve_target(q) {
                                        Some((id, endpoint)) => {
                                            s.window = Some(id);
                                            forward_spawn_to_window(&endpoint, &s).is_ok()
                                        }
                                        None => false,
                                    },
                                    None => false,
                                };
                                // Crash isolation first; in-process only as
                                // fallback (a monarch-resident window beats a
                                // dead launch).
                                if !routed {
                                    if let Err(e) = spawn_window_process(&s) {
                                        log::warn!(
                                            "window-process spawn failed, opening in-process: {e}"
                                        );
                                        let _ = tx.unbounded_send(Forwarded::Spawn(s.argv));
                                    }
                                }
                                let _ = conn.send_response(&ipc::Response::ok());
                            }
                            Ok(ipc::Request::Attach(a)) => {
                                // Handle transfer is gated on the OS-attested
                                // peer PID; an unattestable peer is refused
                                // (fail closed).
                                let resp = match conn.peer_pid() {
                                    Some(peer) => match handle_attach(&tx, a, peer) {
                                        Ok(()) => ipc::Response::ok(),
                                        Err(e) => ipc::Response::error(e.to_string()),
                                    },
                                    None => ipc::Response::error(
                                        "attach refused: peer identity could not be verified",
                                    ),
                                };
                                let _ = conn.send_response(&resp);
                            }
                            Ok(ipc::Request::RegisterWindow(r)) => {
                                let resp = match conn
                                    .peer_pid()
                                    .ok_or_else(|| anyhow::anyhow!("unattested registration peer"))
                                    .and_then(|peer| validate_registration(peer, &r))
                                {
                                    Ok(()) => {
                                        directory.lock().register(r);
                                        ipc::Response::ok()
                                    }
                                    Err(e) => ipc::Response::error(e.to_string()),
                                };
                                let _ = conn.send_response(&resp);
                            }
                            Ok(ipc::Request::RegisterWindows { pid, windows }) => {
                                let resp = match conn
                                    .peer_pid()
                                    .ok_or_else(|| anyhow::anyhow!("unattested registration peer"))
                                    .and_then(|peer| validate_registrations(peer, pid, &windows))
                                {
                                    Ok(()) => {
                                        directory.lock().register_all(pid, windows);
                                        ipc::Response::ok()
                                    }
                                    Err(e) => ipc::Response::error(e.to_string()),
                                };
                                let _ = conn.send_response(&resp);
                            }
                            Ok(ipc::Request::ListWindows) => {
                                let windows = directory.lock().list();
                                let _ = conn.send_response(&ipc::Response {
                                    ok: true,
                                    windows: Some(windows),
                                    ..Default::default()
                                });
                            }
                            Ok(ipc::Request::ResolveWindow { window }) => {
                                let resolved = directory.lock().resolve(window);
                                let resp = match resolved {
                                    Some(endpoint) => ipc::Response {
                                        ok: true,
                                        endpoint: Some(endpoint),
                                        ..Default::default()
                                    },
                                    None => ipc::Response::error(format!(
                                        "window {window} is unknown or unreachable"
                                    )),
                                };
                                let _ = conn.send_response(&resp);
                            }
                            Ok(ipc::Request::Ping) => {
                                let _ = conn.send_response(&ipc::Response::ok());
                            }
                            Ok(_) => {
                                let _ = conn.send_response(&ipc::Response::error(
                                    "request is not valid on the monarch socket",
                                ));
                            }
                            Err(_) => {} // client hung up before sending a frame
                        }
                    })
                    .ok();
            }
        }
    }
}

/// Window-process side of monarch re-election: heartbeat the monarch with
/// our idempotent (upserting) registration every few seconds; when it stops
/// answering, race to bind its socket. The winner starts serving
/// coordination itself — with a fresh directory that every other window's
/// next heartbeat repopulates — so new spawns and tab moves keep working
/// after the monarch window closes, instead of stalling until the next cold
/// start. Losing the race (or a mere blip) needs nothing special: the
/// heartbeat IS the re-registration with whoever won.
/// Re-read config.toml and re-apply everything hot-reloadable: the engine
/// globals (typography, theme, font features, search sheet), keymap,
/// logging, identity, and the shared new-tab menu; then nudge every window
/// so the active tab's per-profile theme wins again and the chrome
/// repaints. acrylic stays start-time-only (window attribute).
fn reload_config(cx: &mut App) {
    let cfg = config::Config::load();
    apply_appearance(&cfg);
    apply_theme(&cfg);
    apply_identity(&cfg);
    session_log::init(cfg.logging.clone());
    keymap::init(&cfg.keys);
    let (wt, wt_default) = wt_profiles::discover();
    let menu = cfg.build_menu(wt, wt_default);
    let profile_icons: Vec<Option<tab_icon::TabIcon>> = menu
        .profiles
        .iter()
        .map(|p| {
            tab_icon::resolve(
                p.argv.first().map(String::as_str).unwrap_or(""),
                p.argv.get(1..).unwrap_or(&[]),
                Some(&p.name),
            )
        })
        .collect();
    cx.set_global(hub::ProfileMenu(menu));
    cx.set_global(hub::ProfileIcons(profile_icons));
    for (_, weak) in hub::all_windows(cx) {
        let _ = weak.update(cx, |view, cx| {
            view.after_tab_change(cx);
            cx.notify();
        });
    }
}

/// Watch config.toml and hot-reload on change. Event-driven end to end
/// (ReadDirectoryChangesW via `notify`) — no polling and nothing on the
/// render path; a save burst is debounced to one reload. Watches the
/// PARENT directory: editors atomic-save via rename, which would drop a
/// watch held on the file itself.
fn spawn_config_watcher(cx: &mut App) {
    use notify::Watcher as _;
    let Some(path) = config::config_path() else {
        return;
    };
    let Some(dir) = path.parent().map(std::path::Path::to_path_buf) else {
        return;
    };
    let file_name = path.file_name().map(std::ffi::OsStr::to_os_string);
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
    std::thread::Builder::new()
        .name("rikka-config-watch".into())
        .spawn(move || {
            let (ev_tx, ev_rx) = std::sync::mpsc::channel();
            let Ok(mut watcher) = notify::recommended_watcher(move |res| {
                let _ = ev_tx.send(res);
            }) else {
                return;
            };
            if watcher
                .watch(&dir, notify::RecursiveMode::NonRecursive)
                .is_err()
            {
                return;
            }
            // Blocks between events; exits when the watcher backend dies.
            while let Ok(ev) = ev_rx.recv() {
                let relevant = match &ev {
                    Ok(ev) => ev
                        .paths
                        .iter()
                        .any(|p| p.file_name() == file_name.as_deref()),
                    Err(_) => false,
                };
                if !relevant {
                    continue;
                }
                // Editors save in bursts (truncate+write+rename); coalesce
                // them into one reload.
                std::thread::sleep(std::time::Duration::from_millis(120));
                while ev_rx.try_recv().is_ok() {}
                if tx.unbounded_send(()).is_err() {
                    return;
                }
            }
        })
        .ok();
    let async_cx = cx.to_async();
    async_cx
        .spawn(async move |cx| {
            use futures::StreamExt as _;
            while rx.next().await.is_some() {
                let _ = cx.update(reload_config);
            }
        })
        .detach();
}

fn spawn_monarch_watcher(cx: &mut App, endpoint: String, window_endpoint: Option<String>) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
    std::thread::Builder::new()
        .name("rikka-monarch-watch".into())
        .spawn(move || {
            let pid = std::process::id();
            let win_endpoint = window_endpoint.unwrap_or_default();
            loop {
                // Heartbeat = registration: EVERY live window of this
                // process (per-window addressing), replacing our previous
                // set so closed windows drop out. Doubles as the liveness
                // probe. First beat runs immediately — a fresh window is
                // addressable before the first 5s tick.
                let windows: Vec<ipc::RegisterWindow> = hub::live_window_ids()
                    .into_iter()
                    .map(|window_id| ipc::RegisterWindow {
                        pid,
                        window_id,
                        endpoint: win_endpoint.clone(),
                    })
                    .collect();
                let alive = ipc::transport::connect(&endpoint)
                    .and_then(|mut conn| {
                        conn.send_request(&ipc::Request::RegisterWindows {
                            pid,
                            windows: windows.clone(),
                        })?;
                        conn.recv_response().map(drop)
                    })
                    .is_ok();
                if !alive {
                    match ipc::transport::Monarch::bind(&endpoint) {
                        Ok(monarch) => {
                            log::info!("monarch is gone — this window won the re-election");
                            let directory = Arc::new(FairMutex::new(WindowDirectory::default()));
                            directory.lock().register_all(pid, windows);
                            monarch_accept_loop(monarch, tx.clone(), directory);
                            // The listener died (extremely rare). Fall back
                            // to heartbeating — maybe another window binds.
                        }
                        // Lost the race — the next heartbeat registers with
                        // the winner.
                        Err(_) => {}
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        })
        .ok();
    let async_cx = cx.to_async();
    async_cx
        .spawn(async move |cx| {
            use futures::StreamExt as _;
            while let Some(msg) = rx.next().await {
                let _ = cx.update(|cx| pump_forwarded(cx, msg));
            }
        })
        .detach();
}

/// Serve this window's OWN socket (IPC.md direct tab-move routing): an
/// `attach` landing here means "adopt as a tab of THIS window" — the sender
/// already resolved the target through the monarch. Everything else is a
/// protocol error; window coordination stays with the monarch.
#[cfg(windows)]
fn window_accept_loop(
    listener: ipc::transport::Monarch,
    tx: futures::channel::mpsc::UnboundedSender<Forwarded>,
) {
    let active_workers = Arc::new(AtomicUsize::new(0));
    loop {
        let Ok(mut conn) = listener.accept() else {
            break;
        };
        let Some(permit) = acquire_ipc_worker(&active_workers) else {
            continue;
        };
        // One worker per connection — same reasoning as the monarch loop: a
        // silent client must not block the next tab move behind it.
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("rikka-win-conn".into())
            .spawn(move || {
                match {
                    let _permit = &permit;
                    conn.recv_request()
                } {
                    Ok(ipc::Request::PrepareAttach(a)) => {
                        let peer = conn.peer_pid();
                        let drop_at = a.drop_at;
                        let target = match a.target {
                            ipc::Target::Window(id) => Some(id),
                            _ => None,
                        };
                        let prepared = peer
                            .ok_or_else(|| anyhow::anyhow!("unattested tab-move peer"))
                            .and_then(|peer| attach::prepare_attach(&a, peer));
                        let pulled = match prepared {
                            Ok(pulled) => pulled,
                            Err(error) => {
                                let _ =
                                    conn.send_response(&ipc::Response::error(error.to_string()));
                                return;
                            }
                        };
                        let startup = pulled.startup.clone();
                        let palette = pulled.palette();
                        let (session, gate) = match pulled.into_gated_session() {
                            Ok(prepared) => prepared,
                            Err(error) => {
                                let _ =
                                    conn.send_response(&ipc::Response::error(error.to_string()));
                                return;
                            }
                        };
                        if conn.send_response(&ipc::Response::ok()).is_err() {
                            return;
                        }
                        if !matches!(conn.recv_request(), Ok(ipc::Request::CommitAttach)) {
                            let _ = conn.send_response(&ipc::Response::error(
                                "tab-move commit was not received",
                            ));
                            return;
                        }
                        if conn.send_response(&ipc::Response::ok()).is_err() {
                            return;
                        }
                        if !matches!(conn.recv_request(), Ok(ipc::Request::FinalizeAttach)) {
                            let _ = conn.send_response(&ipc::Response::error(
                                "tab-move finalize was not received",
                            ));
                            return;
                        }
                        gate.start();
                        let resp = if tx
                            .unbounded_send(Forwarded::AdoptTab(
                                Box::new(session),
                                startup,
                                drop_at,
                                palette,
                                target,
                            ))
                            .is_ok()
                        {
                            ipc::Response::ok()
                        } else {
                            ipc::Response::error("window is shutting down")
                        };
                        let _ = conn.send_response(&resp);
                    }
                    Ok(ipc::Request::Attach(a)) => {
                        // Same fail-closed peer gate as the monarch socket.
                        let resp = match conn.peer_pid() {
                            Some(peer) => match adopt_attach(&tx, a, peer) {
                                Ok(()) => ipc::Response::ok(),
                                Err(e) => ipc::Response::error(e.to_string()),
                            },
                            None => ipc::Response::error(
                                "attach refused: peer identity could not be verified",
                            ),
                        };
                        let _ = conn.send_response(&resp);
                    }
                    Ok(ipc::Request::Spawn(s)) => {
                        // A targeted spawn the monarch routed here (`rt -w`):
                        // the pump opens its tabs in the addressed window.
                        let resp = if tx.unbounded_send(Forwarded::SpawnInWindow(s)).is_ok() {
                            ipc::Response::ok()
                        } else {
                            ipc::Response::error("window is shutting down")
                        };
                        let _ = conn.send_response(&resp);
                    }
                    Ok(ipc::Request::Ping) => {
                        let _ = conn.send_response(&ipc::Response::ok());
                    }
                    Ok(_) => {
                        let _ = conn.send_response(&ipc::Response::error(
                            "window socket: attach, spawn and ping only",
                        ));
                    }
                    Err(_) => {}
                }
            })
            .ok();
    }
}

/// Adopt a direct-routed tab move on the IPC thread: pull the handles while
/// the sender still waits on our response, assemble the session, and hand it
/// to the gpui thread as a tab for this process's window.
#[cfg(windows)]
fn adopt_attach(
    tx: &futures::channel::mpsc::UnboundedSender<Forwarded>,
    args: ipc::AttachArgs,
    peer_pid: u32,
) -> Result<()> {
    let drop_at = args.drop_at;
    // The addressed window, when the sender named one (per-window ids; a
    // legacy pid-form target resolves to nothing here and falls through to
    // drop-point / any-window routing in the pump).
    let target = match args.target {
        ipc::Target::Window(id) => Some(id),
        _ => None,
    };
    let pulled = attach::pull_attach(&args, peer_pid)?;
    adopt_prepared_attach(tx, pulled, drop_at, target)
}

#[cfg(windows)]
fn adopt_prepared_attach(
    tx: &futures::channel::mpsc::UnboundedSender<Forwarded>,
    pulled: attach::LocalAttach,
    drop_at: Option<(i32, i32)>,
    target: Option<u64>,
) -> Result<()> {
    let startup = pulled.startup.clone();
    let palette = pulled.palette();
    let session = pulled.into_session()?;
    tx.unbounded_send(Forwarded::AdoptTab(
        Box::new(session),
        startup,
        drop_at,
        palette,
        target,
    ))
    .map_err(|_| anyhow::anyhow!("window is shutting down"))?;
    Ok(())
}

/// Bind this process's own window socket and serve it. Failure is degraded
/// operation, not fatal: the window still works, it just cannot RECEIVE
/// direct tab moves (and its registration advertises no endpoint).
#[cfg(windows)]
fn spawn_window_accept(cx: &mut App) -> Option<String> {
    let name = ipc::transport::window_endpoint_name(std::process::id());
    let listener = match ipc::transport::Monarch::bind(&name) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("window socket bind failed ({name}): {e}");
            return None;
        }
    };
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
    std::thread::Builder::new()
        .name("rikka-win-ipc".into())
        .spawn(move || window_accept_loop(listener, tx))
        .ok()?;
    let async_cx = cx.to_async();
    async_cx
        .spawn(async move |cx| {
            use futures::StreamExt as _;
            while let Some(msg) = rx.next().await {
                let _ = cx.update(|cx| pump_forwarded(cx, msg));
            }
        })
        .detach();
    Some(name)
}

/// Adopt a direct-routed session as a tab of this process's window. A window
/// process hosts exactly one window; if none is alive (shutdown race), a
/// fresh window still beats losing the session. Lookup and adopt run on the
/// same thread with no await between them, so the entry cannot fall through
/// the gap.
/// The tab window under a physical screen point — routes a drag-merge drop
/// to the window it actually landed on when this process hosts several
/// (in-process detach). gpui's global window bounds are logical; each
/// window's own scale factor converts them to the physical pixels the wire
/// carries.
#[cfg(windows)]
fn window_at_screen_point(
    cx: &mut App,
    (x, y): (i32, i32),
) -> Option<gpui::WeakEntity<TabsWindow>> {
    for (handle, weak) in hub::all_windows(cx) {
        let hit = cx
            .update_window(handle, |_, window, _| {
                let sf = window.scale_factor();
                let b = window.bounds();
                let bx = (b.origin.x / px(1.)) * sf;
                let by = (b.origin.y / px(1.)) * sf;
                let bw = (b.size.width / px(1.)) * sf;
                let bh = (b.size.height / px(1.)) * sf;
                (x as f32) >= bx && (x as f32) < bx + bw && (y as f32) >= by && (y as f32) < by + bh
            })
            .unwrap_or(false);
        if hit {
            return Some(weak);
        }
    }
    None
}

#[cfg(windows)]
fn adopt_forwarded(
    cx: &mut App,
    session: TerminalSession,
    startup: ipc::StartupInfo,
    drop_at: Option<(i32, i32)>,
    palette: Option<Vec<u32>>,
    target_id: Option<u64>,
) {
    // Routing, most specific first: the window the sender ADDRESSED
    // (per-window id), then the window under the drop point (in-process
    // detach can leave this process hosting several), then any live one.
    let target = target_id
        .and_then(|id| hub::window_by_id(cx, id))
        .or_else(|| drop_at.and_then(|pt| window_at_screen_point(cx, pt)))
        .or_else(|| hub::any_window(cx));
    match target {
        Some(view) => {
            let entry = hub::new_tab(cx, session);
            // The palette that rode the move: the tab keeps its profile
            // colors on this side instead of falling back to our default.
            entry.0.set_theme(wire_theme(palette));
            let _ = view.update(cx, |v, cx| v.adopt_dropped(entry, drop_at, cx));
        }
        None => open_attached(cx, session, startup, palette),
    }
}

/// Window for an adopted handoff session. Always its own new window, never an
/// auto-tab (IPC.md's windowing rule); the tab title was seeded from the
/// handoff's startup info when one was carried.
#[cfg(windows)]
fn open_attached(
    cx: &mut App,
    session: TerminalSession,
    startup: ipc::StartupInfo,
    palette: Option<Vec<u32>>,
) {
    let launch = cli::Launch {
        size_cells: (startup.cols >= 2 && startup.rows >= 2)
            .then_some((startup.cols, startup.rows)),
        ..Default::default()
    };
    let entry = hub::new_tab(cx, session);
    entry.0.set_theme(wire_theme(palette));
    open_tabs_window_opts(cx, vec![entry], &launch);
}

/// Open a targeted spawn's tabs in the addressed window of THIS process
/// (`rt -w <id>`, routed here by the monarch): re-parse the forwarded CLI,
/// resolve its relative dirs against the SENDER's cwd, and adopt each tab
/// into that window. A vanished target falls open to a fresh window — the
/// launch must never be lost.
fn spawn_in_window(cx: &mut App, s: ipc::SpawnArgs) {
    let Some(view) = s.window.and_then(|id| hub::window_by_id(cx, id)) else {
        return open_forwarded(cx, s.argv);
    };
    let launch = cli::parse(s.argv.clone()).unwrap_or_default();
    let mut specs = cli::expand_dir_tabs(launch.tabs);
    if specs.is_empty() {
        specs = vec![default_spec(cx)];
    }
    for spec in &mut specs {
        // Relative dirs are the sender's — anchor them to its cwd. (The
        // positional-dir detection in expand_dir_tabs tests OUR cwd; `.`
        // and absolute paths behave, deep relative ones may fall through
        // to running as a command — acceptable for v1.)
        if let (Some(dir), Some(cwd)) = (&spec.dir, &s.cwd) {
            let p = std::path::Path::new(dir);
            if p.is_relative() {
                spec.dir = Some(
                    std::path::Path::new(cwd)
                        .join(p)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let entries: Vec<TabEntry> = specs
        .iter()
        .filter_map(|sp| create_tab_spec(cx, sp))
        .collect();
    if entries.is_empty() {
        return;
    }
    let _ = view.update(cx, |v, cx| {
        for entry in entries {
            v.adopt(entry, cx);
        }
    });
}

/// Re-parse a forwarded CLI and open a window for it IN THIS process — the
/// fallback when the window-process spawn failed (isolation is best-effort;
/// a launch must never be lost).
fn open_forwarded(cx: &mut App, argv: Vec<String>) {
    let launch = cli::parse(argv).unwrap_or_default();
    let specs = cli::expand_dir_tabs(launch.tabs.clone());
    let specs = if specs.is_empty() {
        vec![default_spec(cx)]
    } else {
        specs
    };
    let initial: Vec<TabEntry> = specs
        .iter()
        .filter_map(|spec| create_tab_spec(cx, spec))
        .collect();
    if !initial.is_empty() {
        open_tabs_window_opts(cx, initial, &launch);
    }
}

fn main() {
    // Same field diagnosis as shogun-desktop: panics (any thread) append to
    // %TEMP%/shogun-tsf/panic.log.
    rikka_terminal_core::install_panic_log();
    rikka_terminal_core::install_file_logger("rikka-terminal");
    // wt-compatible CLI (see `cli`). Errors and --help go to a message box —
    // the GUI-subsystem binary has no console.
    let launch = match cli::parse(std::env::args().skip(1).collect()) {
        Ok(launch) => launch,
        Err(msg) => {
            cli::error_box(&msg);
            return;
        }
    };
    // Single-instance election: become the monarch, or forward this launch to
    // the running one and exit (the forwarded launch opens its window there).
    // A monarch-spawned window process skips all of it — the parent owns the
    // socket, and electing or forwarding from here would boomerang the launch.
    let endpoint = ipc::transport::endpoint_name();
    let role = if launch.window_process {
        Role::WindowProcess
    } else {
        match elect(&endpoint, &launch) {
            Some(role) => role,
            None => return,
        }
    };
    Application::new().run(move |cx| {
        // The engine's renderer rides gpui-component primitives; initialise
        // its statics and pin the dark theme (the grid brings its own colors).
        gpui_component::init(cx);
        gpui_component::theme::Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);
        // Bundled fonts (see assets/fonts/ and CREDITS). font-logos: distro
        // logos for tab icons. Twemoji Mozilla: the engine's terminal_font()
        // already names it as the emoji fallback for every grid run —
        // registering it here makes emoji resolve to the same embedded glyphs
        // on every OS instead of the platform emoji font (same setup as
        // shogun-desktop). Non-fatal on failure (a missing glyph is tofu, not
        // a crash).
        let _ = cx.text_system().add_fonts(vec![
            std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/font-logos.ttf").as_slice()),
            std::borrow::Cow::Borrowed(
                include_bytes!("../assets/fonts/Twemoji.Mozilla.ttf").as_slice(),
            ),
        ]);
        // New-tab profiles: wt's list filtered by rikka's config (read once
        // at startup; a broken/absent config or wt just yields an empty menu
        // and the built-in shell search).
        let (wt, wt_default) = wt_profiles::discover();
        let cfg = config::Config::load();
        apply_appearance(&cfg);
        apply_theme(&cfg);
        apply_identity(&cfg);
        session_log::init(cfg.logging.clone());
        keymap::init(&cfg.keys);
        let menu = cfg.build_menu(wt, wt_default);
        // Resolve each new-tab-menu profile's icon once (same resolver the tabs
        // use), so the dropdown shows the shell/distro icon beside each entry.
        let profile_icons: Vec<Option<tab_icon::TabIcon>> = menu
            .profiles
            .iter()
            .map(|p| {
                tab_icon::resolve(
                    p.argv.first().map(String::as_str).unwrap_or(""),
                    p.argv.get(1..).unwrap_or(&[]),
                    Some(&p.name),
                )
            })
            .collect();
        hub::init(cx, menu);
        cx.set_global(hub::ProfileIcons(profile_icons));
        spawn_config_watcher(cx);
        // A cold-start handoff rides in this launch (IPC.md "attach cold"):
        // adopt the inherited handles as the initial window. On failure fall
        // through to a normal launch — a visible window beats a silent death
        // for diagnosis (the file log has the why).
        if !open_inherited(cx, &launch) {
            // `rt <dir>` opens the default shell there (code-style; one tab
            // per directory).
            let specs = cli::expand_dir_tabs(launch.tabs.clone());
            let specs = if specs.is_empty() {
                vec![default_spec(cx)]
            } else {
                specs
            };
            let initial: Vec<TabEntry> = specs
                .iter()
                .filter_map(|spec| create_tab_spec(cx, spec))
                .collect();
            open_tabs_window_opts(cx, initial, &launch);
        }
        // Every window-hosting process serves its own socket for direct
        // tab-move routing (Windows; Unix moves await SCM_RIGHTS).
        #[cfg(windows)]
        let window_endpoint = spawn_window_accept(cx);
        #[cfg(not(windows))]
        let window_endpoint: Option<String> = None;
        // Every role runs the watcher: its immediate first beat registers
        // this process's windows (per-window ids), later beats keep the
        // directory fresh, and — for non-monarchs — a dead monarch triggers
        // the re-election. The monarch heartbeats its own socket, which is
        // simply how its windows enter its own directory.
        match role {
            // Monarch: additionally serve forwarded launches, handoffs and
            // registrations.
            Role::Monarch(monarch) => {
                spawn_ipc_accept(cx, monarch, window_endpoint.clone());
                spawn_monarch_watcher(cx, endpoint, window_endpoint);
            }
            Role::WindowProcess => spawn_monarch_watcher(cx, endpoint, window_endpoint),
            // Standalone lost the original election race entirely — the
            // watcher doubles as its path back into coordination.
            Role::Standalone => spawn_monarch_watcher(cx, endpoint, window_endpoint),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_insert_index_picks_the_nearest_gap() {
        // 5 tabs of 200px: left half of tab 0 → before it, right half →
        // after it, and so on down the strip.
        assert_eq!(strip_insert_index(-50.0, 200.0, 5), 0);
        assert_eq!(strip_insert_index(40.0, 200.0, 5), 0);
        assert_eq!(strip_insert_index(160.0, 200.0, 5), 1);
        assert_eq!(strip_insert_index(430.0, 200.0, 5), 2);
        assert_eq!(strip_insert_index(999.0, 200.0, 5), 5);
        assert_eq!(strip_insert_index(5000.0, 200.0, 5), 5);
        // Degenerate width appends rather than dividing by zero.
        assert_eq!(strip_insert_index(100.0, 0.0, 3), 3);
    }

    #[test]
    fn spawn_identity_defaults_honest_and_ghostty_masquerades() {
        // Default: honest identity, xterm-256color.
        let d = resolve_spawn_identity(&config::TerminalSection::default());
        assert_eq!(d.term, "xterm-256color");
        assert_eq!(d.term_program.0, "rikka-terminal");
        assert!(d.xtversion.starts_with("rikka-terminal "));

        // ghostty: XTVERSION + TERM_PROGRAM masquerade; term still overridable.
        let g = resolve_spawn_identity(&config::TerminalSection {
            term: Some("xterm-ghostty".into()),
            identity: Some("ghostty".into()),
            ..Default::default()
        });
        assert_eq!(g.term, "xterm-ghostty");
        assert_eq!(g.xtversion, "ghostty 1.3.1");
        assert_eq!(g.term_program, ("ghostty".into(), "1.3.1".into()));

        // Unknown identity falls back to honest.
        let u = resolve_spawn_identity(&config::TerminalSection {
            identity: Some("nope".into()),
            ..Default::default()
        });
        assert_eq!(u.term_program.0, "rikka-terminal");
    }

    #[test]
    fn window_directory_upserts_by_window_id_and_replaces_per_pid() {
        let reg = |pid: u32, id: u64| ipc::RegisterWindow {
            pid,
            window_id: id,
            endpoint: String::new(),
        };
        let mut dir = WindowDirectory::default();
        // Single-shot upsert keys on the WINDOW id: one pid, two windows.
        dir.register(reg(100, (100 << 20) | 0));
        dir.register(reg(100, (100 << 20) | 1));
        dir.register(reg(200, 200));
        dir.register(reg(100, (100 << 20) | 1)); // same id → replaced, no dupe
        let ids: Vec<u64> = dir.list().iter().map(|w| w.id).collect();
        assert_eq!(ids, vec![(100 << 20), (100 << 20) | 1, 200]);

        // The heartbeat form replaces the pid's whole set — closed windows
        // drop out; other pids stay.
        dir.register_all(100, vec![reg(100, (100 << 20) | 2)]);
        let ids: Vec<u64> = dir.list().iter().map(|w| w.id).collect();
        assert_eq!(ids, vec![200, (100 << 20) | 2]);
    }

    #[test]
    fn window_directory_resolves_endpoints() {
        let mut dir = WindowDirectory::default();
        dir.register(ipc::RegisterWindow {
            pid: 1,
            window_id: (1 << 20) | 3,
            endpoint: "ep-one".into(),
        });
        dir.register(ipc::RegisterWindow {
            pid: 2,
            window_id: 2 << 20,
            endpoint: String::new(),
        });
        // Exact per-window id…
        assert_eq!(dir.resolve((1 << 20) | 3).as_deref(), Some("ep-one"));
        // …and the bare-pid form (a drag-merge sender only knows the pid).
        assert_eq!(dir.resolve(1).as_deref(), Some("ep-one"));
        // `-w 0` / "last": any reachable window, as a CONCRETE target.
        assert_eq!(
            dir.resolve_target(0),
            Some(((1 << 20) | 3, "ep-one".into()))
        );
        assert_eq!(
            dir.resolve(2 << 20),
            None,
            "endpointless windows are unreachable"
        );
        assert_eq!(dir.resolve(9), None, "unknown ids resolve to nothing");
    }

    #[test]
    fn window_registration_is_bound_to_peer_pid_id_and_endpoint() {
        let pid = 4242;
        let valid = ipc::RegisterWindow {
            pid,
            window_id: (u64::from(pid) << 20) | 7,
            endpoint: ipc::transport::window_endpoint_name(pid),
        };
        assert!(validate_registration(pid, &valid).is_ok());

        let mut forged = valid.clone();
        forged.pid += 1;
        assert!(validate_registration(pid, &forged).is_err());
        let mut forged = valid.clone();
        forged.window_id = (u64::from(pid + 1) << 20) | 7;
        assert!(validate_registration(pid, &forged).is_err());
        let mut forged = valid;
        forged.endpoint = ipc::transport::window_endpoint_name(pid + 1);
        assert!(validate_registration(pid, &forged).is_err());
    }

    #[test]
    fn bulk_registration_cannot_replace_another_process() {
        let peer = 4242;
        let windows = vec![ipc::RegisterWindow {
            pid: peer,
            window_id: u64::from(peer) << 20,
            endpoint: ipc::transport::window_endpoint_name(peer),
        }];
        assert!(validate_registrations(peer, peer, &windows).is_ok());
        assert!(validate_registrations(peer, peer + 1, &windows).is_err());
    }

    /// A window's own socket adopts a direct-routed attach as a tab: the
    /// handles are pulled while the sender waits on the response, and the
    /// live session reaches the gpui pump as `AdoptTab` — never a new
    /// window. This is the receiving half of a cross-process tab move.
    #[cfg(windows)]
    #[test]
    fn window_socket_adopts_direct_attach() {
        use std::os::windows::io::IntoRawHandle as _;
        let name = format!("rikka-test-win-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).expect("bind window socket");
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let accept = std::thread::spawn(move || window_accept_loop(listener, tx));

        let (out_read, _out_write) = std::io::pipe().unwrap();
        let (_in_read, in_write) = std::io::pipe().unwrap();
        let mut conn = ipc::transport::connect(&name).expect("connect window socket");
        conn.send_request(&ipc::Request::Attach(ipc::AttachArgs {
            pid: std::process::id(),
            handles: ipc::Handles {
                input: in_write.into_raw_handle() as isize as i64,
                output: out_read.into_raw_handle() as isize as i64,
                ..Default::default()
            },
            startup: ipc::StartupInfo {
                title: Some("moved".into()),
                ..Default::default()
            },
            target: ipc::Target::Window(u64::from(std::process::id())),
            ..Default::default()
        }))
        .unwrap();
        let resp = conn.recv_response().unwrap();
        assert!(resp.ok, "adopt must succeed: {:?}", resp.error);

        match rx.try_recv().expect("one pumped message") {
            Forwarded::AdoptTab(session, startup, _, palette, target) => {
                assert_eq!(startup.title.as_deref(), Some("moved"));
                assert_eq!(session.title.lock().as_deref(), Some("moved"));
                // No palette was sent — none must arrive (old-sender compat).
                assert_eq!(palette, None);
                // Target::Window rode the wire into the routing slot.
                assert_eq!(target, Some(u64::from(std::process::id())));
            }
            _ => panic!("expected exactly one AdoptTab"),
        }
        drop(conn);
        drop(accept); // detach: the loop parks in accept() until process exit
    }

    /// Monarch re-election: when the monarch dies, a window-side bind wins
    /// the freed socket and `monarch_accept_loop` serves coordination again —
    /// a heartbeat registration repopulates the fresh directory and routing
    /// resolves through it.
    #[cfg(windows)]
    #[test]
    fn re_elected_monarch_serves_after_the_first_dies() {
        let name = format!("rikka-test-reelect-{}.sock", std::process::id());
        let first = ipc::transport::Monarch::bind(&name).expect("first monarch binds");
        drop(first); // the monarch window closes

        // The watcher's bind race: this side wins and starts serving.
        let winner = ipc::transport::Monarch::bind(&name).expect("re-bind after the monarch died");
        let (tx, _rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let directory = Arc::new(FairMutex::new(WindowDirectory::default()));
        let accept = {
            let directory = Arc::clone(&directory);
            std::thread::spawn(move || monarch_accept_loop(winner, tx, directory))
        };

        // A surviving window's next heartbeat = RegisterWindow upsert…
        let mut conn = ipc::transport::connect(&name).expect("connect the re-elected monarch");
        let pid = std::process::id();
        let endpoint = ipc::transport::window_endpoint_name(pid);
        conn.send_request(&ipc::Request::RegisterWindow(ipc::RegisterWindow {
            pid,
            window_id: u64::from(pid),
            endpoint: endpoint.clone(),
        }))
        .unwrap();
        assert!(conn.recv_response().unwrap().ok);

        // …and routing works again through the fresh directory.
        let mut conn = ipc::transport::connect(&name).unwrap();
        conn.send_request(&ipc::Request::ResolveWindow {
            window: u64::from(pid),
        })
        .unwrap();
        let resp = conn.recv_response().unwrap();
        assert!(resp.ok);
        assert_eq!(resp.endpoint.as_deref(), Some(endpoint.as_str()));
        drop(accept); // detach: the loop parks in accept() until process exit
    }

    /// The `rt -w` last hop: a window socket accepts a targeted Spawn and
    /// hands it to the pump as SpawnInWindow, concrete window id intact.
    #[cfg(windows)]
    #[test]
    fn window_socket_accepts_targeted_spawns() {
        let name = format!("rikka-test-wspawn-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).expect("bind window socket");
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let accept = std::thread::spawn(move || window_accept_loop(listener, tx));

        let mut conn = ipc::transport::connect(&name).expect("connect window socket");
        conn.send_request(&ipc::Request::Spawn(ipc::SpawnArgs {
            argv: vec!["new-tab".into(), "-t".into(), "targeted".into()],
            window: Some(77),
            ..Default::default()
        }))
        .unwrap();
        assert!(conn.recv_response().unwrap().ok);

        match rx.try_recv().expect("one pumped message") {
            Forwarded::SpawnInWindow(s) => {
                assert_eq!(s.window, Some(77));
                assert_eq!(s.argv[2], "targeted");
            }
            _ => panic!("expected SpawnInWindow"),
        }
        drop(accept); // detach: the loop parks in accept() until process exit
    }

    /// A client that connects but never sends a frame must not stall the
    /// socket: each connection gets its own worker, so the next client is
    /// served while the silent one sits there.
    #[cfg(windows)]
    #[test]
    fn silent_connection_does_not_block_the_window_socket() {
        let name = format!("rikka-test-silent-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).expect("bind window socket");
        let (tx, _rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let accept = std::thread::spawn(move || window_accept_loop(listener, tx));

        let _silent = ipc::transport::connect(&name).expect("silent client connects");

        // The ping runs on a helper thread so a regression fails by
        // timeout instead of hanging the test run.
        let name2 = name.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut conn = ipc::transport::connect(&name2).expect("second client connects");
            conn.send_request(&ipc::Request::Ping).unwrap();
            done_tx.send(conn.recv_response().unwrap().ok).ok();
        });
        let ok = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("second client starved behind the silent one");
        assert!(ok, "ping must succeed");
        drop(accept);
    }

    /// The SENDING half against the receiving half, one process, real
    /// socket: a LIVE session (reader pumping) is quiesced, its birth-time
    /// duplicates pushed over the window socket, pulled back
    /// (`DUPLICATE_CLOSE_SOURCE` against ourselves) and assembled — and
    /// output written to the PTY *after* the move lands in the receiver's
    /// grid, still flowing after the sender's remains drop. This is the
    /// whole cross-process tab move, minus the second process.
    #[cfg(windows)]
    #[test]
    fn live_tab_moves_across_the_window_socket() {
        use std::io::Write as _;
        use std::os::windows::io::OwnedHandle;

        use rikka_terminal_core::pty_handoff::{HandoffPty, build_handoff_session};

        let (out_read, mut out_write) = std::io::pipe().unwrap();
        let (_in_read, in_write) = std::io::pipe().unwrap();
        let source = build_handoff_session(
            80,
            24,
            HandoffPty {
                input: OwnedHandle::from(in_write),
                output: OwnedHandle::from(out_read),
                signal: None,
                keepalive: Vec::new(),
            },
            "test-identity",
        )
        .expect("live source session");
        *source.title.lock() = Some("mover".into());

        // Screen content the move must carry: parsed into the SENDER's grid
        // (and gone from the pipe) before the transfer starts.
        out_write.write_all(b"BEFORE-MOVE").unwrap();
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let row0: String = source.snapshot.lock().cells[0]
                    .iter()
                    .map(|c| c.c)
                    .collect();
                if row0.starts_with("BEFORE-MOVE") {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "content never reached the sender's grid"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        let name = format!("rikka-test-move-{}.sock", std::process::id());
        let listener = ipc::transport::Monarch::bind(&name).expect("bind window socket");
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Forwarded>();
        let accept = std::thread::spawn(move || window_accept_loop(listener, tx));

        // An image in the sender's store plus its on-screen placeholder
        // cells: both must survive the move.
        out_write
            .write_all(&rikka_terminal_core::sixel::placeholder_bytes(9, 2, 1, 0))
            .unwrap();
        {
            // The snapshot blanks placeholder glyphs and carries the decoded
            // placement on `SnapshotCell.image` instead — poll that.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let seen = source
                    .snapshot
                    .lock()
                    .cells
                    .iter()
                    .any(|row| row.iter().any(|c| c.image.is_some()));
                if seen {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "placeholders never reached the sender's grid"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        #[rustfmt::skip]
        let rgba: Vec<u8> = vec![
            255, 0, 0, 255,   0, 255, 0, 255,
            0, 0, 255, 255,   255, 255, 255, 255,
        ];
        assert!(source.images.insert_rgba(9, 2, 2, rgba, 2, 1));

        // A distinctive palette rides the move (bg/fg/sel + 16 ANSI).
        let sent_palette: Vec<u32> = (0..19).map(|i| 0x100000 + i).collect();
        tab_move::send_tab(
            &source,
            Some(sent_palette.clone()),
            tab_move::Destination::Window {
                id: 1,
                endpoint: name,
                drop_at: Some((123, 45)),
            },
        )
        .expect("send the live tab");

        let received = match rx.try_recv().expect("one pumped message") {
            Forwarded::AdoptTab(session, startup, drop_at, palette, _) => {
                assert_eq!(startup.title.as_deref(), Some("mover"));
                // The drag-drop point survives the wire — the receiver
                // inserts at the strip position under it.
                assert_eq!(drop_at, Some((123, 45)));
                // The tab's colors survive too (v1 restriction lifted).
                assert_eq!(palette, Some(sent_palette));
                session
            }
            _ => panic!("expected exactly one AdoptTab"),
        };

        let sees = |needle: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let grid: String = received
                    .snapshot
                    .lock()
                    .cells
                    .iter()
                    .flat_map(|row| row.iter().map(|c| c.c))
                    .collect();
                if grid.contains(needle) {
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "{needle:?} never reached the receiver's grid"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        // The sender's screen arrived with the move (the state replay ran
        // as the receiver's parser preface)…
        sees("BEFORE-MOVE");
        // …the image store crossed with it (id, pixels re-encoded, placement
        // size), and the replayed grid kept the placeholder cells…
        {
            let img = received
                .images
                .get(9)
                .expect("image store must survive the move");
            assert_eq!((img.cols, img.rows), (2, 1));
            let size = img.image.size(0);
            assert_eq!((size.width.0, size.height.0), (2, 2));
            let has_placement = received
                .snapshot
                .lock()
                .cells
                .iter()
                .any(|row| row.iter().any(|c| c.image.is_some_and(|p| p.id == 9)));
            assert!(
                has_placement,
                "replayed placeholder cells must decode to the carried image"
            );
        }
        // …and the live pipe keeps flowing after it.
        out_write.write_all(b"AFTER-MOVE").unwrap();
        sees("AFTER-MOVE");

        // The sender's teardown must not break the moved pipes: its handles
        // are independent duplicates of what the receiver pulled.
        drop(source);
        out_write.write_all(b" STILL-FLOWING").unwrap();
        sees("STILL-FLOWING");
        drop(accept);
    }
}
