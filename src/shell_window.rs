use crate::settings::load_settings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::terminal::keys::key_to_bytes;
use crate::terminal::pty_session;
use crate::terminal::renderer::render_grid;
use crate::terminal::{GridSnapshot, TerminalSession};
use crate::theme::Colors;
use crate::window::measure_cell_metrics;
use gpui::{
    App, Bounds, Context, IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    size,
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
        };
        win.connect(cx);
        win
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
            div()
                .id("shell-pane")
                .flex_1()
                .w_full()
                .track_scroll(&self.scroll_handle)
                .overflow_y_scroll()
                .focusable()
                // capture_key_down: fires before GPUI action dispatch so built-in
                // bindings (Enter, arrows, Tab, Escape…) cannot consume the event first.
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _win, _cx| {
                    let bytes = key_to_bytes(&event.keystroke);
                    if !bytes.is_empty() {
                        if let Some(s) = &this.session {
                            s.send_bytes(&bytes);
                        }
                    }
                }))
                .p_1()
                .child(render_grid(&snap, MONO_FONT, cw, ch))
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
