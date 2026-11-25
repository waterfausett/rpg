# Character sheet polish: "Refined Gold"

## Intent

A polish pass on the character sheet (`ui/site/`). The Gold Banner look stays, but tightened: clearer
hierarchy, a real header, calmer borders, and tier colors carried wherever an achievement shows up.
The user picked direction A ("Refined Gold") from three mockups (A, a pen-and-paper "Tabletop Sheet",
a video-game "Dungeon HUD"), then approved it carried through all three tabs.

## Goals

- **Header:** the level sits in a round gold crest ("7 / LEVEL"), with "Crawler" / `you` and the XP bar
  beside it. The XP line reads `620 / 1,000 exp to level 8` on the left and `4,820 total` on the right.
- **Tabs:** underline style (gold underline on the active tab). The achievement count becomes a small
  pill on the tab (`14/52`).
- **Section headings:** small-caps Georgia labels followed by a fading rule line.
- **Totals:** stat tiles with a faint top-down gradient and soft borders. Numbers use tabular figures.
- **Skills:** rows separated by hairlines instead of boxed cards, with a thinner bar.
- **Event rows (Recent and Event log):** hairline-separated grid rows. A row with an achievement gets a
  left edge and tint in that achievement's tier color, and its `★ Name +xp` badge is a tier-colored pill.
- **Achievement cards:** a thinner tier border, a glow from the top and an inset highlight. Ladder bars
  fill in the card's tier color. Locked cards are dashed and dimmed. The `off` tag sits top-right.
  "N hidden achievements remain" is centered with small ✦ ornaments.
- **Event log filter:** a row of chips (all, commit, push, pr_opened, pr_merged, review, session start,
  session end) replaces the `<select>`. One click filters, and the active chip is solid gold.

## Non-goals

- No change to the overlay (toasts and banners) or to tier colors.
- No new data on the sheet beyond the event achievements' tier.
- No CSS framework or build step. The site stays plain HTML, CSS and JS embedded by `site.rs`.

## Anti-goals

- Losing the Gold Banner family look (the palette stays `#140e00 / #4a3500 / #ffc83d / #fff3cf`).
- Untrusted text rendered any way except `textContent`.
- A layout that breaks under 640px wide, or a crest that overflows at three-digit levels.

## Approaches

1. **Restyle in place:** rewrite `site.css`, make small markup changes in `index.html` and `site.js`,
   and send each event achievement's tier from the server. **Chosen by the user.**
2. **CSS only:** no markup, JS or server change. Smaller, but loses the tier-tinted rows, the chips,
   the tab pill and the "to level N" wording.

A CSS framework or a build step was dropped: the site has no build, and a restyle doesn't justify one.

## How (decided)

- `ui/site/site.css`: rewritten in the Refined Gold style. Keeps the `:root` tier palette exactly
  (the `tier_palette_matches_between_overlay_and_site` test reads it).
- `ui/site/index.html`: crest header markup, tab pill, chip row in place of the `<select>`.
- `ui/site/site.js`: renders the new header text, sets `--tier` on achievement event rows, badges and
  ladder bars, and drives the chips (same `/api/events?kind=` query as today).
- Event achievements gain a `tier` field: `{ "name", "xp", "tier" }`. The ledger only stores the
  achievement's name, so the store looks up the tier from `unlocks` by name and the event's timestamp
  (the engine writes both with `ev.ts`). If no match is found, `tier` is omitted and the row stays gold.
  Covered by a test in `site.rs`.

## Left to the builder

- Exact spacing, sizes and shadows. The mockups set the direction, not pixel values.
- Mobile (under 640px) layout details, within the anti-goals above.
- Whether the tier lookup lives in `Store::list_events` or in `site.rs`.
- `docs/manual-checklist.md`: add a line for checking the sheet's three tabs by eye.
