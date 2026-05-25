mod agents_tab;
mod dashboard_tab;
pub mod settings_tab;
pub mod shogun_tab;
pub mod terminal_tab;

pub use agents_tab::{render_agents_tab, run_fetch_agents};
pub use dashboard_tab::{render_dashboard_tab, run_fetch_dashboard};
pub use settings_tab::{render_settings_tab, SettingsTab};
pub use terminal_tab::{
    render_terminal_tab, render_terminal_tab_disconnected, render_terminal_tab_empty,
    render_terminal_tab_error,
};
