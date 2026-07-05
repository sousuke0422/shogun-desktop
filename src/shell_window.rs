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
    TERMINAL_KEY_CONTEXT, TERMINAL_PANE_PADDING_PX, TerminalCopy, TerminalPaste,
    TerminalSendBacktab, TerminalSendTab, measure_cell_metrics,
};
use gpui::{
    App, Bounds, Context, ElementInputHandler, Entity, FocusHandle, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent, StatefulInteractiveElement,
    Styled, Window, WindowBounds, WindowOptions, canvas, div, prelude::*, px, size,
};
use gpui_component::menu::ContextMenuExt as _;
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
    /// Fractional wheel-scroll remainder so slow touchpad deltas (< 1 line
    /// per event) still accumulate into scrollback lines.
    scroll_accum: f32,
    /// Pane size actually painted (logical px, padding box), reported by the
    /// overlay canvas. `(0, 0)` until the first paint; `render` derives
    /// rows from this instead of estimating the status-bar height.
    pane_measured: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    /// Mirrors `Window::is_window_active()` (kept fresh by an activation
    /// observer) so the async notification watcher — which has no `Window` —
    /// can apply Ghostty-style focus suppression.
    window_active: bool,
    /// OSC 9 / 777 desktop notifications enabled (settings.terminal).
    desktop_notifications: bool,
}

impl SelectionHost for ShellWindow {
    fn selection_state(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    fn pane_session(&self, _pane: usize) -> Option<&TerminalSession> {
        self.session.as_ref()
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
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let weak = cx.weak_entity();
        let ime = cx.new(|_| TerminalIme::new(weak));
        cx.observe(&ime, |_, _, cx| cx.notify()).detach();
        cx.observe_window_activation(window, |view, window, _cx| {
            view.window_active = window.is_window_active();
        })
        .detach();
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
            scroll_accum: 0.0,
            pane_measured: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            window_active: window.is_window_active(),
            desktop_notifications: load_settings()
                .unwrap_or_default()
                .terminal
                .desktop_notifications,
        };
        win.connect(cx);
        win
    }

