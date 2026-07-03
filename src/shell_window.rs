use crate::settings::load_settings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::terminal::ime::{ImeHost, TerminalIme};
use crate::terminal::keys::key_to_pty_bytes;
use crate::terminal::pty_session;
use crate::terminal::renderer::render_grid;
use crate::terminal::selection::{self, SelectionHost, SelectionState};
use crate::terminal::{GridSnapshot, TerminalSession};
use crate::theme::Colors;
use crate::window::{
    TERMINAL_KEY_CONTEXT, TERMINAL_PANE_PADDING_PX, TerminalCopy, TerminalSendBacktab,
    TerminalSendTab, measure_cell_metrics,
};
use gpui::{
    App, Bounds, Context, ElementInputHandler, Entity, FocusHandle, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Window, WindowBounds,
    WindowOptions, canvas, div, prelude::*, px, size,
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
    /// Shared IME text-input handler (see `terminal::ime`).
    ime: Entity<TerminalIme<Self>>,
    /// Shared mouse-selection state (see `terminal::selection`).
    selection: SelectionState,
}

impl SelectionHost for ShellWindow {
    fn selection_state(&mut self) -> &mut SelectionState {
        &mut self.selection
    }
}

impl ImeHost for ShellWindow {
    fn ime_session(&self) -> Option<&TerminalSession> {
        self.session.as_ref()
    }

    fn ime_font(&self) -> &str {
        MONO_FONT
    }
}

impl ShellWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        let weak = cx.weak_entity();
        let ime = cx.new(|_| TerminalIme::new(weak));
        cx.observe(&ime, |_, _, cx| cx.notify()).detach();
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
            ime,
            selection: SelectionState::default(),
        };
        win.connect(cx);
        win
    }

    fn send_bytes(&self, bytes: &[u8]) {
        if let Some(s) = &self.session {
            s.send_bytes(bytes);
        }
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

    /// Event-driven refresh: parks on the session's `Notify` (zero wakeups
    /// while idle) and coalesces output bursts into ~60fps frames. Mirrors
    /// `ShogunWindow::start_terminal_refresh`.
    fn start_refresh(&self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let generation = std::sync::Arc::clone(&session.generation);
        let notify = std::sync::Arc::clone(&session.notify);
        let scroll = self.scroll_handle.clone();

        cx.spawn(async move |this, cx| {
            let mut last = generation.load(Ordering::Relaxed);
            loop {
                notify.notified().await;
                // Coalesce a burst of PTY chunks into a single frame.
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let cur = generation.load(Ordering::Relaxed);
                if cur == last {
                    continue;
                }
                last = cur;
                let alive = this.update(cx, |view, cx| {
                    view.last_gen = cur;
                    if !view.scroll_locked {
                        scroll.scroll_to_bottom();
                    }
                    view.prev_offset_y = scroll.offset().y / px(1.);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
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
            let new_rows =
                (((vp.height / px(1.)) - 24.0 - TERMINAL_PANE_PADDING_PX).max(ch) / ch) as u16;

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
            let ime = self.ime.clone();
            let ime_preedit = self.ime.read(cx).marked.clone();
            let view = cx.entity();
            let scroll_for_overlay = self.scroll_handle.clone();
            let grid_rows = snap.rows;
            let grid_cols = snap.cols;
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
                .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                    selection::copy_to_clipboard(&this.selection, this.session.as_ref(), cx);
                }))
                // Stop propagation for consumed keys; printable unmodified keys
                // must keep propagating so the platform generates WM_CHAR for
                // the input handler (otherwise every char would double).
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _win, cx| {
                    if let Some(bytes) = key_to_pty_bytes(&event.keystroke) {
                        this.send_bytes(&bytes);
                        cx.stop_propagation();
                    }
                }))
                .p_1()
                // Overlay: registers the IME input handler (GPUI only routes
                // WM_CHAR / IME composition to a registered handler) and the
                // shared mouse-selection listeners. Nothing may be *drawn*
                // from this canvas — paint calls issued here never reach the
                // screen (verified 2026-07-03); the preedit and selection
                // highlight are painted by render_grid.
                .child(
                    canvas(
                        |_bounds, _window, _cx| (),
                        move |bounds, (), window, cx: &mut App| {
                            window.handle_input(
                                &focus_handle,
                                ElementInputHandler::new(bounds, ime.clone()),
                                cx,
                            );
                            selection::register_mouse_selection(
                                window,
                                view.clone(),
                                scroll_for_overlay.clone(),
                                bounds,
                                0,
                                cw,
                                ch,
                                grid_rows,
                                grid_cols,
                            );
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .child(render_grid(
                    &snap,
                    MONO_FONT,
                    cw,
                    ch,
                    self.selection.range_for(0),
                    ime_preedit,
                ))
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
