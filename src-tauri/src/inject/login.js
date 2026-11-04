// R-Browser: tell the browser when this page has a login form, and what was typed when it's submitted.
// The browser decides the site from the tab itself, never from anything this script sends.
(() => {
  if (window.top !== window || !/^https?:$/.test(location.protocol)) return;
  const send = (ev) => { try { window.__TAURI_INTERNALS__.invoke("page_event", { ev }); } catch {} };
  const shown = (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);

  // 1. Key icon: report once that a password field exists.
  let told = false;
  const scan = () => {
    if (told || !document.querySelector("input[type=password]")) return;
    told = true;
    obs.disconnect();
    send({ kind: "login-form" });
  };
  const obs = new MutationObserver(scan);
  obs.observe(document, { subtree: true, childList: true });
  document.addEventListener("DOMContentLoaded", scan);

  // 2. Save prompt: on submit / Enter / button click with a filled password field.
  let last = "";
  const grab = (root) => {
    const pw = [...(root || document).querySelectorAll("input[type=password]")].find((e) => e.value);
    if (!pw) return;
    const all = [...document.querySelectorAll("input")];
    const before = all.slice(0, all.indexOf(pw)).reverse();
    const user = before.find((e) => shown(e) && e.value && /^(text|email|tel|)$/.test(e.type) ) ||
      before.find((e) => e.value && e.autocomplete === "username");
    const username = user ? user.value : "";
    const key = username + "\0" + pw.value;
    if (key === last) return;
    last = key;
    send({ kind: "login-submitted", username, password: pw.value });
  };
  document.addEventListener("submit", (e) => grab(e.target), true);
  document.addEventListener("keydown", (e) => { if (e.key === "Enter" && e.target.type === "password") grab(e.target.form); }, true);
  document.addEventListener("click", (e) => { if (e.target.closest && e.target.closest("button,input[type=submit],[role=button]")) grab(document); }, true);
})();
