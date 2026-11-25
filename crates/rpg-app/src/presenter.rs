//! What to show, and when: XP toasts, the one-at-a-time banner queue, Do Not
//! Disturb, and collapsing of achievement storms / away batches. Pure (no Tauri).
use rpg_core::engine::{Outcome, SkillLevelUp, Unlock};
use rpg_core::event::Event;
use rpg_core::rules::Tier;
use std::collections::VecDeque;

/// More unlocks than this in one live batch is a storm.
pub const STORM_CAP: usize = 3;

pub const ACHIEVEMENT_TITLE: &str = "✦ NEW ACHIEVEMENT ✦";
pub const LEVEL_UP_TITLE: &str = "✦ LEVEL UP ✦";
pub const AWAY_TITLE: &str = "✦ WHILE YOU WERE AWAY ✦";
pub const STORM_TITLE: &str = "✦ ACHIEVEMENT STORM ✦";
pub const NOTICE_TITLE: &str = "✦ RPG ✦";
pub const AWAY_LABEL: &str = "while you were away";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub amount: u64,
    pub label: String,
    /// A plain message (e.g. "Hooks installed") instead of "+N exp".
    pub notice: bool,
    /// A skill level-up ("TypeScript Lv 9"), shown as a plain message in skill colors.
    pub skill: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Achievement,
    LevelUp,
    AwaySummary,
    /// The result of something the user asked for (tray install/uninstall failed).
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundKey {
    Achievement { rule_id: String, tier: Tier },
    LevelUp,
    Summary,
    Silent,
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
    /// Empty for achievements until resolved from `flavor` (see `text_ready`).
    pub text: String,
    /// False until an achievement's flavor text has been resolved into `text`.
    pub text_ready: bool,
    pub reward: String,
    pub tier: Option<Tier>,
    pub sound_key: SoundKey,
    pub flavor: Option<FlavorSource>,
    /// Summary banners: how many achievements and how much of their exp they stand for.
    pub more: Option<(usize, u64)>,
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
    /// Skill level-ups are toasts after the exp toasts, one per skill (its highest level).
    pub fn push(&mut self, batch: &[(Event, Outcome)], away: bool) {
        self.push_xp(batch, away);
        if !self.dnd {
            self.toasts.extend(skill_toasts(batch.iter().flat_map(|(_, o)| &o.skill_level_ups)));
        }
    }

    fn push_xp(&mut self, batch: &[(Event, Outcome)], away: bool) {
        let unlocks: Vec<(&Event, &Unlock)> = batch.iter().flat_map(|(e, o)| o.unlocks.iter().map(move |u| (e, u))).collect();
        let level_ups: Vec<u32> = batch.iter().flat_map(|(_, o)| o.level_ups.iter().copied()).collect();
        let gains = batch.iter().flat_map(|(_, o)| &o.xp_gains);

        let storm = away || unlocks.len() > STORM_CAP;
        if !storm {
            if !self.dnd {
                self.toasts.extend(gains.map(|g| Toast { amount: g.amount, label: g.label.clone(), notice: false, skill: false }));
            }
            self.banners.extend(unlocks.iter().map(|(e, u)| achievement(e, u)));
            self.banners.extend(level_ups.into_iter().map(level_up));
            return;
        }

        let sum: u64 = gains.map(|g| g.amount).sum();
        if sum > 0 && !self.dnd {
            self.toasts.push(Toast { amount: sum, label: if away { AWAY_LABEL.into() } else { String::new() }, notice: false, skill: false });
        }
        let achs = unlocks.iter().map(|(e, u)| achievement(e, u)).collect();
        self.banners.extend(collapse(achs, (0, 0), away));
        if let Some(&top) = level_ups.iter().max() {
            self.banners.push_back(level_up(top));
        }
    }

    /// Report the result of a user action. Success is a small toast; failure a
    /// silent banner at the head of the queue. Both ignore DND: the user just asked.
    pub fn notice(&mut self, ok: bool, title: &str, detail: &str) {
        if ok {
            self.toasts.push(Toast { amount: 0, label: title.into(), notice: true, skill: false });
            return;
        }
        self.banners.push_front(Banner {
            kind: BannerKind::Notice,
            title: NOTICE_TITLE.into(),
            name: title.into(),
            text: detail.into(),
            text_ready: true,
            reward: String::new(),
            tier: None,
            sound_key: SoundKey::Silent,
            flavor: None,
            more: None,
        });
    }

    /// DND on: pending and new toasts are dropped and banners wait. Off: banners flow again.
    /// Turning DND off with more than `STORM_CAP` achievement banners queued
    /// collapses the backlog into storm form (3 biggest, one summary, last level-up).
    pub fn set_dnd(&mut self, on: bool) {
        let was = self.dnd;
        self.dnd = on;
        if on {
            self.toasts.clear();
        } else if was {
            self.collapse_backlog();
        }
    }

    fn collapse_backlog(&mut self) {
        let achs = self.banners.iter().filter(|b| b.kind == BannerKind::Achievement).count();
        if achs <= STORM_CAP {
            return;
        }
        let mut achievements = vec![];
        let mut carried = (0, 0);
        let mut last_level_up = None;
        let mut notices = vec![];
        for b in self.banners.drain(..) {
            match b.kind {
                BannerKind::Notice => notices.push(b),
                BannerKind::Achievement => achievements.push(b),
                BannerKind::AwaySummary => {
                    let (n, xp) = b.more.unwrap_or_default();
                    carried = (carried.0 + n, carried.1 + xp);
                }
                // Levels only go up, so the last one queued is the highest.
                BannerKind::LevelUp => last_level_up = Some(b),
            }
        }
        self.banners.extend(notices);
        self.banners.extend(collapse(achievements, carried, false));
        self.banners.extend(last_level_up);
    }

    /// Take all pending toasts.
    pub fn next_toasts(&mut self) -> Vec<Toast> {
        std::mem::take(&mut self.toasts)
    }

    /// The next banner to show, or `None` while one is showing, DND is on (unless a
    /// notice is next), or the queue is empty.
    pub fn next_banner(&mut self) -> Option<Banner> {
        let notice_next = self.banners.front().is_some_and(|b| b.kind == BannerKind::Notice);
        if self.showing || (self.dnd && !notice_next) {
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

    /// Put a banner taken with `next_banner` back at the head of the queue
    /// (e.g. DND came on), keeping any text already resolved. It is no longer showing.
    pub fn requeue_front(&mut self, b: Banner) {
        self.banners.push_front(b);
        self.showing = false;
    }

    /// Last check before a taken banner goes on screen: `Some` to show it, or
    /// `None` when DND came on meanwhile (it is requeued at the front).
    pub fn confirm_show(&mut self, b: Banner) -> Option<Banner> {
        if self.dnd && b.kind != BannerKind::Notice {
            self.requeue_front(b);
            return None;
        }
        Some(b)
    }
}

/// One toast per skill that leveled up, at its highest new level, in first-leveled order.
fn skill_toasts<'a>(ups: impl Iterator<Item = &'a SkillLevelUp>) -> Vec<Toast> {
    let mut best: Vec<&SkillLevelUp> = vec![];
    for u in ups {
        match best.iter_mut().find(|b| b.skill_id == u.skill_id) {
            Some(b) if u.level > b.level => *b = u,
            Some(_) => {}
            None => best.push(u),
        }
    }
    best.into_iter()
        .map(|u| Toast { amount: 0, label: format!("{} Lv {}", u.name, u.level), notice: true, skill: true })
        .collect()
}

/// Storm form: the `STORM_CAP` biggest achievements (tier, then xp, then
/// earliest event; stable), then one summary for the rest plus `carried`
/// (achievements already summarized earlier), if there are any.
fn collapse(mut achs: Vec<Banner>, carried: (usize, u64), away: bool) -> Vec<Banner> {
    let key = |b: &Banner| {
        let src = b.flavor.as_ref().expect("achievement banners carry their unlock");
        (src.unlock.tier.rank(), src.unlock.xp, src.event.ts)
    };
    achs.sort_by(|a, b| {
        let (ta, xa, sa) = key(a);
        let (tb, xb, sb) = key(b);
        tb.cmp(&ta).then(xb.cmp(&xa)).then(sa.cmp(&sb))
    });
    let rest = achs.split_off(achs.len().min(STORM_CAP));
    let n = carried.0 + rest.len();
    let xp = carried.1 + rest.iter().map(|b| key(b).1).sum::<u64>();
    if n > 0 {
        achs.push(Banner {
            kind: BannerKind::AwaySummary,
            title: if away { AWAY_TITLE } else { STORM_TITLE }.into(),
            name: if away { "Welcome back, Crawler" } else { "The loot keeps coming" }.into(),
            text: format!("…and {n} more achievement{}, plus {xp} exp.", if n == 1 { "" } else { "s" }),
            text_ready: true,
            reward: format!("+{xp} exp"),
            tier: None,
            sound_key: SoundKey::Summary,
            flavor: None,
            more: Some((n, xp)),
        });
    }
    achs
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
        text_ready: false,
        more: None,
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
        text_ready: true,
        more: None,
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
            base_rule_id: format!("rule{id}"),
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

    fn skill_up(id: &str, level: u32) -> SkillLevelUp {
        SkillLevelUp { skill_id: id.into(), name: id.to_uppercase(), level }
    }

    #[test]
    fn skill_level_ups_toast_once_per_skill_after_exp() {
        let mut p = Presenter::new();
        let mut a = Outcome { xp_gains: vec![XpGain { amount: 10, label: "commit".into() }], ..Default::default() };
        a.skill_level_ups = vec![skill_up("ts", 2), skill_up("ts", 3), skill_up("yaml", 4)];
        let b = Outcome { skill_level_ups: vec![skill_up("ts", 4)], ..Default::default() };
        p.push(&[(ev(0), a), (ev(1), b)], false);
        let labels: Vec<(String, bool)> = p.next_toasts().into_iter().map(|t| (t.label, t.skill)).collect();
        assert_eq!(labels, vec![("commit".into(), false), ("TS Lv 4".into(), true), ("YAML Lv 4".into(), true)]);
        assert!(p.next_banner().is_none(), "skills never get a banner");

        p.set_dnd(true);
        p.push(&[(ev(2), Outcome { skill_level_ups: vec![skill_up("ts", 5)], ..Default::default() })], true);
        assert!(p.next_toasts().is_empty(), "DND drops skill toasts like other toasts");
    }

    #[test]
    fn live_batch_toasts_and_queued_banners() {
        let mut p = Presenter::new();
        p.push(&batch(vec![unlock(1, Tier::Bronze, 250), unlock(2, Tier::Silver, 500)]), false);
        let toasts = p.next_toasts();
        assert_eq!(toasts.len(), 4, "every xp gain is a toast: {toasts:?}");
        assert_eq!(toasts[0], Toast { amount: 10, label: "commit".into(), notice: false, skill: false });
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
        assert_eq!(toasts, vec![Toast { amount: total, label: AWAY_LABEL.into(), notice: false, skill: false }]);

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
        assert_eq!(p.next_toasts(), vec![Toast { amount: 20 + 1100, label: AWAY_LABEL.into(), notice: false, skill: false }]);
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
        assert_eq!(toasts, vec![Toast { amount: 50 + 100 + 500 + 300 + 1000 + 250, label: String::new(), notice: false, skill: false }]);
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

    #[test]
    fn requeue_front_shows_again_first_with_its_text() {
        let mut p = Presenter::new();
        p.push(&batch(vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Bronze, 100)]), false);
        let mut b = p.next_banner().unwrap();
        b.text = "RESOLVED".into();
        b.text_ready = true;
        p.requeue_front(b);
        let again = p.next_banner().expect("no longer 'showing', so the head is available");
        assert_eq!((again.name.as_str(), again.text.as_str(), again.text_ready), ("Ach 1", "RESOLVED", true));
        p.banner_done();
        assert_eq!(p.next_banner().unwrap().name, "Ach 2");
    }

    #[test]
    fn confirm_show_requeues_when_dnd_came_on() {
        let mut p = Presenter::new();
        p.push(&batch(vec![unlock(1, Tier::Bronze, 100)]), false);
        let b = p.next_banner().unwrap();
        p.set_dnd(true); // while the banner's text was resolving
        assert!(p.confirm_show(b).is_none(), "not shown under DND");
        assert!(p.next_banner().is_none());
        p.set_dnd(false);
        let b = p.next_banner().expect("released after DND");
        assert_eq!(b.name, "Ach 1");
        assert!(p.confirm_show(b).is_some(), "shown when DND is off");
    }

    #[test]
    fn ok_notice_is_a_toast_even_under_dnd() {
        let mut p = Presenter::new();
        p.set_dnd(true);
        p.notice(true, "Hooks installed", "git + Claude Code");
        assert_eq!(p.next_toasts(), vec![Toast { amount: 0, label: "Hooks installed".into(), notice: true, skill: false }]);
        assert!(p.next_banner().is_none(), "success needs no banner");
    }

    #[test]
    fn failed_notice_banner_jumps_queue_and_ignores_dnd() {
        let mut p = Presenter::new();
        p.push(&batch(vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Bronze, 100)]), false);
        p.set_dnd(true);
        p.notice(false, "Hooks not installed", "includeIf sets core.hooksPath");
        let b = p.next_banner().expect("notice shows despite DND");
        assert_eq!((b.kind, b.name.as_str(), b.text.as_str()), (BannerKind::Notice, "Hooks not installed", "includeIf sets core.hooksPath"));
        assert!(b.text_ready);
        assert_eq!(b.sound_key, SoundKey::Silent);
        let b = p.confirm_show(b).expect("not requeued by DND");
        assert_eq!(b.kind, BannerKind::Notice);
        p.banner_done();
        assert!(p.next_banner().is_none(), "achievements still wait for DND");
    }

    #[test]
    fn dnd_backlog_collapse_keeps_notices_first() {
        let mut p = Presenter::new();
        p.set_dnd(true);
        let us = vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Bronze, 200), unlock(3, Tier::Bronze, 300), unlock(4, Tier::Bronze, 400)];
        p.push(&batch(us), false);
        p.notice(false, "Hooks not removed", "not rpg's");
        // Queued behind a showing notice is fine too: take it, then release DND.
        let n = p.next_banner().expect("notice");
        p.banner_done();
        p.requeue_front(n);
        p.set_dnd(false);
        let bs = drain(&mut p);
        assert_eq!(bs[0].kind, BannerKind::Notice);
        assert_eq!(bs.len(), 1 + 3 + 1, "notice + 3 biggest + summary");
    }

    #[test]
    fn dnd_backlog_collapses_to_storm() {
        let mut p = Presenter::new();
        p.set_dnd(true);
        let mut b1 = batch(vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Silver, 500)]);
        b1[1].1.level_ups = vec![2];
        p.push(&b1, false);
        let mut b2 = batch(vec![unlock(3, Tier::Gold, 1000), unlock(4, Tier::Bronze, 300), unlock(5, Tier::Bronze, 250)]);
        b2[2].1.level_ups = vec![3];
        p.push(&b2, false);
        p.set_dnd(false);
        let bs = drain(&mut p);
        let got: Vec<&str> = bs.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(got.len(), 5, "{got:?}");
        assert_eq!(&got[..3], &["Ach 3", "Ach 2", "Ach 4"]);
        assert_eq!(bs[3].kind, BannerKind::AwaySummary);
        assert_eq!(bs[3].title, STORM_TITLE);
        assert_eq!(bs[3].text, "…and 2 more achievements, plus 350 exp.");
        assert_eq!((bs[4].kind, got[4]), (BannerKind::LevelUp, "Level 3"));
    }

    #[test]
    fn dnd_backlog_folds_existing_summaries() {
        let mut p = Presenter::new();
        p.set_dnd(true);
        // An away batch already collapsed to 3 + "…and 1 more" (100 exp)...
        let us = vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Bronze, 200), unlock(3, Tier::Bronze, 300), unlock(4, Tier::Bronze, 400)];
        p.push(&batch(us), true);
        // ...plus one live unlock: 4 achievement banners queued.
        p.push(&batch(vec![unlock(5, Tier::Gold, 1000)]), false);
        p.set_dnd(false);
        let bs = drain(&mut p);
        let got: Vec<&str> = bs.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(&got[..3], &["Ach 5", "Ach 4", "Ach 3"]);
        assert_eq!(bs.len(), 4);
        assert_eq!(bs[3].text, "…and 2 more achievements, plus 300 exp.");
    }

    #[test]
    fn dnd_backlog_of_three_not_collapsed() {
        let mut p = Presenter::new();
        p.set_dnd(true);
        p.push(&batch(vec![unlock(1, Tier::Bronze, 100), unlock(2, Tier::Gold, 1000)]), false);
        p.push(&batch(vec![unlock(3, Tier::Silver, 500)]), false);
        p.set_dnd(false);
        assert_eq!(names(&mut p), vec!["Ach 1", "Ach 2", "Ach 3"], "FIFO kept");
    }
}
