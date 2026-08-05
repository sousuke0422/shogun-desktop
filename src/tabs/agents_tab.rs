use crate::ansi::parse_ansi_spans;
use crate::settings::ShogunDesktopSettings;
use crate::ssh::SshClient;
use crate::tabs::shogun_tab::MONO_FONT;
use crate::theme::Colors;
use crate::window::{AgentsState, ShogunWindow};
use gpui::{Context, IntoElement, ParentElement, Styled, div, prelude::*, px, rgb};
use gpui_component::{
    Sizable,
    button::Button,
    menu::{ContextMenuExt as _, PopupMenuItem},
    scroll::ScrollableElement,
    text::TextView,
    v_flex,
};
use rikka_terminal_agent_integration::usage::window_label;
use shogun_core::{
    StatusCategory, build_agent_card, build_karo_card, status_category, truncate_summary,
};

const CARD_BG: u32 = 0x242424;

pub use rikka_terminal_agent_integration::usage::UsageData;
pub use shogun_core::AgentCardData;

pub fn run_fetch_agents(settings: ShogunDesktopSettings) -> anyhow::Result<String> {
    if settings.project.path.is_empty() {
        anyhow::bail!("プロジェクトパスが未設定です（設定タブで project_path を入力してください）");
    }
    let client = SshClient::from_settings(&settings)?;
    client.exec(&format!(
        "bash {}/scripts/agent_status.sh",
        settings.project.path
    ))
}

/// Fetch YAML-driven card data for each configured agent via SSH.
pub fn fetch_agent_cards(
    ssh: &SshClient,
    project_path: &str,
    agents: &[String],
) -> Vec<AgentCardData> {
    agents
        .iter()
        .filter_map(|name| fetch_single_agent_card(ssh, project_path, name))
        .collect()
}

fn fetch_single_agent_card(
    ssh: &SshClient,
    project_path: &str,
    name: &str,
) -> Option<AgentCardData> {
    if name == "karo" {
        return fetch_karo_card(ssh, project_path);
    }

    let base = format!("{project_path}/queue");
    let task_yaml = ssh_cat(ssh, &format!("{base}/tasks/{name}.yaml"));
    let inbox_yaml = ssh_cat(ssh, &format!("{base}/inbox/{name}.yaml"));
    let report_yaml = ssh_cat(ssh, &format!("{base}/reports/{name}_report.yaml"));

    build_agent_card(
        name,
        task_yaml.as_deref(),
        inbox_yaml.as_deref(),
        report_yaml.as_deref(),
    )
}

fn fetch_karo_card(ssh: &SshClient, project_path: &str) -> Option<AgentCardData> {
    let base = format!("{project_path}/queue");
    let cmd_yaml = ssh_cat(ssh, &format!("{base}/shogun_to_karo.yaml"));
    let inbox_yaml = ssh_cat(ssh, &format!("{base}/inbox/karo.yaml"));

    build_karo_card(cmd_yaml.as_deref(), inbox_yaml.as_deref())
}

