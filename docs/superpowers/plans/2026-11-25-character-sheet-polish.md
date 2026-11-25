# Character Sheet Polish ("Refined Gold") Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restyle the localhost character sheet in the approved "Refined Gold" look, with tier colors on every achievement surface, including the event rows.

**Architecture:** The data side adds a tier to each ledger gain in `Store::list_events` (looked up from `unlocks` by name + timestamp), and `/api/events` / `/api/character` send it as `achievements[].tier`. The page side rewrites `ui/site/site.css` and makes small markup and JS changes in `index.html` / `site.js`. There's still no build step: `site.rs` embeds `ui/site/` with rust-embed.

**Tech Stack:** Rust (rusqlite, axum, serde_json), vanilla HTML/CSS/JS.

**Spec:** `docs/superpowers/specs/2026-11-25-character-sheet-polish-design.md`. The approved mockups are in `.superpowers/brainstorm/157229-1795643442/content/refined-gold-tabs.html` (gitignored, local only). Open it in a browser for reference.

## Global Constraints

- Palette stays `#140e00 / #4a3500 / #ffc83d / #fff3cf` (`--bg / --glow / --gold / --text`).
- Tier palette in `site.css` `:root` stays byte-identical to `overlay.css`: `--bronze: #cd7f32; --silver: #c0c8d0; --gold: #ffc83d; --platinum: #cfe8ff; --legendary: #c77dff; --celestial: #7df3ff;`.
- The six tiers, in rank order: `bronze, silver, gold, platinum, legendary, celestial`.
- The existing `site.rs` tests must keep passing untouched. They require:
  - a `.ach.t-<tier>` rule for all six tiers;
  - `card-sweep` and a `prefers-reduced-motion` block in `site.css`;
  - `"unlocked t-" + a.tier` in `site.js`.
- All untrusted text is rendered with `textContent`. Tier strings are only used after checking them against the six-tier list.
- The CSP is `default-src 'self'`: no inline `<style>`, no `style="…"` attributes in `index.html`, and no external fonts or scripts. Setting `el.style` from `site.js` is fine.
- Must work under 640px wide with no horizontal scroll.
- Overlay files (`ui/overlay/*`) are not touched.

## Review Focus

1. **An event row with achievements of different tiers** (e.g. a bronze and a gold on one commit): each badge shows its own tier, and the row tint uses the highest tier. Covered by `rowTier` in Task 3 and the by-eye check in Task 5.
2. **Old events or unmatched names** have no `tier`: the row and badge fall back to gold, with no JS error. Covered by `gains_without_unlock_have_no_tier` (Task 1) and `rowTier` in Task 3.
3. **Level 100 or higher in the crest:** three digits fit inside the 64px circle. Covered in Task 2's CSS and its by-eye check.
4. **Long names** (skill, achievement, repo up to 60 chars) truncate with an ellipsis and don't widen the page. Covered in Tasks 2, 3 and 4 and the by-eye check in Task 5.
5. **The 5-second auto-refresh keeps the active filter chip** and doesn't reset to "all". Covered in Task 3 (`currentKind` state) and the by-eye check in Task 5.

---

### Task 1: Event achievements carry their tier

**Files:**
- Modify: `crates/rpg-core/src/store.rs` (struct `EventRow` ~L49-55, `list_events` ~L415-431, test `xp_events_and_lookups` ~L630)
- Modify: `crates/rpg-app/src/site.rs` (`event_json` ~L77-88, test `event_xp_split_into_base_and_achievements` ~L362-369)

**Interfaces:**
- Produces: `pub struct Gain { pub reason: String, pub amount: u64, pub tier: Option<String> }` in `rpg_core::store`; `EventRow.gains: Vec<Gain>`.
- Produces (API): each entry in `achievements` on `/api/events` and `/api/character`.recent is `{ "name", "xp", "tier"? }`. `tier` is a lowercase tier string, omitted when unknown.

- [ ] **Step 1: Write the failing tests**

In `store.rs` tests:

