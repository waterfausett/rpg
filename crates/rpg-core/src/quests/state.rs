//! Quest state: offers, the active quest's steps, the chain, completion and expiry.
//!
//! An accepted quest is a `quests` row. Its `steps` column is a snapshot of the definition's
//! steps (`[{ "kind", "filter", "count", "label" }]`, `filter` being the step's filter table as
//! written), so editing or deleting the quest file never changes a quest in progress.
use super::def::{QuestDef, QuestStep, Same};
use crate::config::{Config, QuestsConfig};
use crate::engine::{apply_rules, in_tx, Outcome, XpGain};
use crate::event::{Event, EventKind, Source};
use crate::filters::{Filter, RawFilter};
use crate::records;
use crate::rules::{Flavor, Rules, Tier};
use crate::store::{NewQuest, QuestRow, Store};
use anyhow::Result;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const QUEST_CHAIN: &str = "quest_chain";
pub const QUEST_BEST_CHAIN: &str = "quest_best_chain";
pub const QUEST_PASSES: &str = "quest_passes";
pub const QUEST_LAST_OFFER: &str = "quest_last_offer";

/// Only events this recent can raise an offer.
const FRESH_MINUTES: i64 = 5;

/// The meta key holding when quest `id` was last offered.
pub fn last_offer_key(id: &str) -> String {
    format!("{QUEST_LAST_OFFER}:{id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Chain {
    /// Completions in a row (0 after a reset).
    pub n: u32,
    pub best: u32,
    /// Offers passed (or ignored) in a row.
    pub passes: u32,
}

fn meta_u32(store: &Store, key: &str) -> Result<u32> {
    Ok(store.meta_get(key)?.and_then(|v| v.parse().ok()).unwrap_or(0))
}

fn set_meta_u32(store: &Store, key: &str, v: u32) -> Result<()> {
    store.meta_set(key, &v.to_string())
}

fn meta_time(store: &Store, key: &str) -> Result<Option<DateTime<Utc>>> {
    Ok(store
        .meta_get(key)?
        .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|t| t.with_timezone(&Utc)))
}

pub fn chain(store: &Store) -> Result<Chain> {
    Ok(Chain {
        n: meta_u32(store, QUEST_CHAIN)?,
        best: meta_u32(store, QUEST_BEST_CHAIN)?,
        passes: meta_u32(store, QUEST_PASSES)?,
    })
}

/// `min(1 + chain_step × n, chain_max)`.
pub fn multiplier(n: u32, cfg: &QuestsConfig) -> f64 {
    (1.0 + cfg.chain_step * n as f64).min(cfg.chain_max)
}

/// `floor(xp × mult)`, with a little slack so float error never drops a whole point.
pub fn reward(xp: u64, mult: f64) -> u64 {
    (xp as f64 * mult + 1e-9).floor().max(0.0) as u64
}

/// What later events must share with the trigger (`same` in the quest file). An unset field
/// is no constraint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
}

/// The repo an event belongs to for `same`: its GitHub repo when it knows it (so clones and
/// GitHub events agree), else its own `repo`.
fn resolved_repo(ev: &Event) -> Option<String> {
    ev.data["github_repo"].as_str().filter(|g| !g.is_empty()).map(str::to_string).or_else(|| ev.repo.clone())
}

fn data_str(ev: &Event, key: &str) -> Option<String> {
    ev.data[key].as_str().map(str::to_string)
}

pub fn bind(same: Option<Same>, trigger: &Event) -> Binding {
    match same {
        None => Binding::default(),
        Some(Same::Repo) => Binding { repo: resolved_repo(trigger), ..Default::default() },
        Some(Same::Branch) => Binding { repo: resolved_repo(trigger), branch: data_str(trigger, "branch"), pr_url: None },
        Some(Same::Pr) => Binding { pr_url: data_str(trigger, "url"), ..Default::default() },
    }
}

impl Binding {
    fn matches(&self, ev: &Event) -> bool {
        let same = |want: &Option<String>, got: Option<String>| want.as_ref().is_none_or(|w| got.as_ref() == Some(w));
        same(&self.repo, resolved_repo(ev)) && same(&self.branch, data_str(ev, "branch")) && same(&self.pr_url, data_str(ev, "url"))
    }
}

/// A step rebuilt from a row's snapshot. `filter` is `None` when the snapshot can't be read:
/// such a step matches nothing.
struct SnapStep {
    kind: Option<EventKind>,
    filter: Option<Filter>,
    need: u32,
    label: String,
}

impl SnapStep {
    fn matches(&self, ev: &Event, tz: FixedOffset) -> bool {
        self.kind == Some(ev.kind) && self.filter.as_ref().is_some_and(|f| f.matches(ev, tz))
    }
}

