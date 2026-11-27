//! Format-preserving, allow-listed edits to `config.toml`. This is the only
//! path by which the app writes the config file.

use crate::config::{validate_path_pattern, Config};
use serde_json::Value;
use std::fmt;
use std::path::Path;
use toml_edit::{DocumentMut, Item, TableLike};

#[derive(Debug)]
pub enum EditError {
    UnknownKey(String),
    BadValue { key: String, msg: String },
    InvalidFile(String),
    Io(String),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::UnknownKey(k) => write!(f, "unknown setting: {k}"),
            EditError::BadValue { key, msg } => write!(f, "bad value for {key}: {msg}"),
            EditError::InvalidFile(m) => write!(f, "config file is invalid: {m}"),
            EditError::Io(m) => write!(f, "could not save config: {m}"),
        }
    }
}

impl std::error::Error for EditError {}

const TIERS: [&str; 6] = ["bronze", "silver", "gold", "platinum", "legendary", "celestial"];

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Announce,
    Volume,
    FileName,
    IntRange(i64, i64),
    /// A number in `min..=max`; when `open_min`, `min` itself is excluded.
    FloatRange { min: f64, max: f64, open_min: bool },
    NonEmptyString,
    Patterns,
}

fn kind_of(key: &str) -> Option<Kind> {
    Some(match key {
        "sound.muted" | "announce.dnd" | "announce.first_banner" | "github.enabled"
        | "llm.enabled" | "quests.enabled" => Kind::Bool,
        "announce.all" => Kind::Announce,
        "sound.volume" => Kind::Volume,
        "sound.achievement" | "sound.level_up" => Kind::FileName,
        "github.poll_minutes" => Kind::IntRange(1, 1440),
        "llm.timeout_secs" => Kind::IntRange(1, 600),
        "updates.check_hours" => Kind::IntRange(0, 8760),
        "quests.offer_seconds" => Kind::IntRange(10, 3600),
        "quests.max_passes" => Kind::IntRange(1, 100),
        "quests.min_gap_minutes" => Kind::IntRange(0, 1440),
        "quests.chain_step" => Kind::FloatRange { min: 0.0, max: 100.0, open_min: true },
        "quests.chain_max" => Kind::FloatRange { min: 1.0, max: 100.0, open_min: false },
        "llm.model" => Kind::NonEmptyString,
        "paths.include" | "paths.exclude" => Kind::Patterns,
        _ => {
            if let Some(t) = key.strip_prefix("announce.") {
                if TIERS.contains(&t) {
                    return Some(Kind::Announce);
                }
            }
            if let Some(t) = key.strip_prefix("sound.by_tier.") {
                if TIERS.contains(&t) {
                    return Some(Kind::FileName);
                }
            }
            return None;
        }
    })
}

fn bad(key: &str, msg: impl Into<String>) -> EditError {
    EditError::BadValue { key: key.to_string(), msg: msg.into() }
}

fn to_item(key: &str, kind: Kind, v: &Value) -> Result<Item, EditError> {
    match kind {
        Kind::Bool => v
            .as_bool()
            .map(toml_edit::value)
            .ok_or_else(|| bad(key, "expected true or false")),
        Kind::Announce => match v.as_str() {
            Some(s @ ("banner" | "toast")) => Ok(toml_edit::value(s)),
            _ => Err(bad(key, "expected \"banner\" or \"toast\"")),
        },
        Kind::Volume => match v.as_f64() {
            Some(f) if (0.0..=1.0).contains(&f) => Ok(toml_edit::value(f)),
            _ => Err(bad(key, "expected a number from 0 to 1")),
        },
        Kind::FileName => match v.as_str() {
            Some(s) if !s.is_empty() && !s.contains(['/', '\\']) && s != "." && s != ".." => {
                Ok(toml_edit::value(s))
            }
            _ => Err(bad(key, "expected a file name without path separators")),
        },
        Kind::IntRange(min, max) => match v.as_i64() {
            Some(n) if (min..=max).contains(&n) => Ok(toml_edit::value(n)),
            _ => Err(bad(key, format!("expected an integer from {min} to {max}"))),
        },
        Kind::FloatRange { min, max, open_min } => match v.as_f64() {
            Some(f) if f <= max && (f > min || (!open_min && f == min)) => Ok(toml_edit::value(f)),
            _ => {
                let low = if open_min { format!("above {min}") } else { format!("from {min}") };
                Err(bad(key, format!("expected a number {low} to {max}")))
            }
        },
        Kind::NonEmptyString => match v.as_str() {
            Some(s) if !s.trim().is_empty() => Ok(toml_edit::value(s)),
            _ => Err(bad(key, "expected a non-empty string")),
        },
        Kind::Patterns => {
            let arr = v.as_array().ok_or_else(|| bad(key, "expected an array of strings"))?;
            let mut out = toml_edit::Array::new();
            for e in arr {
                let s = e.as_str().ok_or_else(|| bad(key, "expected an array of strings"))?;
                validate_path_pattern(s).map_err(|m| bad(key, format!("{s}: {m}")))?;
                out.push(s);
            }
            Ok(toml_edit::value(out))
        }
    }
}

