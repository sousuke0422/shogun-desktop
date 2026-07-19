//! Shared mouse-selection state, hit-testing and clipboard copy for terminal
//! windows.
//!
//! The pieces live in different layers but are all defined here so every
//! terminal window behaves identically:
//! - [`SelectionState`] — anchor/head drag state, owned by the host window.
//! - [`register_mouse_selection`] — window-level mouse listeners registered
//!   from the pane's overlay canvas each paint (pointer → grid cell).
//! - [`copy_to_clipboard`] / [`selection_text`] — extraction of the selected
//!   rows for ctrl-shift-c / cmd-c.
//!
//! The highlight itself is painted by `render_grid` (paint calls from the
//! overlay canvas never reach the screen — see `render_terminal_tab`).

use crate::{MouseReport, ReportButton, ReportMods, TerminalSession};
use gpui::{
    App, Bounds, ClipboardItem, DispatchPhase, Entity, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Window,
};

/// What a window must provide for the shared mouse-selection listeners.
pub trait SelectionHost: 'static {
    fn selection_state(&mut self) -> &mut SelectionState;

    /// The terminal session behind `pane`, consulted for mouse reporting
    /// (clicks/drags forwarded to apps that asked for them — btop, claude
    /// code's clickable menus, tmux `mouse on`). `None` = plain selection.
    fn pane_session(&self, pane: usize) -> Option<&TerminalSession>;

    /// Multi-pane hosts clear the *other* panes' grid selections when a new
    /// drag starts in `keep` — the selection data lives in each pane's Term,
    /// and only one selection per window may stay visible.
    fn clear_selections_except(&self, _keep: usize) {}
}

/// Buttons that participate in reporting. Right is deliberately absent: it
/// owns the local context menu, like Windows Terminal.
fn report_button(button: MouseButton) -> Option<ReportButton> {
    match button {
        MouseButton::Left => Some(ReportButton::Left),
        MouseButton::Middle => Some(ReportButton::Middle),
        _ => None,
    }
}

fn report_mods(mods: &Modifiers) -> ReportMods {
    ReportMods {
        alt: mods.alt,
        ctrl: mods.control,
    }
}

/// A press that was forwarded to the PTY. Drag motion and the release pair
/// with it even if the app toggles reporting modes mid-drag, so the app
/// never sees an unbalanced press/release.
#[derive(Clone, Copy)]
struct ReportDrag {
    pane: usize,
    button: ReportButton,
    mods: ReportMods,
    last_cell: (usize, usize),
}

/// Drag bookkeeping owned by a host window. The selection *data* lives in the
/// pane's `Term` (alacritty `Selection`, written through the session's
/// `selection_*` helpers), which keeps the highlight glued to the text
/// through scrollback scrolling and output rotation — this only tracks the
/// in-flight drag and which pane owns the visible selection. All mutation
/// goes through [`register_mouse_selection`]'s listeners.
/// An in-flight local drag. The anchor is kept in VIEWPORT coordinates so
/// each update can re-pin the whole selection while streaming output rotates
/// the grid underneath (see `TerminalSession::selection_drag`).
#[derive(Clone, Copy)]
struct LocalDrag {
    pane: usize,
    /// Whether the pointer ever moved — a motionless click clears the
    /// selection on release instead of leaving a one-cell highlight behind.
    moved: bool,
    /// The mouse-down cell and side.
    anchor: (usize, usize, bool),
}

#[derive(Default)]
pub struct SelectionState {
    /// In-flight local drag, if any.
    drag: Option<LocalDrag>,
    /// Last cell+side the drag head was set to, for per-cell dedup.
    last_drag_cell: Option<(usize, usize, bool)>,
    /// Pane whose Term holds the current selection (drag or completed) — the
    /// copy path routes through it.
    owner: Option<usize>,
    /// In-flight reported (forwarded-to-PTY) drag, if any.
    report: Option<ReportDrag>,
    /// Last hover cell reported under `?1003` (all-motion), for dedup.
    hover_cell: Option<(usize, usize)>,
    /// OSC 8 hyperlink currently under a ctrl-hover: `(pane, link index)`.
    /// The renderer underlines that link's cells; ctrl-click opens it.
    hover_link: Option<(usize, u16)>,
}

impl SelectionState {
    /// Pane owning the current selection, if any (for copy routing).
    pub fn selected_pane(&self) -> Option<usize> {
        self.owner
    }

    /// The ctrl-hovered hyperlink index when it belongs to `pane` — the shape
    /// `render_grid` takes for the hover underline.
    pub fn hover_link_for(&self, pane: usize) -> Option<u16> {
        let (link_pane, idx) = self.hover_link?;
        (link_pane == pane).then_some(idx)
    }
}

