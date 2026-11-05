// R-Browser: tell the browser which icon this page declares (the sharpest one), for its tab.
// Only the address is sent; the browser checks it and the toolbar loads it like any image.
(() => {
  if (window.top !== window || !/^https?:$/.test(location.protocol)) return;
  let sent = "";
  const score = (l) => {
    const rel = (l.getAttribute("rel") || "").toLowerCase();
    if (/svg/.test(l.type || "") || /\.svg(\?|$)/i.test(l.href)) return 10000; // scales to any size
    const sizes = (l.getAttribute("sizes") || "").match(/\d+/g);
    const px = sizes ? Math.max(...sizes.map(Number)) : rel.includes("apple-touch-icon") ? 180 : 16;
    return px >= 32 ? 5000 - Math.abs(px - 64) : px; // sharp on Retina, but not a huge download
  };
  const pick = () => {
    const links = [...document.querySelectorAll('link[rel~="icon" i], link[rel="shortcut icon" i], link[rel~="apple-touch-icon" i]')].filter((l) => l.href);
    if (!links.length) return;
    const best = links.sort((a, b) => score(b) - score(a))[0].href;
    if (best === sent) return;
    sent = best;
    try { window.__TAURI_INTERNALS__.invoke("page_event", { ev: { kind: "icon", url: best } }); } catch {}
  };
  document.addEventListener("DOMContentLoaded", () => {
    pick();
    new MutationObserver(pick).observe(document.head || document.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: ["href"] });
  });
})();
