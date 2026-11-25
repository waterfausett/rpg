# Tier Styling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give each achievement tier its own look on the banner and on unlocked character-sheet cards, escalating with rarity.

**Architecture:** CSS custom properties. Each stylesheet defines the four tier colors in `:root`; a `--tier` variable is set per element from the tier and drives every colored part. A Rust test keeps the two palettes identical.

**Tech Stack:** Vanilla CSS/JS in `ui/`, Rust tests in `crates/rpg-app`.

**Spec:** `docs/superpowers/specs/2026-11-25-tier-styling-design.md`

## Global Constraints

- Palette, verbatim in both stylesheets: `--bronze: #cd7f32; --silver: #c0c8d0; --gold: #ffc83d; --platinum: #cfe8ff;`
- Banner tier comes from `data-tier` = `Bronze|Silver|Gold|Platinum` (capitalized, set by `overlay.js`); site tiers are lowercase (`Tier::as_str`).
- Banner peak scale ≤ 1.03 (window 560×220, banner `inset: 10px`).
- `prefers-reduced-motion: reduce` → banner fades only: no scale, flash or shimmer.
- Gold Banner frame (layout, fonts, `#140e00` gradient edge, `#fff3cf` text) is the same for every tier.

## Review Focus

- Summary / "while you were away" banner has `data-tier=""`: must keep today's gold look (default `--tier: var(--gold)`).
- A banner shown twice in a row (same or different tier) must replay entrance, flash and shimmer: they live on `.ach-b.in`, which `showBanner` re-adds after a reflow.
- Ladder cards: `a.tier` is the highest unlocked step, so a Silver-step ladder card is silver; a locked card gets no tier color.
- Silver vs Platinum must be tellable apart on the banner (silver is neutral grey, platinum is blue-tinted; tint the background differently).
- Disabled (`enabled === false`) but unlocked cards still show their tier color.

---

### Task 1: Shared palette and sync test

**Files:**
- Modify: `ui/overlay/overlay.css` (add `:root` palette; toast rules lines 15-19 use it)
- Modify: `ui/site/site.css:1-12` (silver already `#c0c8d0`; no value change needed)
- Test: `crates/rpg-app/src/site.rs` (`mod tests`, line 256)

**Interfaces:**
- Produces: CSS vars `--bronze`, `--silver`, `--gold`, `--platinum` in both stylesheets' `:root`.

- [ ] **Step 1: Write the failing test** `tier_palette_matches_between_overlay_and_site`: `include_str!` both `../../../ui/overlay/overlay.css` and `../../../ui/site/site.css`; for each of `bronze silver gold platinum`, extract the value after `--<name>:` up to `;` (trimmed) from each file; assert both are `Some` and equal, and that `silver == "#c0c8d0"`.
- [ ] **Step 2: Run** `cargo test -p rpg-app tier_palette` → FAIL (overlay has no `--bronze`).
- [ ] **Step 3: Implement**: add `:root { --bronze…; --silver…; --gold…; --platinum…; }` to `overlay.css`; change `.xp.ach[data-tier=…]` rules to `--tier: var(--bronze)` etc.
- [ ] **Step 4: Run** `cargo test -p rpg-app` → all PASS.
- [ ] **Step 5: Commit** `Overlay and site share one tier palette; silver is #c0c8d0 everywhere (#32)`.

### Task 2: Tiered banner with escalation

**Files:**
- Modify: `ui/overlay/overlay.css:39-56` (`.ach-b` block)
- Test: `crates/rpg-app/src/site.rs` tests (CSS presence check)

**Interfaces:**
- Consumes: Task 1 palette vars.
- Produces: `.ach-b` uses `--tier` (default `var(--gold)`) and `--tint` (gradient center).

