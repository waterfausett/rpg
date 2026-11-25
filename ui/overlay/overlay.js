// Overlay pages: render toast / banner events from the app. Text is set with
// textContent only (flavor text may come from an LLM).
(function () {
  "use strict";
  const ev = window.__TAURI__.event;
  const mode = document.body.dataset.overlay;
  const BANNER_MS = 8000;
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
    pill.addEventListener("animationend", () => collapse(pill));
    document.getElementById("toasts").appendChild(pill);
  }

  let current = null;
  let timers = [];

  function clearTimers() {
    timers.forEach(clearTimeout);
    timers = [];
  }

  function dismiss(id) {
    if (current !== id) return;
    current = null;
    clearTimers();
    const el = document.getElementById("banner");
    el.className = "ach-b out";
    timers.push(setTimeout(() => {
      el.className = "ach-b";
      ev.emit("banner-done", { id: id });
    }, FADE_MS));
  }

  function showBanner(b) {
    clearTimers();
    current = b.id;
    const set = (id, v) => { document.getElementById(id).textContent = v || ""; };
    set("title", b.title);
    set("name", b.name);
    set("text", b.text);
    set("reward", b.tier && b.reward ? b.reward + " · " + b.tier : b.reward);
    const el = document.getElementById("banner");
    el.dataset.tier = b.tier || "";
    el.dataset.kind = b.kind;
    el.className = "ach-b";
    void el.offsetWidth; // restart the animation
    el.className = "ach-b in";
    timers.push(setTimeout(() => dismiss(b.id), BANNER_MS));
  }

  async function init() {
    if (mode === "toast") {
      await ev.listen("toast", (e) => e.payload.forEach(addToast));
    } else {
      document.getElementById("banner").addEventListener("click", () => dismiss(current));
      await ev.listen("banner", (e) => showBanner(e.payload));
    }
    await ev.emit("overlay-ready", { window: mode });
  }

  init();
})();