fn ssh_cat(ssh: &SshClient, path: &str) -> Option<String> {
    let cmd = format!("cat {path} 2>/dev/null || true");
    match ssh.exec(&cmd) {
        Ok(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    }
}

fn status_color(status: &str) -> gpui::Rgba {
    match status_category(status) {
        StatusCategory::Active => Colors::kinpaku(),
        StatusCategory::Done => Colors::matsuba(),
        StatusCategory::Failed => Colors::kurenai(),
        StatusCategory::Idle | StatusCategory::Unknown => Colors::muted(),
    }
}

/// The YAML pipeline uses `---` (and sometimes emptiness) as "no value".
/// Rendering it verbatim produced lines like `---更新`.
fn is_placeholder(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t.chars().all(|c| c == '-')
}

/// Cap the summary for a card: at most 2 source lines AND a character budget,
/// because a single long line wraps into arbitrarily many visual lines and
/// `truncate_summary` only counts hard newlines — which is how a card's text
/// ran past its own background.
fn clamp_summary(s: &str) -> String {
    let cut = truncate_summary(s, 2);
    const BUDGET: usize = 90;
    if cut.chars().count() > BUDGET {
        let mut out: String = cut.chars().take(BUDGET).collect();
        out.push('…');
        out
    } else {
        cut
    }
}

fn render_agent_card(card: &AgentCardData, cx: &mut Context<ShogunWindow>) -> gpui::AnyElement {
    let status_col = status_color(&card.status);
    let inbox_color = if card.inbox_unread > 0 {
        Colors::kurenai()
    } else {
        Colors::muted()
    };
    let summary = if card.summary.is_empty() {
        String::new()
    } else {
        clamp_summary(&card.summary)
    };
    let name = card.name.clone();

    // The menu-item handlers get `&mut App`, not the view — reach the window
    // through its entity handle instead of cx.listener.
    let entity = cx.entity();
    let menu_open = (entity.clone(), card.name.clone());
    let menu_copy_summary = card.summary.clone();
    let menu_copy_task = if is_placeholder(&card.task_id) {
        None
    } else {
        Some(card.task_id.clone())
    };
    let menu_refresh = entity.clone();

    div()
        .id(gpui::SharedString::from(format!(
            "agent-card-{}",
            card.name
        )))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.agents_state.selected = Some(name.clone());
            cx.notify();
        }))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0x2C2C2C)))
        .w(px(320.))
        .m_1()
        .p_3()
        .rounded(px(6.))
        .bg(rgb(CARD_BG))
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_family(MONO_FONT)
                .text_color(Colors::kinpaku())
                .child(card.name.clone()),
        )
        .when(!is_placeholder(&card.task_id), |el| {
            el.child(
                div()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .text_color(Colors::zouge())
                    .child(card.task_id.clone()),
            )
        })
        .child(
            // The indicator is DRAWN, not typed: `🟡` came out as a colour
            // emoji while `⚪` fell back to a text glyph, so neighbouring
            // cards showed two different kinds of dot. A painted circle
            // cannot vary with font fallback, and the colour already carries
            // the state.
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(status_col))
                .child(
                    div()
                        .text_xs()
                        .font_family(MONO_FONT)
                        .text_color(status_col)
                        .child(card.status.clone()),
                ),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(inbox_color)
                .child(format!("inbox: {}", card.inbox_unread)),
        )
        .when(!is_placeholder(&card.last_report_at), |el| {
            el.child(
                div()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .text_color(Colors::muted())
                    .child(format!("{} 更新", card.last_report_at)),
            )
        })
        .when(!summary.is_empty(), |el| {
            el.child(
                div()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .text_color(Colors::zouge())
                    .line_height(px(16.))
                    // Hard visual bound (3 lines) even if the char budget
                    // above misjudges how the text wraps.
                    .max_h(px(48.))
                    .overflow_hidden()
                    .child(summary),
            )
        })
        // Wraps the element, so it must come after every styling call —
        // ContextMenu<E> re-exposes none of Div's builder methods.
        .context_menu(move |menu, _window, _cx| {
            let (entity, name) = menu_open.clone();
            let menu = menu.item(PopupMenuItem::new("全文を開く").on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    this.agents_state.selected = Some(name.clone());
                    cx.notify();
                });
            }));
            let summary = menu_copy_summary.clone();
            let menu = if summary.is_empty() {
                menu
            } else {
                menu.item(
                    PopupMenuItem::new("報告をコピー").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(summary.clone()));
                    }),
                )
            };
            let menu = if let Some(task) = menu_copy_task.clone() {
                menu.item(
                    PopupMenuItem::new("task_id をコピー").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(task.clone()));
                    }),
                )
            } else {
                menu
            };
            let refresh = menu_refresh.clone();
            menu.separator()
                .item(PopupMenuItem::new("更新").on_click(move |_, _, cx| {
                    refresh.update(cx, |this, cx| this.refresh_agents(cx));
                }))
        })
        .into_any_element()
}

