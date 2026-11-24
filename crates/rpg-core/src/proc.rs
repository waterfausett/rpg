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
