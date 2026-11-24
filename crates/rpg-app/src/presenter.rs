//! What to show, and when: XP toasts, the one-at-a-time banner queue, Do Not
//! Disturb, and collapsing of achievement storms / away batches. Pure (no Tauri).
use rpg_core::engine::{Outcome, Unlock};
use rpg_core::event::Event;
use rpg_core::rules::Tier;
use std::collections::VecDeque;

/// More unlocks than this in one live batch is a storm.
pub const STORM_CAP: usize = 3;

pub const ACHIEVEMENT_TITLE: &str = "✦ NEW ACHIEVEMENT ✦";
pub const LEVEL_UP_TITLE: &str = "✦ LEVEL UP ✦";
pub const AWAY_TITLE: &str = "✦ WHILE YOU WERE AWAY ✦";
pub const STORM_TITLE: &str = "✦ ACHIEVEMENT STORM ✦";
pub const AWAY_LABEL: &str = "while you were away";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub amount: u64,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Achievement,
    LevelUp,
    AwaySummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundKey {
    Achievement { rule_id: String, tier: Tier },
    LevelUp,
    Summary,
}

/// What an achievement banner needs to resolve its flavor text right before it
/// is shown (LLM text is rendered lazily, only for unlocks that get a banner).
#[derive(Debug, Clone)]
pub struct FlavorSource {
    pub unlock: Unlock,
    pub event: Event,
}

#[derive(Debug, Clone)]
pub struct Banner {
    pub kind: BannerKind,
    pub title: String,
    pub name: String,
    /// Empty for achievements until resolved from `flavor`.
    pub text: String,
    pub reward: String,
    pub tier: Option<Tier>,
    pub sound_key: SoundKey,
    pub flavor: Option<FlavorSource>,
}

#[derive(Default)]
pub struct Presenter {
    toasts: Vec<Toast>,
    banners: VecDeque<Banner>,
    showing: bool,
    dnd: bool,
}

impl Presenter {
    pub fn new() -> Presenter {
        Presenter::default()
    }

    /// Queue what a processed batch should show. Away batches (events from while
    /// the app was closed) and live storms (more than `STORM_CAP` unlocks)
    /// collapse: one toast, the 3 biggest unlocks, a summary, one level-up.
    pub fn push(&mut self, batch: &[(Event, Outcome)], away: bool) {
        let unlocks: Vec<(&Event, &Unlock)> = batch.iter().flat_map(|(e, o)| o.unlocks.iter().map(move |u| (e, u))).collect();
        let level_ups: Vec<u32> = batch.iter().flat_map(|(_, o)| o.level_ups.iter().copied()).collect();
        let gains = batch.iter().flat_map(|(_, o)| &o.xp_gains);

        let collapse = away || unlocks.len() > STORM_CAP;
        if !collapse {
            if !self.dnd {
                self.toasts.extend(gains.map(|g| Toast { amount: g.amount, label: g.label.clone() }));
            }
            self.banners.extend(unlocks.iter().map(|(e, u)| achievement(e, u)));
            self.banners.extend(level_ups.into_iter().map(level_up));
            return;
        }

        let sum: u64 = gains.map(|g| g.amount).sum();
        if sum > 0 && !self.dnd {
            self.toasts.push(Toast { amount: sum, label: if away { AWAY_LABEL.into() } else { String::new() } });
        }
        let mut sorted = unlocks;
        // Stable: equal tier/xp/ts keep batch order.
        sorted.sort_by(|(ea, a), (eb, b)| {
            tier_rank(b.tier).cmp(&tier_rank(a.tier)).then(b.xp.cmp(&a.xp)).then(ea.ts.cmp(&eb.ts))
        });
        let rest = sorted.split_off(sorted.len().min(STORM_CAP));
        self.banners.extend(sorted.iter().map(|(e, u)| achievement(e, u)));
        if !rest.is_empty() {
            let rest_xp: u64 = rest.iter().map(|(_, u)| u.xp).sum();
            let n = rest.len();
            self.banners.push_back(Banner {
                kind: BannerKind::AwaySummary,
                title: if away { AWAY_TITLE } else { STORM_TITLE }.into(),
                name: if away { "Welcome back, Crawler" } else { "The loot keeps coming" }.into(),
                text: format!("…and {n} more achievement{}, plus {rest_xp} exp.", if n == 1 { "" } else { "s" }),
                reward: format!("+{rest_xp} exp"),
                tier: None,
                sound_key: SoundKey::Summary,
                flavor: None,
            });
        }
        if let Some(&top) = level_ups.iter().max() {
            self.banners.push_back(level_up(top));
        }
    }

