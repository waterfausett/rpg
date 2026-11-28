// Overlay pages: render toast / banner events from the app. Text is set with
// textContent only (flavor text may come from an LLM).
(function () {
  "use strict";
  const ev = window.__TAURI__.event;
  const mode = document.body.dataset.overlay;
  const BANNER_MS = 8000;
  const HOVER_GRACE_MS = 2000; // after the pointer leaves, linger this long
  // The two rarest tiers linger a little longer.
  const BANNER_MS_BY_TIER = { Legendary: 11000, Celestial: 11000 };
  const FADE_MS = 400;

  // Shrink a faded pill to nothing so the stack above eases down, then drop it.
  function collapse(pill) {
    pill.style.boxSizing = "border-box";
    pill.style.height = pill.offsetHeight + "px";
    void pill.offsetHeight; // commit the pinned height so the change below animates
    pill.classList.add("gone");
    pill.style.height = "0px";
    let done = false;
    const remove = () => { if (!done) { done = true; pill.remove(); } };
    pill.addEventListener("transitionend", (e) => { if (e.propertyName === "height") remove(); });
    setTimeout(remove, 400); // in case transitionend never fires
  }

  function addToast(t) {
    const pill = document.createElement("div");
    pill.className = t.skill ? "xp skill" : t.tier ? "xp ach" : "xp";
    if (t.tier) {
      pill.dataset.tier = t.tier;
      pill.textContent = "✦ " + (t.label || "") + "  +" + t.amount + " exp";
    } else if (t.notice) {
      pill.textContent = (t.skill ? "▲ " : "") + (t.label || "");
    } else {
      pill.textContent = "+" + t.amount + " exp";
      const label = document.createElement("small");
      label.textContent = t.label || "";
      pill.appendChild(label);
    }
    // Only the pill's own life ends it (a tier sweep on ::after also fires animationend).
    pill.addEventListener("animationend", (e) => { if (e.animationName === "xp-life") collapse(pill); });
    document.getElementById("toasts").appendChild(pill);
  }

  // ---- Quest timer pill (toast window) ----
  const HURRY_S = 120;        // pulse red in the last 2 minutes
  const PILL_END_MS = 3500;   // a completed / failed pill stays this long, then fades
  const PILL_OUT_MS = 500;    // the .bye fade
  const PILL_TICK_MS = 250;

  // The pill on screen: { el, id, status, deadline, total, timer, ro, ending }.
  let pill = null;
  let lastRect;               // last value sent as quest-pill-rect (undefined: none sent yet)

  const fmtLeft = (ms) => {
    const s = Math.max(0, Math.ceil(ms / 1000));
    return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0");
  };
  const isLive = (status) => status === "active" || status === "checking";

  // Tell the app where the pill is (CSS px from the window's top-left, ignoring transforms), or null.
  function reportRect(force) {
    let r = null;
    if (pill && pill.el.isConnected) {
      const c = pill.el.offsetParent ? pill.el.offsetParent.getBoundingClientRect() : { left: 0, top: 0 };
      r = { x: c.left + pill.el.offsetLeft, y: c.top + pill.el.offsetTop, w: pill.el.offsetWidth, h: pill.el.offsetHeight };
    }
    const key = JSON.stringify(r);
    if (key === lastRect && !force) return;
    lastRect = key;
    ev.emit("quest-pill-rect", r);
  }

  function setOpen(open) {
    if (!pill || pill.el.classList.contains("open") === open) return;
    pill.el.classList.toggle("open", open);
    ev.emit("quest-card-open", { open: open });
  }

  function tickPill() {
    if (!pill || pill.ending || pill.status !== "active") return;
    const left = pill.deadline - Date.now();
    pill.el.querySelector(".tm").textContent = fmtLeft(left);
    pill.el.querySelector(".meter i").style.width = Math.max(0, Math.min(100, (100 * left) / pill.total)) + "%";
    pill.el.classList.toggle("hurry", left < HURRY_S * 1000);
  }

  function removePill() {
    if (!pill) return;
    const p = pill;
    pill = null;
    clearInterval(p.timer);
    p.timers.forEach(clearTimeout);
    p.ro.disconnect();
    if (p.el.classList.contains("open")) ev.emit("quest-card-open", { open: false });
    p.el.remove();
    reportRect();
  }

  function makePill(q) {
    const el = document.createElement("div");
    el.className = "qp";
    const span = (cls) => { const s = document.createElement("span"); s.className = cls; return s; };
    const bar = document.createElement("div");
    bar.className = "bar0";
    const gem = document.createElement("i");
    gem.className = "gem";
    bar.append(gem, span("nm"), span("tm"), span("dots"));
    const more = document.createElement("div");
    more.className = "more";
    const inner = document.createElement("div");
    const meter = document.createElement("div");
    meter.className = "meter";
    meter.appendChild(document.createElement("i"));
    const foot = document.createElement("div");
    foot.className = "foot";
    foot.append(span("rw"), span("ch"));
    inner.append(document.createElement("ul"), meter, foot);
    more.appendChild(inner);
    el.append(bar, more);
    el.addEventListener("click", () => { if (pill && pill.el === el && !pill.ending) setOpen(!el.classList.contains("open")); });
    const toasts = $("toasts");
    toasts.insertBefore(el, toasts.firstChild); // column-reverse: first is the bottom
    const left = q.deadline_ms - Date.now();
    pill = { el: el, id: q.row_id, status: q.status, deadline: q.deadline_ms, total: Math.max(1, left), timer: 0, timers: [], ending: false };
    pill.ro = new ResizeObserver(() => reportRect());
    pill.ro.observe(el);
    el.addEventListener("transitionend", () => reportRect());
    pill.timer = setInterval(tickPill, PILL_TICK_MS);
  }

  // Name, tier, steps (✓ done, ▸ next) and the stakes, from an update.
  function fillPill(q) {
    const el = pill.el;
    el.dataset.tier = q.tier_label || "";
    el.querySelector(".nm").textContent = q.name || "";
    const dots = el.querySelector(".dots");
    const ul = el.querySelector("ul");
    dots.textContent = "";
    ul.textContent = "";
    let next = true;
    (q.steps || []).forEach((s) => {
      const done = s.have >= s.need;
      const dot = document.createElement("i");
      const li = document.createElement("li");
      li.textContent = s.need > 1 ? s.label + " " + Math.min(s.have, s.need) + "/" + s.need : s.label;
      if (done) { dot.className = "done"; li.className = "done"; }
      else if (next) { li.className = "next"; next = false; }
      dots.appendChild(dot);
      ul.appendChild(li);
    });
    el.querySelector(".rw").textContent = num(q.reward) + " exp";
    el.querySelector(".ch").textContent = "chain " + mult(q.multiplier) + " at stake";
  }

  // Completed / failed: hold the final look, then fade and go. Cancelled: just fade.
  function endPill(q) {
    const p = pill;
    p.ending = true;
    p.status = q.status;
    setOpen(false);
    p.el.classList.remove("hurry");
    const bye = () => {
      p.el.classList.add("bye");
      p.timers.push(setTimeout(() => { if (pill === p) removePill(); }, PILL_OUT_MS));
    };
    if (q.status === "completed") {
      p.el.classList.add("won");
      p.el.querySelector(".nm").textContent = "Quest complete!";
      p.el.querySelector(".tm").textContent = "+" + num(q.reward);
    } else if (q.status === "failed") {
      p.el.classList.add("lost");
      p.el.querySelector(".nm").textContent = "Quest failed · chain lost";
      p.el.querySelector(".tm").textContent = "0:00";
    } else {
      bye();
      return;
    }
    p.timers.push(setTimeout(bye, PILL_END_MS));
  }

  function onQuest(q) {
    if (pill && pill.id !== q.row_id) {
      if (!isLive(q.status)) return; // a late ending for some other quest
      removePill();                  // a new quest replaces the old pill
    }
    if (pill && pill.ending) return;
    if (!pill) {
      if (q.status === "cancelled") { reportRect(true); return; } // the app waits for this null
      makePill(q);
    }
    fillPill(q);
    pill.deadline = q.deadline_ms;
    if (isLive(q.status)) {
      pill.status = q.status;
      pill.el.classList.toggle("checking", q.status === "checking");
      if (q.status === "checking") {
        pill.el.classList.remove("hurry");
        pill.el.querySelector(".tm").textContent = "checking GitHub…";
      } else {
        tickPill();
      }
    } else {
      pill.el.classList.remove("checking");
      endPill(q);
    }
    reportRect();
  }

  // Accept: the offer shrinks into the bottom-right corner (where the quest pill appears).
  const TAKE_MS = 550;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  let current = null;
  let timers = [];
  // The quest offer on screen: { id, offerId, secs, start, last, raf, answered }.
  let offer = null;

  function clearTimers() {
    timers.forEach(clearTimeout);
    timers = [];
  }

  const $ = (id) => document.getElementById(id);
  const bannerClass = (state) => "ach-b" + (offer ? " quest-b" : "") + (state ? " " + state : "");

  function stopFuse() {
    if (offer && offer.raf) cancelAnimationFrame(offer.raf);
    $("banner").querySelectorAll(".smoke").forEach((s) => s.remove());
  }

  // The frame grows with its text: tell the app where it is (for click-through) and size the
  // glow layer to match. Offsets ignore the entry/exit transforms.
  function reportBanner() {
    if (current === null) return;
    const el = $("banner");
    $("pulse").style.setProperty("--bh", el.offsetHeight + "px");
    ev.emit("banner-rect", { id: current, rect: { x: el.offsetLeft, y: el.offsetTop, w: el.offsetWidth, h: el.offsetHeight } });
  }

  // Back to the empty, hidden banner.
  function reset() {
    stopFuse();
    offer = null;
    $("banner").className = "ach-b";
    $("pulse").classList.remove("on");
    $("pulse").dataset.tier = "";
  }

  // `exit` is "out" (the normal fade) or "take" (an accepted offer).
  function dismiss(id, exit) {
    if (current !== id) return;
    current = null;
    clearTimers();
    stopFuse();
    const el = $("banner");
    el.className = bannerClass(exit || "out");
    $("pulse").classList.remove("on");
    timers.push(setTimeout(() => {
      reset();
      ev.emit("banner-done", { id: id });
    }, exit === "take" && !reducedMotion.matches ? TAKE_MS : FADE_MS));
  }

  // Accept / Pass / the fuse burned out (accept = false).
  function answer(accept) {
    if (!offer || offer.answered || current !== offer.id) return;
    offer.answered = true;
    ev.emit("quest-answer", { offer_id: offer.offerId, accept: accept });
    dismiss(offer.id, accept ? "take" : "out");
  }

  const round2 = (m) => Math.round(m * 100) / 100;
  const mult = (m) => "×" + round2(m);
  const num = (n) => Number(n).toLocaleString("en-US");

  function stakesText(q) {
    const n = q.passes_left;
    const breaks = n > 1 ? n + " more passes breaks it" : "passing breaks it";
    return "Chain " + mult(q.multiplier) + " → " + mult(q.next_multiplier) + " on success · " + breaks;
  }

  // "<steps> · <b>15 min</b> · 400 exp × 1.5 = <b>600</b>", built without innerHTML.
  function setGoal(q) {
    const goal = $("goal");
    goal.textContent = "";
    const bold = (t) => { const b = document.createElement("b"); b.textContent = t; return b; };
    goal.append(
      (q.goal ? q.goal + " · " : ""), bold(q.minutes + " min"),
      " · " + num(q.xp) + " exp × " + round2(q.multiplier) + " = ", bold(num(q.reward)),
    );
  }

  function puff(at) {
    const s = document.createElement("i");
    s.className = "smoke";
    s.style.left = at + "%";
    $("banner").appendChild(s);
    setTimeout(() => s.remove(), 900);
  }

  // The fuse burns from full to nothing over the offer window, from when it is shown.
  function tick(now) {
    if (!offer || offer.answered) return;
    const left = offer.secs - (now - offer.start) / 1000;
    const f = Math.max(0, left / offer.secs);
    const dt = (now - offer.last) / 1000;
    offer.last = now;
    $("fuse").style.transform = "scaleX(" + f + ")";
    const spark = $("spark");
    spark.style.left = f * 100 + "%";
    spark.style.display = f > 0 ? "" : "none";
    if (!reducedMotion.matches && f > 0 && Math.random() < dt * 8) puff(f * 100);
    if (left <= 0) {
      answer(false);
      return;
    }
    offer.raf = requestAnimationFrame(tick);
  }

  function showBanner(b) {
    clearTimers();
    stopFuse();
    current = b.id;
    const q = b.quest;
    offer = q ? { id: b.id, offerId: q.offer_id, secs: Math.max(1, q.offer_seconds), answered: false } : null;
    const set = (id, v) => { $(id).textContent = v || ""; };
    set("title", b.title);
    set("name", b.name);
    set("text", b.text);
    set("reward", b.reward); // the tier shows as the banner's color, not as text
    if (q) {
      setGoal(q);
      set("stakes", stakesText(q));
      $("fuse").style.transform = "scaleX(1)";
      $("spark").style.left = "100%";
      $("spark").style.display = "";
    }
    const el = $("banner");
    el.dataset.tier = b.tier || "";
    el.dataset.kind = b.kind;
    el.className = bannerClass();
    void el.offsetWidth; // restart the animation
    el.className = bannerClass("in");
    reportBanner();
    const pulse = $("pulse"); // glow layer for the rarest tiers
    pulse.dataset.tier = b.tier || "";
    pulse.classList.add("on");
    if (offer) {
      offer.start = offer.last = performance.now();
      offer.raf = requestAnimationFrame(tick);
      // In case animation frames stop (e.g. the page is throttled), the clock still runs out.
      timers.push(setTimeout(() => answer(false), offer.secs * 1000 + 250));
    } else {
      timers.push(setTimeout(() => dismiss(b.id), BANNER_MS_BY_TIER[b.tier] || BANNER_MS));
    }
  }

  // The app took the banner away (Do Not Disturb, offer withdrawn): no exit, no banner-done.
  function cancelBanner(id) {
    if (current !== id) return;
    current = null;
    clearTimers();
    reset();
  }

  async function init() {
    if (mode === "toast") {
      await ev.listen("toast", (e) => e.payload.forEach(addToast));
      await ev.listen("quest", (e) => onQuest(e.payload));
      // The card took focus when it opened; a click anywhere else takes it away.
      window.addEventListener("blur", () => setOpen(false));
    } else {
      const banner = $("banner");
      // An offer's body does nothing: only its buttons (and its fuse) end it.
      banner.addEventListener("click", () => { if (!offer) dismiss(current); });
      $("accept").addEventListener("click", (e) => { e.stopPropagation(); answer(true); });
      $("pass").addEventListener("click", (e) => { e.stopPropagation(); answer(false); });
      // Hovering holds the banner so it can be read; leaving starts a short grace.
      // Not for offers: the fuse decides.
      const hover = (hovering) => {
        if (current === null || offer) return;
        const id = current;
        clearTimers();
        if (!hovering) timers.push(setTimeout(() => dismiss(id), HOVER_GRACE_MS));
        ev.emit("banner-hover", { id: id, hovering: hovering });
      };
      new ResizeObserver(() => reportBanner()).observe(banner); // e.g. a late web font
      banner.addEventListener("mouseenter", () => hover(true));
      banner.addEventListener("mouseleave", () => hover(false));
      await ev.listen("banner", (e) => showBanner(e.payload));
      await ev.listen("banner-cancel", (e) => cancelBanner(e.payload));
    }
    await ev.emit("overlay-ready", { window: mode });
  }

  init();
})();