fn snapshot(steps: &[QuestStep]) -> Value {
    Value::Array(
        steps
            .iter()
            .map(|s| {
                let filter = s.source.get("filter").and_then(|f| serde_json::to_value(f).ok()).unwrap_or_else(|| json!({}));
                json!({ "kind": s.kind, "filter": filter, "count": s.count, "label": s.label })
            })
            .collect(),
    )
}

fn snap_steps(steps: &Value) -> Vec<SnapStep> {
    let Some(items) = steps.as_array() else { return vec![] };
    items
        .iter()
        .map(|s| {
            let kind: Option<EventKind> = serde_json::from_value(s["kind"].clone()).ok();
            let raw = if s["filter"].is_null() { Some(RawFilter::default()) } else { serde_json::from_value(s["filter"].clone()).ok() };
            let filter = raw.and_then(|r| Filter::try_from(r).ok());
            let need = s["count"].as_u64().map_or(1, |c| c.clamp(1, u32::MAX as u64) as u32);
            let label = s["label"].as_str().map(str::to_string).unwrap_or_else(|| kind.map_or("?", |k| k.as_str()).to_string());
            SnapStep { kind, filter, need, label }
        })
        .collect()
}

fn is_github_kind(k: EventKind) -> bool {
    matches!(k, EventKind::PrOpened | EventKind::PrMerged | EventKind::ReviewSubmitted)
}

