//! Shared scrollback-search bar: state, key handling and the overlay box,
//! single-sourced so every host window (rikka's tabs, shogun-desktop's shell
//! and terminal panes) searches and looks the same.
//!
//! The host owns a [`SearchBar`] per window, forwards keys to [`SearchBar::key`]
//! while it is open (the bar swallows the keyboard), and places
//! [`SearchBar::render`]'s box in an absolute overlay wherever it fits. The
//! search itself — compiled query, stepping, the gold match highlight — lives
//! on the [`TerminalSession`] (see `search_set` / `search_step`), so the bar
//! is pure UI state.

use gpui::{IntoElement as _, ParentElement as _, Styled as _, div, px, rgb, rgba};

use crate::TerminalSession;

/// Per-window search-bar state. `Default` = closed.
#[derive(Default)]
pub struct SearchBar {
    pub open: bool,
    /// The query as typed.
    pub query: String,
    /// Whether the last step landed on a match (`false` renders the query
    /// red — unknown pattern or an uncompilable half-typed regex).
    pub hit: bool,
}

impl SearchBar {
    /// Open the bar (re-arming a previous query on `session`), or close it.
    pub fn toggle(&mut self, session: Option<&TerminalSession>) {
        if self.open {
            self.close(session);
            return;
        }
        self.open = true;
        self.hit = true;
        if !self.query.is_empty()
            && let Some(s) = session
        {
            s.search_set(&self.query);
            self.hit = s.search_step(1);
        }
    }

    /// Close the bar and drop the session's search highlight.
    pub fn close(&mut self, session: Option<&TerminalSession>) {
        self.open = false;
        if let Some(s) = session {
            s.search_close();
        }
    }

    /// Incremental search: recompile and land on the first match from the
    /// viewport top. An uncompilable half-typed regex keeps the previous
    /// highlight and just flags the query red.
    fn changed(&mut self, session: Option<&TerminalSession>) {
        let Some(s) = session else {
            return;
        };
        if self.query.is_empty() {
            s.search_set("");
            self.hit = true;
        } else if s.search_set(&self.query) {
            self.hit = s.search_step(1);
        } else {
            self.hit = false;
        }
    }

    /// Handle a keystroke while the bar is open; returns whether it was
    /// consumed (always, while open — the bar swallows the keyboard).
    /// `close_chord` = the host's own search chord matched (toggles closed).
    pub fn key(
        &mut self,
        ks: &gpui::Keystroke,
        close_chord: bool,
        session: Option<&TerminalSession>,
    ) -> bool {
        if !self.open {
            return false;
        }
        if close_chord || ks.key == "escape" {
            self.close(session);
            return true;
        }
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "enter" => {
                if let Some(s) = session {
                    self.hit = s.search_step(if m.shift { -1 } else { 1 });
                }
            }
            "backspace" => {
                self.query.pop();
                self.changed(session);
            }
            _ => {
                if !m.control
                    && !m.alt
                    && !m.platform
                    && let Some(text) = ks
                        .key_char
                        .as_ref()
                        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
                {
                    self.query.push_str(text);
                    self.changed(session);
                }
            }
        }
        true
    }

    /// The bar's box (browser-style), when open. The host wraps it in an
    /// absolute overlay wherever it belongs (typically top-right of the
    /// pane).
    pub fn render(&self) -> Option<gpui::AnyElement> {
        if !self.open {
            return None;
        }
        let q = if self.query.is_empty() {
            "…".to_string()
        } else {
            self.query.clone()
        };
        Some(
            div()
                .bg(rgb(0x202020))
                .border_1()
                .border_color(rgba(0xFFFFFF15))
                .rounded(px(6.))
                .px(px(10.))
                .py(px(6.))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .child(div().text_color(rgba(0xFFFFFFC5)).child("検索"))
                .child(
                    div()
                        .text_color(if self.hit {
                            rgb(0xFFFFFF)
                        } else {
                            rgb(0xE81123)
                        })
                        .child(q),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgba(0xFFFFFFC5))
                        .child("Enter 次 / Shift+Enter 前 / Esc 閉"),
                )
                .into_any_element(),
        )
    }
}
