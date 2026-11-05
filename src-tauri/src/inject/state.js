// R-Browser: tells the browser what it needs to decide whether this tab may be frozen when idle —
// a half-typed form or playing sound keeps it alive — and where it was scrolled, to restore after a freeze.
(() => {
  if (window.top !== window || !/^https?:$/.test(location.protocol)) return;
  const send = (ev) => { try { window.__TAURI_INTERNALS__.invoke("page_event", { ev }); } catch {} };

  let dirty = false;
  const setDirty = (d) => { if (d !== dirty) send({ kind: "dirty-form", dirty: (dirty = d) }); };
  // ponytail: a form emptied by script (e.g. sent draft) stays dirty until the next page load — errs on never freezing.
  document.addEventListener("input", (e) => {
    const t = e.target;
    if (t && (t.matches?.("input:not([type=hidden]),textarea,select") || t.isContentEditable)) setDirty(true);
  }, true);
  document.addEventListener("submit", () => setDirty(false), true);

  let timer = 0;
  addEventListener("scroll", () => {
    if (!timer) timer = setTimeout(() => { timer = 0; send({ kind: "scroll", y: scrollY }); }, 1000);
  }, { passive: true });

  // ponytail: <audio>/<video> only; Web Audio (some games) isn't seen and such tabs can be frozen.
  const media = new Set();
  let audible = false;
  const check = () => {
    const on = [...media, ...document.querySelectorAll("audio,video")].some((m) => !m.paused && !m.muted && m.volume > 0);
    if (on !== audible) send({ kind: "audible", on: (audible = on) });
  };
  for (const ev of ["play", "pause", "ended", "volumechange", "emptied"]) document.addEventListener(ev, check, true);
  const play = HTMLMediaElement.prototype.play;
  HTMLMediaElement.prototype.play = function (...a) {
    if (!this.isConnected && !media.has(this)) { // new Audio(): its events never reach the document
      media.add(this);
      for (const ev of ["play", "pause", "ended", "volumechange"]) this.addEventListener(ev, check);
    }
    return play.apply(this, a);
  };
})();
