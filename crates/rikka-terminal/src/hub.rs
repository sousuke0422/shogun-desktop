//! Window-independent session bookkeeping — the anti-tear-out-crash design.
//!
//! Windows Terminal detaches tabs by migrating live UI controls across
//! window threads (XAML islands + COM marshalling), and racing output or
//! resizes against that handoff is exactly where it crashes. Here a session
//! is UI-free by construction (`TerminalSession` is a bundle of Arcs; the
//! renderer is stateless per frame), so a "tab" is nothing but an
//! `Arc<TabSession>` sitting in some window's Vec:
//!
//! - Detach/merge = moving that Arc between Vecs, synchronously, on the UI
//!   thread. The PTY/parse threads never learn a move happened.
//! - Each session has one driver task (spawned once, app-scoped, never
//!   re-homed) that parks on the session's notify and calls a swappable
//!   `waker`. Adopting a tab just swaps the waker; a beat missed during the
//!   swap self-heals on the next PTY output.
//! - Shutdown is a flag + notify: the driver exits, the Arcs drop with the
//!   last window that held them, and dropping the session closes the PTY.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp, Global, WeakEntity};
use parking_lot::Mutex;
use rikka_terminal_core::TerminalSession;

use crate::{FRAME_COALESCE, TabsWindow};

/// Redraw hook: scheduled by the driver task (on the foreground executor,
/// which hands it an `AsyncApp`), installed by whichever window hosts the
/// tab right now.
pub type Waker = Box<dyn Fn(&mut AsyncApp)>;

pub struct TabSession {
    pub session: TerminalSession,
    pub waker: Mutex<Option<Waker>>,
    /// This tab's color palette (from its profile's scheme), installed into
    /// the engine when the tab becomes active. `None` = follow the global
    /// `[theme]` (or the built-in default). Interior-mutable so the tab
    /// factory can attach it without widening `new_tab`'s signature.
    theme: Mutex<Option<rikka_terminal_core::theme::Palette>>,
    /// This tab's shell icon (extracted exe icon or a distro glyph). Attached
    /// by the tab factory the same way as `theme`; `None` = no icon. Like the
    /// palette it does not travel a cross-process tab move (v1).
    icon: Mutex<Option<crate::tab_icon::TabIcon>>,
    closed: Arc<AtomicBool>,
    /// Individually opted into broadcast input (選択ブロードキャスト): this
    /// pane receives a copy of typed input no matter which pane is focused.
    /// Rides the session, so it survives pane/tab rearrangement.
    broadcast_target: AtomicBool,
}

impl TabSession {
    /// Stop the driver task and let the PTY close when the last Arc drops.
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.session.notify.notify_waiters();
    }

    /// Attach this tab's palette (its profile's color scheme).
    pub fn set_theme(&self, palette: Option<rikka_terminal_core::theme::Palette>) {
        *self.theme.lock() = palette;
    }

    /// This tab's palette, if it carries one.
    pub fn theme(&self) -> Option<rikka_terminal_core::theme::Palette> {
        self.theme.lock().clone()
    }

    /// Attach this tab's shell icon (resolved from how the shell was launched).
    pub fn set_icon(&self, icon: Option<crate::tab_icon::TabIcon>) {
        *self.icon.lock() = icon;
    }

    /// This tab's shell icon, if it has one.
    pub fn icon(&self) -> Option<crate::tab_icon::TabIcon> {
        self.icon.lock().clone()
    }

    /// Whether this pane is individually marked as a broadcast recipient.
    pub fn broadcast_target(&self) -> bool {
        self.broadcast_target.load(Ordering::Relaxed)
    }

    /// Flip the individual broadcast mark.
    pub fn toggle_broadcast_target(&self) {
        self.broadcast_target.fetch_xor(true, Ordering::Relaxed);
    }
}

/// One entry of a window's tab strip.
#[derive(Clone)]
pub struct TabEntry(pub Arc<TabSession>);

/// Wrap a fresh session into a tab and spawn its (sole,永住) driver task.
pub fn new_tab(cx: &mut App, session: TerminalSession) -> TabEntry {
    // Every tab passes through here (local spawns, handoffs, adopted
    // moves) — the one spot to apply the configured scrollback.
    if let Some(lines) = crate::configured_scrollback() {
        session.set_scrollback(lines);
    }
    // Same funnel logic for `[logging] auto_start` — a moved tab resumes
    // recording into a fresh file on the receiving side.
    crate::session_log::auto_start(&session);
    let closed = Arc::new(AtomicBool::new(false));
    let generation = Arc::clone(&session.generation);
    let notify = Arc::clone(&session.notify);
    let snapshot = Arc::clone(&session.snapshot);
    let tab = Arc::new(TabSession {
        session,
        waker: Mutex::new(None),
        theme: Mutex::new(None),
        icon: Mutex::new(None),
        closed: Arc::clone(&closed),
        broadcast_target: AtomicBool::new(false),
    });
    let waker_slot = Arc::clone(&tab);
    cx.spawn(async move |cx| {
        let mut last = generation.load(Ordering::Relaxed);
        loop {
            if closed.load(Ordering::Relaxed) {
                break;
            }
            let blink = {
                let s = snapshot.lock();
                s.has_blink || s.cursor_blink
            };
            if blink {
                let timer = cx.background_executor().timer(Duration::from_millis(300));
                futures::future::select(Box::pin(notify.notified()), Box::pin(timer)).await;
            } else {
                notify.notified().await;
            }
            if closed.load(Ordering::Relaxed) {
                break;
            }
            cx.background_executor().timer(FRAME_COALESCE).await;
            let cur = generation.load(Ordering::Relaxed);
            if cur == last && !blink {
                continue;
            }
            last = cur;
            // Redraw whichever window hosts this tab right now. Holding the
            // lock across the call is fine: installs happen on this same
            // thread between polls.
            if let Some(waker) = waker_slot.waker.lock().as_ref() {
                waker(cx);
            }
        }
    })
    .detach();
    TabEntry(tab)
}

