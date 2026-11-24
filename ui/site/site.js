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

  // Last path segment: "C:/Users/me/rpg" and "octo/rpg" both show as "rpg".
  function repoName(repo) {
    const parts = (repo || "").split(/[\\/]+/).filter(Boolean);
    return parts.length ? parts[parts.length - 1] : (repo || "");
  }

  function eventRow(ev) {
    const li = el("li");
    const achs = ev.achievements || [];
    if (achs.length) li.classList.add("has-ach");
    // Base exp here; achievement exp rides on its own badge, so the row adds up to ev.xp.
    const base = ev.base_xp !== undefined ? ev.base_xp : ev.xp;
    const xp = el("span", "xp" + (base ? "" : " zero"), base ? "+" + base + " exp" : achs.length ? "" : "no exp");
    xp.title = "Total: " + ev.xp + " exp";
    li.appendChild(xp);
    const kind = el("span", "kind", ev.kind + (ev.backfill ? " (history)" : ""));
    achs.forEach((a) => {
      const b = el("span", "achbadge", "★ " + a.name + " +" + a.xp);
      b.title = "Achievement: " + a.name + " (+" + a.xp + " exp)";
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
    const pct = c.level_span ? Math.min(100, (c.into_level / c.level_span) * 100) : 0;
    $("xpfill").style.width = pct + "%";
    $("xptext").textContent = c.into_level.toLocaleString() + " / " + c.level_span.toLocaleString() +
      " exp (" + c.total_xp.toLocaleString() + " total)";
    const totals = $("totals");
    totals.replaceChildren();
    TOTAL_LABELS.forEach(([k, label]) => {
      const s = el("div", "stat");
      s.appendChild(el("b", null, c.totals[k]));
      s.appendChild(el("span", null, label));
      totals.appendChild(s);
    });
    fill($("recent"), c.recent, "Nothing yet. Go commit something.");
  }

  async function loadAchievements() {
    const list = await api("/api/achievements");
    const got = list.filter((a) => a.unlocked).length;
    $("achcount").textContent = "(" + got + "/" + list.length + ")";
    const grid = $("achievements");
    grid.replaceChildren();
    if (!list.length) grid.appendChild(el("p", "empty", "No achievement rules loaded."));
    list.forEach((a) => {
      const card = el("div", "ach " + (a.unlocked ? "unlocked" : "locked"));
      card.appendChild(el("div", "tier " + a.tier, a.tier));
      card.appendChild(el("div", "name", a.name));
      const text = a.unlocked ? a.flavor_text_latest : a.hint;
      if (text) card.appendChild(el("div", "desc", text));
      if (a.unlocked) {
        card.appendChild(el("div", "meta", "Unlocked " + a.count + "x" + (a.last_at ? " · last " + ago(a.last_at) : "")));
      }
      grid.appendChild(card);
    });
  }

  async function loadEvents() {
    const kind = $("kind").value;
    const rows = await api("/api/events?limit=200" + (kind ? "&kind=" + encodeURIComponent(kind) : ""));
    fill($("eventlog"), rows, "No events.");
  }

  const loaders = { character: loadCharacter, achievements: loadAchievements, events: loadEvents };
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
  $("kind").addEventListener("change", loadEvents);
  loadAchievements().catch(() => {});
  refresh();
  setInterval(refresh, 5000);
})();
