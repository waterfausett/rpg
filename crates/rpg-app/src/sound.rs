//! Banner sounds: pick a file from `sounds/` per config and play it with rodio.
use crate::presenter::SoundKey;
use rpg_core::config::SoundConfig;
use std::path::{Path, PathBuf};

const ACHIEVEMENTS: &str = "achievements";
const LEVELS: &str = "levels";

/// Pick the sound for `key` among `files` (every file under `root`, the `sounds/` folder).
///
/// Achievements look, most specific first, at: a `by_rule` pin, a random file in
/// `achievements/<rule_id>/`, a `by_tier` pin, `achievements/<tier>/`, the `achievement`
/// pin, then a random file from `achievements/` plus the root. Level-ups use `level_up`,
/// then `levels/` plus the root. Summary banners use the achievement pin and pool. A pin
/// is found in its own folder, then the type's folder, then the root; names that aren't
/// found are skipped. Files in the root belong to every pool.
pub fn resolve_sound(key: &SoundKey, cfg: &SoundConfig, root: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    // Files directly inside `root/<dir...>`.
    let in_dir = |dir: &[&str]| -> Vec<&PathBuf> {
        let want = dir.iter().fold(root.to_path_buf(), |p, d| p.join(d));
        files.iter().filter(|f| f.parent() == Some(want.as_path())).collect()
    };
    let pin = |name: Option<&String>, dirs: &[&[&str]]| -> Option<PathBuf> {
        let name = name?;
        dirs.iter().find_map(|d| in_dir(d).into_iter().find(|f| f.file_name().is_some_and(|n| n == name.as_str())).cloned())
    };
    let random = |dirs: &[&[&str]]| -> Option<PathBuf> {
        let audio: Vec<&PathBuf> = dirs.iter().flat_map(|d| in_dir(d)).filter(|f| is_audio(f)).collect();
        (!audio.is_empty()).then(|| audio[fastrand::usize(..audio.len())].clone())
    };
    const ROOT: &[&str] = &[];
    const ACH: &[&str] = &[ACHIEVEMENTS];
    const LVL: &[&str] = &[LEVELS];
    match key {
        SoundKey::Achievement { rule_id, tier } => {
            let rule_dir = [ACHIEVEMENTS, rule_id.as_str()];
            let tier_dir = [ACHIEVEMENTS, tier.as_str()];
            pin(cfg.by_rule.get(rule_id), &[&rule_dir, ACH, ROOT])
                .or_else(|| random(&[&rule_dir]))
                .or_else(|| pin(cfg.by_tier.get(tier.as_str()), &[&tier_dir, ACH, ROOT]))
                .or_else(|| random(&[&tier_dir]))
                .or_else(|| pin(cfg.achievement.as_ref(), &[ACH, ROOT]))
                .or_else(|| random(&[ACH, ROOT]))
        }
        SoundKey::Summary => pin(cfg.achievement.as_ref(), &[ACH, ROOT]).or_else(|| random(&[ACH, ROOT])),
        SoundKey::LevelUp => pin(cfg.level_up.as_ref(), &[LVL, ROOT]).or_else(|| random(&[LVL, ROOT])),
        SoundKey::Silent => None,
    }
}

pub(crate) fn is_audio(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("wav" | "mp3" | "ogg" | "flac"))
}

/// Every file under `dir`, descending `depth` folder levels (`achievements/<rule>/x.mp3` is 2).
pub(crate) fn collect_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for p in rd.filter_map(|e| e.ok()).map(|e| e.path()) {
        if p.is_file() {
            out.push(p);
        } else if depth > 0 && p.is_dir() {
            collect_files(&p, depth - 1, out);
        }
    }
}

/// Play the sound for `key` on a background thread at `cfg.volume`. Problems
/// (no output device, missing or undecodable file) are reported to `on_error`.
pub fn play(key: &SoundKey, cfg: &SoundConfig, sounds_dir: &Path, muted: bool, on_error: impl Fn(String) + Send + 'static) {
    if muted {
        return;
    }
    let mut files = Vec::new();
    collect_files(sounds_dir, 2, &mut files);
    let Some(file) = resolve_sound(key, cfg, sounds_dir, &files) else { return };
    let volume = cfg.volume.clamp(0.0, 1.0);
    // If no thread can be spawned, stay silent.
    let _ = std::thread::Builder::new().name("rpg-sound".into()).spawn(move || {
        if let Err(e) = play_file(&file, volume) {
            on_error(format!("sound {} failed: {e:#}", file.display()));
        }
    });
}