/// Registry of live tab windows, for merge-all, cleanup, and per-window
/// addressing. Windows register at creation with a process-unique id; dead
/// weak handles are pruned on access (which also refreshes the id snapshot
/// the monarch-heartbeat thread reads).
#[derive(Default)]
pub struct WindowRegistry {
    pub windows: Vec<(u64, AnyWindowHandle, WeakEntity<TabsWindow>)>,
}

impl Global for WindowRegistry {}

/// Live window ids, mirrored out of the registry for threads without an App
/// (the monarch-heartbeat watcher registers these with the directory).
static LIVE_IDS: parking_lot::Mutex<Vec<u64>> = parking_lot::Mutex::new(Vec::new());

/// Snapshot of this process's live window ids (registration order).
pub fn live_window_ids() -> Vec<u64> {
    LIVE_IDS.lock().clone()
}

/// Allocate a process-unique window id: the pid in the high bits, a
/// process-local sequence in the low 20 (ownership stays recoverable, and a
/// bare-pid directory query can never collide with another process's ids in
/// practice).
pub fn alloc_window_id() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    (u64::from(std::process::id()) << 20)
        | (SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xF_FFFF)
}

/// Prune dead windows and refresh the heartbeat id mirror.
fn prune(reg: &mut WindowRegistry) {
    reg.windows.retain(|(_, _, w)| w.upgrade().is_some());
    *LIVE_IDS.lock() = reg.windows.iter().map(|(id, _, _)| *id).collect();
}

/// The new-tab profile menu (wt profiles filtered by rikka's config),
/// app-global so every window — including ones spun off by detach — shares
/// the same list and default.
pub struct ProfileMenu(pub crate::config::Menu);

impl Global for ProfileMenu {}

/// Icons for the new-tab dropdown, aligned by index with `ProfileMenu`'s
/// profiles. Resolved once at startup (exe-icon extraction isn't free, and the
/// list is static) so the dropdown render is a cheap lookup, not a re-resolve
/// on every hover repaint.
pub struct ProfileIcons(pub Vec<Option<crate::tab_icon::TabIcon>>);

impl Global for ProfileIcons {}

pub fn init(cx: &mut App, menu: crate::config::Menu) {
    cx.set_global(WindowRegistry::default());
    cx.set_global(ProfileMenu(menu));
}

pub fn register_window(
    cx: &mut App,
    id: u64,
    handle: AnyWindowHandle,
    entity: WeakEntity<TabsWindow>,
) {
    let reg = cx.global_mut::<WindowRegistry>();
    reg.windows.push((id, handle, entity));
    prune(reg);
}

/// Live tab windows left after pruning the dead. Used by the release
/// observer to quit once the last window is gone (the releasing window's
/// weak is already dead inside its own release callback).
pub fn live_windows(cx: &mut App) -> usize {
    let reg = cx.global_mut::<WindowRegistry>();
    prune(reg);
    reg.windows.len()
}

/// Any live window (the first registered still alive), pruning the dead —
/// the adoption fallback when nothing more specific matches.
pub fn any_window(cx: &mut App) -> Option<WeakEntity<TabsWindow>> {
    let reg = cx.global_mut::<WindowRegistry>();
    prune(reg);
    reg.windows.first().map(|(_, _, w)| w.clone())
}

/// The live window registered under `id` — the per-window addressing hit.
pub fn window_by_id(cx: &mut App, id: u64) -> Option<WeakEntity<TabsWindow>> {
    let reg = cx.global_mut::<WindowRegistry>();
    prune(reg);
    reg.windows
        .iter()
        .find(|(wid, _, _)| *wid == id)
        .map(|(_, _, w)| w.clone())
}

/// Every live tab window with its OS handle, pruning the dead — the
/// drop-point router walks these to find the window under a drag-merge.
pub fn all_windows(cx: &mut App) -> Vec<(AnyWindowHandle, WeakEntity<TabsWindow>)> {
    let reg = cx.global_mut::<WindowRegistry>();
    prune(reg);
    reg.windows
        .iter()
        .map(|(_, h, w)| (*h, w.clone()))
        .collect()
}

/// Every live window except `except`, pruning the dead.
pub fn other_windows(
    cx: &mut App,
    except: gpui::EntityId,
) -> Vec<(AnyWindowHandle, WeakEntity<TabsWindow>)> {
    let reg = cx.global_mut::<WindowRegistry>();
    prune(reg);
    reg.windows
        .iter()
        .filter(|(_, _, w)| w.entity_id() != except)
        .map(|(_, h, w)| (*h, w.clone()))
        .collect()
}
