use crate::ansi::parse_ansi_spans;
use crate::settings::ShogunDesktopSettings;
use crate::ssh::SshClient;
use crate::theme::Colors;
use crate::window::{ShogunState, ShogunWindow};
use gpui::{div, prelude::*, px, rgb, Context, Entity, IntoElement, ParentElement, Styled, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    scroll::ScrollableElement,
    v_flex, Sizable,
};

pub const MONO_FONT: &str = "Consolas";

const SPECIAL_KEYS: [(&str, &str); 9] = [
    ("↵", "\n"),
    ("C-c", "\x03"),
    ("C-b", "\x02"),
    ("↑", "\x1b[A"),
    ("↓", "\x1b[B"),
    ("Tab", "\t"),
    ("ESC", "\x1b"),
    ("C-o", "\x0f"),
    ("C-d", "\x04"),
];

pub struct ShogunTab {
    pub command_input: Entity<InputState>,
}

impl ShogunTab {
    pub fn new(window: &mut Window, cx: &mut Context<ShogunWindow>) -> Self {
        let command_input = cx.new(|cx| InputState::new(window, cx));
        Self { command_input }
    }
}

fn tmux_target(settings: &ShogunDesktopSettings) -> String {
    format!("{}:main", settings.sessions.shogun)
}

fn tmux_key_token(value: &str) -> String {
    match value {
        "\n" => "Enter".into(),
        "\x03" => "C-c".into(),
        "\x02" => "C-b".into(),
        "\x1b[A" => "Up".into(),
        "\x1b[B" => "Down".into(),
        "\t" => "Tab".into(),
        "\x1b" => "Escape".into(),
        "\x0f" => "C-o".into(),
        "\x04" => "C-d".into(),
        other => format!("'{}'", other.replace('\'', "'\\''")),
    }
}

fn capture_pane_command(target: &str) -> String {
    format!("tmux capture-pane -t {target} -p -e -S -500")
}

pub fn run_send_command(settings: ShogunDesktopSettings, text: String) -> anyhow::Result<String> {
    if text.is_empty() {
        return Ok(String::new());
    }
    let target = tmux_target(&settings);
    let escaped = text.replace('\'', "'\\''");
    let client = SshClient::from_settings(&settings)?;
    // Single SSH connection: send text, wait, send Enter, wait, capture
    let cmd = format!(
        "tmux send-keys -t {target} '{escaped}' && sleep 0.3 && \
         tmux send-keys -t {target} Enter && sleep 1.5 && \
         {}",
        capture_pane_command(&target)
    );
    client.exec(&cmd)
}

pub fn run_send_special_key(
    settings: ShogunDesktopSettings,
    key_value: &'static str,
) -> anyhow::Result<()> {
    let target = tmux_target(&settings);
    let token = tmux_key_token(key_value);
    let client = SshClient::from_settings(&settings)?;
    client.exec(&format!("tmux send-keys -t {target} {token}"))?;
    Ok(())
}

/// Shogun tab: jinmaku + pane + special keys + command input (subtask_171_002 + 171_003).
pub fn render_shogun_tab(
    tab: &ShogunTab,
    state: &ShogunState,
    shogun_session: &str,
    cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .size_full()
        .bg(Colors::shikkoku())
        .child(render_jinmaku_bar(state, shogun_session))
        .child(render_pane_content(state))
        .child(render_special_key_bar(cx))
        .child(render_command_input(tab, cx))
}

fn render_jinmaku_bar(state: &ShogunState, shogun_session: &str) -> impl IntoElement {
    let bg_color = if state.is_connected {
        Colors::matsuba()
    } else {
        Colors::kurenai()
    };

    let text = if let Some(err) = &state.error_message {
        err.clone()
    } else if state.is_connected {
        format!("接続中 — {shogun_session}:main")
    } else {
        "未接続".to_string()
    };

    div()
        .w_full()
        .h(px(48.))
        .flex()
        .items_center()
        .px_3()
        .bg(bg_color)
        .text_color(rgb(0xFFFFFF))
        .text_sm()
        .child(text)
}

fn render_pane_content(state: &ShogunState) -> impl IntoElement {
    let inner: gpui::AnyElement = if let Some(err) = &state.error_message {
        div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::kurenai())
            .child(format!("❌ {err}"))
            .into_any_element()
    } else if state.pane_content.is_empty() {
        div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::zouge())
            .child("（pane 出力待ち — SSH接続後に表示されます）")
            .into_any_element()
    } else {
        render_ansi_lines(&state.pane_content).into_any_element()
    };

    div()
        .id("shogun-pane-content")
        .flex_1()
        .w_full()
        .bg(Colors::shikkoku())
        .overflow_y_scrollbar()
        .p_2()
        .child(inner)
}

fn render_ansi_lines(raw: &str) -> impl IntoElement {
    let lines = parse_ansi_spans(raw);
    v_flex().children(lines.into_iter().map(|spans| {
        div()
            .flex()
            .flex_row()
            .children(spans.into_iter().map(|span| {
                let color = span
                    .rgb
                    .map(|(r, g, b)| {
                        rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32)
                    })
                    .unwrap_or(Colors::zouge());
                div()
                    .text_sm()
                    .font_family(MONO_FONT)
                    .text_color(color)
                    .child(span.text)
            }))
    }))
}

fn render_special_key_bar(cx: &mut Context<ShogunWindow>) -> impl IntoElement {
    div()
        .id("special-key-bar")
        .flex()
        .flex_row()
        .gap_1()
        .px_2()
        .py_1()
        .w_full()
        .flex_shrink_0()
        .bg(Colors::sumi())
        .overflow_hidden()
        .children(SPECIAL_KEYS.iter().enumerate().map(|(i, (label, value))| {
            let key_value = *value;
            Button::new(("special-key", i))
                .small()
                .label(*label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.send_shogun_special_key(key_value, window, cx);
                }))
        }))
}

fn render_command_input(tab: &ShogunTab, cx: &mut Context<ShogunWindow>) -> impl IntoElement {
    h_flex()
        .gap_2()
        .p_2()
        .w_full()
        .child(Input::new(&tab.command_input).w_full())
        .child(
            Button::new("shogun-send")
                .primary()
                .label("Send")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.send_shogun_command(window, cx);
                })),
        )
}
