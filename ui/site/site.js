// Everything rendered goes through textContent: event data and LLM text are untrusted.
(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  }

  async function api(path) {
    const r = await fetch(path);
    if (!r.ok) throw new Error(path + ": " + r.status);
    return r.json();
  }

  function ago(iso) {
    if (!iso) return "";
    const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return Math.floor(s / 60) + "m ago";
    if (s < 86400) return Math.floor(s / 3600) + "h ago";
    return Math.floor(s / 86400) + "d ago";
  }

  const num = (n) => (typeof n === "number" ? n.toLocaleString() : String(n));

  // Last path segment: "C:/Users/me/rpg" and "octo/rpg" both show as "rpg".
  function repoName(repo) {
    const parts = (repo || "").split(/[\\/]+/).filter(Boolean);
    return parts.length ? parts[parts.length - 1] : (repo || "");
  }

  // Rank order, lowest first. Tier strings come from the server; only these ever reach CSS.
  const TIERS = ["bronze", "silver", "gold", "platinum", "legendary", "celestial"];
  const tierVar = (t) => (TIERS.includes(t) ? "var(--" + t + ")" : "var(--gold)");

  // The highest known tier among an event's achievements; gold when none is known, null for none.
  function rowTier(achs) {
    if (!achs.length) return null;
    const ranks = achs.map((a) => TIERS.indexOf(a.tier)).filter((i) => i >= 0);
    return ranks.length ? TIERS[Math.max(...ranks)] : "gold";
  }

  function eventRow(ev) {
    const li = el("li");
    const achs = ev.achievements || [];
    const top = rowTier(achs);
    if (top) {
      li.classList.add("has-ach");
      li.style.setProperty("--tier", tierVar(top));
    }
    // Base exp here; achievement exp rides on its own badge, so the row adds up to ev.xp.
    const base = ev.base_xp !== undefined ? ev.base_xp : ev.xp;
    const xp = el("span", "xp" + (base ? "" : " zero"), base ? "+" + num(base) + " exp" : achs.length ? "" : "no exp");
    xp.title = "Total: " + num(ev.xp) + " exp";
    li.appendChild(xp);
    const kind = el("span", "kind", (ev.label || ev.kind) + (ev.backfill ? " (history)" : ""));
    if (ev.quest_xp) {
      const q = el("span", "questbadge", "⚔ Quest +" + num(ev.quest_xp));
      q.title = "Quest reward (+" + num(ev.quest_xp) + " exp)";
      kind.appendChild(q);
    }
    achs.forEach((a) => {
      const b = el("span", "achbadge", "★ " + a.name + " +" + num(a.xp));
      b.style.setProperty("--tier", tierVar(a.tier));
      b.title = "Achievement: " + a.name + " (+" + num(a.xp) + " exp)";
      kind.appendChild(b);
    });
    li.appendChild(kind);
    const repo = el("span", "repo", repoName(ev.repo));
    if (ev.repo) repo.title = ev.repo;
    li.appendChild(repo);
    const when = el("span", "when", ago(ev.ts));
    when.title = ev.ts;
    li.appendChild(when);
    return li;
  }

  function fill(list, rows, emptyText) {
    list.replaceChildren();
    if (!rows.length) {
      list.appendChild(el("li", "empty", emptyText));
      return;
    }
    rows.forEach((r) => list.appendChild(eventRow(r)));
  }

  const TOTAL_LABELS = [
    ["commits", "Commits"], ["prs_opened", "PRs opened"], ["prs_merged", "PRs merged"],
    ["reviews", "Reviews"], ["sessions", "Claude sessions"],
  ];

  async function loadCharacter() {
    const c = await api("/api/character");
    $("level").textContent = c.level;
    $("crest").classList.toggle("wide", String(c.level).length >= 3);
    const pct = c.level_span ? Math.min(100, (c.into_level / c.level_span) * 100) : 0;
    $("xpfill").style.width = pct + "%";
    $("xptext").textContent = c.into_level.toLocaleString() + " / " + c.level_span.toLocaleString() +
      " exp to level " + (c.level + 1);
    $("xptotal").textContent = c.total_xp.toLocaleString() + " total";
    const totals = $("totals");
    totals.replaceChildren();
    TOTAL_LABELS.forEach(([k, label]) => {
      const s = el("div", "stat");
      s.appendChild(el("b", null, c.totals[k]));
      s.appendChild(el("span", null, label));
      totals.appendChild(s);
    });
    fill($("recent"), c.recent, "Nothing yet. Go commit something.");
    renderSkills(c.skills || []);
  }

  function renderSkills(skills) {
    const list = $("skills");
    list.replaceChildren();
    if (!skills.length) {
      list.appendChild(el("li", "empty", "No skills yet. They grow from what you commit, review and ship."));
      return;
    }
    skills.forEach((s) => {
      const li = el("li");
      li.appendChild(el("span", "sname", s.name));
      li.appendChild(el("span", "slvl", "Lv " + s.level));
      const bar = el("div", "sbar");
      bar.setAttribute("role", "progressbar");
      bar.setAttribute("aria-label", s.name);
      const fillEl = el("div", "sfill");
      fillEl.style.width = (s.level_span ? Math.min(100, (s.into_level / s.level_span) * 100) : 0) + "%";
      bar.appendChild(fillEl);
      li.appendChild(bar);
      const xp = el("span", "sxp", s.xp.toLocaleString() + " exp");
      xp.title = s.into_level + " / " + s.level_span + " to Lv " + (s.level + 1) + (s.last_at ? " · last " + ago(s.last_at) : "");
      li.appendChild(xp);
      list.appendChild(li);
    });
  }

  const cap = (s) => (s ? s.charAt(0).toUpperCase() + s.slice(1) : "");

  // A ladder's bar runs toward its next locked step: "Silver · 412 / 1,000".
  function ladderProgress(a) {
    const steps = a.steps || [];
    const next = steps.find((s) => !s.unlocked);
    const have = typeof a.progress === "number" ? a.progress : null;
    const wrap = el("div", "aprog");
    const bar = el("div", "abar");
    bar.setAttribute("role", "progressbar");
    bar.setAttribute("aria-label", a.name);
    const fillEl = el("div", "afill");
    let label;
    if (!next) {
      fillEl.style.width = "100%";
      label = cap(a.tier) + " · complete";
    } else {
      const at = typeof next.at === "number" ? next.at : null;
      fillEl.style.width = (at && have !== null ? Math.min(100, (have / at) * 100) : 0) + "%";
      label = cap(next.tier) + " · " + (have !== null ? num(have) : "?") + " / " + num(next.at);
    }
    bar.appendChild(fillEl);
    bar.title = steps.map((s) => cap(s.tier) + " " + num(s.at) + (s.unlocked ? " ✓" : "")).join(" · ");
    wrap.appendChild(bar);
    wrap.appendChild(el("div", "atext", label));
    return wrap;
  }

  async function loadAchievements() {
    const data = await api("/api/achievements");
    const list = data.achievements || [];
    const hidden = data.hidden_remaining || 0;
    const got = list.filter((a) => a.unlocked).length;
    $("achcount").textContent = got + "/" + (list.length + hidden);
    const grid = $("achievements");
    grid.replaceChildren();
    if (!list.length && !hidden) grid.appendChild(el("p", "empty", "No achievement rules loaded."));
    list.forEach((a) => {
      const card = el("div", "ach " + (a.unlocked ? "unlocked t-" + a.tier : "locked") + (a.ladder ? " ladder" : ""));
      card.appendChild(el("div", "tier " + a.tier, a.tier));
      card.appendChild(el("div", "name", a.name));
      if (a.enabled === false) card.appendChild(el("span", "off", "off"));
      const text = a.unlocked ? a.flavor_text_latest : a.hint;
      if (text) card.appendChild(el("div", "desc", text));
      if (a.ladder) card.appendChild(ladderProgress(a));
      if (a.unlocked) {
        card.appendChild(el("div", "meta", "Unlocked " + a.count + "x" + (a.last_at ? " · last " + ago(a.last_at) : "")));
      }
      grid.appendChild(card);
    });
    const left = $("hiddenleft");
    left.hidden = hidden <= 0;
    left.textContent = hidden === 1 ? "1 hidden achievement remains" : hidden + " hidden achievements remain";
  }

  const pct = (x) => Math.round(x * 1000) / 10 + "%";

  // The Quests section is hidden entirely when quests are switched off.
  async function loadQuests() {
    const q = await api("/api/quests");
    const sec = $("quests");
    sec.hidden = !q.enabled;
    if (!q.enabled) return;
    const totals = $("questtotals");
    totals.replaceChildren();
    [[q.chain, "Chain"], ["×" + q.multiplier, "Multiplier"], [q.best_chain, "Best chain"],
     [q.completed, "Completed"], [q.failed, "Failed"]].forEach(([v, label]) => {
      const s = el("div", "stat");
      s.appendChild(el("b", null, v));
      s.appendChild(el("span", null, label));
      totals.appendChild(s);
    });
    const log = $("questlog");
    log.replaceChildren();
    if (!q.log.length) log.appendChild(el("li", "empty", "No quests resolved yet."));
    q.log.forEach((r) => {
      const done = r.status === "completed";
      const li = el("li", "qrow " + (done ? "done" : "lost"));
      li.style.setProperty("--tier", tierVar(r.tier));
      li.appendChild(el("span", "qres", done ? "Completed" : "Failed"));
      li.appendChild(el("span", "qname", r.name));
      li.appendChild(el("span", "qdetail", done ? "+" + num(r.reward) + " exp" : "chain lost: " + num(r.chain_lost)));
      const when = el("span", "when", ago(r.ts));
      when.title = r.ts || "";
      li.appendChild(when);
      log.appendChild(li);
    });
    const board = $("questboard");
    board.replaceChildren();
    q.board.forEach((b) => {
      const card = el("div", "ach quest t-" + (TIERS.includes(b.tier) ? b.tier : "gold") + (b.completions ? " unlocked" : " locked"));
      card.appendChild(el("div", "tier " + b.tier, b.tier));
      card.appendChild(el("div", "name", b.name));
      if (!b.enabled) card.appendChild(el("span", "off", "off"));
      if (b.hint) card.appendChild(el("div", "desc", b.hint));
      card.appendChild(el("div", "meta", pct(b.chance) + " chance · " + b.minutes + " min · done " + b.completions + "x"));
      board.appendChild(card);
    });
  }

  let currentKind = "";

  async function loadEvents() {
    const kind = currentKind;
    const rows = await api("/api/events?limit=200" + (kind ? "&kind=" + encodeURIComponent(kind) : ""));
    // A refresh started before the last chip click can land after it; drop its stale rows.
    if (kind !== currentKind) return;
    fill($("eventlog"), rows, "No events.");
  }

  const loaders = { character: loadCharacter, achievements: () => Promise.all([loadAchievements(), loadQuests()]), events: loadEvents };
  let current = "character";

  function show(tab) {
    current = tab;
    document.querySelectorAll(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
    document.querySelectorAll(".panel").forEach((p) => { p.hidden = p.id !== "tab-" + tab; });
    refresh();
  }

  function refresh() {
    // The header bar always needs the character; other tabs load their own data.
    const jobs = [loadCharacter()];
    if (current !== "character") jobs.push(loaders[current]());
    Promise.all(jobs).catch((e) => console.error(e));
  }

  document.querySelectorAll(".tabs button").forEach((b) => b.addEventListener("click", () => show(b.dataset.tab)));
  document.querySelectorAll("#kinds .chip").forEach((c) => c.addEventListener("click", () => {
    currentKind = c.dataset.kind;
    document.querySelectorAll("#kinds .chip").forEach((o) => {
      o.classList.toggle("on", o === c);
      o.setAttribute("aria-pressed", String(o === c));
    });
    loadEvents().catch((e) => console.error(e));
  }));
  loadAchievements().catch(() => {});
  loadQuests().catch(() => {});
  refresh();
  setInterval(refresh, 5000);
})();
