//! The Settings window's sound rows: which folder and config key each slot uses,
//! whether a pin exists, what plays when nothing is pinned, and safe file copying.
use crate::presenter::SoundKey;
use crate::sound::{is_audio, resolve_sound};
use rpg_core::config::SoundConfig;
use rpg_core::rules::Tier;
use std::path::{Path, PathBuf};

const ACHIEVEMENTS: &str = "achievements";
const LEVELS: &str = "levels";

const TIERS: [Tier; 6] = [Tier::Bronze, Tier::Silver, Tier::Gold, Tier::Platinum, Tier::Legendary, Tier::Celestial];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Achievement,
    LevelUp,
    Tier(Tier),
}

impl Slot {
    pub fn parse(s: &str) -> Option<Slot> {
        match s {
            "achievement" => Some(Slot::Achievement),
            "level_up" => Some(Slot::LevelUp),
            _ => {
                let t = s.strip_prefix("tier:")?;
                TIERS.iter().find(|x| x.as_str() == t).map(|x| Slot::Tier(*x))
            }
        }
    }

    pub fn id(&self) -> String {
        match self {
            Slot::Achievement => "achievement".into(),
            Slot::LevelUp => "level_up".into(),
            Slot::Tier(t) => format!("tier:{}", t.as_str()),
        }
    }

    pub fn config_key(&self) -> String {
        match self {
            Slot::Achievement => "sound.achievement".into(),
            Slot::LevelUp => "sound.level_up".into(),
            Slot::Tier(t) => format!("sound.by_tier.{}", t.as_str()),
        }
    }

    /// The 8 slots in UI order.
    pub fn all() -> Vec<Slot> {
        let mut v = vec![Slot::Achievement, Slot::LevelUp];
        v.extend(TIERS.iter().map(|t| Slot::Tier(*t)));
        v
    }

    pub fn folder(&self, root: &Path) -> PathBuf {
        match self {
            Slot::Achievement => root.join(ACHIEVEMENTS),
            Slot::LevelUp => root.join(LEVELS),
            Slot::Tier(t) => root.join(ACHIEVEMENTS).join(t.as_str()),
        }
    }

    pub fn sound_key(&self) -> SoundKey {
        match self {
            Slot::Achievement => SoundKey::Summary,
            Slot::LevelUp => SoundKey::LevelUp,
            Slot::Tier(t) => SoundKey::Achievement { rule_id: String::new(), tier: *t },
        }
    }

    pub fn pinned<'a>(&self, cfg: &'a SoundConfig) -> Option<&'a String> {
        match self {
            Slot::Achievement => cfg.achievement.as_ref(),
            Slot::LevelUp => cfg.level_up.as_ref(),
            Slot::Tier(t) => cfg.by_tier.get(t.as_str()),
        }
    }

    /// Folders searched for a pin, most specific first.
    fn pin_dirs(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = vec![self.folder(root)];
        if matches!(self, Slot::Tier(_)) {
            v.push(root.join(ACHIEVEMENTS));
        }
        v.push(root.to_path_buf());
        v
    }

    /// Folders making up the slot's own random pool.
    fn pool_dirs(&self, root: &Path) -> Vec<PathBuf> {
        match self {
            Slot::Tier(_) => vec![self.folder(root)],
            _ => vec![self.folder(root), root.to_path_buf()],
        }
    }
}

fn in_dirs<'a>(files: &'a [PathBuf], dirs: &'a [PathBuf]) -> impl Iterator<Item = &'a PathBuf> {
    files.iter().filter(move |f| f.parent().is_some_and(|p| dirs.iter().any(|d| d == p)))
}

/// Whether a file called `name` is where `resolve_sound` would find a pin for `slot`.
pub fn pin_exists(slot: Slot, name: &str, root: &Path, files: &[PathBuf]) -> bool {
    in_dirs(files, &slot.pin_dirs(root)).any(|f| f.file_name().is_some_and(|n| n == name))
}

/// What plays for `slot` when nothing is pinned: `"pool"`, `"default"` or `"none"`.
pub fn fallback(slot: Slot, cfg: &SoundConfig, root: &Path, files: &[PathBuf]) -> &'static str {
    if in_dirs(files, &slot.pool_dirs(root)).any(|f| is_audio(f)) {
        return "pool";
    }
    match slot {
        Slot::Tier(_) if resolve_sound(&SoundKey::Summary, cfg, root, files).is_some() => "default",
        _ => "none",
    }
}

/// Where `copy_into` put the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// The file name as it is on disk.
    pub name: String,
    /// `true` when this call wrote a new file; `false` when an identical file was reused.
    pub created: bool,
}

