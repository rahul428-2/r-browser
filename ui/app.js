const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
if (navigator.userAgent.includes("Mac")) document.body.classList.add("mac");
let state = { tabs: [], active: null };

const shown = (url) => (url.endsWith("/pages/newtab.html") ? "" : url);

function render() {
  const box = $("tabs");
  box.replaceChildren(...state.tabs.map((t) => {
    const el = document.createElement("div");
    el.className = "tab" + (t.id === state.active ? " active" : "") + (t.frozen ? " frozen" : "");
    el.title = t.title || t.url;
    const title = document.createElement("span");
    title.className = "title";
    title.textContent = t.title || shown(t.url) || "New Tab";
    const x = document.createElement("button");
    x.className = "x";
    x.textContent = "×";
    x.onclick = (e) => { e.stopPropagation(); invoke("close_tab", { id: t.id }); };
    el.onmousedown = (e) => { if (e.button === 1) invoke("close_tab", { id: t.id }); };
    el.onclick = () => invoke("activate_tab", { id: t.id });
    el.append(title, x);
    return el;
  }));
  const cur = state.tabs.find((t) => t.id === state.active);
  if (cur && document.activeElement !== $("url")) $("url").value = shown(cur.url);
}

listen("tabs-changed", (e) => { state = e.payload; render(); });

const act = (cmd) => state.active != null && invoke(cmd, { id: state.active });
$("newtab").onclick = () => invoke("new_tab", {}).then(() => $("url").focus());
$("back").onclick = () => act("go_back");
$("fwd").onclick = () => act("go_forward");
$("reload").onclick = () => act("reload");
$("url").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && state.active != null) {
    invoke("navigate", { id: state.active, input: $("url").value });
    $("url").blur();
  }
  if (e.key === "Escape") { $("url").blur(); render(); }
});
$("url").addEventListener("focus", () => $("url").select());

// Shortcuts while the browser bar has focus. Shortcuts inside pages come from the native menu.
document.addEventListener("keydown", (e) => {
  const mod = e.metaKey || e.ctrlKey;
  if (!mod) return;
  const i = state.tabs.findIndex((t) => t.id === state.active);
  const k = e.key.toLowerCase();
  if (k === "t") { e.preventDefault(); $("newtab").click(); }
  else if (k === "w") { e.preventDefault(); act("close_tab"); }
  else if (k === "l") { e.preventDefault(); $("url").focus(); }
  else if (k === "r") { e.preventDefault(); act("reload"); }
  else if (/^[1-9]$/.test(k) && state.tabs.length) {
    e.preventDefault();
    const t = k === "9" ? state.tabs[state.tabs.length - 1] : state.tabs[+k - 1];
    if (t) invoke("activate_tab", { id: t.id });
  } else if (k === "tab" && state.tabs.length) {
    e.preventDefault();
    const n = state.tabs.length;
    invoke("activate_tab", { id: state.tabs[(i + (e.shiftKey ? n - 1 : 1)) % n].id });
  }
});

invoke("ui_ready");
