use crate::event::Event;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static GEN: Mutex<Option<ulid::Generator>> = Mutex::new(None);

/// Monotonic ULID so files written in one process sort in write order.
fn next_ulid() -> String {
    let mut g = GEN.lock().unwrap_or_else(|e| e.into_inner());
    let gen = g.get_or_insert_with(ulid::Generator::new);
    gen.generate().unwrap_or_else(|_| ulid::Ulid::new()).to_string()
}

/// Atomically write `ev` into the inbox: `<ulid>.json.tmp`, then rename to `<ulid>.json`.
pub fn write_event(inbox: &Path, ev: &Event) -> Result<()> {
    std::fs::create_dir_all(inbox).with_context(|| format!("creating {}", inbox.display()))?;
    let name = next_ulid();
    let tmp = inbox.join(format!("{name}.json.tmp"));
    let dst = inbox.join(format!("{name}.json"));
    std::fs::write(&tmp, serde_json::to_vec(ev)?).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &dst).with_context(|| format!("renaming to {}", dst.display()))?;
    Ok(())
}

/// Parse all `*.json` files in the inbox, sorted by filename, with each event's repo
/// normalized (this is the single ingest point for every source). Unparseable files move to `bad/`;
/// if that move fails the file is left in place and skipped, so one bad file never blocks the
/// rest. The caller deletes each returned file after processing it.
pub fn drain(inbox: &Path) -> Result<Vec<(PathBuf, Event)>> {
    if !inbox.exists() {
        return Ok(vec![]);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(inbox)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let mut out = Vec::new();
    for p in files {
        let parsed = std::fs::read(&p)
            .map_err(anyhow::Error::from)
            .and_then(|b| serde_json::from_slice::<Event>(&b).map_err(anyhow::Error::from));
        match parsed {
            Ok(mut ev) => {
                ev.normalize();
                out.push((p, ev));
            }
            Err(_) => {
                let _ = quarantine(inbox, &p);
            }
        }
    }
    Ok(out)
}

/// Move an inbox file into `<inbox>/bad/`.
pub fn quarantine(inbox: &Path, file: &Path) -> Result<()> {
    let bad = inbox.join("bad");
    std::fs::create_dir_all(&bad).with_context(|| format!("creating {}", bad.display()))?;
    let name = file.file_name().with_context(|| format!("no file name: {}", file.display()))?;
    std::fs::rename(file, bad.join(name)).with_context(|| format!("moving {} to bad/", file.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, Source};

    #[test]
    fn drain_normalizes_repo() {
        let dir = tempfile::tempdir().unwrap();
        let ev = Event::new(EventKind::ClaudeSessionStart, Source::Claude, Some(r"c:\work\rpg".into()), serde_json::json!({}));
        write_event(dir.path(), &ev).unwrap();
        let got = drain(dir.path()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.repo.as_deref(), Some("C:/work/rpg"));
    }
}