    /// DND on: pending and new toasts are dropped and banners wait. Off: banners flow again.
    pub fn set_dnd(&mut self, on: bool) {
        self.dnd = on;
        if on {
            self.toasts.clear();
        }
    }

    /// Take all pending toasts.
    pub fn next_toasts(&mut self) -> Vec<Toast> {
        std::mem::take(&mut self.toasts)
    }

    /// The next banner to show, or `None` while one is showing, DND is on, or the queue is empty.
    pub fn next_banner(&mut self) -> Option<Banner> {
        if self.showing || self.dnd {
            return None;
        }
        let b = self.banners.pop_front()?;
        self.showing = true;
        Some(b)
    }

    /// The showing banner went away (timed out or clicked).
    pub fn banner_done(&mut self) {
        self.showing = false;
    }
}

fn tier_rank(t: Tier) -> u8 {
    match t {
        Tier::Bronze => 0,
        Tier::Silver => 1,
        Tier::Gold => 2,
    }
}

fn achievement(ev: &Event, u: &Unlock) -> Banner {
    Banner {
        kind: BannerKind::Achievement,
        title: ACHIEVEMENT_TITLE.into(),
        name: u.name.clone(),
        text: String::new(),
        reward: format!("+{} exp", u.xp),
        tier: Some(u.tier),
        sound_key: SoundKey::Achievement { rule_id: u.rule_id.clone(), tier: u.tier },
        flavor: Some(FlavorSource { unlock: u.clone(), event: ev.clone() }),
    }
}

