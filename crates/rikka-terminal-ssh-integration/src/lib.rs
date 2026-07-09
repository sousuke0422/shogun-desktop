//! SSH shell integration — modelled on kitty's `kitten ssh` and ghostty's `+ssh`.
//!
//! Companion to the `rikka-terminal` engine, kept out of its core: the engine
//! "knows nothing about SSH clients" (its layering rule), so the remote-shell
//! injection machinery lives here. Callers pass the terminal identity and the
//! project path; this crate returns the shell command string to run over SSH.
//!
//! Problem: `export TERM_PROGRAM=… && exec $SHELL -l` doesn't survive login
//! shell startup scripts (e.g. ghostty's integration unsets TERM_PROGRAM when
//! GHOSTTY_RESOURCES_DIR is absent).
//!
//! Solution: install wrapper files to
//! `~/.local/share/rikka-terminal/integration/{shell}/` on the remote on every
//! connection. Each wrapper sources the user's real startup file then
//! re-asserts TERM_PROGRAM — so our value wins *after* all user scripts run.
//!
//! Shell detection is done remotely via `case "$SHELL"`.
//!
//! zsh  — ZDOTDIR wrapper: four dotfiles (.zshenv/.zprofile/.zshrc/.zlogin)
//!         each source their real counterpart then re-export TERM_PROGRAM.
//!         ZDOTDIR is saved/restored around each source so zsh still finds
//!         *our* next wrapper for the subsequent startup phase.
//!
//! bash — --rcfile wrapper: a single .bashrc that sources the login profile
//!         (bash has no ZDOTDIR equivalent; --rcfile replaces .bashrc for
//!         interactive non-login shells, then we source the login profile
//!         manually to get the same PATH/env setup as -l would give).
//!
//! fish — future work (needs XDG_CONFIG_HOME wrapper).
//! other — best-effort plain `export` fallback.

// ── zsh ──────────────────────────────────────────────────────────────────────

/// Template for each zsh startup-file wrapper.  `{FILE}` is replaced with the
/// dotfile basename (`.zshenv`, `.zprofile`, `.zshrc`, `.zlogin`).
const ZSH_WRAPPER_TEMPLATE: &str = r#"# rikka-terminal zsh integration
_rikka_int="$ZDOTDIR"
_rikka_r="${RIKKA_REAL_ZDOTDIR}/{FILE}"
if [[ -f "$_rikka_r" ]]; then
    ZDOTDIR="$RIKKA_REAL_ZDOTDIR"
    builtin source "$_rikka_r"
    ZDOTDIR="$_rikka_int"
fi
export TERM_PROGRAM="$RIKKA_TERM_PROGRAM" TERM_PROGRAM_VERSION="$RIKKA_TERM_PROGRAM_VERSION" COLORTERM=truecolor
unset _rikka_int _rikka_r
"#;

const ZSH_DOTFILES: &[&str] = &[".zshenv", ".zprofile", ".zshrc", ".zlogin"];

// ── bash ─────────────────────────────────────────────────────────────────────

/// Bash --rcfile wrapper.
///
/// bash has no ZDOTDIR equivalent.  We use `exec bash --rcfile FILE -i` to
/// replace ~/.bashrc with this file for the interactive session.  The file
/// manually sources the login profile chain (.bash_profile → .bash_login →
/// .profile) so PATH and other login-time env vars are set the same as `-l`
/// would give, then re-exports TERM_PROGRAM after everything that may clear it.
const BASH_RCFILE: &str = r#"# rikka-terminal bash integration
_rikka_home="${RIKKA_REAL_HOME:-$HOME}"
if [[ -f "${_rikka_home}/.bash_profile" ]]; then
    source "${_rikka_home}/.bash_profile"
elif [[ -f "${_rikka_home}/.bash_login" ]]; then
    source "${_rikka_home}/.bash_login"
elif [[ -f "${_rikka_home}/.profile" ]]; then
    source "${_rikka_home}/.profile"
fi
export TERM_PROGRAM="$RIKKA_TERM_PROGRAM" TERM_PROGRAM_VERSION="$RIKKA_TERM_PROGRAM_VERSION" COLORTERM=truecolor
unset _rikka_home
"#;

// ── tmux title forwarding (rc route) ───────────────────────────────────────────