fn render_card_grid(cards: &[AgentCardData], cx: &mut Context<ShogunWindow>) -> gpui::AnyElement {
    if cards.is_empty() {
        return div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::zouge())
            .child("（エージェントカード未取得）")
            .into_any_element();
    }

    // Fixed-width cards in a wrapping row: the column count follows the
    // window instead of being pinned at three, so a wide monitor holds a
    // wide formation rather than three columns and a plain of empty space.
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .w_full()
        .children(
            cards
                .iter()
                .map(|c| render_agent_card(c, cx))
                .collect::<Vec<_>>(),
        )
        .into_any_element()
}

/// Full-text view of one card, opened by clicking it. The card clamps its
/// summary to stay a card; this is where the whole report is readable — and
/// selectable, so a task id or a commit hash in it can be copied out.
fn render_detail_overlay(
    card: &AgentCardData,
    window: &mut gpui::Window,
    cx: &mut Context<ShogunWindow>,
) -> gpui::AnyElement {
    let status_col = status_color(&card.status);
    // Read the selection HERE, in render, and freeze it into the menu
    // builder below. The builder runs deferred, outside any view render,
    // where use_keyed_state (inside selection_of) panics on current_view —
    // that was the 0xc0000409 abort ~a minute after opening the overlay.
    // Selection changes repaint, so the frozen value is fresh at menu time.
    let selection = TextView::selection_of("agents-detail-md", window, cx);
    let meta_line = |label: &str, value: String, color: gpui::Rgba| {
        div()
            .text_xs()
            .font_family(MONO_FONT)
            .text_color(color)
            .child(format!("{label}{value}"))
    };

    div()
        .id("agents-detail-backdrop")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(gpui::rgba(0x000000B0))
        // Clicking the wash closes; the panel below stops propagation so a
        // click inside it (text selection!) does not.
        .on_click(cx.listener(|this, _, _, cx| {
            this.agents_state.selected = None;
            cx.notify();
        }))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("agents-detail-panel")
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                }))
                .w(px(560.))
                .max_h(px(640.))
                .m_4()
                .p_4()
                .rounded(px(8.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(Colors::muted())
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(status_col))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_family(MONO_FONT)
                                        .text_color(Colors::kinpaku())
                                        .child(card.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family(MONO_FONT)
                                        .text_color(status_col)
                                        .child(card.status.clone()),
                                ),
                        )
                        .child(
                            Button::new("agents-detail-close")
                                .small()
                                .label("閉じる")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.agents_state.selected = None;
                                    cx.notify();
                                })),
                        ),
                )
                .when(!is_placeholder(&card.task_id), |el| {
                    el.child(meta_line("task: ", card.task_id.clone(), Colors::zouge()))
                })
                .child(meta_line(
                    "inbox: ",
                    card.inbox_unread.to_string(),
                    if card.inbox_unread > 0 {
                        Colors::kurenai()
                    } else {
                        Colors::muted()
                    },
                ))
                .when(!is_placeholder(&card.last_report_at), |el| {
                    el.child(meta_line(
                        "",
                        format!("{} 更新", card.last_report_at),
                        Colors::muted(),
                    ))
                })
                .child(
                    // Same scroll arrangement as the dashboard: the OUTER
                    // container scrolls and TextView fits its full height, so
                    // selection tracks the text instead of sticking to the
                    // viewport (see dashboard_tab for the long version).
                    div()
                        .id("agents-detail-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(if card.summary.is_empty() {
                            div()
                                .text_xs()
                                .font_family(MONO_FONT)
                                .text_color(Colors::muted())
                                .child("（報告なし）")
                                .into_any_element()
                        } else {
                            TextView::markdown("agents-detail-md", card.summary.clone(), window, cx)
                                .text_color(Colors::zouge())
                                .selectable(true)
                                .into_any_element()
                        })
                        .context_menu({
                            let full = card.summary.clone();
                            let close = cx.entity();
                            move |menu, _window, _cx| {
                                let menu = if let Some(text) = selection.clone() {
                                    menu.item(PopupMenuItem::new("選択をコピー").on_click(
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                        },
                                    ))
                                } else {
                                    menu
                                };
                                let full = full.clone();
                                let close = close.clone();
                                menu.item(PopupMenuItem::new("全文をコピー").on_click(
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            full.clone(),
                                        ));
                                    },
                                ))
                                .separator()
                                .item(
                                    PopupMenuItem::new("閉じる").on_click(move |_, _, cx| {
                                        close.update(cx, |this, cx| {
                                            this.agents_state.selected = None;
                                            cx.notify();
                                        });
                                    }),
                                )
                            }
                        }),
                ),
        )
        .into_any_element()
}

