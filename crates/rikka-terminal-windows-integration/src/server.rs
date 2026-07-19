//! The terminal-side COM server for the OS default-terminal handoff.
//!
//! Flow: conhost (or a delegated OpenConsole) reads the DelegationTerminal
//! CLSID from `HKCU\Console\%%Startup`, `CoCreateInstance`s it — COM starts
//! this exe with `-Embedding` — and calls `EstablishPtyHandoff`. We create
//! the VT pipe pair (a v3 responsibility: the terminal picks the buffer
//! size), forward our ends plus the ConPTY lifetime handles to the running
//! `rikka-terminal` monarch as an IPC `attach` (the monarch pulls them with
//! `DuplicateHandle`; see IPC.md), return the console's pipe ends through
//! the `[out]` params, and exit. One activation = one handoff = one process
//! (`REGCLS_SINGLEUSE`).

// COM naming (the IDL's PascalCase methods and Hungarian struct fields) is
// kept verbatim so the code reads against microsoft/terminal's contract; the
// #[interface] macro rejects a per-trait allow, so it sits module-wide.
#![allow(non_snake_case)]

use std::mem::ManuallyDrop;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context as _;
use rikka_terminal_ipc as ipc;
use windows::Win32::Foundation::{
    CLASS_E_NOAGGREGATION, E_FAIL, HANDLE, HANDLE_FLAG_INHERIT, S_OK, SetHandleInformation,
};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoInitializeEx, CoRegisterClassObject,
    CoRevokeClassObject, CoUninitialize, IClassFactory, IClassFactory_Impl, REGCLS_SINGLEUSE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::GetCurrentProcessId;
// IUnknown_Vtbl looks unused but the #[interface] expansion names it (the
// generated vtable embeds the parent's as `base__`).
use windows_core::{
    BOOL, BSTR, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface as _, Ref, implement, interface,
};

/// DelegationTerminal CLSID (minted 2026-07-12). MUST match the `<Clsid>` of
/// the `com.microsoft.windows.terminal.host` appExtension AND the `com:Class`
/// of the rikka-handoff `com:ExeServer` in
/// `crates/rikka-terminal/packaging/AppxManifest.xml` — COM activates this
/// exe for that class, and this factory answers for the same GUID. (The
/// interface IID `6F23DA90-…` is a different thing: that one is WT's
/// published ITerminalHandoff3 contract and must NOT be fresh.)
const CLSID_RIKKA_TERMINAL_HANDOFF: GUID = GUID::from_u128(0x0DA1B045_A599_4133_A9EE_A7A3893E1D62);

/// `STARTF_USECOUNTCHARS` — only then do `dwXCountChars`/`dwYCountChars`
/// carry a requested console size.
const STARTF_USECOUNTCHARS: u32 = 0x8;

/// `TERMINAL_STARTUP_INFO` from microsoft/terminal's ITerminalHandoff.idl:
/// the launching app's STARTUPINFO, relayed by ConPTY. Arrives by const
/// pointer (`[in]`); the RPC stub owns the BSTRs — `ManuallyDrop` keeps this
/// side from double-freeing them.
#[repr(C)]
struct TERMINAL_STARTUP_INFO {
    pszTitle: ManuallyDrop<BSTR>,
    pszIconPath: ManuallyDrop<BSTR>,
    iconIndex: i32,
    dwX: u32,
    dwY: u32,
    dwXSize: u32,
    dwYSize: u32,
    dwXCountChars: u32,
    dwYCountChars: u32,
    dwFillAttribute: u32,
    dwFlags: u32,
    wShowWindow: u16,
}

/// `ITerminalHandoff3` (microsoft/terminal, src/host/proxy/ITerminalHandoff.idl).
///
/// v3 flipped `in`/`out` to `[out]`: the TERMINAL creates the VT pipe pair
/// (so it controls buffering) and returns the console's ends; `signal`,
/// `reference`, `server` and `client` stay `[in]` because ConPTY creates
/// them internally. The IDL's `system_handle` marshaling means every `[in]`
/// HANDLE arrives already duplicated into this process (ours to keep), and
/// the `[out]` HANDLEs are duplicated to the caller while the reply
/// marshals — strictly AFTER this method returns — so our copies of the
/// console-side ends must stay open until the process exits.
#[interface("6F23DA90-15C5-4203-9DB0-64E73F1B1B00")]
unsafe trait ITerminalHandoff3: IUnknown {
    unsafe fn EstablishPtyHandoff(
        &self,
        input: *mut HANDLE,
        output: *mut HANDLE,
        signal: HANDLE,
        reference: HANDLE,
        server: HANDLE,
        client: HANDLE,
        startup_info: *const TERMINAL_STARTUP_INFO,
    ) -> HRESULT;
}

