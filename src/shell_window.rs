use crate::settings::load_settings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::terminal::keys::key_to_bytes;
use crate::terminal::pty_session;
use crate::terminal::renderer::render_grid;
use crate::terminal::{GridSnapshot, TerminalSession};
use crate::theme::Colors;
use crate::window::{
    TERMINAL_KEY_CONTEXT, TerminalSendBacktab, TerminalSendTab, measure_cell_metrics,
};
use gpui::{
    App, Bounds, Context, ElementInputHandler, FocusHandle, IntoElement, KeyDownEvent,
    ParentElement, Pixels, Render, ScrollHandle, StatefulInteractiveElement, Styled,
    UTF16Selection, Window, WindowBounds, WindowOptions, canvas, div, point, prelude::*, px, size,
};
use gpui_component::{Root, v_flex};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct ShellWindow {
    session: Option<TerminalSession>,
    error: Option<String>,
    scroll_handle: ScrollHandle,
    scroll_locked: bool,
    prev_offset_y: f32,
    last_gen: u64,
    terminal_cols: u16,
    terminal_rows: u16,
    /// Focus handle for the shell pane; required so an IME input handler can
    /// be registered (GPUI only routes WM_CHAR / IME composition events to a
    /// registered input handler on the focused element).
    terminal_focus: FocusHandle,
    /// Current IME composition (marked) text; drawn inline at the cursor.
    ime_marked: Option<String>,
}

