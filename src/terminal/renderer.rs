use gpui::{div, px, rgba, IntoElement, ParentElement, Styled};
use gpui_component::v_flex;

use crate::tabs::shogun_tab::MONO_FONT;
use crate::terminal::{GridSnapshot, ResolvedColor};
use crate::theme::Colors;

pub fn render_grid(snap: &GridSnapshot) -> impl IntoElement {
    v_flex()
        .font_family(MONO_FONT)
        .text_size(px(13.))
        .children(snap.cells.iter().map(|row| {
            div()
                .flex()
                .flex_row()
                .h(px(20.))
                .children(coalesce_runs(row).map(|(text, fg, bg)| {
                    let mut el = div().child(text);
                    el = match fg {
                        ResolvedColor::Rgb(r, g, b) => {
                            el.text_color(rgba(u32::from_be_bytes([r, g, b, 0xff])))
                        }
                        ResolvedColor::Default => el.text_color(Colors::zouge()),
                    };
                    el = match bg {
                        ResolvedColor::Rgb(r, g, b) => {
                            el.bg(rgba(u32::from_be_bytes([r, g, b, 0xff])))
                        }
                        ResolvedColor::Default => el,
                    };
                    el
                }))
        }))
}

fn coalesce_runs(
    cells: &[crate::terminal::SnapshotCell],
) -> impl Iterator<Item = (String, ResolvedColor, ResolvedColor)> {
    let mut runs: Vec<(String, ResolvedColor, ResolvedColor)> = Vec::new();
    for cell in cells {
        if let Some(last) = runs.last_mut() {
            if last.1 == cell.fg && last.2 == cell.bg {
                last.0.push(cell.c);
                continue;
            }
        }
        runs.push((cell.c.to_string(), cell.fg, cell.bg));
    }
    runs.into_iter()
}
