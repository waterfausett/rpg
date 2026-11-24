//! The `rpg` command line, shared by the `rpg` binary and `rpg-app` (which handles
//! `rpg-app emit …` itself so installers need only the one binary, #12).
use clap::{Parser, Subcommand};
use crate::{emit, event::EventKind, inbox, install, paths::Paths};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "rpg", about = "Dev RPG")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Drop an event into the inbox (always exits 0)
    Emit {
        /// Use this rpg home (as `RPG_HOME` would) instead of the environment's. Installed
        /// hooks pass it so their events go to the home they were installed from.
        #[arg(long, global = true)]
        home: Option<PathBuf>,
        #[command(subcommand)]
        what: Emit,
    },
    /// Install git and Claude Code hooks
    InstallHooks,
    /// Remove git and Claude Code hooks
    UninstallHooks,
}

#[derive(Subcommand)]
enum Emit {
    Commit {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        hash: String,
    },
    Push {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        remote: String,
    },
    ClaudeSessionStart,
    ClaudeSessionEnd,
}

fn log_line(paths: &Paths, msg: &str) {
    let _ = std::fs::create_dir_all(&paths.data);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(paths.log()) {
        let _ = writeln!(f, "{} rpg emit: {}", epoch_secs(), msg.replace('\n', " "));
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn stdin_string() -> anyhow::Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

fn run_emit(paths: &Paths, what: Emit) -> anyhow::Result<()> {
    let ev = match what {
        Emit::Commit { repo, hash } => emit::commit_event(&repo, &hash)?,
        Emit::Push { repo, remote } => emit::push_event(&repo, &remote)?,
        Emit::ClaudeSessionStart => emit::claude_event(EventKind::ClaudeSessionStart, &stdin_string()?)?,
        Emit::ClaudeSessionEnd => emit::claude_event(EventKind::ClaudeSessionEnd, &stdin_string()?)?,
    };
    inbox::write_event(&paths.inbox(), &ev)
}

/// Print the outcome of one installer step; errors are reported, never fatal.
fn report(step: &str, r: anyhow::Result<String>) {
    match r {
        Ok(msg) => println!("{step}: {msg}"),
        Err(e) => println!("{step}: FAILED: {e:#}"),
    }
}

fn hooks_path_desc(git: &install::GitEnv) -> String {
    match install::global_hooks_path(git) {
        Ok(Some(p)) => format!("global core.hooksPath = {p}"),
        Ok(None) => "global core.hooksPath unset".into(),
        Err(e) => format!("global core.hooksPath unknown ({e:#})"),
    }
}

fn current_exe() -> anyhow::Result<PathBuf> {
    Ok(std::env::current_exe()?)
}

fn install_git(git: &install::GitEnv) -> anyhow::Result<String> {
    let paths = Paths::resolve()?;
    let before = hooks_path_desc(git);
    install::install_git_hooks(&paths, &current_exe()?, git)?;
    Ok(format!(
        "wrote {} shims to {}; was: {before}; now: {}",
        install::HOOK_NAMES.len(),
        paths.git_hooks().display(),
        hooks_path_desc(git)
    ))
}

fn install_claude() -> anyhow::Result<String> {
    let settings = install::claude_settings_path()?;
    install::install_claude_hooks(&settings, &current_exe()?, Paths::resolve()?.home.as_deref())?;
    Ok(format!("added SessionStart/SessionEnd hooks to {} (backup: settings.json.rpg-backup)", settings.display()))
}

fn uninstall_git(git: &install::GitEnv) -> anyhow::Result<String> {
    if !install::uninstall_git_hooks(&Paths::resolve()?, git)? {
        return Ok("not installed (global core.hooksPath unset); nothing to do".into());
    }
    Ok(format!("restored; now: {}", hooks_path_desc(git)))
}

fn uninstall_claude() -> anyhow::Result<String> {
    let settings = install::claude_settings_path()?;
    if !install::uninstall_claude_hooks(&settings)? {
        return Ok(format!("no rpg entries in {}; nothing to do", settings.display()));
    }
    Ok(format!("removed rpg entries from {}", settings.display()))
}

fn install_hooks() {
    let git = install::GitEnv::inherit();
    report("git hooks", install_git(&git));
    report("claude hooks", install_claude());
}

fn uninstall_hooks() {
    let git = install::GitEnv::inherit();
    report("git hooks", uninstall_git(&git));
    report("claude hooks", uninstall_claude());
}


/// True when `args` (as from `std::env::args_os()`) ask for `emit`: the only command
/// `rpg-app` handles itself instead of starting the tray.
pub fn is_emit<I: IntoIterator<Item = OsString>>(args: I) -> bool {
    args.into_iter().nth(1).is_some_and(|a| a == "emit")
}

/// Run the CLI with `args` (including the program name). Never panics or fails: hooks must
/// never fail the caller, so even parse errors are printed and swallowed.
pub fn run<I: IntoIterator<Item = OsString>>(args: I) {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return;
        }
    };

    let _ = std::panic::catch_unwind(|| match cli.cmd {
        Cmd::Emit { home, what } => {
            // Nested `claude -p` calls made by the app must be invisible.
            if std::env::var_os("RPG_INTERNAL").is_some_and(|v| v == "1") {
                return;
            }
            let paths = match home { Some(h) => Paths::from_home(Some(h)), None => Paths::resolve() };
            if let Ok(paths) = paths {
                if let Err(e) = run_emit(&paths, what) {
                    log_line(&paths, &format!("{e:#}"));
                }
            }
        }
        Cmd::InstallHooks => install_hooks(),
        Cmd::UninstallHooks => uninstall_hooks(),
    });
}
