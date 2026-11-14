(() => {
  if (window.__pmNative) return;
  const invoke = (command, args) => {
    const internals = window.__TAURI_INTERNALS__;
    return internals ? internals.invoke(command, args) : Promise.reject(new Error("not in the desktop app"));
  };
  const openWindow = (url) => invoke("open_window", { url: new URL(String(url), location.href).href }).catch(() => {});

  document.documentElement.classList.add("pm-desktop");

  // The window has no native title bar: a slim strip in the page's chrome
  // color holds the window buttons and moves the window.
  const TITLEBAR_PX = 28;
  const addTitlebar = () => {
    if (document.querySelector(".pm-titlebar")) return;
    const strip = document.createElement("div");
    strip.className = "pm-titlebar";
    strip.setAttribute("aria-hidden", "true");
    Object.assign(strip.style, {
      position: "fixed",
      top: "0",
      left: "0",
      right: "0",
      height: `${TITLEBAR_PX}px`,
      zIndex: "2147483000",
      background: "var(--chrome-bg, var(--bg, transparent))",
    });
    document.body.prepend(strip);
    document.body.style.paddingTop = `${TITLEBAR_PX}px`;
  };
  if (document.body) addTitlebar();
  else document.addEventListener("DOMContentLoaded", addTitlebar, { once: true });

  // The strip and the page's top bar move and zoom the window wherever
  // nothing interactive was pressed.
  const windowLabel = () => window.__TAURI_INTERNALS__?.metadata?.currentWindow?.label ?? "main";
  const onTopBar = (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (!target || !target.closest(".topbar, .pm-titlebar")) return false;
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
  // with a stand-in so a caller that checks the result treats it as handled.
  let pendingLink = null;
  window.open = (url) => {
    if (url) {
      const href = new URL(String(url), location.href).href;
      if (pendingLink === href) pendingLink = null;
      openWindow(href);
    }
    return { closed: false, close() {}, focus() {} };
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
