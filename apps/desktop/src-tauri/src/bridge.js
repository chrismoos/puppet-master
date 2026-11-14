(() => {
  if (window.__pmNative) return;
  const invoke = (command, args) => {
    const internals = window.__TAURI_INTERNALS__;
    return internals ? internals.invoke(command, args) : Promise.reject(new Error("not in the desktop app"));
  };
  const openWindow = (url) => invoke("open_window", { url: new URL(String(url), location.href).href }).catch(() => {});

  document.documentElement.classList.add("pm-desktop");

  // With no native title bar, the page's top bar moves and zooms the window
  // wherever nothing interactive was pressed.
  const windowLabel = () => window.__TAURI_INTERNALS__?.metadata?.currentWindow?.label ?? "main";
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

  window.__pmNative = {
    notify: (title, body) => invoke("native_notify", { title: String(title), body: body == null ? null : String(body) }),
    setBadge: (count) => invoke("native_badge", { count: Math.max(0, Math.floor(Number(count) || 0)) }),
    openWindow,
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
