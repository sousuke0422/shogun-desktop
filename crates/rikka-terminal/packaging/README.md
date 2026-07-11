# RikkaTerminal as a Windows default terminal

Make RikkaTerminal selectable (and eventually the active choice) in the
Windows 10/11 **Default terminal application** dropdown — without packaging `rt`
itself into MSIX and without `rt` ever linking COM.

## Mechanism (verified against microsoft/terminal)

Windows hands a newly launched console session to the registered terminal
over COM. Two roles, each a CLSID:

| Role | Interface | Provider here |
|------|-----------|---------------|
| Console host | `IConsoleHandoff` | vendored `OpenConsole.exe`, or our own impl — CLSID caveat below |
| Terminal | `ITerminalHandoff3` (`EstablishPtyHandoff`) | `rikka-handoff.exe` (the shim) |

> **Console-CLSID caveat (a P2 decision).** The *terminal* CLSID is freely ours
> — rikka-handoff.exe registers it. The *console* CLSID is not: OpenConsole
> bakes its console-handoff CLSID per build (WT Stable `{2EACA947}` vs Preview
> `{06EC847C}` differ), so reusing OpenConsole means matching ITS baked value
> (collision-prone, and the ConPTY build may lack the defterm role). Existing
> third parties (contour/wezterm) lean toward implementing `IConsoleHandoff`
> themselves instead — own a fresh console CLSID in rikka-handoff.exe too. For
> P1 (enumeration only) any unique GUID works.

The dropdown is enumerated from two well-known `windows.appExtension` entries in
the package manifest — `com.microsoft.windows.console.host` and
`com.microsoft.windows.terminal.host`, each carrying a `<Clsid>`. The
`DisplayName` is the label shown. Selecting an entry just writes that provider's
two CLSIDs into `HKCU\Console\%%Startup` (`DelegationConsole` /
`DelegationTerminal`); every other registered provider stays inert. This is how
WT Stable/Preview/Canary coexist — we're just another distinct provider.

The dropdown pictured is Windows Terminal's own (Settings > Startup), so this
works on **Windows 10 (2004+) as well as 11** — confirmed on 19045 (Win10
22H2), where RikkaTerminal lists in Settings > For developers > Default
terminal application AND in Windows Terminal's own Startup dropdown. Sparse external-location packages need
Win10 2004 (build 19041) or newer.

`rt` stays a plain unpackaged exe: a **sparse package with external location**
(`uap10:AllowExternalContent`) gives *identity* to binaries that live in a
normal folder. The COM/handoff code lives in its own crate
(`crates/rikka-terminal-windows-integration`), never in `rt`.

## Phases

- **P1 — UI registration (this folder).** Install the sparse package so
  "RikkaTerminal" appears as a *choice* in the dropdown. Do NOT select it, so
  the active default is unchanged and your in-use consoles are untouched.
  `rikka-handoff.exe` is an inert stub in P1.
- **P2 — shared IPC.** One named pipe to the single `rt` main instance, used by
  BOTH the `rt` launcher (Spawn) and the shim (AttachPty). The shim
  `DuplicateHandle`s the PTY handles into `rt` and sends AttachPty — connecting
  DIRECTLY to main (no launcher chain). OS handoffs default to a **new window**
  (never an auto-tab).
- **P3 — go live.** Flip `%%Startup` to RikkaTerminal and validate a real
  handoff end to end (incl. the elevated-handoff -> new elevated process
  branch). Disruptive, so done when the machine is free.

## Files

| File | What |
|------|------|
| `AppxManifest.xml` | Sparse manifest: identity + the two `windows.appExtension` handoff declarations + CLSID->exe map. Has `REPLACE_ME` markers. |
| `Images/` | Placeholder logos (solid #202020) so MSIX validates. |
| `install-default-terminal.ps1` | Build -> self-sign -> pack -> install. **Run on your own machine.** `-Uninstall` to remove. |
| (`rikka-handoff.exe`) | Built from `crates/rikka-terminal-windows-integration`; P1 stub, P2 real shim. |

## Running P1 (on your machine)

1. In `AppxManifest.xml`, replace `REPLACE_ME` Publisher + the two GUIDs
   (`[guid]::NewGuid()`), and make the script's `-Publisher` match exactly.
2. `cargo build --release -p rikka-terminal -p rikka-terminal-windows-integration`
   (produces `rikka-terminal.exe` + `rikka-handoff.exe` in `target\release`).
3. From an **admin** shell (once, for cert trust):
   `crates\rikka-terminal\packaging\install-default-terminal.ps1`
4. Open **Windows Terminal > Settings > Startup > Default terminal
   application** (on Win11, also Settings > System > For developers > Terminal)
   and confirm **RikkaTerminal** is listed. **Do not select it** (that's P3).

`%%Startup` is never written by the script — installing only adds the choice.

## Confirm on-device

Whether the explicit `com:Extension` (comServer) block is required, or the
FullTrust exe self-registering its class factory (`-Embedding`) suffices, needs
a Win11 check — the manifest includes the explicit form as the safe default.
