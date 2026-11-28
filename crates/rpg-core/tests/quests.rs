//! Each starter quest: a trigger that raises the offer and a sequence that completes it, plus a near miss.
use chrono::{DateTime, Duration, FixedOffset, Utc};
use rpg_core::config::Config;
use rpg_core::engine::{expire_quest, process};
use rpg_core::event::{Event, EventKind, Source};
use rpg_core::quests::state::{accept, offer_for, QuestStatus, QuestView};
use rpg_core::quests::{load_dir, QuestDef};
use rpg_core::rules::Rules;
use rpg_core::store::Store;
use serde_json::{json, Value};

const APP: &str = "C:/work/app";
const GH: &str = "o/app";
/// A Monday.
const MON: &str = "2026-01-05T10:00:00Z";
/// Friday 2 January 2026, 16:00 UTC.
const FRI: &str = "2026-01-02T16:00:00Z";
/// Thursday 1 January 2026, 16:00 UTC.
const THU: &str = "2026-01-01T16:00:00Z";

fn tz() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn starter(id: &str) -> QuestDef {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/quests");
    let (quests, errs) = load_dir(&dir);
    assert!(errs.is_empty(), "{errs:?}");
    quests.into_iter().find(|q| q.id == id).unwrap_or_else(|| panic!("no starter quest {id}"))
}

/// Git events in the local clone (which knows its GitHub repo); PR events name the GitHub repo.
fn ev(kind: EventKind, ts: DateTime<Utc>, data: Value) -> Event {
    let (source, repo, mut data) = match kind {
        EventKind::Commit | EventKind::Push => (Source::Git, APP, data),
        _ => (Source::Github, GH, data),
    };
    if source == Source::Git {
        data["github_repo"] = json!(GH);
    }
    let mut e = Event::new(kind, source, Some(repo.into()), data);
    e.ts = ts;
    e
}

struct Run {
    store: Store,
    rules: Rules,
    cfg: Config,
    start: DateTime<Utc>,
}

impl Run {
    fn new(start: &str) -> Run {
        Run { store: Store::open_in_memory().unwrap(), rules: Rules::default(), cfg: Config::default(), start: at(start) }
    }

    fn mins(&self, m: i64) -> DateTime<Utc> {
        self.start + Duration::minutes(m)
    }

    /// Process the trigger (as the app does) and accept the offer it raises, if any, at the start.
    fn trigger(&self, def: &QuestDef, kind: EventKind, data: Value) -> Option<QuestView> {
        let e = ev(kind, self.mins(-1), data);
        process(&self.store, &self.rules, &self.cfg, &e, &[], tz()).unwrap();
        let offer = offer_for(&self.store, std::slice::from_ref(def), &self.cfg, &e, self.start, tz(), &mut || 0.0).unwrap()?;
        Some(accept(&self.store, &offer, self.start).unwrap())
    }

    /// Process events at the given minutes; the quest status after the last one that touched it.
    fn steps(&self, events: &[(i64, EventKind, Value)]) -> Option<QuestStatus> {
        let mut last = None;
        for (m, kind, data) in events {
            let o = process(&self.store, &self.rules, &self.cfg, &ev(*kind, self.mins(*m), data.clone()), &[], tz()).unwrap();
            if let Some(q) = o.quest {
                last = Some(q.status);
            }
        }
        last
    }

    fn completed(&self) -> usize {
        self.store.events_of(&[EventKind::QuestCompleted], None).unwrap().len()
    }

    /// Resolve whatever is still active well after any deadline.
    fn expire(&self) -> Option<QuestStatus> {
        expire_quest(&self.store, &self.rules, &self.cfg, self.mins(24 * 60), tz()).unwrap().map(|(_, _, v)| v.status)
    }
}

fn plain() -> Value {
    json!({})
}

#[test]
fn hot_streak() {
    let def = starter("hot_streak");
    let r = Run::new(MON);
    r.trigger(&def, EventKind::Commit, plain()).expect("offer");
    let commits: Vec<_> = (1..=4).map(|m| (m, EventKind::Commit, plain())).collect();
    assert_eq!(r.steps(&commits), Some(QuestStatus::Completed));
    assert_eq!(r.completed(), 1);

    // Near miss: the fourth commit lands after the 30 minutes.
    let r = Run::new(MON);
    r.trigger(&def, EventKind::Commit, plain()).expect("offer");
    let late = [(1, EventKind::Commit, plain()), (2, EventKind::Commit, plain()), (3, EventKind::Commit, plain()), (31, EventKind::Commit, plain())];
    assert_eq!(r.steps(&late), Some(QuestStatus::Active));
    assert_eq!(r.expire(), Some(QuestStatus::Failed));
}

