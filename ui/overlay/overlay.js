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
      banner.addEventListener("mouseenter", () => hover(true));
      banner.addEventListener("mouseleave", () => hover(false));
      await ev.listen("banner", (e) => showBanner(e.payload));
      await ev.listen("banner-cancel", (e) => cancelBanner(e.payload));
    }
    await ev.emit("overlay-ready", { window: mode });
  }

  init();
})();
