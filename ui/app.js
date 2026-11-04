const { invoke } = window.__TAURI__.core;
const me = window.__TAURI__.webview.getCurrentWebview(); // events targeted at this toolbar only
const myWindow = window.__TAURI__.window.getCurrentWindow().label;
const $ = (id) => document.getElementById(id);
if (navigator.userAgent.includes("Mac")) document.body.classList.add("mac");
const incognito = myWindow.startsWith("incognito-");
if (incognito) document.body.classList.add("incognito");

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
  if (t.frozen || incognito) { mono(); return box; } // frozen tabs make no requests; incognito leaves no favicon traces
  const img = new Image();
  img.onerror = () => { img.remove(); mono(); };
  img.src = u.origin + "/favicon.ico";
  box.append(img);
  return box;
}

// Chrome's 8 group colours, tuned for the night palette.
const GROUP_COLORS = { grey: "#9aa5b8", blue: "#5aa9ff", red: "#ff6b6b", yellow: "#f5c84a", green: "#4fd18b", pink: "#ff79c6", purple: "#b18cff", cyan: "#4fe0e8" };
let renaming = null; // { gid, value, fresh } while a group name is being edited
let rerendering = false; // removing the old input during a re-render must not count as "done editing"

window.startRename = (gid) => {
  const g = (state.groups || []).find((x) => x.id === gid);
  renaming = { gid, value: g ? g.name : "", fresh: true };
  renderTabs();
};

function groupChip(g, count) {
  const color = GROUP_COLORS[g.color] || GROUP_COLORS.grey;
  if (renaming && renaming.gid === g.id) {
    const inp = document.createElement("input");
    inp.className = "gname";
    inp.style.setProperty("--g", color);
    inp.value = renaming.value;
    inp.maxLength = 40;
    inp.placeholder = "Name this group";
    inp.setAttribute("aria-label", "Group name");
    let done = false;
    const finish = (save) => {
      if (done) return; done = true;
      const name = inp.value; renaming = null;
      if (save) invoke("group_rename", { gid: g.id, name }); else renderTabs();
    };
    inp.oninput = () => { renaming.value = inp.value; };
    inp.onkeydown = (e) => { if (e.key === "Enter") finish(true); if (e.key === "Escape") finish(false); };
    inp.onblur = () => { if (!rerendering) finish(true); };
    const fresh = renaming.fresh; renaming.fresh = false;
    const caret = inp.value.length;
    requestAnimationFrame(() => { inp.focus(); if (fresh) inp.select(); else inp.setSelectionRange(caret, caret); });
    return inp;
  }
  const b = document.createElement("button");
  b.dataset.gid = g.id;
  b.className = "gchip" + (g.name ? "" : " dot") + (g.collapsed ? " collapsed" : "");
  b.style.setProperty("--g", color);
  b.textContent = g.name;
  if (g.collapsed) { const n = document.createElement("span"); n.className = "gcount"; n.textContent = count; b.append(n); }
  b.title = `${g.name || "Unnamed group"} · ${count} tab${count === 1 ? "" : "s"} — click to ${g.collapsed ? "expand" : "collapse"}, right-click for options`;
  b.setAttribute("aria-expanded", String(!g.collapsed));
  b.onclick = () => invoke("group_toggle", { gid: g.id });
  b.oncontextmenu = (e) => { e.preventDefault(); invoke("group_menu", { gid: g.id }); };
  return b;
}

function renderTabs() {
  const groups = Object.fromEntries((state.groups || []).map((g) => [g.id, g]));
  const out = [];
  let prev = null;
  for (const t of state.tabs) {
    const g = t.group != null ? groups[t.group] : null;
    if (g && t.group !== prev) out.push(groupChip(g, state.tabs.filter((x) => x.group === g.id).length));
    prev = t.group;
    if (g && g.collapsed) continue;
    const el = tabEl(t);
    if (g) { el.classList.add("grouped"); el.style.setProperty("--g", GROUP_COLORS[g.color] || GROUP_COLORS.grey); }
    out.push(el);
  }
  rerendering = true;
  $("tabs").replaceChildren(...out);
  rerendering = false;
}