/// One gauge row: label, filled bar coloured by pressure, percentage, reset.
fn usage_row(label: String, pct: Option<f32>, resets: Option<String>) -> gpui::AnyElement {
    let fill_color = match pct {
        Some(p) if p >= 90.0 => Colors::kurenai(),
        Some(p) if p >= 70.0 => Colors::kinpaku(),
        Some(_) => Colors::matsuba(),
        None => Colors::muted(),
    };
    let frac = pct.map(|p| (p / 100.0).clamp(0.0, 1.0)).unwrap_or(0.0);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(96.))
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(Colors::zouge())
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .h(px(10.))
                .rounded(px(5.))
                .bg(rgb(0x333333))
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac))
                        .rounded(px(5.))
                        .bg(fill_color),
                ),
        )
        .child(
            div()
                .w(px(56.))
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(fill_color)
                .child(match pct {
                    Some(p) => format!("{p:.0}%"),
                    None => "—".into(),
                }),
        )
        .child(
            div()
                .w(px(120.))
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(Colors::muted())
                .child(match resets {
                    Some(r) => format!("復活 {r}"),
                    None => String::new(),
                }),
        )
        .into_any_element()
}

/// Full-screen usage overlay: Claude and Codex subscription gauges.
pub fn render_usage_overlay(
    state: &AgentsState,
    cx: &mut Context<ShogunWindow>,
) -> gpui::AnyElement {
    let mut body: Vec<gpui::AnyElement> = Vec::new();
    if let Some(err) = &state.usage_error {
        body.push(
            div()
                .text_sm()
                .font_family(MONO_FONT)
                .text_color(Colors::kurenai())
                .child(err.clone())
                .into_any_element(),
        );
    } else if let Some(u) = &state.usage {
        body.push(section_title("Claude"));
        if u.claude_ok {
            body.push(usage_row(
                "5時間".into(),
                u.claude_five_hour_pct,
                u.claude_five_hour_resets.clone(),
            ));
            body.push(usage_row(
                "7日".into(),
                u.claude_seven_day_pct,
                u.claude_seven_day_resets.clone(),
            ));
        } else {
            body.push(unavailable_note());
        }
        body.push(section_title(match u.codex_plan.as_deref() {
            Some(plan) => format!("Codex（{plan}）"),
            None => "Codex".into(),
        }));
        if u.codex_ok {
            if let (Some(pct), Some(win)) = (u.codex_primary_pct, u.codex_primary_window) {
                body.push(usage_row(
                    window_label(win),
                    Some(pct),
                    u.codex_primary_resets.clone(),
                ));
            }
            if let (Some(pct), Some(win)) = (u.codex_secondary_pct, u.codex_secondary_window) {
                body.push(usage_row(
                    window_label(win),
                    Some(pct),
                    u.codex_secondary_resets.clone(),
                ));
            }
            if let Some(age) = u.codex_age_minutes {
                // The snapshot is only as fresh as the last codex turn.
                let (text, color) = if age > 60 {
                    (
                        format!("スナップショットは {age} 分前（codex の最終応答時点）"),
                        Colors::kinpaku(),
                    )
                } else {
                    (format!("{age} 分前の応答時点"), Colors::muted())
                };
                body.push(
                    div()
                        .text_xs()
                        .font_family(MONO_FONT)
                        .text_color(color)
                        .child(text)
                        .into_any_element(),
                );
            }
        } else {
            body.push(unavailable_note());
        }
    } else {
        body.push(
            div()
                .text_sm()
                .font_family(MONO_FONT)
                .text_color(Colors::muted())
                .child("取得中...")
                .into_any_element(),
        );
    }

    div()
        .id("agents-usage-backdrop")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(gpui::rgba(0x000000B0))
        .on_click(cx.listener(|this, _, _, cx| {
            this.agents_state.usage_visible = false;
            cx.notify();
        }))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("agents-usage-panel")
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                }))
                .w(px(640.))
                .m_4()
                .p_4()
                .rounded(px(8.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(Colors::muted())
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .font_family(MONO_FONT)
                                .text_color(Colors::kinpaku())
                                .child("サブスクリプション使用率"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(
                                    Button::new("usage-refresh")
                                        .small()
                                        .label("再取得")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.refresh_usage(cx);
                                        })),
                                )
                                .child(
                                    Button::new("usage-close").small().label("閉じる").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.agents_state.usage_visible = false;
                                            cx.notify();
                                        }),
                                    ),
                                ),
                        ),
                )
                .children(body),
        )
        .into_any_element()
}