/// One handoff per activation: relay to the monarch, signal `run`, done.
#[implement(ITerminalHandoff3)]
struct Handoff {
    done: mpsc::Sender<()>,
}

impl ITerminalHandoff3_Impl for Handoff_Impl {
    unsafe fn EstablishPtyHandoff(
        &self,
        input: *mut HANDLE,
        output: *mut HANDLE,
        signal: HANDLE,
        reference: HANDLE,
        server: HANDLE,
        client: HANDLE,
        startup_info: *const TERMINAL_STARTUP_INFO,
    ) -> HRESULT {
        let relayed = establish(
            input,
            output,
            signal,
            reference,
            server,
            client,
            startup_info,
        );
        let _ = self.done.send(());
        match relayed {
            Ok(()) => S_OK,
            Err(e) => {
                log(&format!("handoff failed: {e:#}"));
                E_FAIL
            }
        }
    }
}

/// A handle value as it travels in the IPC message: the receiver interprets
/// it inside OUR process via `DuplicateHandle` (IPC.md, receiver-pulls).
fn raw(h: HANDLE) -> i64 {
    h.0 as isize as i64
}

/// An anonymous, non-inheritable pipe pair — the handles cross process
/// boundaries by duplication (COM reply marshaling / the monarch's pull),
/// never by inheritance.
fn create_pipe() -> anyhow::Result<(HANDLE, HANDLE)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // 128 KiB, matching the conhost-side buffer appetite; 0 would mean the
    // 4 KiB default and more round-trips for full-screen repaints.
    unsafe { CreatePipe(&mut read, &mut write, None, 128 * 1024) }.context("CreatePipe")?;
    Ok((read, write))
}

/// Create the VT pipes, relay the session to the monarch, hand the console
/// its pipe ends. On error the created ends leak — the process exits
/// moments later and the OS reclaims them.
fn establish(
    input: *mut HANDLE,
    output: *mut HANDLE,
    signal: HANDLE,
    reference: HANDLE,
    server: HANDLE,
    client: HANDLE,
    startup_info: *const TERMINAL_STARTUP_INFO,
) -> anyhow::Result<()> {
    if input.is_null() || output.is_null() {
        anyhow::bail!("null [out] pipe params");
    }
    // The v3 point — terminal-created pipe pairs. We keep the writing end of
    // the console's input and the reading end of its output; the opposite
    // ends go back through the [out] params.
    let (console_in_read, our_in_write) = create_pipe()?;
    let (our_out_read, console_out_write) = create_pipe()?;

    let startup = unsafe { startup_info.as_ref() }
        .map(|si| {
            let title = si.pszTitle.to_string();
            let counted = si.dwFlags & STARTF_USECOUNTCHARS != 0;
            let dim = |v: u32| {
                if counted {
                    v.min(u32::from(u16::MAX)) as u16
                } else {
                    0
                }
            };
            ipc::StartupInfo {
                title: (!title.is_empty()).then_some(title),
                x: 0,
                y: 0,
                cols: dim(si.dwXCountChars),
                rows: dim(si.dwYCountChars),
            }
        })
        .unwrap_or_default();

    let args = ipc::AttachArgs {
        pid: unsafe { GetCurrentProcessId() },
        handles: ipc::Handles {
            input: raw(our_in_write),
            output: raw(our_out_read),
            signal: raw(signal),
            reference: raw(reference),
            server: raw(server),
            client: raw(client),
            ..Default::default()
        },
        startup,
        state: None,
        elevated: false,
        target: ipc::Target::New,
        drop_at: None,
        // A cold start has no profile theme to carry.
        palette: None,
    };

    let endpoint = ipc::transport::endpoint_name();
    match ipc::transport::connect(&endpoint) {
        // Warm: a monarch is running — it pulls our handles while we block
        // on the response (DUPLICATE_CLOSE_SOURCE closes our copies).
        Ok(mut conn) => {
            conn.send_request(&ipc::Request::Attach(args))?;
            let resp = conn.recv_response()?;
            if !resp.ok {
                anyhow::bail!(
                    "monarch rejected the handoff: {}",
                    resp.error.unwrap_or_default()
                );
            }
        }
        // Cold: no monarch — the attach rides in the launch itself (IPC.md
        // "attach cold"), so there is no wait-for-server race to lose.
        Err(_) => cold_start(&args)?,
    }

    // Our forwarded handles are gone (pulled warm, or inherited cold and
    // owned by the child now — the flagged copies die with this process);
    // only the console-side pipe ends are still meaningfully ours. Hand them
    // out — reply marshaling duplicates them into the caller.
    unsafe {
        *input = console_in_read;
        *output = console_out_write;
    }
    Ok(())
}