/// Schemes ctrl-click will hand to the OS. Anything else in an OSC 8 URI
/// (`vscode:`, custom app handlers, …) is ignored — a terminal escape written
/// by whatever program runs in the shell must not launch arbitrary handlers.
fn openable_link(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// The hyperlink under `(row, col)` of `pane`, if any: `(index, uri)`.
fn link_at<V: SelectionHost>(
    this: &V,
    pane: usize,
    row: usize,
    col: usize,
) -> Option<(u16, String)> {
    let session = this.pane_session(pane)?;
    let snap = session.snapshot.lock();
    let cells = snap.cells.get(row)?;
    // Pointer on the spacer half of a wide glyph: the link lives on the base
    // cell to its left (OSC 8 spacers carry no link of their own).
    let mut col = col;
    while col > 0
        && cells
            .get(col)
            .is_some_and(|c| c.display_width == 0 && c.link.is_none())
    {
        col -= 1;
    }
    let idx = cells.get(col)?.link?;
    let uri = snap.links.get(idx as usize)?.clone();
    Some((idx, uri))
}

/// Register the three window-level mouse listeners for one terminal pane.
///
/// Call from the pane's overlay-canvas paint closure each frame (listeners
/// are cleared per frame). `bounds` is the overlay's PAINT bounds, which
/// already include the scroll container's offset — gpui applies
/// `with_element_offset(scroll_offset)` to every child during prepaint,
/// absolute ones included. Do NOT subtract the scroll offset again at event
/// time: that double-counts it, and on a pane whose offset moves while
/// agents stream output, the reported/selected row drifts vertically during
/// a purely horizontal drag (家老陣 tmux bug, 2026-07-04). Cells are clamped
/// to the grid, so dragging past the edges selects the border cells.
pub fn register_mouse_selection<V: SelectionHost>(
    window: &mut Window,
    view: Entity<V>,
    bounds: Bounds<Pixels>,
    pane: usize,
    cw: f32,
    ch: f32,
    grid_rows: usize,
    grid_cols: usize,
) {
    let cell_at = move |pos: Point<Pixels>| {
        let gx = f32::from(pos.x - bounds.origin.x);
        let gy = f32::from(pos.y - bounds.origin.y);
        let col = ((gx / cw).floor().max(0.0) as usize).min(grid_cols.saturating_sub(1));
        let row = ((gy / ch).floor().max(0.0) as usize).min(grid_rows.saturating_sub(1));
        (row, col)
    };
    // Which half of the cell the pointer sits in — alacritty's selection
    // anchors to a cell side, so a drag that starts in the right half doesn't
    // swallow the whole start cell.
    let side_at = move |pos: Point<Pixels>| {
        let gx = f32::from(pos.x - bounds.origin.x);
        (gx / cw).fract() > 0.5
    };
    window.on_mouse_event({
        let view = view.clone();
        move |ev: &MouseDownEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble || !bounds.contains(&ev.position) {
                return;
            }
            let (row, col) = cell_at(ev.position);
            view.update(cx, |this, cx| {
                // Ctrl-click on an OSC 8 hyperlink opens it (wt/VSCode
                // convention — a plain click stays with mouse reporting and
                // selection). Takes precedence over reporting: the ctrl-hover
                // underline promised this exact click would open the link.
                if ev.button == MouseButton::Left
                    && ev.modifiers.control
                    && !ev.modifiers.shift
                    && let Some((_, uri)) = link_at(this, pane, row, col)
                {
                    if openable_link(&uri) {
                        cx.open_url(&uri);
                    }
                    return;
                }
                // Mouse reporting first: apps that asked (btop, claude code
                // menus, tmux `mouse on`) get the click. Shift is the
                // universal bypass — hold it to select locally regardless.
                if !ev.modifiers.shift
                    && let Some(btn) = report_button(ev.button)
                {
                    let mods = report_mods(&ev.modifiers);
                    let forwarded = this
                        .pane_session(pane)
                        .is_some_and(|s| s.mouse_to_pty(MouseReport::Press(btn), mods, col, row));
                    if forwarded {
                        this.selection_state().report = Some(ReportDrag {
                            pane,
                            button: btn,
                            mods,
                            last_cell: (row, col),
                        });
                        return;
                    }
                }
                if ev.button == MouseButton::Left {
                    // One visible selection per window: evict the other
                    // panes', then anchor a fresh one in the grid itself.
                    this.clear_selections_except(pane);
                    let right = side_at(ev.position);
                    if let Some(s) = this.pane_session(pane) {
                        s.selection_begin(row, col, right);
                    }
                    let state = this.selection_state();
                    state.drag = Some(LocalDrag {
                        pane,
                        moved: false,
                        anchor: (row, col, right),
                    });
                    state.last_drag_cell = Some((row, col, right));
                    state.owner = Some(pane);
                    cx.notify();
                }
            });
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |ev: &MouseMoveEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let (row, col) = cell_at(ev.position);
            view.update(cx, |this, cx| {
                // Ctrl-hover hyperlink tracking, before any early return so
                // the underline clears when ctrl lifts or the pointer moves
                // off the link. Only this pane's listener may set the state;
                // clearing is allowed only for a link it owns, otherwise the
                // other pane's listener (same window event) wipes it.
                let hovered = (bounds.contains(&ev.position)
                    && ev.modifiers.control
                    && ev.pressed_button.is_none())
                .then(|| link_at(this, pane, row, col).map(|(idx, _)| (pane, idx)))
                .flatten();
                let current = this.selection_state().hover_link;
                if current != hovered
                    && (hovered.is_some() || current.is_some_and(|(p, _)| p == pane))
                {
                    this.selection_state().hover_link = hovered;
                    cx.notify();
                }
                // Reported drag in flight: forward motion per cell change.
                if let Some(drag) = this.selection_state().report
                    && drag.pane == pane
                {
                    if drag.last_cell != (row, col) {
                        if let Some(s) = this.pane_session(pane) {
                            s.mouse_to_pty(
                                MouseReport::Motion(Some(drag.button)),
                                drag.mods,
                                col,
                                row,
                            );
                        }
                        if let Some(drag) = this.selection_state().report.as_mut() {
                            drag.last_cell = (row, col);
                        }
                    }
                    return;
                }
                // Hover motion (only reported when the app set ?1003).
                if ev.pressed_button.is_none() && bounds.contains(&ev.position) {
                    if this.selection_state().hover_cell != Some((row, col)) {
                        if let Some(s) = this.pane_session(pane) {
                            s.mouse_to_pty(
                                MouseReport::Motion(None),
                                report_mods(&ev.modifiers),
                                col,
                                row,
                            );
                        }
                        this.selection_state().hover_cell = Some((row, col));
                    }
                    return;
                }
                if ev.pressed_button == Some(MouseButton::Left)
                    && let Some(drag) = this.selection_state().drag
                    && drag.pane == pane
                {
                    let right = side_at(ev.position);
                    if this.selection_state().last_drag_cell != Some((row, col, right)) {
                        if let Some(s) = this.pane_session(pane) {
                            // Screen-anchored: re-pins the whole selection at
                            // the live tail so streaming redraw loops cannot
                            // slide it off the pointer.
                            s.selection_drag(drag.anchor, (row, col, right));
                        }
                        let state = this.selection_state();
                        state.last_drag_cell = Some((row, col, right));
                        if let Some(d) = state.drag.as_mut() {
                            d.moved = true;
                        }
                        cx.notify();
                    }
                }
            });
        }
    });
    window.on_mouse_event({
        move |ev: &MouseUpEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let (row, col) = cell_at(ev.position);
            view.update(cx, |this, cx| {
                // Pair the release with a forwarded press, even if the app
                // dropped reporting mid-drag (mouse_to_pty then no-ops).
                if let Some(drag) = this.selection_state().report
                    && drag.pane == pane
                {
                    if report_button(ev.button) == Some(drag.button) {
                        if let Some(s) = this.pane_session(pane) {
                            s.mouse_to_pty(MouseReport::Release(drag.button), drag.mods, col, row);
                        }
                        this.selection_state().report = None;
                    }
                    return;
                }
                if ev.button == MouseButton::Left
                    && let Some(drag) = this.selection_state().drag
                {
                    this.selection_state().drag = None;
                    if !drag.moved {
                        // A motionless click clears rather than leaving a
                        // one-cell highlight behind.
                        if let Some(s) = this.pane_session(drag.pane) {
                            s.selection_clear();
                        }
                        this.selection_state().owner = None;
                    }
                    cx.notify();
                }
            });
        }
    });
}

