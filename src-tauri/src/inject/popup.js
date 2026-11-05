// R-Browser: WebKit already refuses pop-ups opened without a click; this tells the browser so it can offer "Allow".
(() => {
  if (!/^https?:$/.test(location.protocol)) return;
  const open = window.open;
  window.open = function (u, ...rest) {
    if (!navigator.userActivation || navigator.userActivation.isActive) return open.call(window, u, ...rest);
    try {
      const url = u ? new URL(u, location.href).href : "";
      window.__TAURI_INTERNALS__.invoke("page_event", { ev: { kind: "popup-blocked", url } });
    } catch {}
    return null;
  };
})();
