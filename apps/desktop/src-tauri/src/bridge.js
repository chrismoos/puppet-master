(() => {
  if (window.__pmNative) return;
  const invoke = (command, args) => {
    const internals = window.__TAURI_INTERNALS__;
    return internals ? internals.invoke(command, args) : Promise.reject(new Error("not in the desktop app"));
  };
  const openWindow = (url) => invoke("open_window", { url: new URL(String(url), location.href).href }).catch(() => {});

  document.documentElement.classList.add("pm-desktop");

  // The window's own buttons are hidden: these replace them, centered on
  // the page's top bar, which makes room for them.
  const CONTROLS_STYLE = `
    .pm-window-controls {
      position: fixed;
      z-index: 2147483000;
      left: 16px;
      top: calc(var(--topbar-height, 46px) / 2);
      transform: translateY(-50%);
      display: flex;
      gap: 8px;
      -webkit-user-select: none;
      user-select: none;
    }
    .pm-window-controls button {
      all: unset;
      box-sizing: border-box;
      width: 14px;
      height: 14px;
      border-radius: 50%;
      border: 1.5px solid var(--muted, #828d9f);
      color: var(--muted, #828d9f);
      display: grid;
      place-items: center;
      cursor: default;
      transition: background-color 90ms ease-out, border-color 90ms ease-out, color 90ms ease-out;
    }
    .pm-window-controls svg { width: 8px; height: 8px; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; fill: none; }
    .pm-window-controls:hover button { color: var(--text, #c9ceda); border-color: var(--text, #c9ceda); }
    .pm-window-controls button:hover { background: color-mix(in srgb, var(--text, #c9ceda) 16%, transparent); }
    .pm-window-controls button.pm-close:hover { background: var(--red, #ff5d5d); border-color: var(--red, #ff5d5d); color: var(--chrome-bg, var(--bg, #000)); }
    .pm-window-blurred .pm-window-controls { opacity: 0.45; }
    .pm-window-controls .pm-zoom .pm-exit { display: none; }
    .pm-fullscreen .pm-window-controls .pm-zoom .pm-enter { display: none; }
    .pm-fullscreen .pm-window-controls .pm-zoom .pm-exit { display: inline; }
    .pm-desktop .topbar { padding-left: 90px; }
  `;
  const fullScreen = async () => {
    const label = windowLabel();
    const on = await invoke("plugin:window|is_fullscreen", { label });
    await invoke("plugin:window|set_fullscreen", { label, value: !on });
  };
  const syncFullScreen = () => {
    invoke("plugin:window|is_fullscreen", { label: windowLabel() })
      .then((on) => document.documentElement.classList.toggle("pm-fullscreen", Boolean(on)))
      .catch(() => {});
  };
  window.addEventListener("resize", syncFullScreen);
  const CONTROLS = [
    ["pm-close", "Close", () => invoke("plugin:window|close", { label: windowLabel() }), '<path d="M1.5 1.5l5 5M6.5 1.5l-5 5"/>'],
    ["pm-minimize", "Minimize", () => invoke("plugin:window|minimize", { label: windowLabel() }), '<path d="M1.5 4h5"/>'],
    [
      "pm-zoom",
      "Full Screen",
      fullScreen,
      '<g class="pm-enter"><path d="M4.5 1.5h2v2M6.5 1.5L4.5 3.5M3.5 6.5h-2v-2M1.5 6.5l2-2"/></g><g class="pm-exit"><path d="M6.5 3.5h-2v-2M4.5 3.5l2-2M1.5 4.5h2v2M3.5 4.5l-2 2"/></g>',
    ],
  ];
  const windowLabel = () => window.__TAURI_INTERNALS__?.metadata?.currentWindow?.label ?? "main";
  const addControls = () => {
    if (document.querySelector(".pm-window-controls")) return;
    const style = document.createElement("style");
    style.textContent = CONTROLS_STYLE;
    document.head.append(style);
    const group = document.createElement("div");
    group.className = "pm-window-controls";
    for (const [className, label, act, glyph] of CONTROLS) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = className;
      button.setAttribute("aria-label", label);
      button.tabIndex = -1;
      button.innerHTML = `<svg viewBox="0 0 8 8" aria-hidden="true">${glyph}</svg>`;
      button.addEventListener("mousedown", (event) => event.stopPropagation());
      button.addEventListener("click", () => {
        Promise.resolve(act()).then(syncFullScreen, () => {});
      });
      group.append(button);
    }
    document.body.append(group);
  };
  if (document.body) addControls();
  else document.addEventListener("DOMContentLoaded", addControls, { once: true });
  syncFullScreen();
  window.addEventListener("blur", () => document.documentElement.classList.add("pm-window-blurred"));
  window.addEventListener("focus", () => document.documentElement.classList.remove("pm-window-blurred"));

  // The page's top bar moves and zooms the window wherever nothing
  // interactive was pressed.
  const onTopBar = (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (!target || !target.closest(".topbar")) return false;
    return !target.closest("button, a, input, select, textarea, [role=button], [role=menu], [contenteditable]");
  };
  document.addEventListener("mousedown", (event) => {
    if (event.button !== 0 || event.detail > 1 || !onTopBar(event)) return;
    invoke("plugin:window|start_dragging", { label: windowLabel() }).catch(() => {});
  });
  document.addEventListener("dblclick", (event) => {
    if (!onTopBar(event)) return;
    invoke("plugin:window|toggle_maximize", { label: windowLabel() }).catch(() => {});
  });

  // A Tauri IPC channel: the app sends events to the callback this registers.
  const channel = (onMessage) => {
    const internals = window.__TAURI_INTERNALS__;
    const id = internals.transformCallback((raw) => onMessage(raw && typeof raw === "object" && "message" in raw ? raw.message : raw));
    return `__CHANNEL__:${id}`;
  };

  // Native terminals outlive a page load. The new page opened none of them,
  // so any still drawn belong to the page before it.
  if (window.__PM_NATIVE_TERMINALS__) invoke("native_terminal_close_all").catch(() => {});

  window.__pmNative = {
    notify: (title, body) => invoke("native_notify", { title: String(title), body: body == null ? null : String(body) }),
    setBadge: (count) => invoke("native_badge", { count: Math.max(0, Math.floor(Number(count) || 0)) }),
    openWindow,
    terminal: {
      supported: () => invoke("native_terminal_supported").catch(() => false),
      open: (id, attach, placement, onEvent) =>
        invoke("native_terminal_open", { request: { id, ...attach, placement }, onEvent: channel(onEvent) }),
      place: (id, placement) => invoke("native_terminal_place", { id, placement }),
      focus: (id) => invoke("native_terminal_focus", { id }),
      regions: (id, holes, passthrough) => invoke("native_terminal_regions", { id, holes, passthrough }),
      close: (id) => invoke("native_terminal_close", { id }),
    },
  };

  // A web view opens nothing for a new tab on its own. window.open answers
  // with a stand-in so a caller that checks the result treats it as handled,
  // and one that opens a blank tab first and points it somewhere later
  // opens that place once it does.
  let pendingLink = null;
  const openHref = (url) => {
    const href = new URL(String(url), location.href).href;
    if (pendingLink === href) pendingLink = null;
    openWindow(href);
  };
  const standIn = () => {
    const place = {
      assign: openHref,
      replace: openHref,
      toString: () => "about:blank",
    };
    Object.defineProperty(place, "href", { get: () => "about:blank", set: openHref });
    const opened = { closed: false, opener: null, close() { opened.closed = true; }, focus() {} };
    Object.defineProperty(opened, "location", { get: () => place, set: openHref });
    return opened;
  };
  window.open = (url) => {
    if (url) openHref(url);
    return standIn();
  };

  // A plain target=_blank link with no script of its own still opens; one
  // whose handler calls window.open is not opened twice.
  document.addEventListener(
    "click",
    (event) => {
      const link = event.target instanceof Element ? event.target.closest("a[target=_blank]") : null;
      if (!link || !link.href) return;
      event.preventDefault();
      pendingLink = link.href;
      queueMicrotask(() => {
        if (pendingLink) openWindow(pendingLink);
        pendingLink = null;
      });
    },
    true,
  );
})();
