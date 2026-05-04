(() => {
  const RECONNECT_DELAY_MS = 1000;
  const MAX_RECONNECT_DELAY_MS = 10000;
  let reconnectDelayMs = RECONNECT_DELAY_MS;

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
        window.location.reload();
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
    if (document.readyState === "complete") {
      openAfterIdle();
    } else {
      window.addEventListener("load", openAfterIdle, { once: true });
    }
  }

  startAfterPageLoad();
})();
