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

> **Console-CLSID caveat (resolved 2026-07-12).** The *terminal* CLSID is
> freely ours — rikka-handoff.exe registers it. The *console* CLSID is not:
> OpenConsole bakes its console-handoff CLSID per branding (WT Stable
> `{2EACA947}`, Preview `{06EC847C}`, Dev `{1F9F2BF5}`). Our vendored ConPTY
> OpenConsole turned out to be a Stable-branding build WITH the defterm role
> compiled in, so instead of implementing `IConsoleHandoff` ourselves we
> re-brand the deployed copy's baked GUID to our own fresh CLSID at install
> time — see "Console side / full Settings-UI selection" below.

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

> **Native Windows PowerShell 5.1 only (observed on 19045).** Running the
> script from pwsh 7 can fail *silently* at the final step: the
> `Import-Module Appx -UseWindowsPowerShell` bridge never reached the
> deployment service here (cert + pack succeeded, no AppXDeployment-Server
> event, package absent). The script now verifies registration and throws;
> when in doubt run `powershell.exe`, not `pwsh`.

## P3 smoke test — mixed delegation pair (validated 2026-07-12 on 19045)

The two `HKCU\Console\%%Startup` values are read independently, so our
terminal side can be exercised *without* implementing `IConsoleHandoff`:
keep `DelegationConsole` = WT Stable's OpenConsole and point only
`DelegationTerminal` at us. WT's OpenConsole reads the terminal CLSID from
`DelegationConfig` (not baked), so it CoCreates rikka-handoff.

```text
reg add "HKCU\Console\%%Startup" /v DelegationTerminal /t REG_SZ /d "{0DA1B045-A599-4133-A9EE-A7A3893E1D62}" /f
:: DelegationConsole stays {2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69} (WT Stable OpenConsole)
```

Test by launching a console app *outside* any terminal (Win+R → `cmd`);
launching inside an existing terminal window attaches to its ConPTY and no
handoff happens. Failures land in `%TEMP%\rikka-handoff.log`. Roll back any
time:

```text
reg add "HKCU\Console\%%Startup" /v DelegationTerminal /t REG_SZ /d "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}" /f
```

COM plumbing can be pre-verified with no delegation change at all —
`[Activator]::CreateInstance([Type]::GetTypeFromCLSID('{0DA1B045-A599-4133-A9EE-A7A3893E1D62}'))`
must start `rikka-handoff.exe` from the external location (idles out after
60 s; passed 2026-07-12).

## Console side / full Settings-UI selection (validated 2026-07-12)

Selecting RikkaTerminal in the Settings UI writes BOTH pair values from our
package, so the console CLSID needs a live COM server behind it too. Binary
inspection settled the old "does the vendored ConPTY OpenConsole carry the
defterm role?" question: it does — it is a *Stable-branding* build with the
`-Embedding` handoff server, `%%Startup` delegation lookup and
ITerminalHandoff3 compiled in. But that also means it *bakes WT Stable's*
console CLSID `{2EACA947-…}`, which would collide with an installed Windows
Terminal if we declared it. The install script therefore re-brands the
DEPLOYED copy: the two `.rdata` GUID constants are patched to our
`{77F531BA-46BD-4E80-B0DF-8E45E1F7183B}` (the vendored asset stays
pristine; MIT permits it; ConPTY serving never reads that constant). After
patching, `CoCreateInstance` probes pass for BOTH pair CLSIDs, so going
live is simply: **Settings > For developers > Default terminal application
(or WT Settings > Startup) → RikkaTerminal**. Roll back by selecting
Windows Terminal there again — the Settings app is not a console app, so
it keeps working even if a handoff regression breaks console launches.

## Confirm on-device

Whether the explicit `com:Extension` (comServer) block is required, or the
FullTrust exe self-registering its class factory (`-Embedding`) suffices, needs
a Win11 check — the manifest includes the explicit form as the safe default.