fn level_up(level: u32) -> Banner {
    Banner {
        kind: BannerKind::LevelUp,
        title: LEVEL_UP_TITLE.into(),
        name: format!("Level {level}"),
        text: "The dungeon has noticed you. That is rarely good news.".into(),
        reward: String::new(),
        tier: None,
        sound_key: SoundKey::LevelUp,
        flavor: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use rpg_core::engine::XpGain;
    use rpg_core::event::{EventKind, Source};

    fn ev(secs: i64) -> Event {
        let mut e = Event::new(EventKind::Commit, Source::Git, Some("r".into()), serde_json::json!({}));
        e.ts = Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap() + Duration::seconds(secs);
        e
    }

    fn unlock(id: i64, tier: Tier, xp: u64) -> Unlock {
        Unlock {
            unlock_id: id,
            rule_id: format!("rule{id}"),
            name: format!("Ach {id}"),
            tier,
            xp,
            findings: serde_json::json!({}),
            unlock_count: 1,
            last_unlocked_at: None,
        }
    }

    /// One event per unlock, `secs` apart, each with a 10 exp commit gain plus the unlock's xp.
    fn batch(unlocks: Vec<Unlock>) -> Vec<(Event, Outcome)> {
        unlocks
            .into_iter()
            .enumerate()
            .map(|(i, u)| {
                let o = Outcome {
                    xp_gains: vec![
                        XpGain { amount: 10, label: "commit".into() },
                        XpGain { amount: u.xp, label: u.name.clone() },
                    ],
                    unlocks: vec![u],
                    ..Default::default()
                };
                (ev(i as i64), o)
            })
            .collect()
    }

    fn names(p: &mut Presenter) -> Vec<String> {
        let mut out = vec![];
        while let Some(b) = p.next_banner() {
            out.push(b.name.clone());
            p.banner_done();
        }
        out
    }

    fn drain(p: &mut Presenter) -> Vec<Banner> {
        let mut out = vec![];
        while let Some(b) = p.next_banner() {
            out.push(b);
            p.banner_done();
        }
        out
    }

    #[test]
    fn live_batch_toasts_and_queued_banners() {
        let mut p = Presenter::new();
        p.push(&batch(vec![unlock(1, Tier::Bronze, 250), unlock(2, Tier::Silver, 500)]), false);
        let toasts = p.next_toasts();
        assert_eq!(toasts.len(), 4, "every xp gain is a toast: {toasts:?}");
        assert_eq!(toasts[0], Toast { amount: 10, label: "commit".into() });
        assert!(p.next_toasts().is_empty(), "toasts are drained");

        let first = p.next_banner().expect("first banner");
        assert_eq!(first.kind, BannerKind::Achievement);
        assert_eq!(first.title, ACHIEVEMENT_TITLE);
        assert_eq!(first.name, "Ach 1");
        assert_eq!(first.reward, "+250 exp");
        assert_eq!(first.tier, Some(Tier::Bronze));
        assert_eq!(first.sound_key, SoundKey::Achievement { rule_id: "rule1".into(), tier: Tier::Bronze });
        assert_eq!(first.flavor.as_ref().unwrap().unlock.unlock_id, 1);
        assert!(p.next_banner().is_none(), "second waits while the first shows");
        p.banner_done();
        assert_eq!(p.next_banner().expect("second banner").name, "Ach 2");
        p.banner_done();
        assert!(p.next_banner().is_none());
    }

    #[test]
    fn away_batch_caps_banners() {
        let mut p = Presenter::new();
        // Ascending size: bronze first, one gold last.
        let us = vec![
            unlock(1, Tier::Bronze, 100),
            unlock(2, Tier::Bronze, 200),
            unlock(3, Tier::Bronze, 250),
            unlock(4, Tier::Silver, 400),
            unlock(5, Tier::Silver, 500),
            unlock(6, Tier::Silver, 500),
            unlock(7, Tier::Gold, 1000),
        ];
        p.push(&batch(us), true);
        let toasts = p.next_toasts();
        let total = 7 * 10 + 100 + 200 + 250 + 400 + 500 + 500 + 1000;
        assert_eq!(toasts, vec![Toast { amount: total, label: AWAY_LABEL.into() }]);

        let bs = drain(&mut p);
        assert_eq!(bs.len(), 4, "3 achievements + 1 summary");
        let top: Vec<&str> = bs[..3].iter().map(|b| b.name.as_str()).collect();
        // Gold, then silvers by xp desc with earliest ts breaking the tie.
        assert_eq!(top, vec!["Ach 7", "Ach 5", "Ach 6"]);
        assert!(bs[..3].iter().all(|b| b.kind == BannerKind::Achievement));
        let s = &bs[3];
        assert_eq!(s.kind, BannerKind::AwaySummary);
        assert_eq!(s.title, AWAY_TITLE);
        assert!(s.text.starts_with("…and 4 more achievements"), "{}", s.text);
        assert_eq!(s.text, "…and 4 more achievements, plus 950 exp.");
    }

    #[test]
    fn away_batch_without_overflow_has_no_summary() {
        let mut p = Presenter::new();
        p.push(&batch(vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Gold, 1000)]), true);
        assert_eq!(names(&mut p), vec!["Ach 2", "Ach 1"], "sorted, no summary");
        assert_eq!(p.next_toasts(), vec![Toast { amount: 20 + 1100, label: AWAY_LABEL.into() }]);
    }

    #[test]
    fn live_storm_caps_and_sorts() {
        let mut p = Presenter::new();
        let us = vec![
            unlock(1, Tier::Bronze, 100),
            unlock(2, Tier::Silver, 500),
            unlock(3, Tier::Bronze, 300),
            unlock(4, Tier::Gold, 1000),
            unlock(5, Tier::Bronze, 250),
        ];
        p.push(&batch(us), false);
        let toasts = p.next_toasts();
        assert_eq!(toasts, vec![Toast { amount: 50 + 100 + 500 + 300 + 1000 + 250, label: String::new() }]);
        let bs = drain(&mut p);
        let got: Vec<&str> = bs.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(&got[..3], &["Ach 4", "Ach 2", "Ach 3"]);
        assert_eq!(bs.len(), 4);
        assert_eq!(bs[3].kind, BannerKind::AwaySummary);
        assert_eq!(bs[3].title, STORM_TITLE);
        assert!(bs[3].text.starts_with("…and 2 more"), "{}", bs[3].text);
    }

    #[test]
    fn small_live_batch_not_collapsed() {
        let mut p = Presenter::new();
        let us = vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Gold, 1000), unlock(3, Tier::Silver, 500)];
        p.push(&batch(us), false);
        assert_eq!(p.next_toasts().len(), 6);
        let bs = drain(&mut p);
        assert_eq!(bs.len(), 3, "no summary");
        let got: Vec<&str> = bs.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(got, vec!["Ach 1", "Ach 2", "Ach 3"], "FIFO, not sorted");
    }

    #[test]
    fn dnd_queues_banners_and_drops_toasts() {
        let mut p = Presenter::new();
        p.set_dnd(true);
        p.push(&batch(vec![unlock(1, Tier::Bronze, 100)]), false);
        assert!(p.next_toasts().is_empty(), "toasts dropped under DND");
        assert!(p.next_banner().is_none(), "banners held under DND");
        p.set_dnd(false);
        assert!(p.next_toasts().is_empty(), "dropped toasts stay dropped");
        assert_eq!(p.next_banner().expect("released").name, "Ach 1");
    }

    #[test]
    fn dnd_on_drops_pending_toasts() {
        let mut p = Presenter::new();
        p.push(&batch(vec![unlock(1, Tier::Bronze, 100)]), false);
        p.set_dnd(true);
        assert!(p.next_toasts().is_empty());
    }

    #[test]
    fn level_up_banner_after_achievements() {
        let mut p = Presenter::new();
        let mut b = batch(vec![unlock(1, Tier::Bronze, 250)]);
        // The level-up comes from the same event, listed before the unlock is shown.
        b[0].1.level_ups = vec![2];
        b.push((ev(5), Outcome { unlocks: vec![unlock(2, Tier::Bronze, 250)], ..Default::default() }));
        p.push(&b, false);
        let bs = drain(&mut p);
        let got: Vec<(BannerKind, &str)> = bs.iter().map(|b| (b.kind, b.name.as_str())).collect();
        assert_eq!(
            got,
            vec![(BannerKind::Achievement, "Ach 1"), (BannerKind::Achievement, "Ach 2"), (BannerKind::LevelUp, "Level 2")]
        );
        assert_eq!(bs[2].title, LEVEL_UP_TITLE);
        assert_eq!(bs[2].sound_key, SoundKey::LevelUp);
    }

    #[test]
    fn storm_level_ups_collapse_to_final_level() {
        let mut p = Presenter::new();
        let mut b = batch(vec![unlock(1, Tier::Bronze, 250), unlock(2, Tier::Bronze, 250)]);
        b[0].1.level_ups = vec![2, 3];
        b[1].1.level_ups = vec![4];
        p.push(&b, true);
        let level_ups: Vec<String> =
            drain(&mut p).into_iter().filter(|b| b.kind == BannerKind::LevelUp).map(|b| b.name).collect();
        assert_eq!(level_ups, vec!["Level 4"]);
    }

    #[test]
    fn empty_outcomes_produce_nothing() {
        let mut p = Presenter::new();
        p.push(&[(ev(0), Outcome::default()), (ev(1), Outcome::default())], true);
        assert!(p.next_toasts().is_empty());
        assert!(p.next_banner().is_none());
    }
}
