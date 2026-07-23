//! The settings window (設定UI): a small companion window that edits the
//! main knobs of config.toml. Saving WRITES THE FILE ONLY — application
//! happens through the config hot-reload watcher, so UI edits and hand
//! edits share one code path and can never fight. `toml_edit` keeps the
//! user's comments and layout intact, and only the fields the user actually
//! changed are written (a value cleared back to empty removes its key).
//!
//! Layout, wt-style: a nav rail of pages on the left, the selected page on
//! the right (scrollable, for when the OS clamps the window absurdly
//! small), and a pinned footer (status + save) that stays reachable
//! regardless.

use gpui::AppContext as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions,
    div, px, rgb, rgba, size,
};
use rikka_terminal_core::search_bar::{SearchColors, sheet};

use crate::config;

/// Open the settings window, or bring the existing one to front.
pub fn open(cx: &mut App) {
    for handle in cx.windows() {
        if let Some(h) = handle.downcast::<SettingsWindow>() {
            let _ = h.update(cx, |_, window, _| window.activate_window());
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(580.), px(480.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("設定 — RikkaTerminal".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| SettingsWindow::new(window, cx)),
    )
    .ok();
}

/// The nav rail's pages, wt-style groupings.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    Theme,
    Terminal,
    Logging,
}

/// The text-editable fields (click to focus, type/backspace/Ctrl+V).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Font,
    Term,
    LogDir,
    ThemeBg,
    ThemeFg,
}

/// The values as loaded, for only-write-what-changed saves.
#[derive(Clone, PartialEq)]
struct Values {
    font: String,
    font_size: f32,
    line_height: f32,
    search_vscode: bool,
    acrylic: bool,
    scrollback: u32,
    term: String,
    identity_ghostty: bool,
    wt_scheme: String,
    theme_bg: String,
    theme_fg: String,
    log_dir: String,
    log_input: bool,
    auto_start: bool,
}