/// Copy the current selection to the OS clipboard. The host resolves which
/// session the selection's pane maps to (see [`SelectionState::selected_pane`]).
/// Extraction is done by the grid itself, so it spans scrollback and handles
/// wide/wrapped lines correctly.
pub fn copy_to_clipboard(state: &SelectionState, session: Option<&TerminalSession>, cx: &mut App) {
    if state.selected_pane().is_none() {
        return;
    }
    let Some(session) = session else { return };
    if let Some(text) = session.selection_text()
        && !text.is_empty()
    {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Range math, drag rotation and text extraction moved into alacritty's
    // `Selection` (see `TerminalSession::selection_*` and the
    // `selection_tracks_scrollback_and_output` test in lib.rs); only the
    // pane-scoped bookkeeping is left to test here.

    #[test]
    fn hover_link_is_pane_scoped() {
        let mut state = SelectionState::default();
        assert_eq!(state.hover_link_for(0), None);
        state.hover_link = Some((1, 7));
        assert_eq!(state.hover_link_for(1), Some(7));
        assert_eq!(state.hover_link_for(0), None);
    }

    #[test]
    fn openable_link_allows_web_and_mail_only() {
        assert!(openable_link("https://example.com/x"));
        assert!(openable_link("HTTP://EXAMPLE.COM"));
        assert!(openable_link("mailto:lord@example.com"));
        // Custom app handlers / local files from a terminal escape must not
        // launch anything.
        assert!(!openable_link("vscode://file/etc/passwd"));
        assert!(!openable_link("file:///etc/passwd"));
        assert!(!openable_link("javascript:alert(1)"));
        assert!(!openable_link("ftp://example.com"));
    }
}