function tabEl(t) {
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
  el.dataset.id = t.id;
  el.onclick = () => { if (!dragJustEnded) invoke("activate_tab", { id: t.id }); };
  el.onpointerdown = (e) => startDrag(e, el, t);
  el.oncontextmenu = (e) => { e.preventDefault(); invoke("tab_menu", { id: t.id }); };
  el.append(favicon(t), title, x);
  return el;
}

function renderBar() {
  const cur = state.tabs.find((t) => t.id === state.active);
  const url = cur ? shown(cur.url) : "";
  const u = parse(url);
  const st = !u ? "none" : u.protocol === "https:" ? "secure" : u.protocol === "http:" ? "insecure" : "none";
  $("omni").dataset.state = st;
  document.body.dataset.state = st;
  document.body.dataset.loading = cur && cur.loading ? "1" : "";
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
  $("key").hidden = incognito || !(cur && cur.login_form);
  $("star").classList.toggle("on", !!state.bookmarked);
  $("star").title = state.bookmarked ? "Remove bookmark (Ctrl/⌘+D)" : "Bookmark this page (Ctrl/⌘+D)";
  if (document.activeElement !== $("url")) {
    $("url").value = url;
    $("omni").classList.toggle("resting", pretty.childNodes.length > 0);
  }
}

me.listen("tabs-changed", (e) => {
  if (e.payload.win !== myWindow) return; // never render another window's (e.g. incognito) tabs
  state = e.payload;
  if (!drag) renderTabs(); // a re-render mid-drag would drop the tab under the cursor; catch up on drop
  renderBar();
});

// ---- drag tabs left/right, into and out of groups (like Chrome) ----
// The tab under the cursor decides: over a grouped tab -> join that group; over an ungrouped tab,
// a group chip or empty strip -> no group. The Rust core keeps groups contiguous whatever we ask.
let drag = null;
let dragJustEnded = false;
const marker = document.createElement("div");
marker.id = "dropmark";

function startDrag(e, el, t) {
  if (e.button !== 0 || e.target.closest(".x")) return;
  drag = { el, t, x0: e.clientX, moving: false, target: null };
  el.setPointerCapture(e.pointerId);
  el.onpointermove = (ev) => moveDrag(ev);
  el.onpointerup = el.onpointercancel = () => endDrag();
}

function dropTarget(x) {
  const order = state.tabs.filter((t) => t.id !== drag.t.id);
  const next = (id) => { const i = order.findIndex((t) => t.id === id); return order[i + 1] ? order[i + 1].id : null; };
  const items = [...$("tabs").children].filter((e) => e !== drag.el);
  for (const e of items) {
    const r = e.getBoundingClientRect();
    if (x < r.left || x >= r.right) continue;
    if (e.dataset.gid) { // group chip: just before the group, outside it
      const first = order.find((t) => t.group === +e.dataset.gid);
      return { before: first ? first.id : null, group: null, x: r.left, color: null };
    }
    const id = +e.dataset.id, t = order.find((t) => t.id === id);
    const leftHalf = x < r.left + r.width / 2;
    const g = (state.groups || []).find((g) => g.id === t.group);
    return { before: leftHalf ? id : next(id), group: t.group ?? null, x: leftHalf ? r.left : r.right, color: g ? GROUP_COLORS[g.color] : null };
  }
  if (!items.length) return null;
  const first = items[0].getBoundingClientRect(), last = items[items.length - 1].getBoundingClientRect();
  if (x >= last.right) return { before: null, group: null, x: last.right, color: null };
  if (x < first.left) return { before: order[0] ? order[0].id : null, group: null, x: first.left, color: null };
  return null; // over the dragged tab's own empty slot: stay put
}

