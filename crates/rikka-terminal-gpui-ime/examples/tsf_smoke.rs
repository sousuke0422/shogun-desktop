//! Headless smoke test for the TSF COM plumbing:
//! `cargo run --release -p rikka-terminal-gpui-ime --example tsf_smoke`
//! Verifies thread-manager activation, document create/push/SetFocus, blur and
//! teardown succeed in the current session — without needing the full app.
fn main() {
    println!("{}", rikka_terminal_gpui_ime::self_check());
}