/// IPC.md "attach cold": flag the six handles inheritable and start
/// `rikka-terminal` with their raw values on `--attach`; inheritance IS the
/// transfer. The child adopts them and becomes the monarch — or loses the
/// bind race to a sibling and forwards a normal attach from its own process
/// (the values are valid there either way). No post-spawn handshake: the
/// console's writes simply buffer in the pipes until the child drains them.
fn cold_start(args: &ipc::AttachArgs) -> anyhow::Result<()> {
    let handles = [
        args.handles.input,
        args.handles.output,
        args.handles.signal,
        args.handles.reference,
        args.handles.server,
        args.handles.client,
    ];
    for raw in handles {
        if raw != 0 {
            unsafe {
                SetHandleInformation(
                    HANDLE(raw as isize as _),
                    HANDLE_FLAG_INHERIT.0,
                    HANDLE_FLAG_INHERIT,
                )
            }
            .context("SetHandleInformation(HANDLE_FLAG_INHERIT)")?;
        }
    }
    let exe = std::env::current_exe()
        .context("current_exe")?
        .with_file_name("rikka-terminal.exe");
    let csv = handles.map(|v| v.to_string()).join(",");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--attach").arg(csv);
    if let Some(title) = &args.startup.title {
        cmd.arg("--attach-title").arg(title);
    }
    if args.startup.cols >= 2 && args.startup.rows >= 2 {
        cmd.arg("--size")
            .arg(format!("{},{}", args.startup.cols, args.startup.rows));
    }
    // Rust's std spawns with bInheritHandles=TRUE (its STARTF_USESTDHANDLES
    // stdio plumbing requires it) — the "handle leak" footgun is exactly the
    // transfer we want here.
    cmd.spawn()
        .with_context(|| format!("cold start: spawn {}", exe.display()))?;
    Ok(())
}

/// The class factory COM asks for after launching us. Rejects aggregation;
/// each `CreateInstance` mints a fresh single-shot [`Handoff`].
#[implement(IClassFactory)]
struct HandoffFactory {
    done: mpsc::Sender<()>,
}

impl IClassFactory_Impl for HandoffFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let handoff: ITerminalHandoff3 = Handoff {
            done: self.done.clone(),
        }
        .into();
        unsafe { handoff.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Best-effort trace for a headless COM process: `%TEMP%\rikka-handoff.log`.
/// Without it a failed handoff during device testing is indistinguishable
/// from "never activated".
fn log(msg: &str) {
    let path = std::env::temp_dir().join("rikka-handoff.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "[{:?}] {msg}", std::time::SystemTime::now());
    }
}

pub fn run() {
    if let Err(e) = serve() {
        log(&format!("fatal: {e:#}"));
    }
}

fn serve() -> anyhow::Result<()> {
    // MTA: the handoff call may arrive on any RPC thread; nothing here needs
    // a message pump.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .context("CoInitializeEx")?;
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let factory: IClassFactory = HandoffFactory { done: done_tx }.into();
    let cookie = unsafe {
        CoRegisterClassObject(
            &CLSID_RIKKA_TERMINAL_HANDOFF,
            &factory,
            CLSCTX_LOCAL_SERVER,
            REGCLS_SINGLEUSE,
        )
    }
    .context("CoRegisterClassObject")?;

    // One handoff (SINGLEUSE), or a 60 s idle timeout (activated but never
    // called — the launching conhost died). Never linger as a ghost server.
    let _ = done_rx.recv_timeout(Duration::from_secs(60));
    // Grace beat: the [out] pipe handles are duplicated to the caller when
    // the RPC reply marshals, strictly after EstablishPtyHandoff returns.
    // Exiting on the done signal alone could win that race and invalidate
    // the handles before the duplication.
    std::thread::sleep(Duration::from_millis(500));

    unsafe {
        let _ = CoRevokeClassObject(cookie);
        CoUninitialize();
    }
    Ok(())
}