/// Copy `src` into the slot's folder without overwriting anything.
/// Identical bytes reuse the existing file; a clash gets `stem (2).ext`, `stem (3).ext`, ...
pub fn copy_into(slot: Slot, src: &Path, root: &Path) -> std::io::Result<Copied> {
    use std::io::Write;
    let dir = slot.folder(root);
    std::fs::create_dir_all(&dir)?;
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad file name"))?;
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let bytes = std::fs::read(src)?;
    for n in 1u32.. {
        let cand = if n == 1 { name.to_string() } else { format!("{stem} ({n}){ext}") };
        let dest = dir.join(&cand);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&dest) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&bytes) {
                    drop(f);
                    let _ = std::fs::remove_file(&dest);
                    return Err(e);
                }
                return Ok(Copied { name: cand, created: true });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(&dest).is_ok_and(|b| b == bytes) {
                    return Ok(Copied { name: on_disk_name(&dir, cand), created: false });
                }
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// The spelling of `name` as stored in `dir`. On a case-insensitive file system `ding.wav` can name
/// `Ding.wav`; pins are matched exactly, so the stored spelling is the one to pin.
fn on_disk_name(dir: &Path, name: String) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else { return name };
    let names: Vec<String> = entries.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
    if names.contains(&name) {
        return name;
    }
    names.into_iter().find(|n| n.to_lowercase() == name.to_lowercase()).unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(root: &Path, names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| root.join(n)).collect()
    }

    #[test]
    fn slot_ids_round_trip_and_keys() {
        assert_eq!(Slot::all().len(), 8);
        for s in Slot::all() {
            assert_eq!(Slot::parse(&s.id()), Some(s));
        }
        assert_eq!(Slot::Tier(Tier::Gold).config_key(), "sound.by_tier.gold");
        assert_eq!(Slot::parse("tier:nope"), None);
    }

    #[test]
    fn fallback_labels() {
        let r = Path::new("/s");
        let c = SoundConfig::default();
        let gold = Slot::Tier(Tier::Gold);
        assert_eq!(fallback(Slot::Achievement, &c, r, &[]), "none");
        assert_eq!(fallback(gold, &c, r, &[]), "none");
        let f = fs(r, &["achievements/a.mp3"]);
        assert_eq!(fallback(Slot::Achievement, &c, r, &f), "pool");
        assert_eq!(fallback(gold, &c, r, &f), "default");
        let f = fs(r, &["achievements/gold/g.mp3"]);
        assert_eq!(fallback(gold, &c, r, &f), "pool");
        let f = fs(r, &["levels/l.mp3"]);
        assert_eq!(fallback(Slot::LevelUp, &c, r, &f), "pool");
        assert_eq!(fallback(Slot::Achievement, &c, r, &f), "none");
    }

    #[test]
    fn tier_slot_key_plays_the_tier_pin() {
        let r = Path::new("/s");
        let gold = Slot::Tier(Tier::Gold);
        let mut c = SoundConfig::default();
        c.by_tier.insert("gold".into(), "g.wav".into());
        let f = fs(r, &["achievements/a.mp3", "achievements/gold/g.wav"]);
        for _ in 0..50 {
            assert_eq!(resolve_sound(&gold.sound_key(), &c, r, &f), Some(r.join("achievements/gold/g.wav")));
        }
    }

    #[test]
    fn pin_exists_searches_like_resolver() {
        let r = Path::new("/s");
        let gold = Slot::Tier(Tier::Gold);
        assert!(pin_exists(gold, "gong.wav", r, &fs(r, &["gong.wav"])));
        assert!(!pin_exists(gold, "gong.wav", r, &fs(r, &["levels/gong.wav"])));
    }

    #[test]
    fn copy_into_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("Fan Fare é.mp3");
        std::fs::write(&src, b"one").unwrap();
        let slot = Slot::Achievement;
        let copied = |name: &str, created| Copied { name: name.into(), created };
        assert_eq!(copy_into(slot, &src, tmp.path()).unwrap(), copied("Fan Fare é.mp3", true));
        assert_eq!(copy_into(slot, &src, tmp.path()).unwrap(), copied("Fan Fare é.mp3", false), "identical bytes are reused, not created");
        std::fs::write(&src, b"two").unwrap();
        assert_eq!(copy_into(slot, &src, tmp.path()).unwrap(), copied("Fan Fare é (2).mp3", true));
        let dir = slot.folder(tmp.path());
        assert_eq!(std::fs::read(dir.join("Fan Fare é.mp3")).unwrap(), b"one");
        assert_eq!(std::fs::read(dir.join("Fan Fare é (2).mp3")).unwrap(), b"two");
    }

    /// On a case-insensitive file system, `ding.wav` lands on an existing `Ding.wav`. Reusing it must
    /// report the name as it is on disk (pins match file names exactly) and must not claim to have created it.
    #[cfg(windows)]
    #[test]
    fn reuse_reports_the_on_disk_name_and_not_created() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let slot = Slot::Tier(Tier::Bronze);
        let dir = slot.folder(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Ding.wav"), b"same").unwrap();
        let src = src_dir.path().join("ding.wav");
        std::fs::write(&src, b"same").unwrap();
        assert_eq!(copy_into(slot, &src, tmp.path()).unwrap(), Copied { name: "Ding.wav".into(), created: false });
        assert_eq!(std::fs::read(dir.join("Ding.wav")).unwrap(), b"same");
    }
}