- [ ] **Step 1: Write the failing test** `banner_styles_every_tier_and_reduced_motion`: in the `include_str!` of `overlay.css`, assert it contains `.ach-b[data-tier="Bronze"]`, `…"Silver"]`, `…"Gold"]`, `…"Platinum"]` and `prefers-reduced-motion`.
- [ ] **Step 2: Run** `cargo test -p rpg-app banner_styles` → FAIL.
- [ ] **Step 3: Implement** in `.ach-b`:
  - Base: `--tier: var(--gold); --tint: #4a3500;` border, `.hdr` color, `.name` text-shadow, `.rew` color and background center all from these vars; `#140e00` edge kept. Summary banner (no tier) therefore looks as today.
  - Per tier `--tier`, `--tint` (builder picks dark tints; silver neutral grey, platinum dark blue) and glow: Bronze `0 0 14px` ~33% alpha, Silver `18px`, Gold `28px` ~60%, Platinum `32px` ~80% (use `color-mix(in srgb, var(--tier) N%, transparent)`).
  - Gold + Platinum `.in`: keyframe `banner-in-big` from `scale(.7)` overshooting to ≤ `1.03`, ~.45s; plus a one-shot `border-flash` (border/glow briefly brighter, ~.6s, delayed to landing).
  - Platinum `.in`: `::after` overlay (pointer-events none, `border-radius: inherit`, `overflow: hidden` on `.ach-b`) with a diagonal light gradient translated left→right once, ~1s, starting after the entrance.
  - `@media (prefers-reduced-motion: reduce)`: `.ach-b.in` / `.out` use opacity-only keyframes, no transform; flash and `::after` animation `none`.
- [ ] **Step 4: Run** `cargo test -p rpg-app` → PASS; then check by hand (Task 4 checklist lines) that all four tiers and the summary banner look right and Gold/Platinum aren't clipped.
- [ ] **Step 5: Commit** `Banner: each tier gets its own color; rarer tiers glow, pop and shimmer more (#32)`.

### Task 3: Tiered unlocked cards

**Files:**
- Modify: `ui/site/site.js:156` (card class)
- Modify: `ui/site/site.css:62,74` (`.ach.unlocked`, `.ach.unlocked .name`)

**Interfaces:**
- Consumes: Task 1 palette vars.
- Produces: unlocked cards carry class `t-<tier>` (lowercase, e.g. `t-platinum`).

- [ ] **Step 1: Implement**: in `site.js`, unlocked cards also get `" t-" + a.tier`. In `site.css`, `.ach.unlocked { --tier: var(--gold); border-color: var(--tier); box-shadow: 0 0 12px color-mix(in srgb, var(--tier) 35%, transparent); }`, `.ach.t-bronze { --tier: var(--bronze) }` etc., `.ach.t-platinum` glow ~18px / 55%; `.ach.unlocked .name` text-shadow uses `var(--tier)`. Locked cards untouched.
- [ ] **Step 2: Verify**: `cargo test -p rpg-app` PASS (site asset test still serves `site.js`/`site.css`); run the app, open the Achievements tab, confirm one card per tier shows its color, and a locked card shows none.
- [ ] **Step 3: Commit** `Character sheet: unlocked cards take their tier's color (#32)`.

### Task 4: Manual checklist

**Files:**
- Modify: `docs/manual-checklist.md` (replace lines 14 and 29)

- [ ] **Step 1: Edit**: replace the platinum-only banner line with one line per tier: Bronze (copper `#cd7f32`, soft glow), Silver (grey `#c0c8d0`), Gold (`#ffc83d`, bigger pop and border flash), Platinum (pale blue `#cfe8ff`, bigger pop, one shimmer sweep), none clipped; a line that the summary banner stays gold; a reduced-motion line (Windows *Animation effects* off → banner only fades); a `by_tier` sound line. Replace the platinum card line with: an unlocked card's border, glow and name glow match its tier; locked cards are uncolored. Toast line: each tier's pill color matches its banner.
- [ ] **Step 2: Commit** `Checklist: per-tier banner, toast and card checks (#32)`.
