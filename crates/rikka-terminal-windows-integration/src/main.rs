//! RikkaTerminal handoff shim (`rikka-handoff.exe`).
//!
//! Windows 11's "default terminal application" hands a newly launched console
//! session to the registered terminal over COM (`ITerminalHandoff3`). This
//! binary is that registered terminal COM server. It lives in its OWN crate so
//! `rt` itself never links COM/handoff code — the separation the design calls
//! for: the shim owns COM, `rt` stays a plain terminal.
//!
//! # P1 — UI registration (this file)
//! An INERT placeholder. The sparse package manifest only needs this exe to
//! EXIST for RikkaTerminal to enumerate in the default-terminal dropdown;
//! nothing activates it unless the user SELECTS RikkaTerminal as the default,
//! which P1 deliberately does not do. So P1 ships a stub that does nothing.
//!
//! # P2 — shared IPC (next)
//! Implement the COM class factory + `ITerminalHandoff3`. In
//! `EstablishPtyHandoff`, `DuplicateHandle` the PTY in/out/signal pipes into
//! the running `rt` main process (start it once if absent), then send an
//! `AttachPty { .., target: NewWindow }` message over the shared named pipe.
//! The shim connects DIRECTLY to `rt` main (no launcher chain), returns S_OK,
//! and exits. Elevated handoffs branch to a new elevated `rt` (P3).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Inert until P2. COM would launch us with `-Embedding` only after
    // RikkaTerminal is selected as the default terminal (not in P1); until the
    // real class-factory message loop lands, just exit cleanly rather than
    // linger as a dead COM server.
}