#[test]
fn mop_up_duty() {
    let def = starter("mop_up_duty");
    let big = json!({ "lines_added": 400, "lines_removed": 20 });
    let r = Run::new(MON);
    r.trigger(&def, EventKind::Commit, big.clone()).expect("offer");
    let cleanup = json!({ "lines_added": 10, "lines_removed": 90 });
    assert_eq!(r.steps(&[(5, EventKind::Commit, cleanup)]), Some(QuestStatus::Completed));

    // Near miss: a small commit raises no offer.
    let r = Run::new(MON);
    assert!(r.trigger(&def, EventKind::Commit, json!({ "lines_added": 100, "lines_removed": 0 })).is_none());
    // And a cleanup that deletes less than half doesn't count.
    let r = Run::new(MON);
    r.trigger(&def, EventKind::Commit, big).expect("offer");
    assert_eq!(r.steps(&[(5, EventKind::Commit, json!({ "lines_added": 60, "lines_removed": 40 }))]), None);
}

#[test]
fn review_bounty() {
    let def = starter("review_bounty");
    let r = Run::new(MON);
    r.trigger(&def, EventKind::PrOpened, json!({ "url": "u/1" })).expect("offer");
    let reviews = [(5, EventKind::ReviewSubmitted, json!({ "url": "u/7" })), (40, EventKind::ReviewSubmitted, json!({ "url": "u/8" }))];
    assert_eq!(r.steps(&reviews), Some(QuestStatus::Completed));

    // Near miss: one review in the hour.
    let r = Run::new(MON);
    r.trigger(&def, EventKind::PrOpened, json!({ "url": "u/1" })).expect("offer");
    assert_eq!(r.steps(&[(5, EventKind::ReviewSubmitted, json!({ "url": "u/7" }))]), Some(QuestStatus::Active));
    assert_eq!(r.expire(), Some(QuestStatus::Failed));
}

#[test]
fn ship_it_speedrun() {
    let def = starter("ship_it_speedrun");
    let r = Run::new(MON);
    r.trigger(&def, EventKind::Commit, plain()).expect("offer");
    let run = [(2, EventKind::Push, plain()), (6, EventKind::PrOpened, json!({ "url": "u/2" }))];
    assert_eq!(r.steps(&run), Some(QuestStatus::Completed));
    assert_eq!(r.store.total_xp().unwrap(), 10 + 25 + 50 + 500, "commit, push, PR opened, quest");

    // Near miss: the PR opens at minute 16.
    let r = Run::new(MON);
    r.trigger(&def, EventKind::Commit, plain()).expect("offer");
    let slow = [(2, EventKind::Push, plain()), (16, EventKind::PrOpened, json!({ "url": "u/2" }))];
    assert_eq!(r.steps(&slow), Some(QuestStatus::Active));
    assert_eq!(r.expire(), Some(QuestStatus::Failed));
}

#[test]
fn land_the_plane() {
    let def = starter("land_the_plane");
    let r = Run::new(MON);
    r.trigger(&def, EventKind::PrOpened, json!({ "url": "u/3" })).expect("offer");
    assert_eq!(r.steps(&[(90, EventKind::PrMerged, json!({ "url": "u/3" }))]), Some(QuestStatus::Completed));

    // Near miss: a different PR merges.
    let r = Run::new(MON);
    r.trigger(&def, EventKind::PrOpened, json!({ "url": "u/3" })).expect("offer");
    assert_eq!(r.steps(&[(90, EventKind::PrMerged, json!({ "url": "u/4" }))]), None);
    assert_eq!(r.completed(), 0);
}

#[test]
fn friday_gambit() {
    let def = starter("friday_gambit");
    let r = Run::new(FRI);
    r.trigger(&def, EventKind::Commit, plain()).expect("offer");
    let run = [(5, EventKind::Push, plain()), (10, EventKind::PrOpened, json!({ "url": "u/5" })), (55, EventKind::PrMerged, json!({ "url": "u/5" }))];
    assert_eq!(r.steps(&run), Some(QuestStatus::Completed));

    // Near miss: Thursday afternoon raises no offer.
    let r = Run::new(THU);
    assert!(r.trigger(&def, EventKind::Commit, plain()).is_none());
}
