// Settings window. Every change is one IPC command; the page re-renders from the
// snapshot the command returns. On error the status line shows it and the page
// re-renders from settings_get, so the field reverts to what the file says.
(function () {
  "use strict";

  const TIERS = ["bronze", "silver", "gold", "platinum", "legendary", "celestial"];
  const LISTS = {
    include: { title: "Only count these", note: "empty = everywhere" },
    exclude: { title: "Never count these", note: "" },
  };
  const ANNOUNCE = { banner: "a banner", toast: "a pop-up" };

  const $ = (id) => document.getElementById(id);
  const tauri = window.__TAURI__;
  let snap = null;
  let active = "notifications";
  // The inline "Add pattern" row: { list, text, error } while open.
  let adding = null;
  let chain = Promise.resolve();

  // ---- DOM helper: h("div", {class: "f"}, child, "text", ...) ----
  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
      else if (k === "value") el.value = v;
      else el.setAttribute(k, v === true ? "" : v);
    }
    for (const kid of kids.flat(Infinity)) {
      if (kid === null || kid === undefined || kid === false) continue;
      el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
    }
    return el;
  }
  const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);

  // ---- Status line ----
  function saved(what) {
    const s = $("status");
    s.className = "status";
    s.replaceChildren(h("b", null, "✓ Saved"), what ? " · " + what : "");
  }
  function failed(msg) {
    const s = $("status");
    s.className = "status bad";
    s.textContent = String(msg);
  }
  function hint(msg) {
    const s = $("status");
    s.className = "status";
    s.textContent = msg;
  }

  // ---- Commands, one at a time so snapshots land in order ----
  function queue(fn) {
    const next = chain.then(fn, fn);
    chain = next.catch(() => {});
    return next;
  }
  function invoke(cmd, args) {
    if (!tauri || !tauri.core) return Promise.reject("Can't reach the app. Open this window from the tray's Settings… item.");
    return tauri.core.invoke(cmd, args || {});
  }
  function refresh() {
    return queue(() => invoke("settings_get").then(render, failed));
  }
  // Run a command that returns a snapshot; `what(old, new)` names the change, or null for no change.
  function change(cmd, args, what) {
    return queue(() => {
      const before = snap;
      return invoke(cmd, args).then(
        (s) => {
          render(s);
          if (s && s.invalid) return;
          const w = typeof what === "function" ? what(before, s) : what;
          if (w !== null) saved(w);
        },
        (e) => {
          failed(e);
          return invoke("settings_get").then(render, () => {});
        },
      );
    });
  }
  const set = (key, value, what) => change("settings_set", { key, value }, what);
  // A command that returns null (preview, open folder/file): only errors are shown.
  function act(cmd, args) {
    return queue(() => invoke(cmd, args).catch(failed));
  }

  // ---- Controls ----
  function sw(k, on, label, onToggle) {
    return h("button", {
      type: "button", class: "sw", role: "switch", "aria-checked": String(!!on), "aria-label": label, "data-k": k,
      onclick: () => onToggle(!on),
    });
  }
  function seg(k, options, current, disabled, label, onPick) {
    return h("div", { class: "seg", role: "group", "aria-label": label },
      options.map(([value, text]) => h("button", {
        type: "button", "aria-pressed": String(current === value), disabled, "data-k": k + "=" + value,
        onclick: () => { if (current !== value) onPick(value); },
      }, text)));
  }
  function field(label, small, ...controls) {
    return h("div", { class: "f" }, h("div", { class: "l" }, label, small ? h("small", null, small) : null), controls);
  }
  // A whole-number input that saves on change.
  function num(k, key, value, min, label, what) {
    return h("input", {
      type: "number", class: "num", min, step: 1, value: String(value), "aria-label": label, "data-k": k,
      onchange: (e) => {
        const raw = e.target.value.trim();
        const n = Number(raw);
        if (raw === "" || !Number.isInteger(n)) {
          failed("Enter a whole number");
          render(snap);
          return;
        }
        if (n === value) return;
        set(key, n, what(n));
      },
    });
  }

  // ---- Sections ----
  function notifications(n) {
    const all = n.all;
    const tierRows = TIERS.map((t) => {
      const row = field(cap(t), null,
        seg("tier." + t, [["banner", "Banner"], ["toast", "Pop-up"]], n.tiers[t], all !== null, cap(t) + " unlocks",
          (v) => set("announce." + t, v, cap(t) + " unlocks now show " + ANNOUNCE[v])));
      row.prepend(h("span", { class: "tier", style: "background:var(--" + t + ")" }));
      if (all !== null) row.classList.add("disabled");
      return row;
    });
    return [
      h("h2", null, "Notifications"),
      field("Do not disturb", "hold banners and drop pop-ups until you turn it off",
        sw("dnd", n.dnd, "Do not disturb", (on) => set("announce.dnd", on, "Do not disturb " + (on ? "on" : "off")))),
      field("First unlock is always a banner", null,
        sw("first_banner", n.first_banner, "First unlock is always a banner",
          (on) => set("announce.first_banner", on, on ? "First unlocks always show a banner" : "First unlocks follow their tier"))),
      h("h3", null, "By tier"),
      field("Same for every tier", all !== null ? "overrides the tier rows below" : null,
        seg("all", [["off", "Off"], ["banner", "Banner"], ["toast", "Pop-up"]], all === null ? "off" : all, false, "Same for every tier",
          (v) => v === "off"
            ? set("announce.all", null, "Each tier uses its own setting")
            : set("announce.all", v, "Every unlock now shows " + ANNOUNCE[v]))),
      tierRows,
    ];
  }

  function slotKey(id) {
    return id.startsWith("tier:") ? "sound.by_tier." + id.slice(5) : "sound." + id;
  }
  function soundRow(slot, small) {
    const pinned = slot.pinned;
    let chip;
    if (pinned && slot.exists) chip = h("span", { class: "file", title: pinned }, pinned);
    else if (pinned) chip = h("span", { class: "file missing", title: "sounds/ has no " + pinned }, "missing: " + pinned);
    else chip = h("span", { class: "file none" },
      { pool: "random from folder", default: "use default", none: "none" }[slot.fallback] || "none");
    const playable = pinned ? slot.exists : slot.fallback !== "none";
    const row = field(slot.label, small, chip,
      h("button", { type: "button", class: "btn", disabled: !playable, title: "Play", "aria-label": "Play " + slot.label,
        "data-k": "play." + slot.id, onclick: () => act("settings_preview_sound", { slot: slot.id }) }, "▶"),
      h("button", { type: "button", class: "btn", "aria-label": "Choose " + slot.label + " sound", "data-k": "choose." + slot.id,
        onclick: () => change("settings_choose_sound", { slot: slot.id }, (_, s) => {
          const now = s.sounds.slots.find((x) => x.id === slot.id);
          return now && now.pinned && now.pinned !== pinned ? slot.label + " sound set to " + now.pinned : null;
        }) }, "Choose…"),
      pinned ? h("button", { type: "button", class: "btn", title: "Stop using this sound (the file stays in sounds/)",
        "aria-label": "Stop using " + pinned + " for " + slot.label, "data-k": "clear." + slot.id,
        onclick: () => set(slotKey(slot.id), null, slot.label + " sound cleared") }, "✕") : null);
    if (slot.id.startsWith("tier:")) {
      row.prepend(h("span", { class: "tier", style: "background:var(--" + slot.id.slice(5) + ")" }));
    }
    return row;
  }

  function sounds(s) {
    const pct = Math.round(s.volume * 100);
    const label = h("span", { class: "pct" }, pct + "%");
    const banner = s.slots.filter((x) => !x.id.startsWith("tier:"));
    const tiers = s.slots.filter((x) => x.id.startsWith("tier:"));
    return [
      h("h2", null, "Sounds"),
      field("Mute all sounds", null,
        sw("muted", s.muted, "Mute all sounds", (on) => set("sound.muted", on, on ? "Sounds muted" : "Sounds on"))),
      field("Volume", null,
        h("input", { type: "range", min: 0, max: 100, step: 1, value: String(pct), "aria-label": "Volume", "data-k": "volume",
          oninput: (e) => { label.textContent = e.target.value + "%"; },
          onchange: (e) => {
            const v = Number(e.target.value);
            if (v !== pct) set("sound.volume", v / 100, "Volume " + v + "%");
          } }),
        label,
        h("button", { type: "button", class: "btn", "data-k": "test", title: "Play the achievement sound at this volume",
          onclick: () => act("settings_preview_sound", { slot: "achievement" }) }, "▶ Test")),
      h("h3", null, "Banner sounds"),
      banner.map((x) => soundRow(x, x.id === "achievement" ? "default for every achievement" : null)),
      h("h3", null, "By tier ", h("span", { class: "plain" }, "(overrides Achievements)")),
      tiers.map((x) => soundRow(x, null)),
      h("p", { class: "note row-note" },
        h("span", null, "Per-achievement sounds and folder pools are set up in the sounds/ folder."),
        h("button", { type: "button", class: "btn", "data-k": "open-sounds", onclick: () => act("settings_open_sounds_folder") },
          "Open sounds folder")),
    ];
  }

  function integrations(i) {
    const gh = i.github, llm = i.llm, up = i.updates;
    return [
      h("h2", null, "Integrations"),
      h("h3", null, "GitHub"),
      field("GitHub", "needs the gh CLI, logged in",
        sw("gh", gh.enabled, "GitHub", (on) => set("github.enabled", on, "GitHub " + (on ? "on" : "off")))),
      field("Check every", null,
        num("gh.poll", "github.poll_minutes", gh.poll_minutes, 1, "GitHub check interval in minutes",
          (n) => "GitHub checks every " + n + " min"),
        h("span", { class: "unit" }, "min")),
      h("h3", null, "Claude flavor text"),
      field("Claude flavor text", "needs the claude CLI",
        sw("llm", llm.enabled, "Claude flavor text", (on) => set("llm.enabled", on, "Claude flavor text " + (on ? "on" : "off")))),
      field("Model", null,
        h("input", { type: "text", class: "txt", list: "models", value: llm.model, "aria-label": "Model", "data-k": "llm.model",
          spellcheck: "false", onchange: (e) => {
            const v = e.target.value.trim();
            if (v !== llm.model) set("llm.model", v, "Model set to " + v);
          } }),
        h("datalist", { id: "models" }, ["haiku", "sonnet", "opus"].map((m) => h("option", { value: m })))),
      field("Timeout", null,
        num("llm.timeout", "llm.timeout_secs", llm.timeout_secs, 1, "Claude timeout in seconds",
          (n) => "Claude timeout " + n + " s"),
        h("span", { class: "unit" }, "s")),
      h("h3", null, "Updates"),
      field("Check every", "0 = only from the tray",
        num("updates", "updates.check_hours", up.check_hours, 0, "Update check interval in hours",
          (n) => n === 0 ? "Updates checked only from the tray" : "Updates checked every " + n + " h"),
        h("span", { class: "unit" }, "h")),
    ];
  }

  function repoList(list, entries) {
    const meta = LISTS[list];
    const save = (arr, what) => set("paths." + list, arr, what);
    const rows = entries.map((e, idx) => h("div", { class: "f" },
      h("span", { class: "file entry", title: e }, e),
      h("button", { type: "button", class: "btn", title: "Remove", "aria-label": "Remove " + e, "data-k": list + ".rm." + idx,
        onclick: () => save(entries.filter((_, j) => j !== idx), "Removed " + e + " from " + meta.title) }, "✕")));
    const out = [
      h("h3", null, meta.title, meta.note ? h("span", { class: "plain" }, " (" + meta.note + ")") : null),
      rows.length ? rows : h("div", { class: "empty" }, "nothing yet"),
    ];
    if (adding && adding.list === list) {
      const submit = () => {
        const v = input.value.trim();
        adding.text = input.value;
        if (!v) { adding = null; render(snap); return; }
        if (entries.includes(v)) { adding.error = "Already listed"; render(snap); return; }
        const pending = adding;
        queue(() => invoke("settings_set", { key: "paths." + list, value: entries.concat([v]) }).then(
          (s) => { if (adding === pending) adding = null; render(s); if (!s.invalid) saved("Added " + v + " to " + meta.title); },
          (err) => {
            if (adding === pending) pending.error = String(err);
            return invoke("settings_get").then(render, () => render(snap));
          },
        ));
      };
      const input = h("input", { type: "text", class: "txt wide", value: adding.text, placeholder: "glob or owner/name",
        "aria-label": "New pattern for " + meta.title, spellcheck: "false", "data-k": list + ".new",
        oninput: (e) => { adding.text = e.target.value; adding.error = null; },
        onkeydown: (e) => {
          if (e.key === "Enter") { e.preventDefault(); submit(); }
          else if (e.key === "Escape") { e.preventDefault(); adding = null; render(snap); }
        } });
      out.push(h("div", { class: "f" }, input,
        h("button", { type: "button", class: "btn", onclick: submit }, "Add"),
        h("button", { type: "button", class: "btn", onclick: () => { adding = null; render(snap); } }, "Cancel")));
      if (adding.error) out.push(h("div", { class: "err", role: "alert" }, adding.error));
    }
    out.push(h("div", { class: "adds" },
      h("button", { type: "button", class: "btn", "data-k": list + ".folder",
        onclick: () => change("settings_add_folder", { list }, (_, s) => {
          const added = s.repos[list].find((x) => !entries.includes(x));
          return added ? "Added " + added + " to " + meta.title : null;
        }) }, "Add folder…"),
      h("button", { type: "button", class: "btn", "data-k": list + ".pattern",
        onclick: () => {
          adding = { list, text: "", error: null };
          render(snap);
          const el = document.querySelector('[data-k="' + list + '.new"]');
          if (el) el.focus();
        } }, "Add pattern")));
    return out;
  }

  function repos(r) {
    return [h("h2", null, "Repos"), repoList("include", r.include), repoList("exclude", r.exclude)];
  }

  const BUILD = { notifications, sounds, integrations, repos };

  // ---- Render ----
  function render(s) {
    if (!s) return;
    snap = s;
    const focused = document.activeElement && document.activeElement.getAttribute("data-k");
    const broken = !!s.invalid;
    $("broken").hidden = !broken;
    if (broken) $("broken-err").textContent = s.invalid;
    for (const b of document.querySelectorAll("nav button")) {
      b.classList.toggle("on", !broken && b.dataset.section === active);
      b.disabled = broken;
      if (b.dataset.section === active) b.setAttribute("aria-current", "page");
      else b.removeAttribute("aria-current");
    }
    for (const sec of document.querySelectorAll("main section[data-section]")) {
      const name = sec.dataset.section;
      sec.hidden = broken || name !== active;
      if (broken) { sec.replaceChildren(); continue; }
      sec.replaceChildren(...BUILD[name](s[name]).flat(Infinity));
    }
    if (broken) hint("Fix config.toml, then choose Check again");
    if (focused) {
      const el = document.querySelector('[data-k="' + CSS.escape(focused) + '"]');
      if (el && !el.disabled) el.focus();
    }
  }

  function show(section) {
    active = section;
    render(snap);
  }

  // ---- Wire up ----
  for (const b of document.querySelectorAll("nav button")) b.addEventListener("click", () => show(b.dataset.section));
  $("open-config").addEventListener("click", () => act("settings_open_config_file"));
  $("check-again").addEventListener("click", () => queue(() => invoke("settings_get").then((s) => {
    render(s);
    if (!s.invalid) hint("config.toml is fine again");
  }, failed)));

  if (!tauri || !tauri.core || !tauri.event) {
    failed("Can't reach the app. Open this window from the tray's Settings… item.");
    return;
  }
  tauri.event.listen("settings-changed", () => refresh());
  refresh().then(() => { if (snap && !snap.invalid) hint("Changes save as you make them"); });
})();
