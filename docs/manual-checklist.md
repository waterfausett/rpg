# Manual checklist (needs a real, unlocked desktop)

The automated and scripted checks cannot see these. Do them once on your machine. Note: `install-hooks` always changes your real global git config and `~/.claude/settings.json`, even with `RPG_HOME` set. If you use a scratch `RPG_HOME`, run `rpg uninstall-hooks` (with the same `RPG_HOME`) before deleting it, or git silently stops running hooks in every repo.

## Overlay
- [ ] Focus stays in the terminal: type in a terminal, commit, and keep typing while the pop-up and banner appear. No keystroke is lost and the terminal title bar stays active.
- [ ] XP pop-up shows bottom-right on the primary display for about 3s and stacks upward on rapid commits.
- [ ] Banner (gold, style B) shows top-center for about 8s, and **clicking it dismisses it** early.
- [ ] While a banner shows, clicking in the glow around it (outside the banner's border) reaches the window underneath; clicking the banner itself still dismisses it.
- [ ] Clicking in the pop-up area passes through to the window underneath.
- [ ] Real transparency: only the pill and banner are visible, with no white or black rectangle around them.
- [ ] No taskbar button and no Alt+Tab entry for the overlay windows. No console window flashes on commit.
- [ ] A commit that takes a skill to a new level shows a teal `▲ <Skill> Lv N` pop-up after the exp pop-up, with no banner or sound.
- [ ] An unlock announced as a toast (a bronze achievement by default) shows a pill `✦ <name>  +N exp` with a tier-colored border and glow, and plays no sound.
- [ ] Each tier's banner (try `[announce] all = "banner"`) has its own border, heading, name glow, reward line and background tint, and still looks like the same gold-banner frame:
  - [ ] Bronze: copper (`#cd7f32`), soft glow, normal pop.
  - [ ] Silver: grey (`#c0c8d0`), slightly stronger glow, normal pop. Easy to tell apart from platinum.
  - [ ] Gold: gold (`#ffc83d`), strong glow, a bigger pop and one border flash as it lands.
  - [ ] Platinum: pale blue (`#cfe8ff`) with a blue-tinted background, the strongest glow and the bigger pop, then the border flash and one light sweep across the banner at the same time.
  - [ ] No banner or glow is cut off at the window edge, even during the bigger pop and the flash. The banner still sits top-center where it used to.
- [ ] The "While you were away" summary banner keeps the plain gold look.
- [ ] Two banners in a row each replay their entrance (and flash or sweep).
- [ ] With Windows **Animation effects** off (macOS: Reduce motion), a banner only fades in and out: no pop, flash or sweep.
- [ ] A ladder step (e.g. `merge_conflict_of_interest` silver) shows the plain name on its banner and toast, with no `(Silver)` and no `· Silver` after the exp: the color tells the tier. History still lists it as `Merge Conflict of Interest (Silver)`.
- [ ] Each tier's toast pill color matches its banner color.
- [ ] A platinum toast pill runs one light sweep after it slides in (not with reduced motion).
- [ ] A banner plays a sound (drop a file in `sounds/` first); **Mute sounds** silences it.
- [ ] With files in `sounds/achievements/` and `sounds/levels/`, an achievement banner plays only from `achievements/` plus the `sounds/` root, and a level-up only from `levels/` plus the root. A file in `achievements/<tier>/` or `achievements/<rule id>/` wins for that tier or rule, unless a `by_rule` pin beats it.
- [ ] With a different file in each `sounds/achievements/<tier>/` folder (or a `[sound] by_tier` pin per tier), each tier's banner plays its own sound.

## Tray
- [ ] Tray icon is visible (check the overflow area) and left click opens the menu.
- [ ] Open character sheet opens `http://127.0.0.1:47474` with your level, XP and achievements, and a Skills list (biggest first) once you have committed something.
- [ ] Mute sounds toggles (check mark) and takes effect on the next banner.
- [ ] Do not disturb: turn on, commit, and see no pop-up and no banner. Turn off and the queued banners appear (at most 3 plus a summary if many).
- [ ] Quests (check item) turns quests off and on, and the choice is saved to `[quests] enabled` (it survives a restart, and the Settings window agrees).
- [ ] Start at login: check it, sign out and back in, and the tray icon is back with no second instance. Task Manager > Startup apps lists rpg as Enabled; disable it there and the menu shows it unchecked after the app restarts. Uncheck it in the menu and the entry is gone.
- [ ] Open rules folder opens `rules/` in Explorer; `quests/` is inside it.
- [ ] Install hooks, Uninstall hooks and Quit all work (see below).

## Settings window
- [ ] Tray **Settings…** (below Open character sheet) opens the window. A second click focuses the same window.
- [ ] Each change applies without a restart: a tier set to Pop-up, the volume, GitHub off then on (a poll appears in `rpg.log`), and Claude flavor off.
- [ ] The tray Mute and Do not disturb check marks and the window agree both ways, and both survive a restart.
- [ ] A hand edit of `config.toml` while the window is open shows up in the window.
- [ ] Breaking `config.toml` shows the error panel. The tray Mute still works and shows the "Couldn't save" pop-up. Fixing the file and pressing **Check again** restores the sections.
- [ ] **Choose…** with a file with spaces in its name: it lands in the right folder and plays on the next matching banner. ▶ previews it even while muted. ✕ unpins it, and the file stays in `sounds/`.
- [ ] Repos: **Add folder…** adds a path. A bad pattern shows an error and isn't saved.
- [ ] Quests section: the Quests switch and the tray Quests check mark agree both ways and survive a restart. Turning it off there withdraws a pending offer and cancels an active quest, exactly like the tray.
- [ ] Quests section: Offer window 9, Passes 0, Chain bonus 0 and Chain cap 0.5 each show an error and revert; valid values save to `[quests]` in `config.toml` (comments kept). A new Offer window applies to the next offer, and Minimum time 0 lets offers come back to back, with no restart.

## Character sheet
- [ ] Achievements tab: a ladder card (e.g. a `lifetime` rule with `tiers`) shows its current tier and a progress bar toward the next step, labelled like `Silver · 412 / 1,000`; hovering the bar lists the steps, with `???` past the next one.
- [ ] Before its first catch-up event, a ladder card may show progress past its next step (e.g. `Bronze · 151 / 100`, for a rule added or turned on with history already behind it); the next matching event unlocks the step and the card moves on.
- [ ] With a `hidden = true` rule not yet earned, the tab shows no card for it and a line `1 hidden achievement remains` (plural for more) below the grid; the line is gone when none remain.
- [ ] An unlocked card's border, glow, name glow, tier label and ladder bar take its tier color (bronze, silver, gold, pale-blue platinum, purple legendary, cyan celestial; platinum and up glow stronger, and legendary and celestial get a ★ and a slow light sweep); a ladder card uses its highest unlocked step. Locked cards are dashed and dimmed, with no tier color.
- [ ] The header shows the level in a gold crest, with `N / M exp to level L+1` and the total beside the XP bar. A three-digit level stays inside the crest.
- [ ] Event rows (Recent and Event log) with an achievement have a left edge and a badge in its tier color. A row with mixed tiers uses the highest; older rows without a tier are gold.
- [ ] The Event log chips filter in one click, and the active chip survives the 5-second refresh.
- [ ] At about 400px wide, all three tabs fit with no sideways scroll, and long skill, achievement and repo names truncate or wrap.

## Quests
Use a quest file with `chance = 1.0` (and `cooldown_hours = 0`, `min_gap_minutes = 0`) to see offers on demand.

### Offer banner
- [ ] One offer per tier (bronze to celestial) has a crimson border and dark crimson radial background, warmed slightly by the tier. The header `◆ GOLD QUEST OFFERED ◆` is in the tier color and the glow grows with the tier.
- [ ] Gold and up: bigger pop and a flash. Platinum and up: light sweep. Legendary and up: pulsing glow behind the banner. Celestial: star field and aurora name.
- [ ] The name, the 2-line text, the goal line (`<steps> · 15 min · 400 exp × 1.5 = 600`), the Accept (filled) and Pass (outline) buttons and the stakes line all fit inside the 540x200 frame with nothing cut off. Try a long LLM text and a 3-step goal.
- [ ] Stakes copy at passes 0, 1 and 2: "3 more passes breaks it", "2 more passes breaks it", "passing breaks it".
- [ ] The fuse along the bottom edge shrinks right to left over `offer_seconds` (set about 10 to check), with a flickering spark at the tip and smoke puffs.
- [ ] Ignore: the fuse running out exits normally and counts as a pass.
- [ ] Pass: normal exit, counts as a pass.
- [ ] Accept: the banner shrinks and fades toward the bottom-right, stays inside the window, and the quest starts.
- [ ] Clicking the banner body does nothing, and hovering doesn't stop the fuse. Clicks on the transparent glow margin reach the window underneath, and the buttons are clickable.
- [ ] An offer raised while an achievement banner shows comes next, ahead of other queued achievements.
- [ ] Achievement, level-up, summary and notice banners look and behave as before (no goal, buttons or fuse; clicking dismisses).
- [ ] With reduced motion, the banner fades in and out with no scale, flash or sweep; the fuse still shrinks, with no spark flicker or smoke.

### Timer pill and card
- [ ] After Accept, the pill slides in bottom-right with the tier gem in the tier color, the name, the `m:ss` countdown and one dot per step.
- [ ] Exp toasts stack above the pill and fade while the pill stays put. Clicks on the exp pills and the empty window area reach the window underneath.
- [ ] Clicking the pill widens it into the card (steps ✓ and ▸, a draining bar, the exp and "chain ×M at stake"). Clicking it again closes it.
- [ ] Clicking anywhere else (the desktop or another app) collapses the card, and the terminal gets keyboard focus back after you click into it.
- [ ] The toast window never appears in the taskbar or Alt-Tab while the card is open.
- [ ] Completing a step fills its dot and moves ▸ to the next step.
- [ ] Under 2 minutes left, the pill pulses red.
- [ ] A GitHub step still open at the deadline shows "checking GitHub…" instead of the time, then the quest resolves.
- [ ] Complete a quest (ideally at gold, platinum and legendary): the pill takes the tier's color, border and glow (a sweep from platinum, a gradient border from legendary) and reads "Quest complete! +N". It fades after about 3.5 s, then clicks pass through everywhere. Any level-up or achievement it caused announces as usual.
- [ ] Let one expire: the pill turns grey with a struck-through `0:00`, shakes, reads "Quest failed · chain lost" and fades.
- [ ] With reduced motion: no pop or shake, the hurry state is a steady red glow, and pills fade out.

### State
- [ ] Do not disturb turned on during an active quest: the pill and timer stay. With it on, no new offer appears, and one that is showing disappears at once without counting as a pass. Achievements queued behind it show after DND is turned off.
- [ ] Quests turned off in the tray while an offer shows: the offer disappears. While a quest is active: the pill fades quietly, no failure is recorded and the chain is kept.
- [ ] Restart the app mid-quest: the pill comes back with the right time left. A commit made while the app was closed still counts if it falls before the deadline.
- [ ] An old commit (over 5 minutes) and a "While you were away" batch raise no offer.

### Character sheet
- [ ] With quests enabled, the Achievements tab shows a Quests section below the grid: the chain and multiplier, the best chain, completions and failures, a log of resolved quests tinted by tier, and a board of every quest (disabled ones with an "off" badge).
- [ ] The quest's exp shows in the exp column of its log row. In the event log, a completed quest's row shows its reward as "+N exp" in the exp column, with no base exp.
- [ ] `[quests] enabled = false`, or Quests turned off in the tray (then reload the sheet), hides the section. It still looks right at about 400px wide and with reduced motion.

## Real install
- [ ] First note `git config --global --get core.hooksPath` (probably unset) and keep a copy of `~/.claude/settings.json`.
- [ ] Tray **Install hooks** (a "Hooks installed" pop-up appears; or `rpg install-hooks` from a terminal). Check that `git config --global core.hooksPath` points at `...\rpg\data\git-hooks` and that `~/.claude/settings.json` has the rpg `SessionStart`/`SessionEnd` entries with your other settings intact.
- [ ] Make a real commit in a real repo: the pop-up appears. A repo with its own pre-commit hook still runs it, and a failing one still blocks the commit.
- [ ] End a real Claude Code session: `+15 exp · Claude session` appears.
- [ ] Open a real PR and wait up to `poll_minutes`: `+50 exp · PR opened`. The backfilled PRs showed 0 XP and you started at level 1.
- [ ] Quit the app, commit a few times in new repos, relaunch: one collapsed toast, at most 3 banners and a "While you were away" summary.
- [ ] **Uninstall hooks**: `core.hooksPath` is back to what it was (or unset) and the rpg entries are gone from `settings.json`, with the rest untouched.
