mod agents_tab;
mod dashboard_tab;
pub mod settings_tab;
mod shogun_tab;

pub use agents_tab::render_agents_tab;
pub use dashboard_tab::render_dashboard_tab;
pub use settings_tab::{render_settings_tab, SettingsTab};
pub use shogun_tab::render_shogun_tab;
