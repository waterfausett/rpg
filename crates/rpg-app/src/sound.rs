//! Banner sounds: pick a file from `sounds/` per config and play it with rodio.
use crate::presenter::SoundKey;
use rpg_core::config::SoundConfig;
use rpg_core::rules::Tier;
use std::path::{Path, PathBuf};

/// Pick the sound for `key` among `files` (the contents of `sounds/`):
/// `by_rule` → `by_tier` → `achievement`/`level_up` → a random audio file → none.
/// Configured names that are not in `files` are skipped.
pub fn resolve_sound(key: &SoundKey, cfg: &SoundConfig, files: &[PathBuf]) -> Option<PathBuf> {
    let configured: Vec<Option<&String>> = match key {
        SoundKey::Achievement { rule_id, tier } => {
            vec![cfg.by_rule.get(rule_id), cfg.by_tier.get(tier_key(*tier)), cfg.achievement.as_ref()]
        }
        SoundKey::LevelUp => vec![cfg.level_up.as_ref()],
        SoundKey::Summary => vec![cfg.achievement.as_ref()],
    };
    let find = |name: &str| files.iter().find(|f| f.file_name().is_some_and(|n| n == name)).cloned();
    if let Some(found) = configured.into_iter().flatten().find_map(|n| find(n)) {
        return Some(found);
    }
    let audio: Vec<&PathBuf> = files.iter().filter(|f| is_audio(f)).collect();
    (!audio.is_empty()).then(|| audio[fastrand::usize(..audio.len())].clone())
}

fn tier_key(t: Tier) -> &'static str {
    match t {
        Tier::Bronze => "bronze",
        Tier::Silver => "silver",
        Tier::Gold => "gold",
    }
}

fn is_audio(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("wav" | "mp3" | "ogg" | "flac"))
}

/// Play the sound for `key` on a background thread at `cfg.volume`. Problems
/// (no output device, missing or undecodable file) are reported to `on_error`.
pub fn play(key: &SoundKey, cfg: &SoundConfig, sounds_dir: &Path, muted: bool, on_error: impl Fn(String) + Send + 'static) {
    if muted {
        return;
    }
    let files: Vec<PathBuf> = match std::fs::read_dir(sounds_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect(),
        Err(_) => return,
    };
    let Some(file) = resolve_sound(key, cfg, &files) else { return };
    let volume = cfg.volume.clamp(0.0, 1.0);
    // If no thread can be spawned, stay silent.
    let _ = std::thread::Builder::new().name("rpg-sound".into()).spawn(move || {
        if let Err(e) = play_file(&file, volume) {
            on_error(format!("sound {} failed: {e:#}", file.display()));
        }
    });
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

    #[test]
    fn sound_resolution_order() {
        let fs = files(&["dig.wav", "gold.wav", "ach.wav", "lvl.wav", "other.mp3"]);
        let c = cfg();
        let name = |k: &SoundKey, fs: &[PathBuf]| resolve_sound(k, &c, fs).map(|p| p.file_name().unwrap().to_string_lossy().into_owned());
        assert_eq!(name(&ach("archaeologist", Tier::Gold), &fs).as_deref(), Some("dig.wav"), "by_rule beats by_tier");
        assert_eq!(name(&ach("other", Tier::Gold), &fs).as_deref(), Some("gold.wav"));
        assert_eq!(name(&ach("other", Tier::Bronze), &fs).as_deref(), Some("ach.wav"));
        assert_eq!(name(&SoundKey::LevelUp, &fs).as_deref(), Some("lvl.wav"));
        // A configured file that is missing falls through to the next level.
        let fs2 = files(&["gold.wav"]);
        assert_eq!(name(&ach("archaeologist", Tier::Gold), &fs2).as_deref(), Some("gold.wav"));
        // Nothing configured matches: a random audio file; none at all: silence.
        let fs3 = files(&["only.ogg", "readme.txt"]);
        assert_eq!(name(&ach("x", Tier::Bronze), &fs3).as_deref(), Some("only.ogg"));
        assert_eq!(name(&ach("x", Tier::Bronze), &files(&["readme.txt"])), None);
        assert_eq!(resolve_sound(&SoundKey::Summary, &SoundConfig::default(), &[]), None);
    }
}
