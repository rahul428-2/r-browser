const { invoke } = window.__TAURI__.core;
const me = window.__TAURI__.webview.getCurrentWebview(); // events targeted at this toolbar only
const myWindow = window.__TAURI__.window.getCurrentWindow().label;
const $ = (id) => document.getElementById(id);
if (navigator.userAgent.includes("Mac")) document.body.classList.add("mac");
// Windows: frameless window, so the strip carries minimize / maximize / close. Double-clicking the strip maximizes (Tauri's drag region).
if (navigator.userAgent.includes("Windows")) {
  const w = window.__TAURI__.window.getCurrentWindow();
  document.body.classList.add("win", "focused");
  $("caption").hidden = false;
  $("wmin").onclick = () => w.minimize();
  $("wmax").onclick = () => w.toggleMaximize();
  $("wclose").onclick = () => w.close();
  const sync = async () => {
    const m = await w.isMaximized();
    document.body.classList.toggle("maxed", m);
    $("wmax").title = $("wmax").ariaLabel = m ? "Restore" : "Maximize";
  };
  w.onResized(sync); sync();
  w.onFocusChanged(({ payload }) => document.body.classList.toggle("focused", payload));
}
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

// Tab icon: the page's declared icon, else /favicon.ico, else a plain globe. The browser's own pages get the shield.
function favicon(t) {
  const box = document.createElement("span");
  box.className = "fav";
  const u = parse(t.url);
  if (isNewtab(t.url) || !u || !/^https?:$/.test(u.protocol)) { box.classList.add("own"); box.append(icon("i-shield")); return box; }
  const globe = () => { box.replaceChildren(icon("i-globe")); box.classList.add("none"); };
  if (t.frozen || incognito) { globe(); return box; } // frozen tabs make no requests; incognito leaves no favicon traces
  const tries = [t.icon, u.origin + "/favicon.ico"].filter(Boolean);
  const img = new Image();
  img.decoding = "async";
  img.onerror = () => { tries.shift(); if (tries.length) img.src = tries[0]; else globe(); };
  img.src = tries[0];
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

// The strip is updated in place: each tab keeps its element for its whole life. Rebuilding on every page
// event made the hovered tab lose its hover (the ✕ flickered), dropped clicks that landed mid-rebuild and
// re-fetched favicons.
const tabEls = new Map();  // tab id -> { el, title, favKey }
const chipEls = new Map(); // group id -> { el, sig }

function renderTabs() {
  const groups = Object.fromEntries((state.groups || []).map((g) => [g.id, g]));
  const out = [];
  let prev = null;
  for (const t of state.tabs) {
    const g = t.group != null ? groups[t.group] : null;
    if (g && t.group !== prev) out.push(chipFor(g, state.tabs.filter((x) => x.group === g.id).length));
    prev = t.group;
    if (g && g.collapsed) continue;
    out.push(tabFor(t, g));
  }
  const live = new Set(state.tabs.map((t) => t.id));
  for (const id of tabEls.keys()) if (!live.has(id)) tabEls.delete(id);
  for (const id of chipEls.keys()) if (!groups[id]) chipEls.delete(id);
  const cur = [...$("tabs").children];
  if (cur.length === out.length && cur.every((n, i) => n === out[i])) return; // same order: nothing to move
  rerendering = true;
  $("tabs").replaceChildren(...out);
  rerendering = false;
}

function chipFor(g, count) {
  if (renaming && renaming.gid === g.id) { chipEls.delete(g.id); return groupChip(g, count); }
  const sig = [g.name, g.color, g.collapsed, count].join("|");
  const c = chipEls.get(g.id);
  if (c && c.sig === sig) { c.el.style.transform = ""; return c.el; } // may still be slid aside from a drag
  const el = groupChip(g, count);
  chipEls.set(g.id, { el, sig });
  return el;
}

function tabFor(t, g) {
  let c = tabEls.get(t.id);
  if (!c) { c = makeTab(t.id); tabEls.set(t.id, c); }
  const { el } = c;
  el.className = "tab" + (t.id === state.active ? " active" : "") + (t.frozen ? " frozen" : "") + (g ? " grouped" : "");
  el.style.transform = el.style.transition = "";
  if (g) el.style.setProperty("--g", GROUP_COLORS[g.color] || GROUP_COLORS.grey);
  const text = t.title || shown(t.url) || "New tab";
  if (el.title !== text) { el.title = text; c.title.textContent = text; }
  const u = parse(t.url);
  const favKey = isNewtab(t.url) || !u || !/^https?:$/.test(u.protocol) ? "logo" : (t.frozen || incognito ? "globe:" : "img:" + (t.icon || u.origin));
  if (c.favKey !== favKey) { c.favKey = favKey; el.firstChild.replaceWith(favicon(t)); }
  return el;
}

function makeTab(id) {
  const tab = () => state.tabs.find((x) => x.id === id);
  const el = document.createElement("div");
  el.setAttribute("role", "tab");
  el.dataset.id = id;
  const title = document.createElement("span");
  title.className = "title";
  const x = document.createElement("button");
  x.className = "icon x";
  x.setAttribute("aria-label", "Close tab");
  x.append(icon("i-x"));
  x.onclick = (e) => { e.stopPropagation(); invoke("close_tab", { id }); };
  el.onmousedown = (e) => { if (e.button === 1) invoke("close_tab", { id }); };
  el.onclick = () => { if (!dragJustEnded) invoke("activate_tab", { id }); };
  el.onpointerdown = (e) => { const t = tab(); if (t) startDrag(e, el, t); };
  el.oncontextmenu = (e) => { e.preventDefault(); invoke("tab_menu", { id }); };
  el.append(document.createElement("span"), title, x); // first child = favicon slot
  return { el, title, favKey: null };
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
  $("chip").title = st === "secure" ? "Encrypted connection. Click for ad blocking on this site" : st === "insecure" ? "Not secure: this connection isn't encrypted. Click for ad blocking on this site" : "";

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
  $("unv").hidden = !(state.unverified || state.checked);
  $("unv").classList.toggle("soft", !!state.checked);
  $("unv").lastChild.textContent = state.checked ? "Checked" : "Unverified";
  if ($("unv").dataset.icon !== (state.checked ? "c" : "u")) { $("unv").dataset.icon = state.checked ? "c" : "u"; $("unv").firstChild.replaceWith(icon(state.checked ? "i-shield" : "i-warn")); }
  $("unv").title = state.checked ? "Passed the safety check, but not a well-known site: click for details" : "Unverified site: click to see why";
  // The styled address overlays the input: keep it where the input starts (the badge pushes it right).
  $("pretty").style.left = $("url").offsetLeft + "px";
  $("star").title = state.bookmarked ? "Remove bookmark (Ctrl/⌘+D)" : "Bookmark this page (Ctrl/⌘+D)";
  if (document.activeElement !== $("url")) {
    $("url").value = url;
    $("omni").classList.toggle("resting", pretty.childNodes.length > 0);
  }
}

let settling = false; // a dropped tab is gliding into its slot; the new order renders right after
me.listen("tabs-changed", (e) => {
  if (e.payload.win !== myWindow) return; // never render another window's (e.g. incognito) tabs
  state = e.payload;
  if (!drag && !settling) renderTabs(); // re-rendering mid-drag would yank the tab from under the cursor
  renderBar();
});

// ---- drag tabs, like Chrome ----
// Sideways: the tab follows the cursor and the others slide aside; on release it glides into its slot.
// Over a grouped tab it joins that group, over an ungrouped tab or a chip it leaves (the Rust core keeps
// groups contiguous). Pulled down into the page or above the window: it tears off into its own window that
// follows the cursor (the only tab: the window itself moves). Released over any R-Browser tab strip, even
// the one it came from, it attaches there; anywhere else it stays a window.
let drag = null;
let dragJustEnded = false;
const marker = document.createElement("div");
marker.id = "dropmark";

function startDrag(e, el, t) {
  if (e.button !== 0 || e.target.closest(".x") || drag || settling) return;
  const items = [...$("tabs").children];
  drag = { el, t, x0: e.clientX, y0: e.clientY, moving: false, torn: false, items, idx: items.indexOf(el),
    rects: items.map((i) => i.getBoundingClientRect()), others: [], k: null, group: t.group ?? null, last: e, raf: 0 };
  el.setPointerCapture(e.pointerId);
  el.onpointermove = (ev) => { if (!drag) return; drag.last = ev; if (!drag.raf) drag.raf = requestAnimationFrame(frame); };
  el.onpointerup = el.onpointercancel = (ev) => { if (drag) drag.last = ev; endDrag(); };
}

function frame() {
  const d = drag;
  if (!d) return;
  d.raf = 0;
  const e = d.last, dx = e.clientX - d.x0;
  if (d.torn) { invoke("tear_move", { x: e.screenX, y: e.screenY }); return; }
  if (!d.moving && Math.abs(dx) < 5 && Math.abs(e.clientY - d.y0) < 14) return;
  if (!d.moving) { d.moving = true; d.el.classList.add("dragging"); $("tabs").classList.add("reordering"); }
  if (e.clientY > innerHeight + 40 || e.clientY < -25) { tearOff(e); return; }

  const r0 = d.rects[d.idx], w = r0.width;
  const lo = d.rects[0].left, hi = Math.max(...d.rects.map((r) => r.right));
  const left = Math.min(Math.max(r0.left + dx, lo), hi - w);
  d.el.style.transform = `translateX(${left - r0.left}px)`;
  const center = left + w / 2;
  d.others = d.items.map((_, i) => i).filter((i) => i !== d.idx);
  const mid = (i) => d.rects[i].left + d.rects[i].width / 2;
  d.k = d.others.filter((i) => mid(i) < center).length; // the slot: how many items end up left of it
  d.others.forEach((i, j) => {
    const shift = i < d.idx && j >= d.k ? w : i > d.idx && j < d.k ? -w : 0;
    d.items[i].style.transform = shift ? `translateX(${shift}px)` : "";
  });
  // the group it would land in: that of the tab its centre is over (over a chip / ungrouped tab: none)
  const over = d.others.find((i) => center >= d.rects[i].left && center < d.rects[i].right);
  if (over != null) {
    const it = d.items[over], ot = it.dataset.id ? state.tabs.find((t) => t.id === +it.dataset.id) : null;
    d.group = ot ? ot.group ?? null : null;
  }
  const g = (state.groups || []).find((x) => x.id === d.group);
  d.el.classList.toggle("grouped", !!g);
  if (g) d.el.style.setProperty("--g", GROUP_COLORS[g.color]);
}

function tearOff(e) {
  const d = drag;
  d.torn = true;
  const single = state.tabs.length < 2;
  d.items.forEach((i) => { i.style.transform = ""; });
  if (!single) d.el.classList.add("torn"); // the others close the gap
  // keep the cursor where it grabbed the tab: on the new window's first tab, or (only tab) on this window
  const r0 = d.rects[d.idx];
  const ox = single ? d.x0 : $("tabs").getBoundingClientRect().left + (d.x0 - r0.left);
  invoke("tear_start", { id: d.t.id, x: e.screenX, y: e.screenY, ox, oy: d.y0 });
}

function endDrag() {
  const d = drag;
  if (!d) return;
  drag = null;
  cancelAnimationFrame(d.raf);
  d.el.onpointermove = d.el.onpointerup = d.el.onpointercancel = null;
  if (!d.moving) return;
  dragJustEnded = true; // the click that follows pointerup must not activate the tab
  setTimeout(() => { dragJustEnded = false; }, 0);
  const done = () => { settling = false; $("tabs").classList.remove("reordering"); renderTabs(); };
  if (d.torn) { invoke("tear_end", { x: d.last.screenX, y: d.last.screenY }); done(); return; }
  if (d.k == null) { done(); return; }
  // glide into the slot, then let the new order render in place
  const r0 = d.rects[d.idx], w = r0.width, p = d.idx;
  const left = d.k < p ? d.rects[d.others[d.k]].left : d.k === p ? r0.left : d.rects[d.others[d.k - 1]].right - w;
  d.el.style.transition = "transform .16s ease";
  d.el.style.transform = `translateX(${left - r0.left}px)`;
  const next = d.items[d.others[d.k]];
  const before = !next ? null : next.dataset.id ? +next.dataset.id
    : (state.tabs.find((t) => t.group === +next.dataset.gid && t.id !== d.t.id) || {}).id ?? null;
  settling = true;
  invoke("move_tab", { id: d.t.id, before, group: d.group });
  setTimeout(done, 170);
}

// A tab torn off another window (or this one) hovering over / dropped on this strip: Rust tells us where.
function slotAt(x) {
  const items = [...$("tabs").children].filter((e) => !e.classList.contains("torn"));
  const firstOf = (gid) => (state.tabs.find((t) => t.group === gid) || {}).id ?? null;
  const idOf = (e) => (e ? (e.dataset.id ? +e.dataset.id : firstOf(+e.dataset.gid)) : null);
  for (let i = 0; i < items.length; i++) {
    const e = items[i], r = e.getBoundingClientRect();
    if (x < r.left || x >= r.right) continue;
    if (e.dataset.gid) return { before: idOf(e), group: null, x: r.left, color: null };
    const t = state.tabs.find((t) => t.id === +e.dataset.id), leftHalf = x < r.left + r.width / 2;
    const g = t && (state.groups || []).find((g) => g.id === t.group);
    return { before: leftHalf ? +e.dataset.id : idOf(items[i + 1]), group: t ? t.group ?? null : null,
      x: leftHalf ? r.left : r.right, color: g ? GROUP_COLORS[g.color] : null };
  }
  const last = items[items.length - 1];
  if (!last || x >= last.getBoundingClientRect().right) {
    return { before: null, group: null, x: last ? last.getBoundingClientRect().right : $("tabs").getBoundingClientRect().left, color: null };
  }
  return { before: idOf(items[0]), group: null, x: items[0].getBoundingClientRect().left, color: null };
}
window.dropHint = (x) => {
  if (x == null) { marker.remove(); return; }
  const s = slotAt(x);
  marker.style.left = `${s.x - $("strip").getBoundingClientRect().left - 1}px`;
  marker.style.background = s.color || "var(--signal)";
  if (!marker.isConnected) $("strip").append(marker);
};
window.dropForeign = (id, x) => {
  marker.remove();
  const s = slotAt(x);
  invoke("attach_tab", { id, before: s.before, group: s.group });
};

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
$("more").onclick = () => invoke("app_menu").catch(() => {});
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

$("unv").onclick = (e) => { e.preventDefault(); invoke("trust_menu").catch(() => {}); };

// ---- shield chip: per-site blocking switch; blocked pop-up prompt ----
$("chip").onclick = (e) => { e.preventDefault(); if ($("omni").dataset.state !== "none") invoke("shield_menu").catch(() => {}); };
let popTimer;
const hidePop = () => { $("popblk").hidden = true; clearTimeout(popTimer); };
me.listen("popup-blocked", (e) => {
  const [win, host] = e.payload;
  if (win !== myWindow) return;
  $("popblk").querySelector("span").textContent = `Pop-up blocked on ${host}`;
  $("popblk").hidden = false;
  clearTimeout(popTimer);
  popTimer = setTimeout(() => { hidePop(); invoke("popup_dismiss"); }, 30000);
});
$("popallow").onclick = () => { hidePop(); invoke("popup_allow").catch(() => {}); };
$("popx").onclick = () => { hidePop(); invoke("popup_dismiss"); };

// ---- update downloaded and verified: offer a restart (stays until clicked) ----
me.listen("update-ready", (e) => {
  $("upd").querySelector("span").textContent = `Update ${e.payload} ready`;
  $("upd").hidden = false;
});
$("updgo").onclick = () => invoke("update_restart").catch(() => {});

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
// "3 min left" / "12 s left"; shared wording with the Downloads page.
const left = (s) => s == null ? "" : s < 60 ? `${s} s left` : s < 3600 ? `${Math.round(s / 60)} min left` : `${Math.floor(s / 3600)} h ${Math.round((s % 3600) / 60)} min left`;
const mb = (b) => b < 1e6 ? `${Math.round(b / 1e3)} KB` : b < 1e9 ? `${(b / 1e6).toFixed(1)} MB` : `${(b / 1e9).toFixed(2)} GB`;
me.listen("download", (e) => {
  const [win, name, status, got, pct, eta] = e.payload;
  if (win !== myWindow) return;
  const t = $("toast");
  t.hidden = false;
  t.classList.toggle("failed", status === "failed");
  t.classList.toggle("live", status === "progress" && pct != null);
  if (pct != null) t.style.setProperty("--p", `${pct}%`);
  t.querySelector("span").textContent =
    status === "started" ? `Downloading ${name}`
    : status === "progress" ? [`${name}`, pct != null ? `${pct}%` : mb(got), left(eta)].filter(Boolean).join(" · ")
    : status === "done" ? `Downloaded ${name}` : `Download failed: ${name}`;
  t.title = t.querySelector("span").textContent;
  clearTimeout(toastTimer);
  if (status === "done" || status === "failed") toastTimer = setTimeout(() => { t.hidden = true; }, 6000);
});
$("toast").onclick = () => invoke("new_tab", { url: "rb://downloads" });

invoke("ui_ready");