fn set_in(table: &mut dyn TableLike, parts: &[&str], item: Option<Item>) -> Result<(), String> {
    let (first, rest) = parts.split_first().expect("non-empty key path");
    if rest.is_empty() {
        match item {
            Some(mut new) => {
                // Keep the old value's surrounding whitespace and trailing comment.
                // Mutate in place so the key's own decor (comments above, indent) survives.
                if let Some(slot) = table.get_mut(first) {
                    if let (Item::Value(old), Item::Value(nv)) = (&*slot, &mut new) {
                        *nv.decor_mut() = old.decor().clone();
                    }
                    *slot = new;
                } else {
                    table.insert(first, new);
                }
            }
            None => {
                table.remove(first);
            }
        }
        return Ok(());
    }
    if table.get(first).is_none() {
        if item.is_none() {
            return Ok(());
        }
        table.insert(first, Item::Table(toml_edit::Table::new()));
    }
    match table.get_mut(first).and_then(Item::as_table_like_mut) {
        Some(t) => set_in(t, rest, item),
        None => Err(format!("{first} is not a table")),
    }
}

/// Set (or, for `Null`, remove) one allow-listed key, preserving comments and
/// formatting. Returns the new file text and its parsed `Config`.
pub fn apply_edit(text: &str, key: &str, value: &Value) -> Result<(String, Config), EditError> {
    Config::parse(text).map_err(|e| EditError::InvalidFile(format!("{e:#}")))?;
    let kind = kind_of(key).ok_or_else(|| EditError::UnknownKey(key.to_string()))?;
    let item = if value.is_null() { None } else { Some(to_item(key, kind, value)?) };

    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| EditError::InvalidFile(e.to_string()))?;
    let parts: Vec<&str> = key.split('.').collect();
    set_in(doc.as_table_mut(), &parts, item).map_err(|m| bad(key, m))?;

    let mut out = doc.to_string();
    if text.contains("\r\n") {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    let cfg = Config::parse(&out).map_err(|e| bad(key, format!("{e:#}")))?;
    Ok((out, cfg))
}

