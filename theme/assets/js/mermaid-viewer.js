(() => {
  const ZOOM_STEP = 0.2;
  const ZOOM_MIN = 0.2;
  const ZOOM_MAX = 8;

  let modal = null;
  let currentScale = 1;
  let panOrigin = null;
  let panStart = { x: 0, y: 0 };
  let panOffset = { x: 0, y: 0 };

  function buildModal() {
    const overlay = document.createElement("div");
    overlay.className = "mermaid-modal";
    overlay.innerHTML = `
      <div class="mermaid-modal-backdrop"></div>
      <div class="mermaid-modal-panel">
        <div class="mermaid-modal-toolbar">
          <div class="mermaid-modal-controls">
            <button class="mermaid-ctrl-btn" data-action="zoom-out" title="Zoom out">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                <circle cx="7" cy="7" r="5.5" stroke="currentColor" stroke-width="1.5"/>
                <line x1="4.5" y1="7" x2="9.5" y2="7" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
                <line x1="11" y1="11" x2="14" y2="14" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              </svg>
            </button>
            <span class="mermaid-zoom-label">100%</span>
            <button class="mermaid-ctrl-btn" data-action="zoom-in" title="Zoom in">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                <circle cx="7" cy="7" r="5.5" stroke="currentColor" stroke-width="1.5"/>
                <line x1="7" y1="4.5" x2="7" y2="9.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
                <line x1="4.5" y1="7" x2="9.5" y2="7" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
                <line x1="11" y1="11" x2="14" y2="14" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              </svg>
            </button>
            <button class="mermaid-ctrl-btn mermaid-ctrl-sep" data-action="zoom-reset" title="Reset zoom">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                <path d="M2.5 8a5.5 5.5 0 1 1 1.1 3.3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
                <polyline points="2.5,11.5 2.5,8 6,8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
              </svg>
            </button>
          </div>
          <button class="mermaid-ctrl-btn mermaid-close-btn" data-action="close" title="Close (Esc)">
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
              <line x1="3" y1="3" x2="13" y2="13" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="13" y1="3" x2="3" y2="13" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
            </svg>
          </button>
        </div>
        <div class="mermaid-modal-stage">
          <div class="mermaid-modal-canvas"></div>
        </div>
        <p class="mermaid-modal-hint">Scroll to zoom · Drag to pan</p>
      </div>
    `;
    document.body.appendChild(overlay);
    return overlay;
  }

  function getModal() {
    if (!modal) modal = buildModal();
    return modal;
  }

  function applyTransform() {
    const canvas = modal.querySelector(".mermaid-modal-canvas");
    canvas.style.transform = `translate(${panOffset.x}px, ${panOffset.y}px) scale(${currentScale})`;
    modal.querySelector(".mermaid-zoom-label").textContent =
      Math.round(currentScale * 100) + "%";
  }

  function resetView() {
    currentScale = 1;
    panOffset = { x: 0, y: 0 };
    applyTransform();
  }

  function clampScale(scale) {
    return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, scale));
  }

  function zoomAt(deltaScale, centerX, centerY) {
    const stage = modal.querySelector(".mermaid-modal-stage");
    const rect = stage.getBoundingClientRect();
    const originX = centerX - rect.left - rect.width / 2;
    const originY = centerY - rect.top - rect.height / 2;

    const newScale = clampScale(currentScale + deltaScale);
    const scaleDiff = newScale / currentScale;

    panOffset.x = (panOffset.x - originX) * scaleDiff + originX;
    panOffset.y = (panOffset.y - originY) * scaleDiff + originY;
    currentScale = newScale;
    applyTransform();
  }

  function openModal(svgSource) {
    const overlay = getModal();
    const canvas = overlay.querySelector(".mermaid-modal-canvas");
    canvas.innerHTML = svgSource;

    const svg = canvas.querySelector("svg");
    if (svg) {
      svg.removeAttribute("width");
      svg.removeAttribute("height");
      svg.style.cssText = "display:block;width:100%;height:100%;";
    }

    resetView();
    overlay.classList.add("is-open");
    document.body.style.overflow = "hidden";
  }

  function closeModal() {
    if (!modal) return;
    modal.classList.remove("is-open");
    document.body.style.overflow = "";
  }

  function attachModalEvents(overlay) {
    overlay.querySelector(".mermaid-modal-backdrop").addEventListener("click", closeModal);

    overlay.addEventListener("click", (event) => {
      const action = event.target.closest("[data-action]")?.dataset.action;
      const stage = overlay.querySelector(".mermaid-modal-stage");
      const stageBounds = stage.getBoundingClientRect();

      if (action === "zoom-in") {
        zoomAt(ZOOM_STEP, stageBounds.left + stageBounds.width / 2, stageBounds.top + stageBounds.height / 2);
      } else if (action === "zoom-out") {
        zoomAt(-ZOOM_STEP, stageBounds.left + stageBounds.width / 2, stageBounds.top + stageBounds.height / 2);
      } else if (action === "zoom-reset") {
        resetView();
      } else if (action === "close") {
        closeModal();
      }
    });

    overlay.querySelector(".mermaid-modal-stage").addEventListener("wheel", (event) => {
      event.preventDefault();
      const delta = event.deltaY > 0 ? -ZOOM_STEP : ZOOM_STEP;
      zoomAt(delta, event.clientX, event.clientY);
    }, { passive: false });

    const stage = overlay.querySelector(".mermaid-modal-stage");
    stage.addEventListener("mousedown", (event) => {
      if (event.button !== 0) return;
      panOrigin = { x: event.clientX, y: event.clientY };
      panStart = { ...panOffset };
      stage.style.cursor = "grabbing";
    });
    window.addEventListener("mousemove", (event) => {
      if (!panOrigin) return;
      panOffset.x = panStart.x + (event.clientX - panOrigin.x);
      panOffset.y = panStart.y + (event.clientY - panOrigin.y);
      applyTransform();
    });
    window.addEventListener("mouseup", () => {
      if (!panOrigin) return;
      panOrigin = null;
      stage.style.cursor = "";
    });

    let lastTouchDist = null;
    stage.addEventListener("touchstart", (event) => {
      if (event.touches.length === 2) {
        const dx = event.touches[0].clientX - event.touches[1].clientX;
        const dy = event.touches[0].clientY - event.touches[1].clientY;
        lastTouchDist = Math.hypot(dx, dy);
      }
    }, { passive: true });
    stage.addEventListener("touchmove", (event) => {
      if (event.touches.length !== 2 || !lastTouchDist) return;
      event.preventDefault();
      const dx = event.touches[0].clientX - event.touches[1].clientX;
      const dy = event.touches[0].clientY - event.touches[1].clientY;
      const dist = Math.hypot(dx, dy);
      const midX = (event.touches[0].clientX + event.touches[1].clientX) / 2;
      const midY = (event.touches[0].clientY + event.touches[1].clientY) / 2;
      zoomAt((dist - lastTouchDist) * 0.01, midX, midY);
      lastTouchDist = dist;
    }, { passive: false });
    stage.addEventListener("touchend", () => { lastTouchDist = null; });
  }

  document.addEventListener("keydown", (event) => {
    if (!modal?.classList.contains("is-open")) return;
    if (event.key === "Escape") closeModal();
    if (event.key === "+" || event.key === "=") zoomAt(ZOOM_STEP, window.innerWidth / 2, window.innerHeight / 2);
    if (event.key === "-") zoomAt(-ZOOM_STEP, window.innerWidth / 2, window.innerHeight / 2);
    if (event.key === "0") resetView();
  });

  function decorateDiagram(wrapper) {
    const expandBtn = document.createElement("button");
    expandBtn.className = "mermaid-expand-btn";
    expandBtn.title = "Expand diagram";
    expandBtn.innerHTML = `
      <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
        <polyline points="1,5 1,1 5,1" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        <polyline points="9,1 13,1 13,5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        <polyline points="13,9 13,13 9,13" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        <polyline points="5,13 1,13 1,9" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
      </svg>
    `;
    expandBtn.addEventListener("click", () => {
      const svg = wrapper.querySelector("svg");
      if (svg) openModal(svg.outerHTML);
    });
    wrapper.appendChild(expandBtn);

    const overlay = getModal();
    if (!overlay._eventsAttached) {
      attachModalEvents(overlay);
      overlay._eventsAttached = true;
    }
  }

  window.mermaidViewerDecorate = decorateDiagram;
})();
