use std::process::Command;

/// Build a `Command` for `program`, resolved through `which` when possible.
/// On Windows the child is spawned with `CREATE_NO_WINDOW` so no console flashes.
pub fn command(program: &str) -> Command {
    let resolved = which::which(program).unwrap_or_else(|_| program.into());
    #[allow(unused_mut)]
    let mut cmd = Command::new(resolved);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Mark a file executable (0755) on unix. On Windows git runs any hook file with a shebang.
pub fn make_executable(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The user's home dir as Node's `os.homedir()` (and so Claude Code) sees it:
/// `USERPROFILE` on Windows, `HOME` elsewhere, falling back to the platform lookup.
pub fn home_dir() -> Option<std::path::PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()))
}