/// Play `file` once for the Settings window. Opening and decoding happen here so the
/// caller gets the error; playback itself runs on a background `rpg-sound` thread.
#[allow(dead_code)] // used by settings commands (Task 7)
pub fn preview(file: &Path, volume: f32) -> anyhow::Result<()> {
    // The output stream isn't `Send`, so the thread opens the device and reports back.
    let source = rodio::Decoder::new(std::io::BufReader::new(std::fs::File::open(file)?))?;
    let volume = volume.clamp(0.0, 1.0);
    let (tx, rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    std::thread::Builder::new().name("rpg-sound".into()).spawn(move || {
        let opened = (|| -> anyhow::Result<_> {
            let (stream, handle) = rodio::OutputStream::try_default()?;
            let sink = rodio::Sink::try_new(&handle)?;
            Ok((stream, sink))
        })();
        match opened {
            Ok((_stream, sink)) => {
                sink.set_volume(volume);
                sink.append(source);
                let _ = tx.send(Ok(()));
                sink.sleep_until_end();
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        }
    })?;
    rx.recv()?
}

fn play_file(file: &Path, volume: f32) -> anyhow::Result<()> {
    let (_stream, handle) = rodio::OutputStream::try_default()?;
    let sink = rodio::Sink::try_new(&handle)?;
    let source = rodio::Decoder::new(std::io::BufReader::new(std::fs::File::open(file)?))?;
    sink.set_volume(volume);
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpg_core::rules::Tier;
    use std::collections::HashSet;

    fn files(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from("/s").join(n)).collect()
    }

    fn cfg() -> SoundConfig {
        let mut c = SoundConfig::default();
        c.by_rule.insert("archaeologist".into(), "dig.wav".into());
        c.by_tier.insert("gold".into(), "gold.wav".into());
        c.achievement = Some("ach.wav".into());
        c.level_up = Some("lvl.wav".into());
        c
    }

    fn ach(rule: &str, tier: Tier) -> SoundKey {
        SoundKey::Achievement { rule_id: rule.into(), tier }
    }

    /// The pick, as a `/`-separated path relative to the sounds folder.
    fn name(k: &SoundKey, c: &SoundConfig, fs: &[PathBuf]) -> Option<String> {
        resolve_sound(k, c, Path::new("/s"), fs).map(|p| {
            p.strip_prefix("/s").unwrap().components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
        })
    }

    fn set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sound_resolution_order() {
        let fs = files(&["dig.wav", "gold.wav", "ach.wav", "lvl.wav", "other.mp3"]);
        let c = cfg();
        assert_eq!(name(&ach("archaeologist", Tier::Gold), &c, &fs).as_deref(), Some("dig.wav"), "by_rule beats by_tier");
        assert_eq!(name(&ach("other", Tier::Gold), &c, &fs).as_deref(), Some("gold.wav"));
        assert_eq!(name(&ach("other", Tier::Bronze), &c, &fs).as_deref(), Some("ach.wav"));
        assert_eq!(name(&SoundKey::LevelUp, &c, &fs).as_deref(), Some("lvl.wav"));
        // A configured file that is missing falls through to the next level.
        let fs2 = files(&["gold.wav"]);
        assert_eq!(name(&ach("archaeologist", Tier::Gold), &c, &fs2).as_deref(), Some("gold.wav"));
        // Nothing configured matches: a random audio file; none at all: silence.
        let fs3 = files(&["only.ogg", "readme.txt"]);
        assert_eq!(name(&ach("x", Tier::Bronze), &c, &fs3).as_deref(), Some("only.ogg"));
        assert_eq!(name(&ach("x", Tier::Bronze), &c, &files(&["readme.txt"])), None);
        assert_eq!(name(&SoundKey::Summary, &SoundConfig::default(), &[]), None);
    }

    #[test]
    fn type_folders_split_the_pools_and_share_the_root() {
        let none = SoundConfig::default();
        let fs = files(&["shared.mp3", "achievements/a.mp3", "levels/l.mp3"]);
        let got = |k: &SoundKey| -> HashSet<String> { (0..200).filter_map(|_| name(k, &none, &fs)).collect() };
        assert_eq!(got(&ach("x", Tier::Bronze)), set(&["shared.mp3", "achievements/a.mp3"]));
        assert_eq!(got(&SoundKey::Summary), set(&["shared.mp3", "achievements/a.mp3"]));
        assert_eq!(got(&SoundKey::LevelUp), set(&["shared.mp3", "levels/l.mp3"]));
        // Only a root file: every type still has something to play.
        assert_eq!(name(&SoundKey::LevelUp, &none, &files(&["shared.mp3"])).as_deref(), Some("shared.mp3"));
    }

    #[test]
    fn pins_are_found_in_type_folders_and_pins_beat_their_own_folder() {
        let mut c = SoundConfig { achievement: Some("a.mp3".into()), level_up: Some("l.mp3".into()), ..Default::default() };
        let fs = files(&["achievements/a.mp3", "levels/l.mp3", "other.mp3"]);
        assert_eq!(name(&ach("x", Tier::Bronze), &c, &fs).as_deref(), Some("achievements/a.mp3"));
        assert_eq!(name(&SoundKey::LevelUp, &c, &fs).as_deref(), Some("levels/l.mp3"));
        // A level-up never borrows from the achievements folder.
        c.level_up = Some("a.mp3".into());
        let got: HashSet<String> = (0..200).filter_map(|_| name(&SoundKey::LevelUp, &c, &fs)).collect();
        assert_eq!(got, set(&["levels/l.mp3", "other.mp3"]));
    }

    #[test]
    fn rule_and_tier_folders_rank_with_their_pins() {
        let c = cfg();
        let fs = files(&["ach.wav", "gold.wav", "achievements/gold/g.mp3", "achievements/archaeologist/r.mp3"]);
        // A rule folder beats the tier pin; the rule's own pin beats its folder.
        assert_eq!(name(&ach("archaeologist", Tier::Gold), &c, &fs).as_deref(), Some("achievements/archaeologist/r.mp3"));
        let mut pinned = cfg();
        pinned.by_rule.insert("archaeologist".into(), "gold.wav".into());
        assert_eq!(name(&ach("archaeologist", Tier::Gold), &pinned, &fs).as_deref(), Some("gold.wav"));
        // A tier pin beats its folder; with no tier pin the folder beats the achievement pin.
        assert_eq!(name(&ach("other", Tier::Gold), &c, &fs).as_deref(), Some("gold.wav"));
        let mut no_tier = cfg();
        no_tier.by_tier.clear();
        assert_eq!(name(&ach("other", Tier::Gold), &no_tier, &fs).as_deref(), Some("achievements/gold/g.mp3"));
        // Other tiers are unaffected.
        assert_eq!(name(&ach("other", Tier::Bronze), &c, &fs).as_deref(), Some("ach.wav"));
    }
}
