//! RikkaTerminal handoff shim (`rikka-handoff.exe`).
//!
//! Windows' "default terminal application" hands a newly launched console
//! session to the registered terminal over COM (`ITerminalHandoff3`). This
//! binary is that COM server. It lives in its OWN crate so `rt` itself never
//! links COM/handoff code — the shim owns COM, `rt` stays a plain terminal.
//!
//! P1 shipped it as an inert dropdown placeholder; P2 (this) implements the
//! server: COM activates the exe with `-Embedding` only once the user has
//! selected RikkaTerminal as the default terminal, `EstablishPtyHandoff`
//! relays the console session to the running `rikka-terminal` monarch over
//! the shared IPC (see `server`), and the process exits. Without a running
//! monarch it cold-starts one instead — `rikka-terminal --attach` with the
//! handle values, transferred by CreateProcess inheritance (IPC.md "attach
//! cold").
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod server;

fn main() {
    // COM launches a LocalServer32 with `-Embedding`; anything else is a
    // manual or installer launch and stays inert (P1 behavior — the sparse
    // package only needs the exe to exist).
    #[cfg(windows)]
    if std::env::args()
        .any(|a| a.eq_ignore_ascii_case("-embedding") || a.eq_ignore_ascii_case("/embedding"))
    {
        server::run();
    }
}