    fn send_bytes(&self, bytes: &[u8]) {
        if let Some(s) = &self.session {
            // Typing snaps the view back to the live bottom, like every
            // terminal emulator.
            s.scroll_display_to_bottom();
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
        let snapshot = std::sync::Arc::clone(&session.snapshot);
        let notifications = std::sync::Arc::clone(&session.notifications);
        let scroll = self.scroll_handle.clone();

        cx.spawn(async move |this, cx| {
            let mut last = generation.load(Ordering::Relaxed);
            loop {
                // Race the PTY wakeup against the SGR-blink phase timer while
                // blink cells are on screen (see ShogunWindow's refresh task).
                let blink = snapshot.lock().has_blink;
                if blink {
                    let timer = cx.background_executor().timer(Duration::from_millis(300));
                    futures::future::select(Box::pin(notify.notified()), Box::pin(timer)).await;
                } else {
                    notify.notified().await;
                }
                // Coalesce a burst of PTY chunks into a single frame.
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let cur = generation.load(Ordering::Relaxed);
                let data_changed = cur != last;
                if !data_changed && !blink {
                    continue;
                }
                last = cur;
                let alive = this.update(cx, |view, cx| {
                    // OSC 9 / 777 desktop notifications, Ghostty-style focus
                    // suppression: a single-surface window, so suppress
                    // whenever the window itself is active. Always drain.
                    let pending = crate::terminal::notify::take_notifications(&notifications);
                    if !pending.is_empty() && view.desktop_notifications && !view.window_active {
                        for n in &pending {
                            crate::notify_toast::show(
                                n.title.as_deref().unwrap_or("シェル"),
                                &n.body,
                            );
                        }
                    }
                    // Blink-only ticks repaint without touching the scroll.
                    if data_changed {
                        view.last_gen = cur;
                        if !view.scroll_locked {
                            scroll.scroll_to_bottom();
                        }
                        view.prev_offset_y = scroll.offset().y / px(1.);
                    }
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

    fn term_mode(&self) -> alacritty_terminal::term::TermMode {
        self.session
            .as_ref()
            .map(|s| *s.term.lock().mode())
            .unwrap_or_else(alacritty_terminal::term::TermMode::empty)
    }

    /// Tab / back-tab via the key encoder — see ShogunWindow::send_tab_to_active.
    fn send_tab(&self, shift: bool) {
        let ks = gpui::Keystroke {
            key: "tab".to_string(),
            modifiers: gpui::Modifiers {
                shift,
                ..Default::default()
            },
            key_char: None,
        };
        if let Some(bytes) = key_to_pty_bytes(&ks, self.term_mode()) {
            self.send_bytes(&bytes);
        }
    }
}

impl Render for ShellWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = self.last_gen;

        // Resize: full viewport (no chrome except tiny status bar of 24px)
        let (cw, ch) = measure_cell_metrics(&cx.text_system(), MONO_FONT, window.scale_factor());
        {
            let vp = window.viewport_size();
            // Prefer the painted pane size (the overlay canvas is pinned to
            // the content box, so this is the grid area directly); estimate
            // chrome only for the first, pre-paint frame.
            let (mw, mh) = self.pane_measured.get();
            let content_w = if mw > 0.0 { mw } else { vp.width / px(1.) };
            let content_h = if mh > 0.0 {
                mh.max(ch)
            } else {
                ((vp.height / px(1.)) - 24.0 - TERMINAL_PANE_PADDING_PX).max(ch)
            };
            let new_cols = ((content_w / cw) as u16).max(1);
            let new_rows = ((content_h / ch) as u16).max(1);

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
        let snap_opt = self.snap();
        let scrollback = snap_opt.as_ref().map_or(0, |s| s.display_offset);
        let status_text = if let Some(ref e) = self.error {
            e.clone()
        } else if scrollback > 0 {
            format!("シェル — 履歴 {scrollback}行上（入力で最下部へ）")
        } else if is_connected {
            "シェル — 接続中".into()
        } else {
            "未接続".into()
        };

        let terminal_body: gpui::AnyElement = if let Some(snap) = snap_opt {
            let focus_handle = self.terminal_focus.clone();
            let menu_focus = focus_handle.clone();
            let ime = self.ime.clone();
            let ime_preedit = self.ime.read(cx).marked.clone();
            let view = cx.entity();
            let pane_measured = self.pane_measured.clone();
            let grid_rows = snap.rows;
            let grid_cols = snap.cols;
            let pane = div()
                .id("shell-pane")
                .size_full()
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
                    this.send_tab(false);
                }))
                .on_action(cx.listener(|this, _: &TerminalSendBacktab, _window, _cx| {
                    this.send_tab(true);
                }))
                .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                    selection::copy_to_clipboard(&this.selection, this.session.as_ref(), cx);
                }))
                .on_action(cx.listener(|this, _: &TerminalPaste, _window, cx| {
                    let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                        return;
                    };
                    if let Some(s) = &this.session {
                        s.paste(&text);
                    }
                }))
                // Stop propagation for consumed keys; printable unmodified keys
                // must keep propagating so the platform generates WM_CHAR for
                // the input handler (otherwise every char would double).
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _win, cx| {
                    let ks = &event.keystroke;
                    // Shift+PageUp/PageDown: page through the scrollback.
                    if ks.modifiers.shift && (ks.key == "pageup" || ks.key == "pagedown") {
                        if let Some(s) = &this.session {
                            let page = s.rows.load(Ordering::Relaxed).saturating_sub(1) as i32;
                            s.scroll_display(if ks.key == "pageup" { page } else { -page });
                        }
                        cx.stop_propagation();
                        return;
                    }
                    if let Some(bytes) = key_to_pty_bytes(ks, this.term_mode()) {
                        this.send_bytes(&bytes);
                        cx.stop_propagation();
                    }
                }))
                // Wheel: routed to the PTY when the app asked for it (mouse
                // reporting / alternate scroll — btop, less…), otherwise it
                // scrolls the emulator's scrollback (alacritty history). The
                // gpui container never scrolls: after the padding fix the
                // grid fits the pane exactly.
                // gpui wheel-up yields positive y (Zed's terminal does the
                // same direct mapping); Scroll::Delta(positive) = older lines.
                .on_scroll_wheel(
                    cx.listener(move |this, event: &ScrollWheelEvent, _win, _cx| {
                        let lines = match &event.delta {
                            ScrollDelta::Pixels(p) => (p.y / px(1.)) / ch,
                            ScrollDelta::Lines(l) => l.y,
                        };
                        this.scroll_accum += lines;
                        let whole = this.scroll_accum.trunc() as i32;
                        if whole != 0 {
                            this.scroll_accum -= whole as f32;
                            if let Some(s) = &this.session {
                                // Pointed-at cell for mouse reporting; the
                                // pane sits at the window origin + padding.
                                let pad = TERMINAL_PANE_PADDING_PX / 2.0;
                                let cols = s.cols.load(Ordering::Relaxed).max(1) as usize;
                                let rows = s.rows.load(Ordering::Relaxed).max(1) as usize;
                                let col = ((((event.position.x / px(1.)) - pad) / cw).max(0.0)
                                    as usize)
                                    .min(cols - 1);
                                let row = ((((event.position.y / px(1.)) - pad) / ch).max(0.0)
                                    as usize)
                                    .min(rows - 1);
                                let mods = crate::terminal::ReportMods {
                                    alt: event.modifiers.alt,
                                    ctrl: event.modifiers.control,
                                };
                                if !s.wheel_to_pty(whole, col, row, mods) {
                                    s.scroll_display(whole);
                                }
                            }
                        }
                    }),
                )
                .p_1()
                .child(render_grid(
                    &snap,
                    MONO_FONT,
                    cw,
                    ch,
                    self.selection.range_for(0),
                    ime_preedit,
                ))
                // Right-click menu dispatching the same actions as the
                // keyboard shortcuts (see render_terminal_tab).
                .context_menu(move |menu, _window, _cx| {
                    menu.action_context(menu_focus.clone())
                        .menu("コピー", Box::new(TerminalCopy))
                        .menu("ペースト", Box::new(TerminalPaste))
                });

            // Overlay: registers the IME input handler (GPUI only routes
            // WM_CHAR / IME composition to a registered handler) and the
            // shared mouse-selection listeners, and reports the pane size for
            // PTY resize. Nothing may be *drawn* from this canvas — paint
            // calls issued here never reach the screen (verified 2026-07-03);
            // the preedit and selection highlight are painted by render_grid.
            //
            // The canvas lives OUTSIDE the scroll container, as a sibling in
            // a relative wrapper: taffy sizes absolute children of a scroll
            // container to its CONTENT box, so inside the pane the overlay's
            // height tracked the grid (rows × cell) instead of the viewport —
            // rows could then never shrink or grow past the spawn size (the
            // resize-never-fires bug, found 2026-07-05). The wrapper's box IS
            // the pane's border box, and the pane never scrolls (the grid
            // always fits exactly), so the overlay geometry is identical.
            let overlay = canvas(
                |_bounds, _window, _cx| (),
                move |bounds, (), window, cx: &mut App| {
                    // Report the painted pane size back to the view
                    // (deferred notify — we are inside paint) so the
                    // PTY is resized to the true fit.
                    let painted = (bounds.size.width / px(1.), bounds.size.height / px(1.));
                    let (pw, ph) = pane_measured.get();
                    if (pw - painted.0).abs() > 0.5 || (ph - painted.1).abs() > 0.5 {
                        pane_measured.set(painted);
                        let view = view.clone();
                        cx.defer(move |cx| view.update(cx, |_, cx| cx.notify()));
                    }

                    window.handle_input(
                        &focus_handle,
                        ElementInputHandler::new(bounds, ime.clone()),
                        cx,
                    );
                    selection::register_mouse_selection(
                        window,
                        view.clone(),
                        bounds,
                        0,
                        cw,
                        ch,
                        grid_rows,
                        grid_cols,
                    );
                },
            )
            // Pinned to the pane's content box (border box minus the
            // pane's p_1 padding) via the relative wrapper.
            .absolute()
            .top(px(TERMINAL_PANE_PADDING_PX / 2.0))
            .left(px(TERMINAL_PANE_PADDING_PX / 2.0))
            .right(px(TERMINAL_PANE_PADDING_PX / 2.0))
            .bottom(px(TERMINAL_PANE_PADDING_PX / 2.0));

            div()
                .relative()
                .size_full()
                .child(pane)
                .child(overlay)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .text_color(Colors::muted())
                .child("接続中...")
                .into_any_element()
        };

        // Root carries the bundled-emoji fallback (see renderer::with_emoji_fallback).
        crate::terminal::renderer::with_emoji_fallback(v_flex())
            .size_full()
            .bg(Colors::shikkoku())
            .child(
                div()
                    .w_full()
                    .h(px(24.))
                    .bg(status_bg)
                    .relative()
                    .flex()
                    .items_center()
                    .px_2()
                    .text_color(Colors::zouge())
                    .text_size(px(12.))
                    .child(status_text)
                    // OSC 9;4 progress from the running application, drawn
                    // along the status bar's bottom edge.
                    .children(
                        self.session
                            .as_ref()
                            .and_then(|s| s.progress.get())
                            .map(|p| crate::window::render_progress_bar("shell-progress", p)),
                    ),
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
            let view = cx.new(|cx| ShellWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("open shell window");
}