fn parse_tier(s: &str) -> Tier {
    serde_json::from_value(Value::String(s.to_string())).unwrap_or(Tier::Bronze)
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub quest_id: String,
    pub name: String,
    pub tier: Tier,
    pub fallback: String,
    pub flavor: Flavor,
    pub minutes: u32,
    pub step_labels: Vec<String>,
    pub xp: u64,
    pub multiplier: f64,
    pub next_multiplier: f64,
    pub reward: u64,
    pub passes: u32,
    pub max_passes: u32,
    pub has_github_step: bool,
    pub trigger: Event,
    pub binding: Binding,
    pub steps: Vec<QuestStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestStatus {
    Active,
    Checking,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepView {
    pub label: String,
    pub have: u32,
    pub need: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestView {
    pub row_id: i64,
    pub quest_id: String,
    pub name: String,
    pub tier: Tier,
    pub deadline: DateTime<Utc>,
    pub steps: Vec<StepView>,
    pub reward: u64,
    pub multiplier: f64,
    pub status: QuestStatus,
    pub has_github_step: bool,
}

pub fn offer_for(
    store: &Store,
    quests: &[QuestDef],
    cfg: &Config,
    ev: &Event,
    now: DateTime<Utc>,
    tz: FixedOffset,
    roll: &mut dyn FnMut() -> f64,
) -> Result<Option<Offer>> {
    if !cfg.quests.enabled || !cfg.paths.allows(ev.repo.as_deref()) || ev.kind.is_quest() || ev.backfill {
        return Ok(None);
    }
    if now - ev.ts > Duration::minutes(FRESH_MINUTES) || store.active_quest()?.is_some() {
        return Ok(None);
    }
    if let Some(last) = meta_time(store, QUEST_LAST_OFFER)? {
        if now < last + Duration::minutes(cfg.quests.min_gap_minutes as i64) {
            return Ok(None);
        }
    }
    for def in quests.iter().filter(|q| q.enabled && q.trigger.contains(&ev.kind) && q.filter.matches(ev, tz)) {
        if let Some(last) = meta_time(store, &last_offer_key(&def.id))? {
            if now < last + Duration::hours(def.cooldown_hours as i64) {
                continue;
            }
        }
        if roll() >= def.chance {
            continue;
        }
        let stamp = now.to_rfc3339();
        store.meta_set(QUEST_LAST_OFFER, &stamp)?;
        store.meta_set(&last_offer_key(&def.id), &stamp)?;
        let ch = chain(store)?;
        let mult = multiplier(ch.n, &cfg.quests);
        return Ok(Some(Offer {
            quest_id: def.id.clone(),
            name: def.name.clone(),
            tier: def.tier,
            fallback: def.fallback.clone(),
            flavor: def.flavor.clone(),
            minutes: def.minutes,
            step_labels: def.steps.iter().map(|s| s.label.clone()).collect(),
            xp: def.xp,
            multiplier: mult,
            next_multiplier: multiplier(ch.n + 1, &cfg.quests),
            reward: reward(def.xp, mult),
            passes: ch.passes,
            max_passes: cfg.quests.max_passes,
            has_github_step: def.has_github_step(),
            trigger: ev.clone(),
            binding: bind(def.same, ev),
            steps: def.steps.clone(),
        }));
    }
    Ok(None)
}

/// Start `offer`'s quest now: passes reset to 0 and the deadline is `minutes` from `now`.
pub fn accept(store: &Store, offer: &Offer, now: DateTime<Utc>) -> Result<QuestView> {
    in_tx(store, || {
        set_meta_u32(store, QUEST_PASSES, 0)?;
        let id = store.insert_quest(&NewQuest {
            quest_id: offer.quest_id.clone(),
            name: offer.name.clone(),
            tier: offer.tier.as_str().to_string(),
            trigger_event_id: offer.trigger.id.clone(),
            binding: serde_json::to_value(&offer.binding)?,
            steps: snapshot(&offer.steps),
            accepted_at: now,
            deadline: now + Duration::minutes(offer.minutes as i64),
            chain_at_start: chain(store)?.n,
            multiplier: offer.multiplier,
            xp: offer.xp,
        })?;
        let row = store.active_quest()?.filter(|r| r.id == id).ok_or_else(|| anyhow::anyhow!("quest {id} not stored"))?;
        Ok(view(&row, QuestStatus::Active))
    })
}

/// An offer passed or ignored. Returns whether that broke the chain (`max_passes` in a row).
pub fn pass(store: &Store, cfg: &QuestsConfig) -> Result<bool> {
    in_tx(store, || {
        let passes = meta_u32(store, QUEST_PASSES)? + 1;
        if passes >= cfg.max_passes {
            set_meta_u32(store, QUEST_CHAIN, 0)?;
            set_meta_u32(store, QUEST_PASSES, 0)?;
            Ok(true)
        } else {
            set_meta_u32(store, QUEST_PASSES, passes)?;
            Ok(false)
        }
    })
}

/// End the active quest quietly (quests turned off): no event, chain and passes kept.
pub fn cancel(store: &Store, now: DateTime<Utc>) -> Result<Option<QuestView>> {
    let Some(row) = store.active_quest()? else { return Ok(None) };
    store.resolve_quest(row.id, "cancelled", None, now)?;
    Ok(Some(view(&row, QuestStatus::Cancelled)))
}

pub fn view(row: &QuestRow, status: QuestStatus) -> QuestView {
    let steps = snap_steps(&row.steps);
    QuestView {
        row_id: row.id,
        quest_id: row.quest_id.clone(),
        name: row.name.clone(),
        tier: parse_tier(&row.tier),
        deadline: row.deadline,
        has_github_step: steps.iter().any(|s| s.kind.is_some_and(is_github_kind)),
        steps: steps
            .iter()
            .enumerate()
            .map(|(i, s)| StepView { label: s.label.clone(), have: row.progress.get(i).copied().unwrap_or(0).min(s.need), need: s.need })
            .collect(),
        reward: reward(row.xp, row.multiplier),
        multiplier: row.multiplier,
        status,
    }
}

pub fn active_view(store: &Store) -> Result<Option<QuestView>> {
    Ok(store.active_quest()?.map(|r| view(&r, QuestStatus::Active)))
}

/// Credit `ev` (a live, newly stored, non-quest event) to the active quest's current step, and
/// complete the quest when that fills the last one. Runs inside `process`'s transaction.
pub(crate) fn advance(store: &Store, rules: &Rules, cfg: &Config, ev: &Event, tz: FixedOffset, out: &mut Outcome) -> Result<()> {
    let Some(mut row) = store.active_quest()? else { return Ok(()) };
    if ev.ts < row.accepted_at || ev.ts > row.deadline || ev.id == row.trigger_event_id {
        return Ok(());
    }
    let steps = snap_steps(&row.steps);
    if steps.is_empty() {
        return Ok(());
    }
    row.progress.resize(steps.len(), 0);
    let Some(i) = (0..steps.len()).find(|&i| row.progress[i] < steps[i].need) else { return Ok(()) };
    let binding: Binding = serde_json::from_value(row.binding.clone()).unwrap_or_default();
    if !steps[i].matches(ev, tz) || !binding.matches(ev) {
        return Ok(());
    }
    row.progress[i] += 1;
    store.set_quest_progress(row.id, &row.progress)?;
    if row.progress.iter().zip(&steps).any(|(have, s)| *have < s.need) {
        out.quest = Some(view(&row, QuestStatus::Active));
        return Ok(());
    }

    // Completed: pay the reward and raise the chain, then let rules see the quest_completed event.
    let ch = chain(store)?;
    let n = ch.n + 1;
    set_meta_u32(store, QUEST_CHAIN, n)?;
    set_meta_u32(store, QUEST_BEST_CHAIN, ch.best.max(n))?;
    let xp = reward(row.xp, row.multiplier);
    let data = json!({ "quest_id": row.quest_id, "tier": row.tier, "chain": n, "multiplier": row.multiplier, "xp": xp });
    let mut qev = Event::new(EventKind::QuestCompleted, Source::Quest, None, data);
    qev.ts = ev.ts;
    store.insert_event(&qev)?;
    let broken = records::update(store, &qev, tz)?;
    store.resolve_quest(row.id, "completed", Some(&qev.id), ev.ts)?;
    if xp > 0 {
        let label = format!("Quest: {}", row.name);
        store.add_xp(&qev.id, xp, &label, None, qev.ts)?;
        out.xp_gains.push(XpGain { amount: xp, label, achievement: None });
    }
    apply_rules(store, rules, cfg, &qev, &[], tz, &broken, out)?;
    out.quest = Some(view(&row, QuestStatus::Completed));
    Ok(())
}

/// Fail `row` (the active quest, past its deadline): reset the chain, record `quest_failed` at the
/// deadline and run its rules into `out`. The caller holds the transaction.
pub(crate) fn fail(store: &Store, rules: &Rules, cfg: &Config, row: &QuestRow, tz: FixedOffset, out: &mut Outcome) -> Result<(Event, QuestView)> {
    let lost = chain(store)?.n;
    set_meta_u32(store, QUEST_CHAIN, 0)?;
    let data = json!({ "quest_id": row.quest_id, "tier": row.tier, "chain_lost": lost });
    let mut qev = Event::new(EventKind::QuestFailed, Source::Quest, None, data);
    qev.ts = row.deadline;
    store.insert_event(&qev)?;
    let broken = records::update(store, &qev, tz)?;
    store.resolve_quest(row.id, "failed", Some(&qev.id), row.deadline)?;
    apply_rules(store, rules, cfg, &qev, &[], tz, &broken, out)?;
    Ok((qev, view(row, QuestStatus::Failed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{expire_quest, process};
    use crate::event::{EventKind, Source};
    use crate::quests::parse_quest;
    use crate::rules::parse_rule;
    use chrono::Duration;
    use serde_json::{json, Value};

    const SHIP_IT: &str = include_str!("../../assets/quests/ship_it_speedrun.toml");
    const HOT_STREAK: &str = include_str!("../../assets/quests/hot_streak.toml");
    const LAND: &str = include_str!("../../assets/quests/land_the_plane.toml");

    fn tz() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-05T10:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn mins(m: i64) -> DateTime<Utc> {
        t0() + Duration::minutes(m)
    }

    fn ev(kind: EventKind, repo: &str, ts: DateTime<Utc>, data: Value) -> Event {
        let source = if matches!(kind, EventKind::Commit | EventKind::Push) { Source::Git } else { Source::Github };
        let mut e = Event::new(kind, source, Some(repo.into()), data);
        e.ts = ts;
        e
    }

    fn q(text: &str) -> QuestDef {
        parse_quest(text).unwrap()
    }

    /// A quest with no cooldown that fires on commits with probability `chance`.
    fn easy(chance: f64) -> QuestDef {
        q(&format!(
            "id = \"easy\"\nname = \"Easy\"\ntier = \"bronze\"\ntrigger = [\"commit\"]\nchance = {chance}\nminutes = 30\nsteps = [{{ kind = \"push\" }}]\nfallback = \"f\"\n"
        ))
    }

    fn no_gap() -> Config {
        toml::from_str("[quests]\nmin_gap_minutes = 0\n").unwrap()
    }

    fn zero() -> impl FnMut() -> f64 {
        || 0.0
    }

    fn run(store: &Store, rules: &Rules, e: &Event) -> Outcome {
        process(store, rules, &Config::default(), e, &[], tz()).unwrap()
    }

    /// Process `trigger`, offer `def` for it and accept at T (`t0`).
    fn start(store: &Store, def: &QuestDef, trigger: &Event) -> QuestView {
        run(store, &Rules::default(), trigger);
        let offer = offer_for(store, std::slice::from_ref(def), &Config::default(), trigger, t0(), tz(), &mut zero())
            .unwrap()
            .expect("offer");
        accept(store, &offer, t0()).unwrap()
    }

    fn progress(store: &Store) -> Vec<u32> {
        store.active_quest().unwrap().expect("active").progress
    }

    #[test]
    fn multiplier_steps_and_caps() {
        let cfg = QuestsConfig::default();
        assert_eq!(multiplier(0, &cfg), 1.0);
        assert_eq!(multiplier(1, &cfg), 1.5);
        assert_eq!(multiplier(8, &cfg), 5.0);
        assert_eq!(multiplier(20, &cfg), 5.0);
        assert_eq!(reward(500, 1.5), 750);
        assert_eq!(reward(125, 1.5), 187);
    }

    #[test]
    fn offer_requires_live_fresh_event() {
        let store = Store::open_in_memory().unwrap();
        let cfg = Config::default();
        let quests = [easy(1.0)];
        let offer = |e: &Event, roll: f64| offer_for(&store, &quests, &cfg, e, t0(), tz(), &mut || roll).unwrap();
        let mut old = ev(EventKind::Commit, "r", mins(-1), json!({}));
        old.backfill = true;
        assert!(offer(&old, 0.0).is_none(), "backfill");
        assert!(offer(&ev(EventKind::Commit, "r", mins(-6), json!({})), 0.0).is_none(), "6 min old");
        let o = offer(&ev(EventKind::Commit, "r", mins(-4), json!({})), 0.0).expect("4 min old offers");
        assert_eq!((o.quest_id.as_str(), o.reward, o.multiplier, o.next_multiplier), ("easy", 125, 1.0, 1.5));
        assert_eq!(o.step_labels, vec!["Push".to_string()]);
        assert!(offer(&ev(EventKind::Commit, "r", mins(0), json!({})), 0.0).is_none(), "min gap");

        let store = Store::open_in_memory().unwrap();
        let half = [easy(0.5)];
        let e = ev(EventKind::Commit, "r", mins(0), json!({}));
        assert!(offer_for(&store, &half, &cfg, &e, t0(), tz(), &mut || 0.7).unwrap().is_none(), "roll misses");
        assert!(offer_for(&store, &half, &cfg, &e, t0(), tz(), &mut || 0.3).unwrap().is_some(), "a miss starts no gap");
    }

    #[test]
    fn offer_respects_per_quest_cooldown() {
        let store = Store::open_in_memory().unwrap();
        let quests = [q(SHIP_IT)];
        let cfg = no_gap();
        let at = |m: i64| {
            let e = ev(EventKind::Commit, "r", mins(m), json!({}));
            offer_for(&store, &quests, &cfg, &e, mins(m), tz(), &mut zero()).unwrap()
        };
        assert!(at(0).is_some());
        assert!(at(60).is_none(), "24 h cooldown");
        assert!(at(24 * 60).is_some());
    }

    #[test]
    fn offer_blocked_while_quest_active() {
        let store = Store::open_in_memory().unwrap();
        let cfg = no_gap();
        let quests = [easy(1.0)];
        let e = ev(EventKind::Commit, "r", mins(0), json!({}));
        let o = offer_for(&store, &quests, &cfg, &e, t0(), tz(), &mut zero()).unwrap().unwrap();
        accept(&store, &o, t0()).unwrap();
        assert!(offer_for(&store, &quests, &cfg, &e, t0(), tz(), &mut zero()).unwrap().is_none());
        cancel(&store, t0()).unwrap();
        assert!(offer_for(&store, &quests, &cfg, &e, t0(), tz(), &mut zero()).unwrap().is_some());
    }

    #[test]
    fn quest_events_never_offer() {
        let store = Store::open_in_memory().unwrap();
        let mut def = easy(1.0);
        def.trigger.push(EventKind::QuestCompleted);
        let mut e = Event::new(EventKind::QuestCompleted, Source::Quest, None, json!({}));
        e.ts = t0();
        assert!(offer_for(&store, &[def], &Config::default(), &e, t0(), tz(), &mut zero()).unwrap().is_none());
    }

    #[test]
    fn excluded_repo_never_offers() {
        let store = Store::open_in_memory().unwrap();
        let cfg: Config = toml::from_str("[paths]\nexclude = ['C:/Temp']\n").unwrap();
        let e = ev(EventKind::Commit, "C:/Temp/scratch", t0(), json!({}));
        assert!(offer_for(&store, &[easy(1.0)], &cfg, &e, t0(), tz(), &mut zero()).unwrap().is_none());
        let e = ev(EventKind::Commit, "C:/work/app", t0(), json!({}));
        assert!(offer_for(&store, &[easy(1.0)], &cfg, &e, t0(), tz(), &mut zero()).unwrap().is_some());
    }

    #[test]
    fn disabled_quests_never_offer() {
        let store = Store::open_in_memory().unwrap();
        let cfg: Config = toml::from_str("[quests]\nenabled = false\n").unwrap();
        let e = ev(EventKind::Commit, "r", t0(), json!({}));
        assert!(offer_for(&store, &[easy(1.0)], &cfg, &e, t0(), tz(), &mut zero()).unwrap().is_none());
        let mut off = easy(1.0);
        off.enabled = false;
        assert!(offer_for(&store, &[off], &Config::default(), &e, t0(), tz(), &mut zero()).unwrap().is_none());
    }

    #[test]
    fn steps_complete_in_order_within_window() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        let v = start(&store, &q(SHIP_IT), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        assert_eq!((v.status, v.reward, v.deadline), (QuestStatus::Active, 500, mins(15)));
        assert!(v.has_github_step);
        let o = run(&store, &rules, &ev(EventKind::PrOpened, "r", mins(1), json!({"url": "u1"})));
        assert!(o.quest.is_none(), "push comes first");
        let o = run(&store, &rules, &ev(EventKind::Push, "r", mins(2), json!({})));
        let qv = o.quest.expect("advanced");
        assert_eq!(qv.status, QuestStatus::Active);
        assert_eq!(qv.steps.iter().map(|s| (s.have, s.need)).collect::<Vec<_>>(), vec![(1, 1), (0, 1)]);
        let o = run(&store, &rules, &ev(EventKind::PrOpened, "r", mins(3), json!({"url": "u2"})));
        assert_eq!(o.quest.as_ref().unwrap().status, QuestStatus::Completed);
        let gain = o.xp_gains.iter().find(|g| g.label == "Quest: Ship It (Speedrun)").expect("quest gain");
        assert_eq!((gain.amount, gain.achievement), (500, None));
        assert_eq!(o.total_xp, store.total_xp().unwrap());
        assert_eq!(chain(&store).unwrap(), Chain { n: 1, best: 1, passes: 0 });
        assert!(store.active_quest().unwrap().is_none());
        let done = store.events_of(&[EventKind::QuestCompleted], None).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!((done[0].data["chain"].as_u64(), done[0].data["xp"].as_u64()), (Some(1), Some(500)));
        assert_eq!((done[0].ts, done[0].source, done[0].repo.as_deref()), (mins(3), Source::Quest, None));
        let row = &store.list_quests(1).unwrap()[0];
        assert_eq!((row.status.as_str(), row.result_event_id.as_deref()), ("completed", Some(done[0].id.as_str())));
        // The ledger row is against the quest_completed event.
        let listed = store.list_events(Some(EventKind::QuestCompleted), 1).unwrap();
        let gains: Vec<(&str, u64)> = listed[0].gains.iter().map(|g| (g.reason.as_str(), g.amount)).collect();
        assert_eq!(gains, vec![("Quest: Ship It (Speedrun)", 500)]);
    }

    #[test]
    fn completion_pays_the_chain_multiplier() {
        let store = Store::open_in_memory().unwrap();
        store.meta_set(QUEST_CHAIN, "1").unwrap();
        let v = start(&store, &q(HOT_STREAK), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        assert_eq!((v.multiplier, v.reward), (1.5, 187));
        for m in 1..=4 {
            run(&store, &Rules::default(), &ev(EventKind::Commit, "r", mins(m), json!({})));
        }
        assert_eq!(store.total_xp().unwrap(), 5 * 10 + 187);
        assert_eq!(chain(&store).unwrap().n, 2);
    }

    #[test]
    fn events_before_accept_or_after_deadline_dont_count() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        let trigger = ev(EventKind::Commit, "r", mins(-1), json!({}));
        start(&store, &q(HOT_STREAK), &trigger);
        assert!(run(&store, &rules, &trigger).quest.is_none(), "the trigger itself");
        let early = ev(EventKind::Commit, "r", t0() - Duration::seconds(1), json!({}));
        assert!(run(&store, &rules, &early).quest.is_none(), "before accept");
        assert!(run(&store, &rules, &ev(EventKind::Commit, "r", mins(31), json!({}))).quest.is_none(), "after deadline");
        assert_eq!(progress(&store), vec![0]);
        assert!(run(&store, &rules, &ev(EventKind::Commit, "r", mins(30), json!({}))).quest.is_some(), "at the deadline");
    }

    #[test]
    fn binding_scopes_steps() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        start(&store, &q(SHIP_IT), &ev(EventKind::Commit, "C:/a", mins(-1), json!({"github_repo": "o/a"})));
        assert!(run(&store, &rules, &ev(EventKind::Push, "C:/b", mins(1), json!({"github_repo": "o/b"}))).quest.is_none());
        // Another clone of the same GitHub repo counts, and so does the PR event (repo `owner/name`).
        assert!(run(&store, &rules, &ev(EventKind::Push, "C:/a2", mins(2), json!({"github_repo": "o/a"}))).quest.is_some());
        assert!(run(&store, &rules, &ev(EventKind::PrOpened, "o/b", mins(3), json!({}))).quest.is_none());
        let o = run(&store, &rules, &ev(EventKind::PrOpened, "o/a", mins(4), json!({})));
        assert_eq!(o.quest.unwrap().status, QuestStatus::Completed);

        let store = Store::open_in_memory().unwrap();
        start(&store, &q(LAND), &ev(EventKind::PrOpened, "o/a", mins(-1), json!({"url": "u1"})));
        assert!(run(&store, &rules, &ev(EventKind::PrMerged, "o/a", mins(1), json!({"url": "u2"}))).quest.is_none());
        let o = run(&store, &rules, &ev(EventKind::PrMerged, "o/a", mins(2), json!({"url": "u1"})));
        assert_eq!(o.quest.unwrap().status, QuestStatus::Completed);
    }

    #[test]
    fn branch_binding_needs_same_repo_and_branch() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        let def = q(&SHIP_IT.replace("same = \"repo\"", "same = \"branch\""));
        start(&store, &def, &ev(EventKind::Commit, "r", mins(-1), json!({"branch": "feat"})));
        assert!(run(&store, &rules, &ev(EventKind::Push, "r", mins(1), json!({"branch": "main"}))).quest.is_none());
        assert!(run(&store, &rules, &ev(EventKind::Push, "r2", mins(2), json!({"branch": "feat"}))).quest.is_none());
        assert!(run(&store, &rules, &ev(EventKind::Push, "r", mins(3), json!({"branch": "feat"}))).quest.is_some());
    }

    #[test]
    fn duplicate_event_counts_once() {
        let store = Store::open_in_memory().unwrap();
        let rules = Rules::default();
        start(&store, &q(SHIP_IT), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        let push = ev(EventKind::Push, "r", mins(1), json!({}));
        assert!(run(&store, &rules, &push).quest.is_some());
        assert!(run(&store, &rules, &push).quest.is_none());
        assert_eq!(progress(&store), vec![1, 0]);
    }

    #[test]
    fn active_quest_survives_definition_removal() {
        let store = Store::open_in_memory().unwrap();
        start(&store, &q(SHIP_IT), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        let rules = Rules { quests: vec![], ..Default::default() };
        run(&store, &rules, &ev(EventKind::Push, "r", mins(1), json!({})));
        let o = run(&store, &rules, &ev(EventKind::PrOpened, "r", mins(2), json!({})));
        assert_eq!(o.quest.unwrap().status, QuestStatus::Completed);
    }

    #[test]
    fn step_filters_come_from_the_snapshot() {
        let store = Store::open_in_memory().unwrap();
        let def = q(include_str!("../../assets/quests/mop_up_duty.toml"));
        start(&store, &def, &ev(EventKind::Commit, "r", mins(-1), json!({"lines_added": 400, "lines_removed": 0})));
        let row = store.active_quest().unwrap().unwrap();
        assert_eq!(
            row.steps,
            json!([{ "kind": "commit", "filter": { "lines": { "min_delete_ratio": 0.5 } }, "count": 1, "label": "Commit a mostly-deleting cleanup" }])
        );
        assert_eq!(row.binding, json!({ "repo": "r", "branch": null, "pr_url": null }));
        let rules = Rules::default();
        let mixed = ev(EventKind::Commit, "r", mins(1), json!({"lines_added": 60, "lines_removed": 40}));
        assert!(run(&store, &rules, &mixed).quest.is_none());
        let o = run(&store, &rules, &ev(EventKind::Commit, "r", mins(2), json!({"lines_added": 10, "lines_removed": 90})));
        assert_eq!(o.quest.unwrap().status, QuestStatus::Completed);
    }

    #[test]
    fn unreadable_snapshot_step_matches_nothing() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_quest(&crate::store::NewQuest {
                quest_id: "x".into(),
                name: "X".into(),
                tier: "bronze".into(),
                trigger_event_id: "t".into(),
                binding: json!({}),
                steps: json!([{ "kind": "push", "filter": { "bogus": 1 }, "count": 1, "label": "Push" }]),
                accepted_at: t0(),
                deadline: mins(30),
                chain_at_start: 0,
                multiplier: 1.0,
                xp: 100,
            })
            .unwrap();
        assert!(run(&store, &Rules::default(), &ev(EventKind::Push, "r", mins(1), json!({}))).quest.is_none());
        let v = active_view(&store).unwrap().unwrap();
        assert_eq!(v.steps, vec![StepView { label: "Push".into(), have: 0, need: 1 }]);
    }

    #[test]
    fn expire_fails_and_resets_chain() {
        let store = Store::open_in_memory().unwrap();
        store.meta_set(QUEST_CHAIN, "3").unwrap();
        store.meta_set(QUEST_BEST_CHAIN, "3").unwrap();
        start(&store, &q(HOT_STREAK), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        let cfg = Config::default();
        assert!(expire_quest(&store, &Rules::default(), &cfg, mins(29), tz()).unwrap().is_none());
        let (e, out, v) = expire_quest(&store, &Rules::default(), &cfg, mins(31), tz()).unwrap().expect("expired");
        assert_eq!(v.status, QuestStatus::Failed);
        assert_eq!((e.kind, e.ts, e.data["chain_lost"].as_u64()), (EventKind::QuestFailed, mins(30), Some(3)));
        assert!(out.xp_gains.is_empty());
        assert_eq!(store.events_of(&[EventKind::QuestFailed], None).unwrap().len(), 1);
        assert_eq!(chain(&store).unwrap(), Chain { n: 0, best: 3, passes: 0 });
        assert!(store.active_quest().unwrap().is_none());
        assert_eq!(store.list_quests(1).unwrap()[0].status, "failed");
        assert!(expire_quest(&store, &Rules::default(), &cfg, mins(60), tz()).unwrap().is_none());
    }

    #[test]
    fn passes_break_chain() {
        let store = Store::open_in_memory().unwrap();
        let cfg = QuestsConfig::default();
        store.meta_set(QUEST_CHAIN, "2").unwrap();
        assert!(!pass(&store, &cfg).unwrap());
        assert!(!pass(&store, &cfg).unwrap());
        assert_eq!(chain(&store).unwrap().passes, 2);
        assert!(pass(&store, &cfg).unwrap());
        assert_eq!((chain(&store).unwrap().n, chain(&store).unwrap().passes), (0, 0));
        pass(&store, &cfg).unwrap();
        assert_eq!(chain(&store).unwrap().passes, 1);
        start(&store, &q(HOT_STREAK), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        assert_eq!(chain(&store).unwrap().passes, 0, "accept resets passes");
    }

    #[test]
    fn cancel_keeps_chain() {
        let store = Store::open_in_memory().unwrap();
        store.meta_set(QUEST_CHAIN, "2").unwrap();
        assert!(cancel(&store, t0()).unwrap().is_none(), "nothing active");
        start(&store, &q(HOT_STREAK), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        let v = cancel(&store, mins(1)).unwrap().expect("cancelled");
        assert_eq!(v.status, QuestStatus::Cancelled);
        assert!(store.active_quest().unwrap().is_none());
        assert_eq!(chain(&store).unwrap().n, 2);
        assert!(store.events_of(&[EventKind::QuestCompleted, EventKind::QuestFailed], None).unwrap().is_empty());
    }

    #[test]
    fn quest_event_unlocks_rules() {
        let store = Store::open_in_memory().unwrap();
        let fumble = parse_rule(
            "id = \"fumble\"\nname = \"Fumble\"\ntier = \"bronze\"\nxp = 5\nfallback = \"f\"\ntrigger = [\"quest_failed\"]\ncounter = { type = \"run\", at = 3 }\n",
        )
        .unwrap();
        let rules = Rules { list: vec![fumble], ..Default::default() };
        let trigger = ev(EventKind::Commit, "r", mins(-1), json!({}));
        run(&store, &rules, &trigger);
        let offer = offer_for(&store, &[q(HOT_STREAK)], &Config::default(), &trigger, t0(), tz(), &mut zero()).unwrap().unwrap();
        let mut unlocked = vec![];
        for i in 0..3 {
            accept(&store, &offer, mins(i * 120)).unwrap();
            let (_, out, _) = expire_quest(&store, &rules, &Config::default(), mins(i * 120 + 31), tz()).unwrap().unwrap();
            unlocked.push(out.unlocks.iter().map(|u| u.rule_id.clone()).collect::<Vec<_>>());
        }
        assert_eq!(unlocked, vec![vec![], vec![], vec!["fumble".to_string()]]);
        assert_eq!(store.total_xp().unwrap(), 10 + 5);
    }

    #[test]
    fn quest_completed_rules_see_the_new_chain() {
        let store = Store::open_in_memory().unwrap();
        let gang = parse_rule(
            "id = \"gang\"\nname = \"Gang\"\ntier = \"bronze\"\nxp = 7\nfallback = \"f\"\ntrigger = [\"quest_completed\"]\nfilter = { chain = { min = 2 } }\n",
        )
        .unwrap();
        let rules = Rules { list: vec![gang], ..Default::default() };
        store.meta_set(QUEST_CHAIN, "1").unwrap();
        start(&store, &q(SHIP_IT), &ev(EventKind::Commit, "r", mins(-1), json!({})));
        run(&store, &rules, &ev(EventKind::Push, "r", mins(1), json!({})));
        let o = run(&store, &rules, &ev(EventKind::PrOpened, "r", mins(2), json!({})));
        assert_eq!(o.unlocks.iter().map(|u| u.rule_id.as_str()).collect::<Vec<_>>(), vec!["gang"]);
        assert!(o.xp_gains.iter().any(|g| g.label == "Quest: Ship It (Speedrun)" && g.amount == 750));
    }
}