fn section_title(text: impl Into<String>) -> gpui::AnyElement {
    div()
        .mt_1()
        .text_sm()
        .font_family(MONO_FONT)
        .text_color(Colors::zouge())
        .child(text.into())
        .into_any_element()
}

fn unavailable_note() -> gpui::AnyElement {
    div()
        .text_xs()
        .font_family(MONO_FONT)
        .text_color(Colors::muted())
        .child("（取得できず — ホスト側の認証情報を確認）")
        .into_any_element()
}

pub fn render_agents_tab(
    state: &AgentsState,
    window: &mut gpui::Window,
    cx: &mut Context<ShogunWindow>,
) -> impl IntoElement {
    let bg_color = if state.is_connected {
        Colors::matsuba()
    } else {
        Colors::kurenai()
    };

    let status_text = if let Some(err) = &state.error_message {
        err.clone()
    } else if state.is_connected {
        let secs = state.last_refresh.elapsed().unwrap_or_default().as_secs();
        format!("布陣一覧 — {}秒前に更新", secs)
    } else {
        "未接続".to_string()
    };

    let body: gpui::AnyElement = if let Some(err) = &state.error_message {
        div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::kurenai())
            .child(format!("❌ {err}"))
            .into_any_element()
    } else if !state.cards.is_empty() {
        render_card_grid(&state.cards, cx)
    } else if state.content.is_empty() {
        div()
            .text_sm()
            .font_family(MONO_FONT)
            .text_color(Colors::zouge())
            .child("（稼働確認中...）")
            .into_any_element()
    } else {
        render_ansi_lines(&state.content).into_any_element()
    };

    let detail = state
        .selected
        .as_ref()
        .and_then(|name| state.cards.iter().find(|c| &c.name == name))
        .map(|card| render_detail_overlay(card, window, cx));

    v_flex()
        .flex_1()
        .size_full()
        .relative()
        .bg(Colors::shikkoku())
        .child(
            div()
                .w_full()
                .h(px(crate::window::STATUS_BAR_HEIGHT_PX))
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .bg(bg_color)
                .text_color(Colors::zouge())
                .text_size(px(12.))
                .child(status_text)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            Button::new("agents-usage")
                                .small()
                                .label("使用率")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.agents_state.usage_visible = true;
                                    this.refresh_usage(cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("agents-refresh")
                                .small()
                                .label("更新")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_agents(cx);
                                })),
                        ),
                ),
        )
        .child(
            div()
                .id("agents-pane-content")
                .flex_1()
                // See dashboard_tab: min-height:0 keeps the status bar from
                // being squeezed by tall scrollable content.
                .min_h_0()
                .w_full()
                .bg(Colors::shikkoku())
                .overflow_y_scrollbar()
                .p_2()
                .child(body),
        )
        .when_some(detail, |el, overlay| el.child(overlay))
        .when(state.usage_visible, |el| {
            el.child(render_usage_overlay(state, cx))
        })
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
                    .map(|(r, g, b)| rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
                    .unwrap_or(Colors::zouge());
                div()
                    .text_sm()
                    .font_family(MONO_FONT)
                    .text_color(color)
                    .child(span.text)
            }))
    }))
}