function moveDrag(e) {
  const dx = e.clientX - drag.x0;
  if (!drag.moving && Math.abs(dx) < 6 && Math.abs(e.clientY - 18) < 12) return;
  drag.moving = true;
  drag.el.classList.add("dragging");
  drag.el.style.transform = `translateX(${dx}px)`;
  // Pulled well below the toolbar (into the page) or above the window: drop = open in a new window.
  // The toolbar keeps getting the moves because the drag started here, even over the page.
  drag.detach = state.tabs.length > 1 && (e.clientY > innerHeight + 40 || e.clientY < -25) ? { x: e.screenX, y: e.screenY } : null;
  drag.el.classList.toggle("detaching", !!drag.detach);
  drag.target = drag.detach ? null : dropTarget(e.clientX);
  if (!drag.target) { marker.remove(); return; }
  const strip = $("strip").getBoundingClientRect();
  marker.style.left = `${drag.target.x - strip.left - 1}px`;
  marker.style.background = drag.target.color || "var(--signal)";
  if (!marker.isConnected) $("strip").append(marker);
}

function endDrag() {
  const d = drag;
  drag = null;
  marker.remove();
  d.el.onpointermove = d.el.onpointerup = d.el.onpointercancel = null;
  if (!d.moving) return;
  dragJustEnded = true; // the click that follows pointerup must not activate the tab
  setTimeout(() => { dragJustEnded = false; }, 0);
  d.el.classList.remove("dragging", "detaching");
  d.el.style.transform = "";
  if (d.detach) invoke("detach_tab", { id: d.t.id, x: d.detach.x, y: d.detach.y });
  else if (d.target) invoke("move_tab", { id: d.t.id, before: d.target.before, group: d.target.group });
  renderTabs();
}

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

// ---- bookmark star ----
$("star").onclick = (e) => { e.preventDefault(); invoke("toggle_bookmark"); };

// ---- passwords: fill (key icon) and save prompt ----
$("key").onclick = (e) => { e.preventDefault(); invoke("password_menu").catch(() => {}); };
let pwTimer;
const hidePw = () => { $("savepw").hidden = true; clearTimeout(pwTimer); };
me.listen("save-password", (e) => {
  const [win, host, user] = e.payload;
  if (win !== myWindow) return;
  $("savepw").querySelector("span").textContent = user ? `Save password for ${user} on ${host}?` : `Save password for ${host}?`;
  $("savepw").hidden = false;
  clearTimeout(pwTimer);
  pwTimer = setTimeout(() => { hidePw(); invoke("password_dismiss"); }, 60000);
});
$("pwsave").onclick = () => { hidePw(); invoke("password_save").catch(() => {}); };
$("pwnever").onclick = () => { hidePw(); invoke("password_never"); };
$("pwx").onclick = () => { hidePw(); invoke("password_dismiss"); };

// ---- suggestions from bookmarks + history (native datalist popup) ----
let sugTimer;
url.addEventListener("input", () => {
  clearTimeout(sugTimer);
  sugTimer = setTimeout(async () => {
    const list = await invoke("suggest", { query: url.value });
    $("sugg").replaceChildren(...list.map((s) => {
      const o = document.createElement("option");
      o.value = s.url;
      o.label = (s.bookmarked ? "★ " : "") + (s.title || s.url);
      return o;
    }));
  }, 80);
});

// ---- find in page ----
window.openFind = () => { $("find").hidden = false; $("findq").focus(); $("findq").select(); };
const findGo = (backwards) => state.active != null && $("findq").value &&
  invoke("find_in_page", { id: state.active, query: $("findq").value, backwards });
$("find").onsubmit = (e) => { e.preventDefault(); findGo(false); };
$("findprev").onclick = () => findGo(true);
$("findclose").onclick = () => { $("find").hidden = true; };
$("findq").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && e.shiftKey) { e.preventDefault(); findGo(true); }
  if (e.key === "Escape") $("find").hidden = true;
});

// ---- download notice ----
let toastTimer;
me.listen("download", (e) => {
  const [win, name, status] = e.payload;
  if (win !== myWindow) return;
  const t = $("toast");
  t.hidden = false;
  t.classList.toggle("failed", status === "failed");
  t.querySelector("span").textContent =
    status === "started" ? `Downloading ${name}` : status === "done" ? `Downloaded ${name}` : `Download failed: ${name}`;
  clearTimeout(toastTimer);
  if (status !== "started") toastTimer = setTimeout(() => { t.hidden = true; }, 6000);
});
$("toast").onclick = () => invoke("new_tab", { url: "rb://downloads" });

invoke("ui_ready");