/// Appended to the interactive rc wrapper (bash `.bashrc`, zsh `.zshrc`) so
/// that — when the wrapper runs *inside* tmux — tmux forwards the active pane's
/// title to the outer terminal. The host reads a title spinner as progress for
/// agents that drop OSC 9;4 in a multiplexer (e.g. Claude Code).
///
/// This is the "rc route": it covers plain-ssh sessions whose login shell lands
/// in tmux, which the app's one-shot `tmux attach` prefix (only on the tmux
/// tabs) does not. Safe outside tmux — the `$TMUX` guard skips it, and `tmux
/// set -g` on a socket with no server just errors (it never starts a stray
/// one). The pane-title format is **double**-quoted on purpose: the wrapper is
/// installed via `printf '%b' '…'`, so a single quote here would end that
/// single-quoted argument early.
const TMUX_FORWARD_TITLES: &str = r##"if [[ -n "$TMUX" && -n "$RIKKA_TMUX_TITLES" ]]; then
    tmux set -g set-titles on 2>/dev/null
    tmux set -g set-titles-string "#{pane_title}" 2>/dev/null
fi
"##;

/// Exported (before `exec`) so the rc wrapper's [`TMUX_FORWARD_TITLES`] block
/// actually fires. `shell_window_cmd(.., forward_titles = true)` includes it; a
/// plain manual ssh leaves the var unset, so the block is a no-op — the whole
/// feature is opt-in, and the host drives it from a default-on setting/toggle.
const RIKKA_TMUX_TITLES_EXPORT: &str = " RIKKA_TMUX_TITLES=1";

// ── helpers ───────────────────────────────────────────────────────────────────

/// Escape a multi-line string for safe use as a `printf '%b'` argument inside
/// a single-quoted shell string.  Newlines → literal `\n`; backslashes → `\\`.
fn printf_b_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

// ── public API ────────────────────────────────────────────────────────────────

/// A POSIX shell snippet that exports the terminal identity into the remote
/// shell, for prepending to an `ssh … <cmd>` command string (e.g. before a
/// `tmux attach-session`). Ends with ` && ` so the caller appends the real
/// command directly.
///
/// Uses `export` (not inline prefix assignment) so the vars survive `exec` and
/// any subsequent `&&`-chained commands — inline prefix assignment (`VAR=val
/// cmd`) is only in effect for that single command and is gone by the next
/// `&&` clause. This is the lightweight sibling of [`shell_window_cmd`], used
/// on paths (like tmux attach) that don't start a fresh login shell and so
/// don't need the full startup-file wrappers.
///
/// For local PTY spawning, set the vars with the process env directly instead.
/// Values must be shell-safe (no spaces, quotes, or special chars).
pub fn remote_env_prefix(term_program: &str, term_program_version: &str) -> String {
    format!(
        "export TERM_PROGRAM={term_program} TERM_PROGRAM_VERSION={term_program_version} COLORTERM=truecolor && "
    )
}

