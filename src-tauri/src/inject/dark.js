// R-Browser: dark Google search results. Google serves this engine a basic, light-only results page, so we
// invert its colours (photos, videos and logos are inverted back so they look normal). Only Google's /search
// page: every other website is left exactly as it is (Rahul's call). The filter sits on <html>, which the spec
// exempts from the "filter makes a containing block" rule, so fixed headers keep working.
(() => {
  if (window.top !== window || location.pathname !== "/search" || !/^www\.google\.[a-z]{2,3}(\.[a-z]{2})?$/.test(location.hostname)) return;
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
