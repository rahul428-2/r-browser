// R-Browser auto dark mode: pages that are light even though we ask for dark (prefers-color-scheme) get their
// colours inverted. Photos/videos are inverted back so they look normal. The filter sits on <html>: the spec
// exempts the root element from the "filter makes a containing block" rule, so position:fixed headers keep working.
(() => {
  if (window.top !== window) return; // the filter on the top page already covers its frames
  const ATTR = "data-rb-dark";
  const css = `
html[${ATTR}] { filter: invert(.9) hue-rotate(180deg) !important; background: #fff !important; }
html[${ATTR}] :is(img, video, picture, canvas, embed, object, svg image, [style*="background-image"]) {
  filter: invert(1) hue-rotate(180deg) !important; }`;
  const lum = (c) => {
    const m = (c || "").match(/[\d.]+/g);
    if (!m || (m.length > 3 && +m[3] < 0.1)) return null; // transparent
    const [r, g, b] = m.map(Number);
    return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
  };
  const check = () => {
    const root = document.documentElement, body = document.body;
    if (!root || !body) return;
    if (!document.getElementById("rb-dark")) {
      const s = document.createElement("style");
      s.id = "rb-dark";
      s.textContent = css;
      (document.head || root).append(s);
    }
    root.removeAttribute(ATTR); // measure the page's own colours
    const bg = lum(getComputedStyle(body).backgroundColor) ?? lum(getComputedStyle(root).backgroundColor) ??
      (getComputedStyle(root).colorScheme.includes("dark") ? 0 : 1); // transparent: the canvas is white unless dark is declared
    if (bg > 0.6) root.setAttribute(ATTR, "");
  };
  document.addEventListener("DOMContentLoaded", check);
  addEventListener("load", () => { check(); setTimeout(check, 1500); }); // late-styled single-page apps
})();
