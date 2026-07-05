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

use crate::terminal::{GridSnapshot, MouseReport, ReportButton, ReportMods, TerminalSession};
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

/// A mouse-driven cell selection over one terminal pane's grid.
///
/// `anchor` is the cell where the drag started; `head` follows the pointer.
/// Both are `(row, col)` grid coordinates. The selection is linear (reading
/// order): rows strictly between anchor and head are selected full-width.
struct TerminalSelection {
    /// Which pane of the host window owns the selection (windows with a
    /// single pane always use 0).
    pane: usize,
    anchor: (usize, usize),
    head: (usize, usize),
    dragging: bool,
}

impl TerminalSelection {
    /// Inclusive `(start, end)` cell range in reading order.
    fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        if self.head < self.anchor {
            (self.head, self.anchor)
        } else {
            (self.anchor, self.head)
        }
    }
}

/// Drag state owned by a host window. All mutation goes through
/// [`register_mouse_selection`]'s listeners.
#[derive(Default)]
pub struct SelectionState {
    sel: Option<TerminalSelection>,
    /// In-flight reported (forwarded-to-PTY) drag, if any.
    report: Option<ReportDrag>,
    /// Last hover cell reported under `?1003` (all-motion), for dedup.
    hover_cell: Option<(usize, usize)>,
    /// OSC 8 hyperlink currently under a ctrl-hover: `(pane, link index)`.
    /// The renderer underlines that link's cells; ctrl-click opens it.
    hover_link: Option<(usize, u16)>,
}

impl SelectionState {
    fn begin(&mut self, pane: usize, row: usize, col: usize) {
        self.sel = Some(TerminalSelection {
            pane,
            anchor: (row, col),
            head: (row, col),
            dragging: true,
        });
    }

    /// Move the selection head while dragging. Returns `true` when it moved.
    fn update_head(&mut self, row: usize, col: usize) -> bool {
        match self.sel.as_mut() {
            Some(sel) if sel.dragging && sel.head != (row, col) => {
                sel.head = (row, col);
                true
            }
            _ => false,
        }
    }

    /// Finish a drag. A click without movement clears the selection.
    /// Returns `true` when the visual state changed.
    fn end_drag(&mut self) -> bool {
        match self.sel.as_mut() {
            Some(sel) if sel.dragging => {
                sel.dragging = false;
                if sel.anchor == sel.head {
                    self.sel = None;
                }
                true
            }
            _ => false,
        }
    }

    /// The owning pane and normalized range of the current selection.
    pub fn selected(&self) -> Option<(usize, (usize, usize), (usize, usize))> {
        let sel = self.sel.as_ref()?;
        let (start, end) = sel.normalized();
        Some((sel.pane, start, end))
    }

    /// The normalized range when the selection belongs to `pane` — the shape
    /// `render_grid` takes for the highlight.
    pub fn range_for(&self, pane: usize) -> Option<((usize, usize), (usize, usize))> {
        let (sel_pane, start, end) = self.selected()?;
        (sel_pane == pane).then_some((start, end))
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
    let idx = snap.cells.get(row)?.get(col)?.link?;
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
                    this.selection_state().begin(pane, row, col);
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
                    && this.selection_state().update_head(row, col)
                {
                    cx.notify();
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
                if ev.button == MouseButton::Left && this.selection_state().end_drag() {
                    cx.notify();
                }
            });
        }
    });
}

