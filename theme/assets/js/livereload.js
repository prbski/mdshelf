(() => {
  const RECONNECT_DELAY_MS = 1000;
  const MAX_RECONNECT_DELAY_MS = 10000;
  const RELOAD_DEBOUNCE_MS = 200;
  const SCROLL_STORAGE_KEY = "mdshelf-scroll-y";
  let reconnectDelayMs = RECONNECT_DELAY_MS;
  let reloadTimer = null;

  function restoreScrollPosition() {
    try {
      const storedScroll = sessionStorage.getItem(SCROLL_STORAGE_KEY);
      if (storedScroll === null) {
        return;
      }
      sessionStorage.removeItem(SCROLL_STORAGE_KEY);
      const scrollY = Number.parseInt(storedScroll, 10);
      if (!Number.isFinite(scrollY)) {
        return;
      }
      window.requestAnimationFrame(() => {
        window.scrollTo(0, scrollY);
      });
    } catch {
      /* ignore */
    }
  }

  function rememberScrollPosition() {
    try {
      sessionStorage.setItem(SCROLL_STORAGE_KEY, String(window.scrollY));
    } catch {
      /* ignore */
    }
  }

  function scheduleReload() {
    rememberScrollPosition();
    window.clearTimeout(reloadTimer);
    reloadTimer = window.setTimeout(() => {
      window.location.reload();
    }, RELOAD_DEBOUNCE_MS);
  }

  function buildLiveReloadUrl() {
    const wsScheme = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${wsScheme}//${window.location.host}/__livereload`;
  }

  function openLiveReloadStream() {
    const liveReloadSocket = new WebSocket(buildLiveReloadUrl());

    liveReloadSocket.addEventListener("open", () => {
      reconnectDelayMs = RECONNECT_DELAY_MS;
    });

    liveReloadSocket.addEventListener("message", (event) => {
      if (event.data === "reload") {
        scheduleReload();
      }
    });

    liveReloadSocket.addEventListener("close", scheduleReconnect);
    liveReloadSocket.addEventListener("error", () => {
      liveReloadSocket.close();
    });
  }

  function scheduleReconnect() {
    window.setTimeout(openLiveReloadStream, reconnectDelayMs);
    reconnectDelayMs = Math.min(reconnectDelayMs * 2, MAX_RECONNECT_DELAY_MS);
  }

  function openAfterIdle() {
    const idleCallback = window.requestIdleCallback;
    if (typeof idleCallback === "function") {
      idleCallback(() => {
        openLiveReloadStream();
      });
    } else {
      window.setTimeout(openLiveReloadStream, 1);
    }
  }

  function startAfterPageLoad() {
    restoreScrollPosition();
    if (document.readyState === "complete") {
      openAfterIdle();
    } else {
      window.addEventListener("load", openAfterIdle, { once: true });
    }
  }

  startAfterPageLoad();
})();
