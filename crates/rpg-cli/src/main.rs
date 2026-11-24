use clap::{Parser, Subcommand};
use rpg_core::{emit, event::EventKind, inbox, install, paths::Paths};
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
    install::install_claude_hooks(&settings, &current_exe()?)?;
    Ok(format!("added SessionStart/SessionEnd hooks to {} (backup: settings.json.rpg-backup)", settings.display()))
}

fn uninstall_git(git: &install::GitEnv) -> anyhow::Result<String> {
    install::uninstall_git_hooks(&Paths::resolve()?, git)?;
    Ok(format!("restored; now: {}", hooks_path_desc(git)))
}

fn uninstall_claude() -> anyhow::Result<String> {
    let settings = install::claude_settings_path()?;
    install::uninstall_claude_hooks(&settings)?;
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

fn main() {
    // Nested `claude -p` calls made by the app must be invisible.
    if std::env::var_os("RPG_INTERNAL").is_some_and(|v| v == "1") {
        return;
    }
    // Hooks must never fail the caller, so even parse errors exit 0.
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return;
        }
    };
    let _ = std::panic::catch_unwind(|| match cli.cmd {
        Cmd::Emit { what } => {
            if let Ok(paths) = Paths::resolve() {
                if let Err(e) = run_emit(&paths, what) {
                    log_line(&paths, &format!("{e:#}"));
                }
            }
        }
        Cmd::InstallHooks => install_hooks(),
        Cmd::UninstallHooks => uninstall_hooks(),
    });
}
