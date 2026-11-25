# Manual checklist (needs a real, unlocked desktop)

The automated and scripted checks cannot see these. Do them once on your machine. Note: `install-hooks` always changes your real global git config and `~/.claude/settings.json`, even with `RPG_HOME` set. If you use a scratch `RPG_HOME`, run `rpg uninstall-hooks` (with the same `RPG_HOME`) before deleting it, or git silently stops running hooks in every repo.

## Overlay
- [ ] Focus stays in the terminal: type in a terminal, commit, and keep typing while the pop-up and banner appear. No keystroke is lost and the terminal title bar stays active.
- [ ] XP pop-up shows bottom-right on the primary display for about 3s and stacks upward on rapid commits.
- [ ] Banner (gold, style B) shows top-center for about 8s, and **clicking it dismisses it** early.
- [ ] Clicking in the pop-up area passes through to the window underneath.
- [ ] Real transparency: only the pill and banner are visible, with no white or black rectangle around them.
- [ ] No taskbar button and no Alt+Tab entry for the overlay windows. No console window flashes on commit.
- [ ] A commit that takes a skill to a new level shows a teal `▲ <Skill> Lv N` pop-up after the exp pop-up, with no banner or sound.
- [ ] An unlock announced as a toast (a bronze achievement by default) shows a pill `✦ <name>  +N exp` with a tier-colored border and glow, and plays no sound.
- [ ] Each tier's banner (try `[announce] all = "banner"`) has its own border, heading, name glow, reward line and background tint, and still looks like the same gold-banner frame:
  - [ ] Bronze: copper (`#cd7f32`), soft glow, normal pop.
  - [ ] Silver: grey (`#c0c8d0`), slightly stronger glow, normal pop. Easy to tell apart from platinum.
  - [ ] Gold: gold (`#ffc83d`), strong glow, a bigger pop and one border flash as it lands.
  - [ ] Platinum: pale blue (`#cfe8ff`) with a blue-tinted background, the strongest glow, the bigger pop and flash, then one light sweep across the banner.
  - [ ] No banner is clipped at the window edge during the bigger pop.
- [ ] The "While you were away" summary banner keeps the plain gold look.
- [ ] Two banners in a row each replay their entrance (and flash or sweep).
- [ ] With Windows **Animation effects** off (macOS: Reduce motion), a banner only fades in and out: no pop, flash or sweep.
- [ ] Each tier's toast pill color matches its banner color.
- [ ] A banner plays a sound (drop a file in `sounds/` first); **Mute sounds** silences it.
- [ ] With `[sound] by_tier` mapping each tier to a different file, each tier's banner plays its own sound.

## Tray
- [ ] Tray icon is visible (check the overflow area) and left click opens the menu.
- [ ] Open character sheet opens `http://127.0.0.1:47474` with your level, XP and achievements, and a Skills list (biggest first) once you have committed something.
- [ ] Mute sounds toggles (check mark) and takes effect on the next banner.
- [ ] Do not disturb: turn on, commit, and see no pop-up and no banner. Turn off and the queued banners appear (at most 3 plus a summary if many).
- [ ] Open rules folder opens `rules/` in Explorer.
- [ ] Install hooks, Uninstall hooks and Quit all work (see below).

## Character sheet
- [ ] Achievements tab: a ladder card (e.g. a `lifetime` rule with `tiers`) shows its current tier and a progress bar toward the next step, labelled like `Silver · 412 / 1,000`; hovering the bar lists the steps, with `???` past the next one.
- [ ] Before its first catch-up event, a ladder card may show progress past its next step (e.g. `Bronze · 151 / 100`, for a rule added or turned on with history already behind it); the next matching event unlocks the step and the card moves on.
- [ ] With a `hidden = true` rule not yet earned, the tab shows no card for it and a line `1 hidden achievement remains` (plural for more) below the grid; the line is gone when none remain.
- [ ] An unlocked card's border, glow, name glow and tier label take its tier color (bronze, silver, gold, pale-blue platinum with a slightly stronger glow); a ladder card uses its highest unlocked step. Locked cards have no tier color.

## Real install
- [ ] First note `git config --global --get core.hooksPath` (probably unset) and keep a copy of `~/.claude/settings.json`.
- [ ] Tray **Install hooks** (a "Hooks installed" pop-up appears; or `rpg install-hooks` from a terminal). Check that `git config --global core.hooksPath` points at `...\rpg\data\git-hooks` and that `~/.claude/settings.json` has the rpg `SessionStart`/`SessionEnd` entries with your other settings intact.
- [ ] Make a real commit in a real repo: the pop-up appears. A repo with its own pre-commit hook still runs it, and a failing one still blocks the commit.
- [ ] End a real Claude Code session: `+15 exp · Claude session` appears.
- [ ] Open a real PR and wait up to `poll_minutes`: `+50 exp · PR opened`. The backfilled PRs showed 0 XP and you started at level 1.
- [ ] Quit the app, commit a few times in new repos, relaunch: one collapsed toast, at most 3 banners and a "While you were away" summary.
- [ ] **Uninstall hooks**: `core.hooksPath` is back to what it was (or unset) and the rpg entries are gone from `settings.json`, with the rest untouched.