/// Returns the remote SSH command that (a) installs the shell integration and
/// (b) starts an interactive shell with TERM_PROGRAM reliably set regardless of
/// what login startup scripts do to it.
///
/// # Why the payload is base64-wrapped
///
/// The raw script (see [`shell_window_script`]) contains ~110 double-quote
/// characters — every `"$HOME"`, `"$SHELL"`, `"$ZDOTDIR"` etc.  On Windows the
/// app spawns ssh via `cmd.exe /c ssh -t host "<remote-cmd>"` (ConPTY can't run
/// ssh.exe directly).  `cmd.exe` treats `"` as a quote toggle it never removes,
/// so the first inner `"` ends cmd's quoting and the remote command shatters
/// into fragments — ssh receives garbage, the integration never runs, and
/// TERM_PROGRAM stays empty in the shell.  (The tmux-attach path survives only
/// because it has *zero* inner quotes.)
///
/// Encoding the whole script as base64 makes the argument quote-free and
/// metacharacter-free (`[A-Za-z0-9+/=]` plus spaces/`|`/`>` which are harmless
/// inside cmd's outer quotes), so it crosses `cmd.exe` intact.  This is the same
/// trick kitty's `kitten ssh` and ghostty's `+ssh` use for exactly this reason.
/// The remote decodes it to a temp file and `source`s it, so the interactive
/// shell that `exec`s at the end inherits the ssh pty (not a consumed pipe) as
/// its stdin.
/// `forward_titles` opts this shell into asking tmux to forward pane titles
/// (see [`TMUX_FORWARD_TITLES`]); it exports `RIKKA_TMUX_TITLES=1` so the rc
/// wrapper acts when the shell lands in tmux. The host passes its default-on
/// setting through.
pub fn shell_window_cmd(
    term_program: &str,
    term_program_version: &str,
    project_path: &str,
    forward_titles: bool,
) -> String {
    use base64::Engine as _;
    // Self-cleanup runs first, in the sourcing shell where RIKKA_BOOT is still
    // set (the script `exec`s into a fresh shell that would not inherit it).
    // Unlinking an open, being-sourced file is safe on Unix — the fd stays
    // valid until the shell finishes reading it.
    let script = format!(
        "rm -f \"$RIKKA_BOOT\"\n{}",
        shell_window_script(
            term_program,
            term_program_version,
            project_path,
            forward_titles
        )
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    // Quote-free, so it survives Windows `cmd.exe /c ssh … "<this>"`. mktemp
    // gives a private, collision-free path for concurrent shell windows.
    format!("RIKKA_BOOT=$(mktemp) && printf %s {b64} | base64 -d > $RIKKA_BOOT && . $RIKKA_BOOT")
}

/// Builds the raw remote shell script (the `case "$SHELL"` dispatcher with the
/// zsh/bash/fallback branches).  Kept separate from [`shell_window_cmd`] so the
/// base64 delivery wrapper can be reasoned about — and tested — independently.
fn shell_window_script(
    term_program: &str,
    term_program_version: &str,
    project_path: &str,
    forward_titles: bool,
) -> String {
    // Extra env var, exported alongside the identity, that arms the rc wrapper's
    // tmux title-forwarding (empty = feature off for this shell).
    let titles = if forward_titles {
        RIKKA_TMUX_TITLES_EXPORT
    } else {
        ""
    };
    // ── zsh branch ────────────────────────────────────────────────────────────
    let mut zsh_steps: Vec<String> = vec![
        r#"_rikka_zsh="$HOME/.local/share/rikka-terminal/integration/zsh""#.into(),
        r#"mkdir -p "$_rikka_zsh""#.into(),
    ];
    for name in ZSH_DOTFILES {
        let mut content = ZSH_WRAPPER_TEMPLATE.replace("{FILE}", name);
        // Only the interactive rc needs to touch tmux, and only once.
        if *name == ".zshrc" {
            content.push_str(TMUX_FORWARD_TITLES);
        }
        let escaped = printf_b_escape(&content);
        zsh_steps.push(format!("printf '%b' '{escaped}' > \"$_rikka_zsh/{name}\""));
    }
    let zsh_install = zsh_steps.join(" && ");

    let zsh_branch = format!(
        "{zsh_install} && \
         export RIKKA_REAL_ZDOTDIR=\"${{ZDOTDIR:-$HOME}}\" \
         RIKKA_TERM_PROGRAM='{term_program}' \
         RIKKA_TERM_PROGRAM_VERSION='{term_program_version}'{titles} \
         ZDOTDIR=\"$_rikka_zsh\" && \
         cd '{project_path}' && exec zsh -l"
    );

    // ── bash branch ───────────────────────────────────────────────────────────
    let bash_rcfile = format!("{BASH_RCFILE}{TMUX_FORWARD_TITLES}");
    let bash_escaped = printf_b_escape(&bash_rcfile);
    let bash_branch = format!(
        "_rikka_bash=\"$HOME/.local/share/rikka-terminal/integration/bash\" && \
         mkdir -p \"$_rikka_bash\" && \
         printf '%b' '{bash_escaped}' > \"$_rikka_bash/.bashrc\" && \
         export RIKKA_REAL_HOME=\"$HOME\" \
         RIKKA_TERM_PROGRAM='{term_program}' \
         RIKKA_TERM_PROGRAM_VERSION='{term_program_version}'{titles} && \
         cd '{project_path}' && exec \"$SHELL\" --rcfile \"$_rikka_bash/.bashrc\" -i"
    );

    // ── fallback ──────────────────────────────────────────────────────────────
    let fallback = format!(
        "export TERM_PROGRAM='{term_program}' \
         TERM_PROGRAM_VERSION='{term_program_version}' \
         COLORTERM=truecolor && \
         cd '{project_path}' && exec $SHELL -l"
    );

    format!(
        "case \"$SHELL\" in \
         *zsh) {zsh_branch} ;; \
         *bash) {bash_branch} ;; \
         *) {fallback} ;; \
         esac"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// The delivered command must carry no double quotes: a single `"` would
    /// end `cmd.exe`'s outer quoting on Windows and shatter the ssh argument.
    /// This is the whole reason the payload is base64-wrapped — guard it.
    #[test]
    fn delivered_cmd_has_no_double_quotes() {
        let cmd = shell_window_cmd("ghostty", "1.3.1", "/home/aki/proj", true);
        assert!(
            !cmd.contains('"'),
            "remote command must be quote-free for cmd.exe; got: {cmd}"
        );
        // …even though the underlying script is full of them.
        assert!(shell_window_script("ghostty", "1.3.1", "/home/aki/proj", true).contains('"'));
    }

    /// The base64 blob must round-trip back to the real script (with the
    /// self-cleanup line prepended), so the remote runs exactly what we built.
    #[test]
    fn delivered_cmd_decodes_to_script() {
        let cmd = shell_window_cmd("ghostty", "1.3.1", "/home/aki/proj", true);
        let b64 = cmd
            .split(" | base64 -d")
            .next()
            .unwrap()
            .rsplit("printf %s ")
            .next()
            .unwrap();
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .expect("valid base64"),
        )
        .unwrap();
        assert!(decoded.starts_with("rm -f \"$RIKKA_BOOT\"\n"));
        assert!(decoded.contains(&shell_window_script(
            "ghostty",
            "1.3.1",
            "/home/aki/proj",
            true
        )));
    }

    /// The identity values reach every branch of the dispatcher.
    #[test]
    fn script_injects_identity_into_each_branch() {
        let s = shell_window_script("ghostty", "1.3.1", "/home/aki/proj", true);
        assert_eq!(s.matches("RIKKA_TERM_PROGRAM='ghostty'").count(), 2); // zsh + bash
        assert!(s.contains("TERM_PROGRAM='ghostty'")); // fallback
        assert!(s.contains("cd '/home/aki/proj'"));
        assert!(s.contains("*zsh)") && s.contains("*bash)"));
    }

    /// The interactive rc wrappers ask tmux to forward the pane title (the "rc
    /// route" so plain-ssh sessions whose login shell lands in tmux get the
    /// title-spinner progress too). The pane-title format must stay
    /// double-quoted so it survives the `printf '%b' '…'` (single-quoted)
    /// delivery of the wrapper content.
    #[test]
    fn rc_wrappers_forward_tmux_pane_title() {
        let s = shell_window_script("ghostty", "1.3.1", "/p", true);
        // bash `.bashrc` + zsh `.zshrc` (not the other three zsh dotfiles).
        assert_eq!(s.matches("tmux set -g set-titles on").count(), 2);
        assert!(s.contains("set-titles-string \"#{pane_title}\""));
        // Gated so it is inert unless both tmux and the opt-in var are present.
        assert!(s.contains("[[ -n \"$TMUX\" && -n \"$RIKKA_TMUX_TITLES\" ]]"));
    }

    /// `forward_titles` is what arms the rc route: it exports the opt-in var,
    /// and only then. Off → the wrapper's guard can never pass.
    #[test]
    fn forward_titles_flag_toggles_the_opt_in_var() {
        let on = shell_window_script("ghostty", "1.3.1", "/p", true);
        let off = shell_window_script("ghostty", "1.3.1", "/p", false);
        assert_eq!(on.matches("RIKKA_TMUX_TITLES=1").count(), 2); // zsh + bash exports
        assert!(!off.contains("RIKKA_TMUX_TITLES=1"));
        // The wrapper's guard (and its set-titles block) ship regardless; only
        // the export differs, so the feature stays fully opt-in.
        assert!(off.contains("RIKKA_TMUX_TITLES"));
    }
}