/// Copy the current selection to the OS clipboard. The host resolves which
/// session the selection's pane maps to.
pub fn copy_to_clipboard(state: &SelectionState, session: Option<&TerminalSession>, cx: &mut App) {
    let Some((_pane, start, end)) = state.selected() else {
        return;
    };
    let Some(session) = session else { return };
    let text = selection_text(&session.snapshot.lock(), start, end);
    if !text.is_empty() {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// Extract the text covered by an inclusive `(start, end)` cell range.
///
/// Rows strictly between start and end are taken full-width. Trailing spaces
/// are trimmed per line. Wide-char spacer cells (`display_width == 0`) are
/// skipped so double-width characters appear exactly once.
fn selection_text(snap: &GridSnapshot, start: (usize, usize), end: (usize, usize)) -> String {
    let mut lines = Vec::new();
    for (row, cells) in snap.cells.iter().enumerate() {
        if row < start.0 || row > end.0 {
            continue;
        }
        let mut c0 = if row == start.0 { start.1 } else { 0 };
        // Selection anchored on the spacer half of a wide glyph (CJK/emoji)
        // must still copy the glyph — pull back to its base cell, matching
        // the highlight snap in renderer::selection_cols_for_row.
        while c0 > 0 && cells.get(c0).is_some_and(|c| c.display_width == 0) {
            c0 -= 1;
        }
        let c1 = if row == end.0 { end.1 + 1 } else { cells.len() };
        let mut line = String::new();
        for (col, cell) in cells.iter().enumerate() {
            if col < c0 || col >= c1 || cell.display_width == 0 {
                continue;
            }
            line.push(cell.c);
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::SnapshotCell;

    fn snap_from_lines(lines: &[&str]) -> GridSnapshot {
        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let mut snap = GridSnapshot::blank(cols, lines.len());
        for (row, line) in lines.iter().enumerate() {
            for (col, c) in line.chars().enumerate() {
                snap.cells[row][col].c = c;
            }
        }
        snap
    }

    #[test]
    fn selection_text_single_row_range() {
        let snap = snap_from_lines(&["hello world"]);
        assert_eq!(selection_text(&snap, (0, 6), (0, 10)), "world");
    }

    #[test]
    fn selection_text_multi_row_takes_middle_rows_full_width() {
        let snap = snap_from_lines(&["abcde", "fghij", "klmno"]);
        assert_eq!(selection_text(&snap, (0, 3), (2, 1)), "de\nfghij\nkl");
    }

    #[test]
    fn selection_text_trims_trailing_spaces_per_line() {
        let snap = snap_from_lines(&["ab   ", "cd   "]);
        assert_eq!(selection_text(&snap, (0, 0), (1, 4)), "ab\ncd");
    }

    #[test]
    fn selection_text_skips_wide_char_spacers() {
        let mut snap = GridSnapshot::blank(4, 1);
        snap.cells[0][0] = SnapshotCell {
            c: 'あ',
            display_width: 2,
            ..SnapshotCell::blank()
        };
        snap.cells[0][1] = SnapshotCell {
            c: ' ',
            display_width: 0,
            ..SnapshotCell::blank()
        };
        snap.cells[0][2].c = 'x';
        assert_eq!(selection_text(&snap, (0, 0), (0, 2)), "あx");
    }

    #[test]
    fn selection_text_start_on_wide_spacer_includes_the_glyph() {
        let mut snap = GridSnapshot::blank(4, 1);
        snap.cells[0][0] = SnapshotCell {
            c: 'あ',
            display_width: 2,
            ..SnapshotCell::blank()
        };
        snap.cells[0][1] = SnapshotCell {
            c: ' ',
            display_width: 0,
            ..SnapshotCell::blank()
        };
        snap.cells[0][2].c = 'x';
        // Anchor on the spacer half (col 1): the wide glyph is still copied.
        assert_eq!(selection_text(&snap, (0, 1), (0, 2)), "あx");
    }

    #[test]
    fn selection_normalized_orders_reading_direction() {
        let sel = TerminalSelection {
            pane: 0,
            anchor: (5, 2),
            head: (3, 7),
            dragging: true,
        };
        assert_eq!(sel.normalized(), ((3, 7), (5, 2)));
    }

    #[test]
    fn selection_state_drag_lifecycle() {
        let mut state = SelectionState::default();
        state.begin(1, 2, 3);
        assert!(state.update_head(4, 5));
        assert!(!state.update_head(4, 5)); // unchanged head → no repaint
        assert!(state.end_drag());
        assert_eq!(state.selected(), Some((1, (2, 3), (4, 5))));
        assert_eq!(state.range_for(1), Some(((2, 3), (4, 5))));
        assert_eq!(state.range_for(0), None);
    }

    #[test]
    fn selection_state_click_without_drag_clears() {
        let mut state = SelectionState::default();
        state.begin(0, 2, 3);
        assert!(state.end_drag());
        assert_eq!(state.selected(), None);
    }

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