impl ShellWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut win = Self {
            session: None,
            error: None,
            scroll_handle: ScrollHandle::default(),
            scroll_locked: false,
            prev_offset_y: 0.0,
            last_gen: 0,
            terminal_cols: 0,
            terminal_rows: 0,
            terminal_focus: cx.focus_handle(),
            ime_marked: None,
        };
        win.connect(cx);
        win
    }

    fn send_bytes(&self, bytes: &[u8]) {
        if let Some(s) = &self.session {
            s.send_bytes(bytes);
        }
    }

    /// Handle a key-down aimed at the shell terminal. Returns `true` when the
    /// key was consumed here (caller stops propagation); `false` when the key
    /// is left for the platform text-input path (WM_CHAR / IME →
    /// EntityInputHandler). Same double-input guard as
    /// `ShogunWindow::handle_terminal_key`.
    fn handle_key(&mut self, event: &KeyDownEvent) -> bool {
        let ks = &event.keystroke;
        if !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform
            && ks
                .key_char
                .as_ref()
                .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_control))
        {
            return false;
        }
        let bytes = key_to_bytes(ks);
        if bytes.is_empty() {
            return false;
        }
        self.send_bytes(&bytes);
        true
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let settings = load_settings().unwrap_or_default();
        if settings.ssh.host.is_empty() {
            self.error = Some("SSH ホストが未設定です".into());
            return;
        }
        let project_path = settings.project.path.clone();
        if project_path.is_empty() {
            self.error = Some("プロジェクトパスが未設定です".into());
            return;
        }

        cx.spawn(async move |this, cx| {
            let settings_bg = settings.clone();
            let connect = cx
                .background_executor()
                .spawn(async move { SshClient::from_settings(&settings_bg) })
                .await;

            let ssh = match connect {
                Ok(c) => c,
                Err(e) => {
                    let _ = this.update(cx, |view, cx| {
                        view.error = Some(format!("SSH接続失敗: {e}"));
                        cx.notify();
                    });
                    return;
                }
            };

            let control_path = ssh.control_socket_path();
            let result = cx
                .background_executor()
                .spawn(async move {
                    pty_session::spawn_shell(&ssh, &project_path, 220, 50, control_path)
                })
                .await;

            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(session) => {
                        view.session = Some(session);
                        view.error = None;
                        view.start_refresh(cx);
                    }
                    Err(e) => view.error = Some(format!("シェル起動失敗: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_refresh(&self, cx: &mut Context<Self>) {
        let gen_arc = self
            .session
            .as_ref()
            .map(|s| std::sync::Arc::clone(&s.generation));
        let scroll = self.scroll_handle.clone();

        cx.spawn(async move |this, cx| {
            let mut last = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let cur = gen_arc
                    .as_ref()
                    .map(|g| g.load(Ordering::Relaxed))
                    .unwrap_or(0);
                if cur != last {
                    last = cur;
                    let _ = this.update(cx, |view, cx| {
                        view.last_gen = cur;
                        if !view.scroll_locked {
                            scroll.scroll_to_bottom();
                        }
                        view.prev_offset_y = scroll.offset().y / px(1.);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn snap(&self) -> Option<GridSnapshot> {
        self.session
            .as_ref()
            .filter(|s| s.is_connected())
            .map(|s| s.snapshot.lock().clone())
    }
}

impl Render for ShellWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = self.last_gen;

        // Resize: full viewport (no chrome except tiny status bar of 24px)
        let (cw, ch) = measure_cell_metrics(&cx.text_system(), MONO_FONT, window.scale_factor());
        {
            let vp = window.viewport_size();
            let new_cols = ((vp.width / px(1.)) / cw) as u16;
            let new_rows = (((vp.height / px(1.)) - 24.0).max(ch) / ch) as u16;

            let needs = |s: &Option<TerminalSession>| {
                s.as_ref().map_or(false, |sess| {
                    sess.cols.load(Ordering::Relaxed) != new_cols
                        || sess.rows.load(Ordering::Relaxed) != new_rows
                })
            };
            if new_cols != self.terminal_cols
                || new_rows != self.terminal_rows
                || needs(&self.session)
            {
                self.terminal_cols = new_cols;
                self.terminal_rows = new_rows;
                if let Some(s) = &self.session {
                    s.resize(new_cols, new_rows);
                }
            }
        }

        let is_connected = self
            .session
            .as_ref()
            .map(|s| s.is_connected())
            .unwrap_or(false);
        let status_bg = if is_connected {
            Colors::matsuba()
        } else {
            Colors::kurenai()
        };
        let status_text = if let Some(ref e) = self.error {
            e.clone()
        } else if is_connected {
            "シェル — 接続中".into()
        } else {
            "未接続".into()
        };

        let terminal_body: gpui::AnyElement = if let Some(snap) = self.snap() {
            let focus_handle = self.terminal_focus.clone();
            let view = cx.entity();
            let ime_preedit = self.ime_marked.clone();
            div()
                .id("shell-pane")
                .flex_1()
                .w_full()
                .track_scroll(&self.scroll_handle)
                .overflow_y_scroll()
                // track_focus binds OUR FocusHandle so the IME input handler
                // below can be registered against the same handle (mirrors
                // render_terminal_tab).
                .track_focus(&focus_handle)
                // Reclaim tab / shift-tab from Root's focus cycling — the
                // bindings in main.rs target this key context.
                .key_context(TERMINAL_KEY_CONTEXT)
                .on_action(cx.listener(|this, _: &TerminalSendTab, _window, _cx| {
                    this.send_bytes(b"\t");
                }))
                .on_action(cx.listener(|this, _: &TerminalSendBacktab, _window, _cx| {
                    this.send_bytes(b"\x1b[Z");
                }))
                // Stop propagation for consumed keys; printable unmodified keys
                // must keep propagating so the platform generates WM_CHAR for
                // the input handler (otherwise every char would double).
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _win, cx| {
                    if this.handle_key(event) {
                        cx.stop_propagation();
                    }
                }))
                .p_1()
                // Overlay: registers the IME input handler (GPUI only routes
                // WM_CHAR / IME composition to a registered handler). Nothing
                // may be *drawn* from this canvas — paint calls issued here
                // never reach the screen (verified 2026-07-03); the preedit is
                // painted by render_grid at the cursor row.
                .child(
                    canvas(
                        |_bounds, _window, _cx| (),
                        move |bounds, (), window, cx: &mut App| {
                            window.handle_input(
                                &focus_handle,
                                ElementInputHandler::new(bounds, view.clone()),
                                cx,
                            );
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .child(render_grid(&snap, MONO_FONT, cw, ch, None, ime_preedit))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .text_color(Colors::muted())
                .child("接続中...")
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(Colors::shikkoku())
            .child(
                div()
                    .w_full()
                    .h(px(24.))
                    .bg(status_bg)
                    .flex()
                    .items_center()
                    .px_2()
                    .text_color(Colors::zouge())
                    .text_size(px(12.))
                    .child(status_text),
            )
            .child(div().flex_1().overflow_hidden().child(terminal_body))
    }
}

/// IME / text-input integration — the shell window counterpart of
/// `ShogunWindow`'s impl. A terminal has no editable document: committed text
/// goes to the PTY, the preedit is drawn inline at the cursor, and all
/// document queries answer "empty".
impl gpui::EntityInputHandler for ShellWindow {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // A zero-width caret; required so the platform can query the caret
        // rect (bounds_for_range) to position the IME candidate window.
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.ime_marked
            .as_ref()
            .map(|s| 0..s.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ime_marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_marked = None;
        self.send_bytes(text.as_bytes());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_marked = Some(new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let session = self.session.as_ref()?;
        let snap = session.snapshot.lock();
        let (row, col) = snap.cursor;
        let rows = snap.rows;
        drop(snap);

        let (cw, ch) = measure_cell_metrics(&cx.text_system(), MONO_FONT, window.scale_factor());

        // The grid is bottom-anchored in its scroll viewport when it is taller
        // than the visible area (auto scroll-to-bottom), so shift the caret up
        // by the overflow.
        let grid_h = rows as f32 * ch;
        let viewport_h = f32::from(element_bounds.size.height);
        let scroll_overflow = (grid_h - viewport_h).max(0.0);

        Some(Bounds {
            origin: point(
                element_bounds.origin.x + px(col as f32 * cw),
                element_bounds.origin.y + px(row as f32 * ch - scroll_overflow),
            ),
            size: size(px(cw), px(ch)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

pub fn open_shell_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("シェル".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            ..Default::default()
        },
        |window, cx| {
            let _ = window;
            let view = cx.new(|cx| ShellWindow::new(cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("open shell window");
}
