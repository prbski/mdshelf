(() => {
  if (window.location.pathname === "/") {
    try {
      const lastSite = localStorage.getItem("mdshelf-last-site");
      if (lastSite && lastSite !== "/") {
        window.location.replace(lastSite);
        return;
      }
    } catch {
      /* ignore */
    }
  }

  const documentElement = document.documentElement;
  const storageKey = "mdshelf-theme";

  function readStoredTheme() {
    try {
      return localStorage.getItem(storageKey);
    } catch {
      return null;
    }
  }

  function writeStoredTheme(value) {
    try {
      localStorage.setItem(storageKey, value);
    } catch {
      /* ignore */
    }
  }

  function applyTheme(mode) {
    if (mode === "dark") {
      documentElement.setAttribute("data-theme", "dark");
    } else if (mode === "light") {
      documentElement.setAttribute("data-theme", "light");
    } else {
      documentElement.removeAttribute("data-theme");
    }
  }

  document.querySelectorAll("#theme-toggle").forEach((button) => {
    button.addEventListener("click", () => {
      const current = documentElement.getAttribute("data-theme");
      let next = "light";
      if (current === "light") {
        next = "dark";
      } else if (current === "dark") {
        next = "";
      } else {
        const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
        next = prefersDark ? "light" : "dark";
      }
      if (next === "") {
        documentElement.removeAttribute("data-theme");
        writeStoredTheme("");
      } else {
        applyTheme(next);
        writeStoredTheme(next);
      }
    });
  });

  const shell = document.querySelector(".doc-shell");
  if (!shell) {
    return;
  }

  try {
    localStorage.setItem("mdshelf-last-site", window.location.pathname);
  } catch {
    /* ignore */
  }

  const sidebarPanel = document.getElementById("sidebar-panel");
  const sidebarBackdrop = document.getElementById("sidebar-backdrop");
  const sidebarOpen = document.getElementById("sidebar-open");
  const tocPanel = document.getElementById("toc-panel");
  const tocBackdrop = document.getElementById("toc-backdrop");
  const tocOpen = document.getElementById("toc-open");

  if (tocOpen && !documentElement.classList.contains("toc-hidden")) {
    tocOpen.classList.add("is-active");
  }

  // Make sidebar folder sections collapsible
  const siteMount = shell.dataset.siteMount || "";
  const allSidebarItems = Array.from(document.querySelectorAll(".sidebar-item"));
  const chevronSvg = `<svg class="sidebar-chevron" xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9"/></svg>`;

  // Collect all folder items: either a section span (no URL) or a link that has descendants
  const folders = [];
  allSidebarItems.forEach((item, index) => {
    const depth = parseInt(item.dataset.sidebarDepth || "0", 10);
    const descendants = [];
    for (let i = index + 1; i < allSidebarItems.length; i++) {
      const childDepth = parseInt(allSidebarItems[i].dataset.sidebarDepth || "0", 10);
      if (childDepth <= depth) break;
      descendants.push(allSidebarItems[i]);
    }
    if (descendants.length === 0) return;

    const sectionSpan = item.querySelector(".sidebar-section");
    const linkAnchor = item.querySelector(".sidebar-link");
    if (!sectionSpan && !linkAnchor) return;

    const folderTitle = (sectionSpan || linkAnchor).textContent.trim();
    const storageKey = `mdshelf-folder:${siteMount}:${depth}:${folderTitle}`;
    const hasActive = descendants.some(child => child.querySelector(".is-active"));

    let isExpanded;
    try {
      const stored = localStorage.getItem(storageKey);
      if (stored === null) {
        isExpanded = hasActive;
        localStorage.setItem(storageKey, String(isExpanded));
      } else {
        isExpanded = stored === "true";
      }
    } catch {
      isExpanded = hasActive;
    }

    folders.push({ item, index, depth, descendants, sectionSpan, linkAnchor, folderTitle, storageKey, isExpanded });
  });

  // Initialize hidden-by counters so nested collapse works correctly.
  // Each item tracks how many collapsed ancestor folders are hiding it.
  // An item is visible only when its counter reaches 0.
  allSidebarItems.forEach(item => { item.dataset.hiddenBy = "0"; });
  folders.forEach(({ isExpanded, descendants }) => {
    if (!isExpanded) {
      descendants.forEach(desc => {
        desc.dataset.hiddenBy = String(parseInt(desc.dataset.hiddenBy) + 1);
      });
    }
  });
  allSidebarItems.forEach(item => {
    item.classList.toggle("sidebar-folder-hidden", parseInt(item.dataset.hiddenBy) > 0);
  });
  documentElement.classList.remove("sidebar-loading");

  function applyToggle(toggleEl, folder) {
    const next = toggleEl.getAttribute("aria-expanded") !== "true";
    toggleEl.setAttribute("aria-expanded", next ? "true" : "false");
    folder.isExpanded = next;
    folder.descendants.forEach(desc => {
      const count = parseInt(desc.dataset.hiddenBy) + (next ? -1 : 1);
      desc.dataset.hiddenBy = String(Math.max(0, count));
      desc.classList.toggle("sidebar-folder-hidden", parseInt(desc.dataset.hiddenBy) > 0);
    });
    try { localStorage.setItem(folder.storageKey, String(next)); } catch { /* ignore */ }
  }

  // Build toggle UI for each folder
  folders.forEach(folder => {
    const { sectionSpan, isExpanded, folderTitle } = folder;
    if (!sectionSpan) return;
    const toggle = document.createElement("button");
    toggle.className = "sidebar-folder-toggle";
    toggle.setAttribute("aria-expanded", isExpanded ? "true" : "false");
    toggle.innerHTML = `<span>${folderTitle}</span>${chevronSvg}`;
    sectionSpan.replaceWith(toggle);
    toggle.addEventListener("click", () => applyToggle(toggle, folder));
  });

  // Scroll the active sidebar item to the top of the sidebar on page load
  const activeLink = document.querySelector(".sidebar-link.is-active");
  const sidebarInner = document.querySelector(".doc-sidebar-inner");
  if (activeLink && sidebarInner) {
    const linkRect = activeLink.getBoundingClientRect();
    const containerRect = sidebarInner.getBoundingClientRect();
    sidebarInner.scrollTop = sidebarInner.scrollTop + (linkRect.top - containerRect.top) - 16;
  }

  function setSidebarOpen(open) {
    shell.classList.toggle("sidebar-open", open);
    if (sidebarBackdrop) {
      sidebarBackdrop.hidden = !open;
    }
    if (sidebarOpen) {
      sidebarOpen.setAttribute("aria-expanded", open ? "true" : "false");
    }
    if (open) {
      document.body.style.overflow = "hidden";
    } else {
      document.body.style.overflow = "";
    }
  }

  function setTocOpen(open) {
    shell.classList.toggle("toc-open", open);
    if (tocBackdrop) {
      tocBackdrop.hidden = !open;
    }
    if (tocOpen) {
      tocOpen.setAttribute("aria-expanded", open ? "true" : "false");
    }
    if (open) {
      document.body.style.overflow = "hidden";
    } else {
      document.body.style.overflow = "";
    }
  }

  sidebarOpen?.addEventListener("click", () => {
    const open = !shell.classList.contains("sidebar-open");
    setSidebarOpen(open);
    if (open) {
      setTocOpen(false);
    }
  });

  tocOpen?.addEventListener("click", () => {
    if (window.innerWidth >= 1280) {
      const isHidden = documentElement.classList.contains("toc-hidden");
      if (isHidden) {
        documentElement.classList.remove("toc-hidden");
        localStorage.setItem("mdshelf-toc-hidden", "false");
        tocOpen.classList.add("is-active");
      } else {
        documentElement.classList.add("toc-hidden");
        localStorage.setItem("mdshelf-toc-hidden", "true");
        tocOpen.classList.remove("is-active");
      }
    } else {
      const open = !shell.classList.contains("toc-open");
      setTocOpen(open);
      if (open) {
        setSidebarOpen(false);
      }
    }
  });

  sidebarBackdrop?.addEventListener("click", () => setSidebarOpen(false));
  tocBackdrop?.addEventListener("click", () => setTocOpen(false));

  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      setSidebarOpen(false);
      setTocOpen(false);
      if (popoverMenu && popoverButton) {
        popoverMenu.hidden = true;
        popoverButton.setAttribute("aria-expanded", "false");
      }
    }
  });

  const popoverButton = document.getElementById("site-popover-button");
  const popoverMenu = document.getElementById("site-popover-menu");

  if (popoverButton && popoverMenu) {
    popoverButton.addEventListener("click", (e) => {
      e.stopPropagation();
      const isHidden = popoverMenu.hidden;
      popoverMenu.hidden = !isHidden;
      popoverButton.setAttribute("aria-expanded", isHidden ? "true" : "false");
    });

    document.addEventListener("click", (e) => {
      if (!popoverMenu.hidden && !popoverMenu.contains(e.target) && !popoverButton.contains(e.target)) {
        popoverMenu.hidden = true;
        popoverButton.setAttribute("aria-expanded", "false");
      }
    });
  }

  const resizeHandle = document.getElementById("sidebar-resize-handle");
  if (resizeHandle && window.innerWidth >= 1100) {
    let isResizing = false;
    let startX = 0;
    let startWidth = 0;
    
    const onPointerDown = (e) => {
      isResizing = true;
      startX = e.clientX || (e.touches && e.touches[0].clientX);
      startWidth = sidebarPanel.offsetWidth;
      resizeHandle.classList.add("is-resizing");
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      e.preventDefault();
    };

    const onPointerMove = (e) => {
      if (!isResizing) return;
      const clientX = e.clientX || (e.touches && e.touches[0].clientX);
      const deltaX = clientX - startX;
      let newWidth = startWidth + deltaX;
      
      // Constraints
      newWidth = Math.max(220, Math.min(newWidth, 600));
      documentElement.style.setProperty("--sidebar-w", `${newWidth}px`);
    };

    const onPointerUp = () => {
      if (!isResizing) return;
      isResizing = false;
      resizeHandle.classList.remove("is-resizing");
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      
      // Persist user preference
      const finalWidth = documentElement.style.getPropertyValue("--sidebar-w");
      if (finalWidth) {
        localStorage.setItem("mdshelf-sidebar-w", finalWidth);
      }
    };

    resizeHandle.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("pointermove", onPointerMove);
    document.addEventListener("pointerup", onPointerUp);
  }

  // Wrap prose tables in a scrollable container
  document.querySelectorAll(".prose table").forEach(table => {
    const wrapper = document.createElement("div");
    wrapper.className = "table-wrapper";
    table.parentNode.insertBefore(wrapper, table);
    wrapper.appendChild(table);
  });

  // Add anchor links to headings
  document.querySelectorAll(".prose :is(h2, h3, h4, h5, h6)[id]").forEach(heading => {
    const anchor = document.createElement("a");
    anchor.className = "heading-anchor";
    anchor.href = `#${heading.id}`;
    anchor.setAttribute("aria-hidden", "true");
    anchor.setAttribute("tabindex", "-1");
    anchor.innerHTML = `<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></svg>`;
    heading.appendChild(anchor);
  });

  // Add language labels to fenced code blocks
  document.querySelectorAll(".prose pre > code[class]").forEach(code => {
    const match = Array.from(code.classList).find(c => c.startsWith("language-"));
    if (!match) return;
    const lang = match.replace("language-", "");
    if (!lang || lang === "text" || lang === "plain") return;
    const label = document.createElement("span");
    label.className = "code-lang";
    label.textContent = lang;
    code.parentElement.appendChild(label);
  });

  const headings = Array.from(document.querySelectorAll(".prose h2[id], .prose h3[id]"));
  const tocLinks = Array.from(document.querySelectorAll(".toc-item a"));
  if (headings.length && tocLinks.length && "IntersectionObserver" in window) {
    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          const id = entry.target.getAttribute("id");
          if (!id) {
            return;
          }
          const link = tocLinks.find((candidate) => candidate.getAttribute("href") === `#${id}`);
          if (!link) {
            return;
          }
          if (entry.isIntersecting) {
            tocLinks.forEach((item) => item.classList.remove("is-active"));
            link.classList.add("is-active");
          }
        });
      },
      { rootMargin: "-45% 0px -45% 0px", threshold: 0.01 },
    );
    headings.forEach((heading) => observer.observe(heading));
  }
})();
