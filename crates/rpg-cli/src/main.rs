use clap::{Parser, Subcommand};
use rpg_core::{emit, event::EventKind, inbox, paths::Paths};
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
        Cmd::InstallHooks | Cmd::UninstallHooks => println!("not yet implemented"),
    });
}