/// Write `text` to a temp file next to `path`, then rename it over the target.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        // Flush to disk before the rename, or a power cut can leave an empty file.
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Announce;
    use serde_json::{json, Value};

    const SAMPLE: &str = "# my config\n[sound]\nvolume = 0.8 # loud\nby_tier = { bronze = \"ding.wav\" }\n\n[xp]\ncommit = 99\n";

    #[test] fn edit_keeps_comments_and_other_keys() {
        let (t, c) = apply_edit(SAMPLE, "sound.volume", &json!(0.5)).unwrap();
        assert!(t.starts_with("# my config\n[sound]\nvolume = 0.5"));
        assert!(t.contains("[xp]\ncommit = 99\n"));
        assert_eq!(c.xp.commit, 99);
    }
    #[test] fn dots_inside_file_names_are_fine_but_dotdot_is_not() {
        let (_, c) = apply_edit(SAMPLE, "sound.achievement", &json!("Wait...what.mp3")).unwrap();
        assert_eq!(c.sound.achievement.as_deref(), Some("Wait...what.mp3"));
        assert!(apply_edit(SAMPLE, "sound.achievement", &json!("..")).is_err());
        assert!(apply_edit(SAMPLE, "sound.achievement", &json!(".")).is_err());
    }
    #[test] fn huge_intervals_are_rejected() {
        assert!(apply_edit(SAMPLE, "github.poll_minutes", &json!(1441)).is_err());
        assert!(apply_edit(SAMPLE, "github.poll_minutes", &json!(1440)).is_ok());
        assert!(apply_edit(SAMPLE, "updates.check_hours", &json!(8761)).is_err());
        assert!(apply_edit(SAMPLE, "llm.timeout_secs", &json!(601)).is_err());
        assert!(apply_edit(SAMPLE, "llm.timeout_secs", &json!(i64::MAX)).is_err());
    }
    #[test] fn backslash_paths_round_trip() {
        let (t, _) = apply_edit(SAMPLE, "paths.exclude", &json!([r"C:\Users\x"])).unwrap();
        let (_, exclude) = crate::config::raw_paths(&t);
        assert_eq!(exclude, vec![r"C:\Users\x".to_string()]);
    }
    #[test] fn inline_table_gets_new_tier_entry() {
        let (t, c) = apply_edit(SAMPLE, "sound.by_tier.gold", &json!("gong.wav")).unwrap();
        assert!(!t.contains("[sound.by_tier]"));
        assert_eq!(c.sound.by_tier.get("gold").map(String::as_str), Some("gong.wav"));
        assert_eq!(c.sound.by_tier.get("bronze").map(String::as_str), Some("ding.wav"));
    }
    #[test] fn missing_file_and_tables_are_created() {
        let (t, c) = apply_edit("", "announce.gold", &json!("toast")).unwrap();
        assert_eq!(c.announce.gold, Announce::Toast);
        assert!(t.contains("[announce]"));
    }
    #[test] fn null_removes_key() {
        let (t, c) = apply_edit(SAMPLE, "sound.volume", &Value::Null).unwrap();
        assert!(!t.contains("volume"));
        assert_eq!(c.sound.volume, 0.8);
    }
    #[test] fn crlf_lines_survive() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        let (t, _) = apply_edit(&crlf, "github.enabled", &json!(false)).unwrap();
        assert!(t.contains("[xp]\r\ncommit = 99\r\n"));
    }
    #[test] fn rejects_unknown_keys_and_bad_values() {
        for (k, v) in [("xp.commit", json!(5)), ("site.port", json!(1)), ("sound.by_rule.x", json!("a.wav"))] {
            assert!(matches!(apply_edit(SAMPLE, k, &v), Err(EditError::UnknownKey(_))), "{k}");
        }
        for (k, v) in [("sound.volume", json!(1.5)), ("github.poll_minutes", json!(0)), ("announce.gold", json!("loud")),
                       ("sound.achievement", json!("../x.wav")), ("paths.exclude", json!(["["])), ("llm.model", json!(""))] {
            assert!(matches!(apply_edit(SAMPLE, k, &v), Err(EditError::BadValue { .. })), "{k}");
        }
    }
    #[test] fn quests_toggle_is_saved() {
        let (t, c) = apply_edit(SAMPLE, "quests.enabled", &json!(false)).unwrap();
        assert!(t.contains("[quests]"));
        assert!(!c.quests.enabled);
        assert!(matches!(apply_edit(SAMPLE, "quests.enabled", &json!("no")), Err(EditError::BadValue { .. })));
    }
    #[test] fn quests_numbers_are_validated() {
        for (k, v) in [("quests.offer_seconds", json!(10)), ("quests.max_passes", json!(1)), ("quests.min_gap_minutes", json!(0)),
                       ("quests.chain_step", json!(0.25)), ("quests.chain_max", json!(1)), ("quests.chain_max", json!(7.5))] {
            assert!(apply_edit(SAMPLE, k, &v).is_ok(), "{k} = {v}");
        }
        for (k, v) in [("quests.offer_seconds", json!(9)), ("quests.offer_seconds", json!(1.5)), ("quests.max_passes", json!(0)),
                       ("quests.min_gap_minutes", json!(-1)), ("quests.chain_step", json!(0)), ("quests.chain_step", json!(-0.5)),
                       ("quests.chain_step", json!("1")), ("quests.chain_max", json!(0.99))] {
            assert!(matches!(apply_edit(SAMPLE, k, &v), Err(EditError::BadValue { .. })), "{k} = {v}");
        }
        let (t, c) = apply_edit(SAMPLE, "quests.chain_step", &json!(0.25)).unwrap();
        assert!(t.contains("chain_step = 0.25"));
        assert_eq!(c.quests.chain_step, 0.25);
        // An integer for a float key is saved as a float, so it still parses.
        let (t, c) = apply_edit("", "quests.chain_max", &json!(3)).unwrap();
        assert!(t.contains("chain_max = 3.0"), "{t}");
        assert_eq!(c.quests.chain_max, 3.0);
        let (_, c) = apply_edit("", "quests.offer_seconds", &json!(90)).unwrap();
        assert_eq!(c.quests.offer_seconds, 90);
    }
    #[test] fn broken_file_is_refused_untouched() {
        let err = apply_edit("[sound\nvolume = 1", "sound.volume", &json!(0.5)).unwrap_err();
        assert!(matches!(err, EditError::InvalidFile(ref m) if m.contains("line")));
    }
    #[test] fn volume_accepts_integer_endpoints() {
        let (t, c) = apply_edit("", "sound.volume", &json!(1)).unwrap();
        assert!(t.contains("volume = 1.0"));
        assert_eq!(c.sound.volume, 1.0);
        assert!(apply_edit("", "sound.volume", &json!(0)).is_ok());
    }
    #[test] fn edit_keeps_key_decor_and_neighbours() {
        let src = "[sound]
# how loud
volume = 0.8 # x
  muted = true
";
        let (t, _) = apply_edit(src, "sound.volume", &json!(0.5)).unwrap();
        assert_eq!(t, "[sound]
# how loud
volume = 0.5 # x
  muted = true
");
        let (t, _) = apply_edit(src, "sound.muted", &json!(false)).unwrap();
        assert_eq!(t, "[sound]
# how loud
volume = 0.8 # x
  muted = false
");
    }
    #[test] fn inline_table_edit_keeps_neighbours() {
        let src = "[sound]
by_tier = { bronze = \"a.wav\",  silver = \"b.wav\" }
";
        let (t, _) = apply_edit(src, "sound.by_tier.bronze", &json!("c.wav")).unwrap();
        assert_eq!(t, "[sound]
by_tier = { bronze = \"c.wav\",  silver = \"b.wav\" }
");
    }
    #[test] fn write_atomic_replaces_and_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        std::fs::write(&p, "old").unwrap();
        write_atomic(&p, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
