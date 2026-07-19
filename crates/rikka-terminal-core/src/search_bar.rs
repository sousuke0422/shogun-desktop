//! Shared scrollback-search bar: state, key handling and the overlay box,
//! single-sourced so every host window (rikka's tabs, shogun-desktop's shell
//! and terminal panes) searches and looks the same.
//!
//! The host owns a [`SearchBar`] per window, forwards keys to [`SearchBar::key`]
//! while it is open (the bar swallows the keyboard), and places
//! [`SearchBar::render`]'s box in an absolute overlay wherever it fits. The
//! search itself — compiled query, stepping, the gold match highlights, the
//! match counter — lives on the [`TerminalSession`] (see `search_set` /
//! `search_step` / `search_status`), so the bar is pure UI state.
//!
//! The box is VSCode/wt-shaped: an input field (block caret, red border when
//! nothing matches), an `Aa` case toggle, the `3/12` counter and prev / next
//! / close buttons. Buttons need host listeners (the host entity owns the
//! state), so [`SearchBar::render`] takes a [`SearchHandlers`] of boxed
//! click callbacks the host builds with `cx.listener`.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb, rgba,
};

use crate::{SearchStatus, TerminalSession};

/// Boxed host click-listeners for the bar's buttons, built with
/// `cx.listener(...)` so they can mutate the host entity.
pub struct SearchHandlers {
    pub prev: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    pub next: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    pub close: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    pub case: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    pub regex: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
}

/// The bar's color sheet (`0xRRGGBB` each), injected by the host so the
/// widget can match the app's look — rikka keeps the VSCode-flavored
/// default; shogun-desktop hands in its own shikkoku/zouge sheet instead of
/// having the shared widget break its visual language.
#[derive(Clone, Copy)]
pub struct SearchColors {
    /// Bar box fill.
    pub bg: u32,
    /// Bar box border.
    pub border: u32,
    /// Input field fill.
    pub input_bg: u32,
    /// Input field border (its resting accent — VSCode paints this its
    /// focus blue; a host can pick something quieter).
    pub input_border: u32,
    /// Lit toggles.
    pub accent: u32,
    /// Query text; dimmed variants derive from it by alpha.
    pub text: u32,
    /// No-match / bad-regex signal (input border, counter).
    pub error: u32,
}

impl Default for SearchColors {
    /// The VSCode-flavored sheet the bar shipped with.
    fn default() -> Self {
        Self {
            bg: 0x252526,
            border: 0x454545,
            input_bg: 0x313131,
            input_border: 0x007FD4,
            accent: 0x007FD4,
            text: 0xEDEDED,
            error: 0xF14C4C,
        }
    }
}

/// `0xRRGGBB` + alpha → gpui rgba.
fn a(rgb: u32, alpha: u8) -> gpui::Rgba {
    rgba((rgb << 8) | alpha as u32)
}

/// Per-window search-bar state. `Default` = closed.
#[derive(Default)]
pub struct SearchBar {
    pub open: bool,
    /// The query as typed.
    pub query: String,
    /// Whether the last step landed on a match (`false` renders the input
    /// border red — no match or an uncompilable half-typed regex).
    pub hit: bool,
    /// `Aa` toggle: force case-sensitive matching. Off = alacritty's
    /// smart-case (an all-lowercase query matches case-insensitively).
    pub case_sensitive: bool,
    /// `.*` toggle: treat the query as a regex. Off (default) = literal
    /// search, VSCode-style — the query is meta-escaped before compiling.
    pub regex: bool,
}