pub struct SettingsWindow {
    v: Values,
    initial: Values,
    page: Page,
    focused: Option<Field>,
    /// `(message, is_error)` — errors render rust red, successes muted.
    status: Option<(String, bool)>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The wt-scheme picker overlay (list from [`crate::wt_schemes::catalog`],
    /// loaded ONCE at window open — never per frame).
    scheme_menu: bool,
    schemes: Vec<(String, rikka_terminal_core::theme::Palette)>,
    scheme_scroll: ScrollHandle,
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cfg = config::Config::load();
        let v = Values {
            font: cfg.appearance.font.clone().unwrap_or_default(),
            font_size: cfg.appearance.font_size.unwrap_or(13.0),
            line_height: cfg.appearance.line_height.unwrap_or(1.2),
            search_vscode: cfg.appearance.search_style.as_deref() == Some("vscode"),
            acrylic: cfg.appearance.acrylic.unwrap_or(false),
            scrollback: cfg.terminal.scrollback.unwrap_or(10_000),
            term: cfg
                .terminal
                .term
                .clone()
                .unwrap_or_else(|| "xterm-256color".into()),
            identity_ghostty: cfg.terminal.identity.as_deref() == Some("ghostty"),
            wt_scheme: cfg.theme.wt_scheme.clone().unwrap_or_default(),
            theme_bg: cfg.theme.background.clone().unwrap_or_default(),
            theme_fg: cfg.theme.foreground.clone().unwrap_or_default(),
            log_dir: cfg.logging.directory.clone().unwrap_or_default(),
            log_input: cfg.logging.log_input.unwrap_or(false),
            auto_start: cfg.logging.auto_start.unwrap_or(false),
        };
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            initial: v.clone(),
            v,
            page: Page::Appearance,
            focused: None,
            status: None,
            focus,
            scroll: ScrollHandle::default(),
            scheme_menu: false,
            schemes: crate::wt_schemes::catalog(rikka_terminal_core::theme::DEFAULT),
            scheme_scroll: ScrollHandle::default(),
        }
    }

    /// Route typing into the focused text field (search-bar rules:
    /// printable + backspace + Ctrl+V paste; Esc/Enter drop focus).
    fn handle_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.focused else {
            return false;
        };
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        let buf = match field {
            Field::Font => &mut self.v.font,
            Field::Term => &mut self.v.term,
            Field::LogDir => &mut self.v.log_dir,
            Field::ThemeBg => &mut self.v.theme_bg,
            Field::ThemeFg => &mut self.v.theme_fg,
        };
        match ks.key.as_str() {
            "escape" | "enter" => self.focused = None,
            "backspace" => {
                buf.pop();
            }
            "v" if m.control && !m.alt => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let clean: String = text
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();
                    buf.push_str(clean.trim());
                }
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
                    buf.push_str(text);
                } else {
                    return false;
                }
            }
        }
        self.status = None;
        cx.notify();
        true
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = config::config_path() else {
            self.status = Some(("設定パスを解決できませんでした".into(), true));
            return;
        };
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        match apply_edits(&raw, &self.edits()) {
            Ok(out) => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::write(&path, out) {
                    Ok(()) => {
                        self.initial = self.v.clone();
                        self.status =
                            Some(("保存しました — 実行中の窓に自動反映されます".into(), false));
                    }
                    Err(e) => self.status = Some((format!("書き込み失敗: {e}"), true)),
                }
            }
            Err(e) => {
                self.status = Some((
                    format!("config.toml を解析できません（手修正が必要）: {e}"),
                    true,
                ))
            }
        }
        cx.notify();
    }

    /// The `(section, key, value)` writes this save needs: only fields that
    /// differ from what the window loaded. `None` removes the key (a text
    /// value cleared back to empty).
    fn edits(&self) -> Vec<(&'static str, &'static str, Option<toml_edit::Value>)> {
        use toml_edit::Value;
        let mut out: Vec<(&'static str, &'static str, Option<Value>)> = Vec::new();
        let (v, i) = (&self.v, &self.initial);
        let text = |s: &str| {
            if s.is_empty() {
                None
            } else {
                Some(Value::from(s.to_string()))
            }
        };
        if v.font != i.font {
            out.push(("appearance", "font", text(&v.font)));
        }
        if v.font_size != i.font_size {
            out.push((
                "appearance",
                "font_size",
                Some(Value::from(f64::from(v.font_size))),
            ));
        }
        if v.line_height != i.line_height {
            out.push((
                "appearance",
                "line_height",
                Some(Value::from(f64::from(v.line_height))),
            ));
        }
        if v.search_vscode != i.search_vscode {
            let s = if v.search_vscode { "vscode" } else { "winui" };
            out.push(("appearance", "search_style", Some(Value::from(s))));
        }
        if v.acrylic != i.acrylic {
            out.push(("appearance", "acrylic", Some(Value::from(v.acrylic))));
        }
        if v.scrollback != i.scrollback {
            out.push((
                "terminal",
                "scrollback",
                Some(Value::from(i64::from(v.scrollback))),
            ));
        }
        if v.term != i.term {
            out.push(("terminal", "term", text(&v.term)));
        }
        if v.identity_ghostty != i.identity_ghostty {
            let s = if v.identity_ghostty {
                "ghostty"
            } else {
                "honest"
            };
            out.push(("terminal", "identity", Some(Value::from(s))));
        }
        if v.wt_scheme != i.wt_scheme {
            out.push(("theme", "wt_scheme", text(&v.wt_scheme)));
        }
        if v.theme_bg != i.theme_bg {
            out.push(("theme", "background", text(&v.theme_bg)));
        }
        if v.theme_fg != i.theme_fg {
            out.push(("theme", "foreground", text(&v.theme_fg)));
        }
        if v.log_dir != i.log_dir {
            out.push(("logging", "directory", text(&v.log_dir)));
        }
        if v.log_input != i.log_input {
            out.push(("logging", "log_input", Some(Value::from(v.log_input))));
        }
        if v.auto_start != i.auto_start {
            out.push(("logging", "auto_start", Some(Value::from(v.auto_start))));
        }
        out
    }

    // ── UI pieces (search-bar sheet for a consistent look) ──────────────

    /// A wt-style nav rail entry: accent bar + label, lit when selected.
    fn nav_item(
        &self,
        page: Page,
        label: &'static str,
        seq: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = sheet();
        let selected = self.page == page;
        div()
            .id(("sf-nav", seq))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(4.))
            .text_size(px(13.))
            .when(selected, |d| d.bg(rgb(c.input_bg)).text_color(rgb(c.text)))
            .when(!selected, |d| {
                d.text_color(rgba((c.text << 8) | 0xA0))
                    .hover(|t| t.bg(rgba(0xFFFFFF10)))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                cx.stop_propagation();
                this.page = page;
                this.focused = None;
                this.scheme_menu = false;
                cx.notify();
            }))
            .child(
                div()
                    .w(px(3.))
                    .h(px(14.))
                    .rounded(px(2.))
                    .when(selected, |d| d.bg(rgb(c.accent))),
            )
            .child(label)
    }

    fn row(label: &'static str, control: impl IntoElement, c: &SearchColors) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .w(px(120.))
                    .flex_shrink_0()
                    .text_size(px(13.))
                    .text_color(rgba((c.text << 8) | 0xC0))
                    .child(label),
            )
            .child(control)
    }

    fn text_field(&self, field: Field, seq: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let c = sheet();
        let focused = self.focused == Some(field);
        let value = match field {
            Field::Font => &self.v.font,
            Field::Term => &self.v.term,
            Field::LogDir => &self.v.log_dir,
            Field::ThemeBg => &self.v.theme_bg,
            Field::ThemeFg => &self.v.theme_fg,
        };
        div()
            .id(("sf-text", seq))
            .flex_1()
            .min_w_0()
            .px(px(8.))
            .py(px(4.))
            .bg(rgb(c.input_bg))
            .border_1()
            .border_color(if focused {
                rgb(c.accent)
            } else {
                rgb(c.input_border)
            })
            .rounded(px(4.))
            .text_size(px(13.))
            .flex()
            .flex_row()
            .items_center()
            .child(if value.is_empty() && !focused {
                div()
                    .text_color(rgba((c.text << 8) | 0x50))
                    .child("(既定)")
                    .into_any_element()
            } else {
                div()
                    .text_color(rgb(c.text))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(value.clone())
                    .into_any_element()
            })
            .when(focused, |d| {
                d.child(
                    div()
                        .w(px(1.5))
                        .h(px(14.))
                        .ml(px(1.))
                        .bg(rgba((c.text << 8) | 0xC8)),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                cx.stop_propagation();
                this.focused = Some(field);
                cx.notify();
            }))
    }

    /// A `#RRGGBB` text field with a live color swatch beside it.
    fn color_field(&self, field: Field, seq: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let c = sheet();
        let value = match field {
            Field::ThemeBg => &self.v.theme_bg,
            Field::ThemeFg => &self.v.theme_fg,
            _ => unreachable!(),
        };
        let parsed = parse_hex(value);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .child(self.text_field(field, seq, cx))
            .child(
                div()
                    .w(px(18.))
                    .h(px(18.))
                    .flex_shrink_0()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(rgb(c.input_border))
                    .when_some(parsed, |d, hex| d.bg(rgb(hex))),
            )
    }

    /// The wt-scheme picker button: current selection + ▾, opening the
    /// overlay list.
    fn scheme_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = sheet();
        let empty = self.v.wt_scheme.is_empty();
        let label = if empty {
            "(なし)".to_string()
        } else {
            self.v.wt_scheme.clone()
        };
        div()
            .id("sf-scheme-btn")
            .flex_1()
            .min_w_0()
            .px(px(8.))
            .py(px(4.))
            .bg(rgb(c.input_bg))
            .border_1()
            .border_color(rgb(c.input_border))
            .rounded(px(4.))
            .text_size(px(13.))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .child(
                div()
                    .text_color(if empty {
                        rgba((c.text << 8) | 0x50)
                    } else {
                        rgb(c.text)
                    })
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgba((c.text << 8) | 0x80))
                    .child("▾"),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                cx.stop_propagation();
                this.scheme_menu = true;
                cx.notify();
            }))
    }

    /// wt-style palette preview: bg / fg / selection + the 16 ANSI colors.
    fn palette_strip(
        pal: &rikka_terminal_core::theme::Palette,
        c: &SearchColors,
    ) -> impl IntoElement {
        let pack = |col: rikka_terminal_core::theme::Rgb| {
            ((col.r as u32) << 16) | ((col.g as u32) << 8) | col.b as u32
        };
        let mut colors: Vec<u32> = vec![
            pack(pal.background),
            pack(pal.foreground),
            pack(pal.selection),
        ];
        colors.extend(pal.ansi.iter().map(|&col| pack(col)));
        let border = rgb(c.input_border);
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(3.))
            .children(colors.into_iter().map(move |col| {
                div()
                    .w(px(16.))
                    .h(px(16.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(border)
                    .bg(rgb(col))
            }))
    }

    fn small_btn(
        id: (&'static str, usize),
        label: &'static str,
        c: &SearchColors,
        on: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (text, hover) = (rgba((c.text << 8) | 0xB8), rgba((c.text << 8) | 0x14));
        div()
            .id(id)
            .w(px(24.))
            .h(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .text_size(px(13.))
            .text_color(text)
            .hover(move |s| s.bg(hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                cx.stop_propagation();
                on(this, cx);
                this.status = None;
                cx.notify();
            }))
            .child(label)
    }

    fn stepper(
        &self,
        seq: usize,
        display: String,
        dec: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        inc: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = sheet();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .child(Self::small_btn(("sf-dec", seq), "−", &c, dec, cx))
            .child(
                div()
                    .min_w(px(64.))
                    .px(px(6.))
                    .py(px(3.))
                    .bg(rgb(c.input_bg))
                    .border_1()
                    .border_color(rgb(c.input_border))
                    .rounded(px(4.))
                    .text_size(px(13.))
                    .text_color(rgb(c.text))
                    .flex()
                    .justify_center()
                    .child(display),
            )
            .child(Self::small_btn(("sf-inc", seq), "＋", &c, inc, cx))
    }

    fn segment(
        &self,
        seq: usize,
        options: [&'static str; 2],
        selected: bool, // false = first, true = second
        set: impl Fn(&mut Self, bool) + 'static + Copy,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = sheet();
        let mk = |ix: usize, label: &'static str, on_state: bool, cx: &mut Context<Self>| {
            let accent = c.accent;
            let text = c.text;
            div()
                .id(("sf-seg", seq * 2 + ix))
                .px(px(12.))
                .py(px(3.))
                .rounded(px(4.))
                .text_size(px(12.))
                .when(on_state, |d| d.bg(rgb(accent)).text_color(rgb(text)))
                .when(!on_state, |d| {
                    d.text_color(rgba((text << 8) | 0xA0))
                        .hover(move |s| s.bg(rgba((text << 8) | 0x14)))
                })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                    cx.stop_propagation();
                    set(this, ix == 1);
                    this.status = None;
                    cx.notify();
                }))
                .child(label)
        };
        div()
            .flex()
            .flex_row()
            .gap(px(4.))
            .child(mk(0, options[0], !selected, cx))
            .child(mk(1, options[1], selected, cx))
    }

    fn checkbox(
        &self,
        seq: usize,
        label: &'static str,
        on_state: bool,
        set: impl Fn(&mut Self, bool) + 'static + Copy,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = sheet();
        div()
            .id(("sf-check", seq))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _win, cx| {
                cx.stop_propagation();
                set(this, !on_state);
                this.status = None;
                cx.notify();
            }))
            .child(
                div()
                    .w(px(16.))
                    .h(px(16.))
                    .flex_shrink_0()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(if on_state {
                        rgb(c.accent)
                    } else {
                        rgb(c.input_border)
                    })
                    .when(on_state, |d| d.bg(rgb(c.accent)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(rgb(c.text))
                    .when(on_state, |d| d.child("✓")),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(rgba((c.text << 8) | 0xC0))
                    .child(label),
            )
    }

    // ── Pages ───────────────────────────────────────────────────────────

    /// Page header, wt-style: big title over the rows.
    fn page_title(title: &'static str, c: &SearchColors) -> impl IntoElement {
        div()
            .text_size(px(16.))
            .text_color(rgb(c.text))
            .mb(px(4.))
            .child(title)
    }

    fn page_appearance(&self, c: &SearchColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::page_title("外観", c))
            .child(Self::row(
                "フォント",
                self.text_field(Field::Font, 0, cx),
                c,
            ))
            .child(Self::row(
                "フォントサイズ",
                self.stepper(
                    0,
                    format!("{:.1}", self.v.font_size),
                    |t, _| t.v.font_size = (t.v.font_size - 1.0).max(6.0),
                    |t, _| t.v.font_size = (t.v.font_size + 1.0).min(40.0),
                    cx,
                ),
                c,
            ))
            .child(Self::row(
                "行の高さ",
                self.stepper(
                    1,
                    format!("{:.2}", self.v.line_height),
                    |t, _| t.v.line_height = ((t.v.line_height - 0.05) * 100.).round() / 100.,
                    |t, _| t.v.line_height = ((t.v.line_height + 0.05) * 100.).round() / 100.,
                    cx,
                ),
                c,
            ))
            .child(Self::row(
                "検索バーの意匠",
                self.segment(
                    0,
                    ["WinUI", "VSCode"],
                    self.v.search_vscode,
                    |t, second| t.v.search_vscode = second,
                    cx,
                ),
                c,
            ))
            .child(self.checkbox(
                0,
                "アクリル背景（再起動が必要）",
                self.v.acrylic,
                |t, on| t.v.acrylic = on,
                cx,
            ))
            .into_any_element()
    }

    fn page_theme(&self, c: &SearchColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::page_title("テーマ", c))
            .child(Self::row("wt スキーム", self.scheme_button(cx), c))
            .children({
                // Preview from the cached catalog — a Vec lookup, no I/O
                // per frame.
                self.schemes
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&self.v.wt_scheme))
                    .map(|(_, pal)| Self::palette_strip(pal, c))
            })
            .child(Self::row(
                "背景色",
                self.color_field(Field::ThemeBg, 4, cx),
                c,
            ))
            .child(Self::row(
                "文字色",
                self.color_field(Field::ThemeFg, 5, cx),
                c,
            ))
            .into_any_element()
    }

    fn page_terminal(&self, c: &SearchColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::page_title("ターミナル", c))
            .child(Self::row(
                "スクロールバック",
                self.stepper(
                    2,
                    format!("{}", self.v.scrollback),
                    |t, _| t.v.scrollback = t.v.scrollback.saturating_sub(5_000).max(1_000),
                    |t, _| t.v.scrollback = (t.v.scrollback + 5_000).min(200_000),
                    cx,
                ),
                c,
            ))
            .child(Self::row("TERM", self.text_field(Field::Term, 1, cx), c))
            .child(Self::row(
                "識別 (XTVERSION)",
                self.segment(
                    1,
                    ["honest", "ghostty"],
                    self.v.identity_ghostty,
                    |t, second| t.v.identity_ghostty = second,
                    cx,
                ),
                c,
            ))
            .into_any_element()
    }

    fn page_logging(&self, c: &SearchColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::page_title("セッションログ", c))
            .child(Self::row(
                "保存先",
                self.text_field(Field::LogDir, 2, cx),
                c,
            ))
            .child(self.checkbox(
                1,
                "入力も記録する（パスワードも写る・意図して有効に）",
                self.v.log_input,
                |t, on| t.v.log_input = on,
                cx,
            ))
            .child(self.checkbox(
                2,
                "新しいタブで自動的にログを開始",
                self.v.auto_start,
                |t, on| t.v.auto_start = on,
                cx,
            ))
            .into_any_element()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = sheet();
        let dirty = self.v != self.initial;
        div()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _win, cx| {
                if this.handle_key(ev, cx) {
                    cx.stop_propagation();
                }
            }))
            // Click on empty space drops text-field focus.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _win, cx| {
                    this.focused = None;
                    cx.notify();
                }),
            )
            .size_full()
            .bg(rgb(0x1F1E1C))
            .flex()
            .flex_col()
            // Body: nav rail + the selected page.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .w(px(140.))
                            .flex_shrink_0()
                            .border_r_1()
                            .border_color(rgb(c.border))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .p(px(8.))
                            .child(self.nav_item(Page::Appearance, "外観", 0, cx))
                            .child(self.nav_item(Page::Theme, "テーマ", 1, cx))
                            .child(self.nav_item(Page::Terminal, "ターミナル", 2, cx))
                            .child(self.nav_item(Page::Logging, "セッションログ", 3, cx)),
                    )
                    .child(
                        div()
                            .id("settings-scroll")
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(div().p(px(14.)).child(match self.page {
                                Page::Appearance => self.page_appearance(&c, cx),
                                Page::Theme => self.page_theme(&c, cx),
                                Page::Terminal => self.page_terminal(&c, cx),
                                Page::Logging => self.page_logging(&c, cx),
                            })),
                    ),
            )
            // Pinned footer: status + save, always reachable.
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(c.border))
                    .px(px(14.))
                    .py(px(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .child({
                        let (msg, is_err) = self.status.clone().unwrap_or_default();
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(if is_err {
                                rgb(c.error)
                            } else {
                                rgba((c.text << 8) | 0x90)
                            })
                            .child(msg)
                    })
                    .child(
                        div()
                            .id("sf-save")
                            .px(px(18.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .text_size(px(13.))
                            .when(dirty, |d| {
                                d.bg(rgb(c.accent)).text_color(rgb(c.text)).cursor_pointer()
                            })
                            .when(!dirty, |d| {
                                d.bg(rgb(c.input_bg)).text_color(rgba((c.text << 8) | 0x50))
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                                cx.stop_propagation();
                                this.save(cx);
                            }))
                            .child("保存"),
                    ),
            )
            // Scheme picker overlay: scrim + a centered list (rendered on
            // the ROOT, never inside the scroll container — an absolute
            // child of a scroll container inflates its content size).
            .when(self.scheme_menu, |root| {
                let names: Vec<(String, u32, u32)> = self
                    .schemes
                    .iter()
                    .map(|(n, pal)| {
                        let pack = |col: rikka_terminal_core::theme::Rgb| {
                            ((col.r as u32) << 16) | ((col.g as u32) << 8) | col.b as u32
                        };
                        (n.clone(), pack(pal.background), pack(pal.foreground))
                    })
                    .collect();
                let item = |id: usize, c: &SearchColors| {
                    div()
                        .id(("sf-scheme-item", id))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(4.))
                        .text_size(px(13.))
                        .text_color(rgba((c.text << 8) | 0xC0))
                        .hover(|t| t.bg(rgba(0xFFFFFF14)))
                        .cursor_pointer()
                };
                root.child(
                    div()
                        .id("sf-scheme-scrim")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .on_click(cx.listener(|this, _: &ClickEvent, _win, cx| {
                            this.scheme_menu = false;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(40.))
                        .left(px(80.))
                        .right(px(80.))
                        .max_h(px(380.))
                        .bg(rgb(c.bg))
                        .border_1()
                        .border_color(rgb(c.border))
                        .rounded(px(8.))
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .px(px(12.))
                                .py(px(8.))
                                .text_size(px(12.))
                                .text_color(rgba((c.text << 8) | 0x70))
                                .child("配色スキーム"),
                        )
                        .child(
                            div()
                                .id("sf-scheme-list")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .track_scroll(&self.scheme_scroll)
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .p(px(4.))
                                        .child(
                                            item(usize::MAX, &c)
                                                .child(
                                                    div()
                                                        .text_color(rgba((c.text << 8) | 0x70))
                                                        .child("(なし — 既定の配色)"),
                                                )
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _win, cx| {
                                                        cx.stop_propagation();
                                                        this.v.wt_scheme.clear();
                                                        this.scheme_menu = false;
                                                        this.status = None;
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                        .children(names.into_iter().enumerate().map(
                                            |(ix, (name, bg, fg))| {
                                                let pick = name.clone();
                                                item(ix, &c)
                                                    .child(
                                                        div()
                                                            .w(px(14.))
                                                            .h(px(14.))
                                                            .flex_shrink_0()
                                                            .rounded(px(3.))
                                                            .border_1()
                                                            .border_color(rgb(c.input_border))
                                                            .bg(rgb(bg)),
                                                    )
                                                    .child(
                                                        div()
                                                            .w(px(14.))
                                                            .h(px(14.))
                                                            .flex_shrink_0()
                                                            .rounded(px(3.))
                                                            .border_1()
                                                            .border_color(rgb(c.input_border))
                                                            .bg(rgb(fg)),
                                                    )
                                                    .child(name)
                                                    .on_click(cx.listener(
                                                        move |this, _: &ClickEvent, _win, cx| {
                                                            cx.stop_propagation();
                                                            this.v.wt_scheme = pick.clone();
                                                            this.scheme_menu = false;
                                                            this.status = None;
                                                            cx.notify();
                                                        },
                                                    ))
                                            },
                                        )),
                                ),
                        ),
                )
            })
    }
}

/// `#RRGGBB` → `0xRRGGBB`.
fn parse_hex(s: &str) -> Option<u32> {
    let hex = s.strip_prefix('#')?;
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// Apply `(section, key, value)` writes to a config.toml source, keeping the
/// user's comments and layout (toml_edit). `None` removes the key. Fails
/// when the existing file is unparseable — the caller must NOT clobber a
/// hand-broken config.
fn apply_edits(
    raw: &str,
    edits: &[(&'static str, &'static str, Option<toml_edit::Value>)],
) -> Result<String, toml_edit::TomlError> {
    let mut doc: toml_edit::DocumentMut = raw.trim_start_matches('\u{feff}').parse()?;
    for (section, key, value) in edits {
        let Some(value) = value else {
            // Clearing a field removes its key (falling back to the
            // built-in default); an absent section means nothing to do.
            if let Some(table) = doc.get_mut(*section).and_then(|i| i.as_table_mut()) {
                table.remove(key);
            }
            continue;
        };
        // An absent section becomes an explicit [section] table at the end
        // — plain indexing would materialize an inline `section = {...}`
        // pinned to the top of the file instead.
        if doc.get(section).is_none() {
            doc.insert(section, toml_edit::table());
        }
        let item = &mut doc[section][key];
        if let toml_edit::Item::Value(old) = item {
            // Existing key: carry the decor over so an end-of-line comment
            // on the value survives the update.
            let mut v = value.clone();
            *v.decor_mut() = old.decor().clone();
            *item = toml_edit::Item::Value(v);
        } else {
            *item = toml_edit::Item::Value(value.clone());
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::{apply_edits, parse_hex};

    #[test]
    fn edits_preserve_comments_and_layout() {
        let raw = "# my precious comment\n[appearance]\n# keep me\nfont = \"Consolas\"\n";
        let out = apply_edits(
            raw,
            &[
                (
                    "appearance",
                    "font_size",
                    Some(toml_edit::Value::from(18.0)),
                ),
                (
                    "terminal",
                    "scrollback",
                    Some(toml_edit::Value::from(50_000_i64)),
                ),
            ],
        )
        .unwrap();
        assert!(out.contains("# my precious comment"), "{out}");
        assert!(out.contains("# keep me"), "{out}");
        assert!(out.contains("font = \"Consolas\""), "{out}");
        assert!(out.contains("font_size = 18.0"), "{out}");
        assert!(out.contains("[terminal]"), "{out}");
        assert!(out.contains("scrollback = 50000"), "{out}");
    }

    #[test]
    fn broken_config_is_never_clobbered() {
        assert!(apply_edits("[appearance\nfont = ", &[]).is_err());
    }

    #[test]
    fn edited_value_overwrites_in_place() {
        let raw = "[appearance]\nfont_size = 13.0 # chosen with care\n";
        let out = apply_edits(
            raw,
            &[(
                "appearance",
                "font_size",
                Some(toml_edit::Value::from(20.0)),
            )],
        )
        .unwrap();
        assert!(out.contains("font_size = 20.0"), "{out}");
        assert!(out.contains("# chosen with care"), "{out}");
    }

    #[test]
    fn cleared_value_removes_its_key() {
        let raw = "[theme]\nwt_scheme = \"Ubuntu\"\nbackground = \"#300A24\"\n";
        let out = apply_edits(raw, &[("theme", "wt_scheme", None)]).unwrap();
        assert!(!out.contains("wt_scheme"), "{out}");
        assert!(out.contains("background = \"#300A24\""), "{out}");
        // Removing from an absent section is a no-op, not a crash.
        let out2 = apply_edits("", &[("theme", "wt_scheme", None)]).unwrap();
        assert_eq!(out2, "");
    }

    #[test]
    fn hex_swatch_parsing() {
        assert_eq!(parse_hex("#300A24"), Some(0x300A24));
        assert_eq!(parse_hex("300A24"), None);
        assert_eq!(parse_hex("#30A"), None);
        assert_eq!(parse_hex("#GGGGGG"), None);
    }
}
