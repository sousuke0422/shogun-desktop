mod agents_tab;
mod dashboard_tab;
pub mod settings_tab;
pub mod shogun_tab;

pub use agents_tab::{render_agents_tab, run_fetch_agents};
pub use dashboard_tab::{render_dashboard_tab, run_fetch_dashboard};
pub use settings_tab::{render_settings_tab, SettingsTab};
pub use shogun_tab::{
    render_shogun_tab, run_send_command, run_send_special_key, ShogunTab,
};