```rust
#[test]
fn gains_carry_unlock_tier() {
    let s = Store::open_in_memory().unwrap();
    let now = Utc::now();
    s.insert_event(&ev("c1", EventKind::Commit, now)).unwrap();
    s.add_xp("c1", 10, "commit", now).unwrap();
    s.insert_unlock(&NewUnlock { rule_id: "hat".into(), name: "Hat Trick".into(), tier: "silver".into(),
        xp: 50, context: serde_json::json!({}), ts: now }).unwrap();
    s.add_xp("c1", 50, "Hat Trick", now).unwrap();
    let g = &s.list_events(None, 10).unwrap()[0].gains;
    assert_eq!((g[0].reason.as_str(), g[0].tier.as_deref()), ("commit", None));
    assert_eq!((g[1].reason.as_str(), g[1].amount, g[1].tier.as_deref()), ("Hat Trick", 50, Some("silver")));
}

#[test]
fn gains_without_unlock_have_no_tier() {
    // Same name unlocked at a different time must not match.
    let s = Store::open_in_memory().unwrap();
    let now = Utc::now();
    s.insert_event(&ev("c1", EventKind::Commit, now)).unwrap();
    s.insert_unlock(&NewUnlock { rule_id: "hat".into(), name: "Hat Trick".into(), tier: "silver".into(),
        xp: 50, context: serde_json::json!({}), ts: now - chrono::Duration::days(1) }).unwrap();
    s.add_xp("c1", 50, "Hat Trick", now).unwrap();
    assert_eq!(s.list_events(None, 10).unwrap()[0].gains[0].tier, None);
}
```

In `site.rs`, change the expectation in `event_xp_split_into_base_and_achievements` (`RULE` is bronze):

```rust
assert_eq!(v[0]["achievements"], json!([{ "name": "Merge Maestro", "xp": 250, "tier": "bronze" }]));
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --workspace gains_ event_xp_split`
Expected: compile error (no `Gain` / `.reason`), or after stubbing, FAIL on the tier assertions.

- [ ] **Step 3: Implement**

- `store.rs`: add `Gain`, change `EventRow.gains` to `Vec<Gain>`, and update the doc comment. In `list_events`, fetch the tier in the existing per-event gains query with a correlated subquery:
  `(SELECT u.tier FROM unlocks u WHERE u.name = l.reason AND u.ts = l.ts ORDER BY u.id LIMIT 1)`
  over `xp_ledger l`. The engine writes both rows with `ev.ts` via `ts_str`, so the strings match exactly. Fix the existing `g.1` in `xp_events_and_lookups` to `g.amount`.
- `site.rs` `event_json`: partition on `g.reason`, and build each achievement as `{name, xp}`, adding `"tier"` only when `Some`.

- [ ] **Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: PASS, with no warnings about the old tuple.

- [ ] **Step 5: Commit**

```bash
git add crates/rpg-core/src/store.rs crates/rpg-app/src/site.rs
git commit -m "Events API: each achievement carries its tier"
```

---

### Task 2: Base styles, crest header, tabs and the Character tab

**Files:**
- Modify: `ui/site/site.css` (rewrite)
- Modify: `ui/site/index.html` (header, tabs)
- Modify: `ui/site/site.js` (`loadCharacter`, `renderSkills`, `loadAchievements` count line)
- Test: `crates/rpg-app/src/site.rs` tests module

