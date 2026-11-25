# Tier styling on banners and achievement cards (#32)

## Intent

Today only the toast pill is tiered. Every banner wears the same gold, so a bronze banner looks like a gold one,
and unlocked character-sheet cards only color their small tier label. Each of the four tiers should be
recognizable at a glance on the banner and on unlocked cards, while everything still reads as one Gold Banner family.
Rarer tiers should *feel* rarer.

## Goals

- One tier color drives the banner's border, heading, name glow, reward line and background tint.
- Escalation by rarity: glow strength grows Bronze → Platinum; Gold and Platinum get a bigger entrance;
  Platinum gets a shimmer sweep.
- Unlocked cards take the tier color for border, glow and name glow.
- One palette, shared by overlay and site, with a test that keeps them in sync (fixes the silver mismatch).
- `prefers-reduced-motion` respected.
- `docs/manual-checklist.md` covers each tier's banner, toast and card.

## Non-goals

- The history-list `achbadge` stays gold.
- No sound code changes: `[sound] by_tier` already lets tiers sound different.
- No shared-asset route between overlay and site.

## Anti-goals

- Tiers that no longer look like one family (the Gold Banner frame, layout and dark edge stay the same).
- A banner or its glow clipped by its transparent window.
- Unreadable silver/platinum text on the dark background.

## Approaches considered

- **A. Color only:** each tier recolors the same frame. Smallest, but rare unlocks don't feel special.
- **B. Color + escalation:** glow scales with rarity, one Platinum shimmer.
- **C. B + a bigger entrance for Gold and Platinum.** **Chosen by the user.**

For one source of truth: a shared CSS file would need a new site route (the site embeds only `ui/site/`),
so instead each stylesheet defines the palette and a Rust test asserts they match.

## Design

**Palette** (in `:root` of both `ui/overlay/overlay.css` and `ui/site/site.css`):
`--bronze: #cd7f32; --silver: #c0c8d0; --gold: #ffc83d; --platinum: #cfe8ff;`
Silver uses the site's cooler value. The toast pill uses these variables instead of literals.
A Rust test (`include_str!` both files) fails if the four values differ.

**Banner** (`.ach-b`, keyed by the existing `data-tier="Bronze|Silver|Gold|Platinum"`):

- `--tier` sets border, `.hdr`, `.name` text-shadow and `.rew` color.
- Background: radial gradient whose center is a dark tint of the tier color; edge stays `#140e00`.
- No tier (summary banner): unchanged gold look.

| Tier | Glow | Entrance | Extra |
|---|---|---|---|
| Bronze | 14px, soft | current pop | — |
| Silver | 18px | current pop | — |
| Gold | 28px | from scale .7, stronger overshoot | one border flash |
| Platinum | 32px, strongest | same as Gold | border flash and one shimmer sweep together |

Peak scale stays ≤ 1.03. The banner window is 620×280 with a 40px inset (frame unchanged at 540×200, same screen position), so the full glow and flash show instead of being cut off at the window edge.

**Toast pill:** the Platinum pill also runs one shimmer sweep after it slides in.

Under `prefers-reduced-motion: reduce`: opacity fade only, no scale, flash or shimmer.

**Cards** (`site.js` adds a tier class to unlocked cards, e.g. `t-gold`): `--tier` drives border, glow and
name glow. Platinum's glow is a little stronger, static. Locked cards unchanged.

**Checklist:** replace the platinum-only lines with a per-tier banner/toast/card check, a reduced-motion check,
and a `by_tier` sound check.

## Left to the builder

Exact tint colors, keyframe timing and shimmer gradient, within the constraints above; contrast checked by eye
on the dark background.