/// Escape regex metacharacters (the `regex_syntax::escape` set — every
/// `\c` in it is a legal literal escape) for the literal-search mode.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl SearchBar {
    /// The pattern actually handed to the engine: the query — meta-escaped
    /// unless `.*` is on — forced case-sensitive via an inline flag when
    /// `Aa` is on (inline flags win over the smart-case default the engine
    /// compiles with).
    fn effective_pattern(&self) -> String {
        let base = if self.regex {
            self.query.clone()
        } else {
            regex_escape(&self.query)
        };
        if self.case_sensitive {
            format!("(?-i){base}")
        } else {
            base
        }
    }

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
            s.search_set(&self.effective_pattern());
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

    /// Step to the next (`dir >= 0`) / previous match — the ↑ ↓ buttons and
    /// Enter / Shift+Enter.
    pub fn nav(&mut self, dir: i32, session: Option<&TerminalSession>) {
        if let Some(s) = session {
            self.hit = s.search_step(dir);
        }
    }

    /// Flip the `Aa` toggle and re-run the search under the new casing.
    pub fn toggle_case(&mut self, session: Option<&TerminalSession>) {
        self.case_sensitive = !self.case_sensitive;
        self.changed(session);
    }

    /// Flip the `.*` toggle and re-run the search under the new mode.
    pub fn toggle_regex(&mut self, session: Option<&TerminalSession>) {
        self.regex = !self.regex;
        self.changed(session);
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
        } else if s.search_set(&self.effective_pattern()) {
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
        cx: &mut App,
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
            "enter" => self.nav(if m.shift { -1 } else { 1 }, session),
            "backspace" => {
                self.query.pop();
                self.changed(session);
            }
            // Ctrl+V pastes into the query (single line: newlines → spaces).
            "v" if m.control && !m.alt => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let clean: String = text
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();
                    self.query.push_str(clean.trim());
                    self.changed(session);
                }
            }
            // Alt+C / Alt+R flip the Aa / .* toggles (VSCode's chords).
            "c" if m.alt && !m.control => self.toggle_case(session),
            "r" if m.alt && !m.control => self.toggle_regex(session),
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

    /// A 22×22 hover-highlighted icon button.
    fn button(
        id: &'static str,
        label: &'static str,
        c: &SearchColors,
        on: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    ) -> impl gpui::IntoElement {
        let (text, hover_bg) = (a(c.text, 0xB8), a(c.text, 0x16));
        div()
            .id(id)
            .w(px(22.))
            .h(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .text_size(px(12.))
            .text_color(text)
            .hover(move |s| s.bg(hover_bg))
            .cursor_pointer()
            .on_click(on)
            .child(label)
    }

    /// A 24×22 toggle button (`Aa` / `.*`): accent-lit when on.
    fn toggle_button(
        id: &'static str,
        label: &'static str,
        on_state: bool,
        c: &SearchColors,
        on: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    ) -> impl gpui::IntoElement {
        let hover_bg = a(c.text, 0x16);
        div()
            .id(id)
            .w(px(24.))
            .h(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .text_size(px(11.))
            .when(on_state, |d| {
                d.bg(a(c.accent, 0x52))
                    .border_1()
                    .border_color(rgb(c.accent))
                    .text_color(rgb(c.text))
            })
            .when(!on_state, |d| d.text_color(a(c.text, 0xA0)))
            .hover(move |s| s.bg(hover_bg))
            .cursor_pointer()
            .on_click(on)
            .child(label)
    }

    /// The bar's box (VSCode/wt-style), when open. The host wraps it in an
    /// absolute overlay wherever it belongs (typically top-right of the
    /// pane). `status` comes from `TerminalSession::search_status`.
    pub fn render(
        &self,
        status: Option<SearchStatus>,
        h: SearchHandlers,
        c: &SearchColors,
    ) -> Option<gpui::AnyElement> {
        if !self.open {
            return None;
        }
        let empty = self.query.is_empty();
        let error = !self.hit && !empty;
        // Counter: "3/12", "?/999+" while stale, "結果なし" on zero.
        let counter = match status {
            Some(st) if !empty => {
                if st.total == 0 {
                    "結果なし".to_string()
                } else {
                    let total = if st.truncated {
                        format!("{}+", st.total)
                    } else {
                        st.total.to_string()
                    };
                    let index = if st.index == 0 {
                        "?".to_string()
                    } else {
                        st.index.to_string()
                    };
                    format!("{index}/{total}")
                }
            }
            _ => String::new(),
        };
        let case_on = self.case_sensitive;
        let regex_on = self.regex;
        Some(
            div()
                .bg(rgb(c.bg))
                .border_1()
                .border_color(rgb(c.border))
                .rounded(px(6.))
                .shadow_lg()
                .px(px(6.))
                .py(px(4.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .text_size(px(13.))
                // Input field: query + block caret; red border = no match.
                .child(
                    div()
                        .bg(rgb(c.input_bg))
                        .border_1()
                        .border_color(if error {
                            rgb(c.error)
                        } else {
                            rgb(c.input_border)
                        })
                        .rounded(px(3.))
                        .px(px(7.))
                        .py(px(2.))
                        .min_w(px(170.))
                        .max_w(px(340.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(if empty {
                            div()
                                .text_color(a(c.text, 0x60))
                                .child("検索")
                                .into_any_element()
                        } else {
                            div()
                                .text_color(rgb(c.text))
                                .overflow_hidden()
                                .child(self.query.clone())
                                .into_any_element()
                        })
                        .child(
                            // Static caret at the end of the query.
                            div().w(px(1.5)).h(px(15.)).ml(px(1.)).bg(a(c.text, 0xC8)),
                        ),
                )
                // Aa case / .* regex toggles (Alt+C / Alt+R).
                .child(Self::toggle_button("search-case", "Aa", case_on, c, h.case))
                .child(Self::toggle_button(
                    "search-regex",
                    ".*",
                    regex_on,
                    c,
                    h.regex,
                ))
                // Match counter.
                .child(
                    div()
                        .min_w(px(52.))
                        .px(px(2.))
                        .text_size(px(11.5))
                        .text_color(if error { rgb(c.error) } else { a(c.text, 0xA0) })
                        .child(counter),
                )
                .child(Self::button("search-prev", "↑", c, h.prev))
                .child(Self::button("search-next", "↓", c, h.next))
                .child(Self::button("search-close", "✕", c, h.close))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::regex_escape;
    use alacritty_terminal::term::search::RegexSearch;

    /// Every metacharacter the literal mode escapes must stay a legal
    /// escape for the engine's regex dialect — an escape it rejects would
    /// turn literal search into a permanent red-border compile error.
    #[test]
    fn escaped_metacharacters_compile() {
        let raw = r"a.b+c*d?e(f)g|h[i]j{k}l^m$n#o&p-q~r\s";
        let escaped = regex_escape(raw);
        assert!(RegexSearch::new(&escaped).is_ok(), "pattern: {escaped}");
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(regex_escape("needle 検索"), "needle 検索");
    }

    /// The escaped form must not stay a functioning metacharacter: "1.5"
    /// literal-escaped must not match "1x5".
    #[test]
    fn dot_loses_its_magic() {
        assert_eq!(regex_escape("1.5"), r"1\.5");
    }
}