**Interfaces:**
- Produces (DOM ids used by later tasks, unchanged where they exist): `#level`, `#xpfill`, `#xptext`, `#xptotal` (new), `#achcount`, `.tabs button[data-tab]`, `.panel`.
- Produces (CSS used by later tasks): `--line: rgba(255, 200, 61, .14)` hairline token; `.bar` / `.bar > i` thin progress bar (6px, gold gradient `#8a6200 → var(--gold)`, fill color overridable via `--fill`); `.pill` count pill.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn sheet_has_crest_header_and_no_inline_styles() {
    let html = include_str!("../../../ui/site/index.html");
    for needle in ["class=\"crest\"", "id=\"xptotal\"", "class=\"pill\""] {
        assert!(html.contains(needle), "index.html lacks {needle}");
    }
    assert!(!html.contains("style="), "inline style attributes are blocked by the CSP");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p rpg-app sheet_has_crest_header`
Expected: FAIL, "index.html lacks class=\"crest\"".

- [ ] **Step 3: Implement**

Follow the Character panel of the mockup.
- **Header:** a `.crest` circle (64px, 2px gold border, gold ring shadow) holding `#level` over a "LEVEL" caption. Beside it, "Crawler", the name `you`, the XP bar, and a line with `#xptext` on the left and `#xptotal` on the right.
  - `#xptext` reads `620 / 1,000 exp to level 8`, using `toLocaleString` and `c.level + 1`.
  - `#xptotal` reads `4,820 total`.
  - At 3+ digits the crest number drops to 20px (`site.js` toggles a `.crest.wide` class when `String(c.level).length >= 3`).
- **Tabs:** underline style. `#achcount` becomes a `.pill` reading `14/52` (no parentheses).
- **Headings:** `h2` gets a fading rule line after it (`::after`, flex: 1).
- **Totals:** tiles with tabular numbers.
- **Skills:** hairline-separated rows using `.bar`. Long names get an ellipsis.
- **Under 640px:** the header wraps, totals drop to 2 columns, and skill rows put the bar on a second line.
- Keep the six-tier `:root` palette, all `.ach.t-<tier>` rules, `card-sweep` and the reduced-motion block. Port them as-is for now; Task 4 restyles cards.

- [ ] **Step 4: Run tests and check by eye**

Run: `cargo test --workspace`. Expected: PASS.
Then run the app (`cd crates/rpg-app && cargo tauri dev`) and open the sheet. Check that the Character tab matches the mockup at full width and at 400px. To test a three-digit level, set `$("level")` to `123` in devtools and add `.wide`; the number must stay inside the crest.

- [ ] **Step 5: Commit**

```bash
git add ui/site/ crates/rpg-app/src/site.rs
git commit -m "Character sheet: Refined Gold header, tabs and Character tab"
```

---

### Task 3: Tier-tinted event rows and filter chips

**Files:**
- Modify: `ui/site/index.html` (event-log `<label class="filter">…<select>` → chip row)
- Modify: `ui/site/site.js` (`eventRow`, `loadEvents`, listeners)
- Modify: `ui/site/site.css`
- Test: `crates/rpg-app/src/site.rs` tests module

**Interfaces:**
- Consumes: `achievements[].tier` from Task 1; `--line` and `.pill` from Task 2.
- Produces (`site.js`): `const TIERS = ["bronze","silver","gold","platinum","legendary","celestial"]`; `tierVar(t) -> string` returns `"var(--" + t + ")"` when `TIERS.includes(t)`, else `"var(--gold)"`; `rowTier(achs) -> string|null` returns the highest-ranked known tier among `achs`, `"gold"` if they're all unknown, or `null` for none. Task 4 reuses `TIERS` and `tierVar`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn event_log_uses_chips_for_every_kind() {
    let html = include_str!("../../../ui/site/index.html");
    assert!(!html.contains("<select"), "the kind dropdown should be chips now");
    for kind in ["\"\"", "commit", "push", "pr_opened", "pr_merged", "review_submitted", "claude_session_start", "claude_session_end"] {
        assert!(html.contains(&format!("data-kind={kind}")) || html.contains(&format!("data-kind=\"{kind}\"")), "no chip for {kind}");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p rpg-app event_log_uses_chips`
Expected: FAIL, "the kind dropdown should be chips now".

- [ ] **Step 3: Implement**

- **Chips:** `<div class="chips" role="group" aria-label="Kind">` holding `<button class="chip" data-kind="…">` buttons. The labels are all, commit, push, pr_opened, pr_merged, review, session start, session end.
  - The active chip gets `.on` (solid gold, `#140e00` text) and `aria-pressed="true"`.
  - A module-level `let currentKind = ""` holds the filter; `loadEvents` reads it, so the 5s refresh keeps it. Remove the `$("kind")` listener.
- **Rows:** use a grid `62px 1fr auto 56px` with hairline separators. Kind and repo get an ellipsis.
  - When `rowTier` returns a tier, set `li.style.setProperty("--tier", tierVar(t))` and add `.has-ach`. That gives a 2px left border plus a tint of `color-mix(var(--tier) 14%)` fading to transparent.
  - Each `.achbadge` sets its own `--tier` from `tierVar(a.tier)` and renders as a rounded pill in that color.
- The same `eventRow` renders the Recent list on the Character tab, so it gets the same look for free.

- [ ] **Step 4: Run tests and check by eye**

Run: `cargo test --workspace`. Expected: PASS.
In the app, click through every chip and wait 6s on one: it stays active. Rows with a silver achievement show a silver edge and badge. Rows from before this change (no `tier`) show gold.

- [ ] **Step 5: Commit**

```bash
git add ui/site/ crates/rpg-app/src/site.rs
git commit -m "Character sheet: tier-tinted event rows and kind chips"
```

---

### Task 4: Achievement cards

**Files:**
- Modify: `ui/site/site.css` (`.ach*` rules, `.hiddenleft`)
- Modify: `ui/site/site.js` (`ladderProgress`, the card builder in `loadAchievements`)

**Interfaces:**
- Consumes: `TIERS`, `tierVar` (Task 3); `.bar` with `--fill` (Task 2).

- [ ] **Step 1: Implement**

Follow the Achievements panel of the mockup.
- **Unlocked cards:**
  - 1px border at 60% tier color, 10px radius.
  - A radial tint from the top (`color-mix(var(--tier) 16%)` → `var(--panel)`).
  - Outer glow `0 0 14px` at 22%, plus an inset top highlight.
  - Platinum and up keep the stronger glow (`18px` at 55%). Legendary and Celestial keep `card-sweep` and the ★ before the tier label.
- **Ladder bars:** set `--fill` to `tierVar(a.tier)` so they fill in the card's tier color. Bars on locked cards stay gold.
- **Locked cards:** dashed border, 0.65 opacity, no glow.
- **`off` tag:** absolutely positioned top-right.
- **`.hiddenleft`:** centered, italic Georgia, with ✦ ornaments via `::before` / `::after`.
- Keep the existing class names (`ach`, `unlocked t-<tier>`, `locked`, `tier <tier>`, `name`, `desc`, `meta`, `off`), so the existing tests and the reduced-motion rule still apply.
- Long names wrap within the card; the grid never overflows.

- [ ] **Step 2: Run tests and check by eye**

Run: `cargo test --workspace`. Expected: PASS (`unlocked_cards_style_every_tier` and `top_tiers_have_their_extra_flourishes` included).
In the app, check one card of each tier you have, a ladder card, a locked card, an `enabled = false` card, and the hidden-remaining line. Turn on reduced motion in the OS: the sweep stops.

- [ ] **Step 3: Commit**

```bash
git add ui/site/
git commit -m "Character sheet: Refined Gold achievement cards"
```

---

### Task 5: Checklist and final pass

**Files:**
- Modify: `docs/manual-checklist.md` (`## Character sheet` section)

- [ ] **Step 1: Update the checklist**

Add these items under `## Character sheet`:
- The header shows the level in a gold crest, with `N / M exp to level L+1` and the total. A three-digit level stays inside the crest.
- Event rows with an achievement have a left edge and a badge in its tier color. A row with mixed tiers uses the highest. Older rows without a tier are gold.
- The Event log chips filter in one click, and the active chip survives the 5-second refresh.
- At about 400px wide, all three tabs fit with no sideways scroll, and long skill, achievement and repo names truncate or wrap.

Also update the existing unlocked-card item to name all six tiers.

- [ ] **Step 2: Full verification**

Run: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`. Expected: both clean.
Walk through the checklist items above in the running app.

- [ ] **Step 3: Commit**

```bash
git add docs/manual-checklist.md
git commit -m "Checklist: Refined Gold character sheet"
```
