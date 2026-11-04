const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
if (navigator.userAgent.includes("Mac")) document.body.classList.add("mac");
if (window.__TAURI__.window.getCurrentWindow().label.startsWith("incognito-")) document.body.classList.add("incognito");

let state = { tabs: [], active: null };
const parse = (url) => { try { return new URL(url); } catch { return null; } };
// Only our own page counts as "new tab" — a site serving /pages/newtab.html must not blank the address bar.
const isNewtab = (url) => { if (!url) return true; const u = parse(url); return !!u && u.origin === location.origin && u.pathname === "/pages/newtab.html"; };
const shown = (url) => (isNewtab(url) ? "" : url);

function icon(id) {
  const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  const u = document.createElementNS("http://www.w3.org/2000/svg", "use");
  u.setAttribute("href", "#" + id);
  s.append(u);
  return s;
}

function favicon(t) {
  const box = document.createElement("span");
  box.className = "fav";
  const u = parse(t.url);
  if (isNewtab(t.url) || !u || !/^https?:$/.test(u.protocol)) {
    const img = new Image();
    img.src = "logo.png";
    box.append(img);
    return box;
  }
  const mono = () => { box.className = "fav mono"; box.textContent = (u.hostname.replace(/^www\./, "")[0] || "?").toUpperCase(); };
  if (t.frozen) { mono(); return box; } // frozen tabs make no network requests
  const img = new Image();
  img.onerror = () => { img.remove(); mono(); };
  img.src = u.origin + "/favicon.ico";
  box.append(img);
  return box;
}

function renderTabs() {
  $("tabs").replaceChildren(...state.tabs.map((t) => {
    const el = document.createElement("div");
    el.className = "tab" + (t.id === state.active ? " active" : "") + (t.frozen ? " frozen" : "");
    el.setAttribute("role", "tab");
    el.title = t.title || shown(t.url) || "New tab";
    const title = document.createElement("span");
    title.className = "title";
    title.textContent = t.title || shown(t.url) || "New tab";
    const x = document.createElement("button");
    x.className = "icon x";
    x.setAttribute("aria-label", "Close tab");
    x.append(icon("i-x"));
    x.onclick = (e) => { e.stopPropagation(); invoke("close_tab", { id: t.id }); };
    el.onmousedown = (e) => { if (e.button === 1) invoke("close_tab", { id: t.id }); };
    el.onclick = () => invoke("activate_tab", { id: t.id });
    el.append(favicon(t), title, x);
    return el;
  }));
}

function renderBar() {
  const cur = state.tabs.find((t) => t.id === state.active);
  const url = cur ? shown(cur.url) : "";
  const u = parse(url);
  const st = !u ? "none" : u.protocol === "https:" ? "secure" : u.protocol === "http:" ? "insecure" : "none";
  $("omni").dataset.state = st;
  document.body.dataset.state = st;
  $("chip").replaceChildren(icon(st === "secure" ? "i-shield" : st === "insecure" ? "i-warn" : "i-search"));
  $("chip").title = st === "secure" ? "Encrypted connection" : st === "insecure" ? "Not secure: this connection isn't encrypted" : "";

  const pretty = $("pretty");
  pretty.replaceChildren();
  // Built from parsed parts (never string slicing), and never shows user:pass@ — so the bold part is the real host.
  if (u && /^https?:$/.test(u.protocol) && u.host) {
    const rest = (u.pathname === "/" ? "" : u.pathname) + u.search + u.hash;
    const b = document.createElement("b");
    b.textContent = u.host.replace(/^www\./, "");
    pretty.append(b, rest);
  }
  if (document.activeElement !== $("url")) {
    $("url").value = url;
    $("omni").classList.toggle("resting", pretty.childNodes.length > 0);
  }
}

listen("tabs-changed", (e) => { state = e.payload; renderTabs(); renderBar(); });

const act = (cmd) => state.active != null && invoke(cmd, { id: state.active });
$("newtab").onclick = () => invoke("new_tab", {}).then(() => $("url").focus());
$("back").onclick = () => act("go_back");
$("fwd").onclick = () => act("go_forward");
$("reload").onclick = () => act("reload");

const url = $("url");
url.addEventListener("focus", () => { $("omni").classList.remove("resting"); url.select(); });
url.addEventListener("blur", renderBar);
url.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && state.active != null && url.value.trim()) {
    invoke("navigate", { id: state.active, input: url.value });
    url.blur();
  }
  if (e.key === "Escape") url.blur();
});

invoke("ui_ready");
