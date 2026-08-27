(() => {
  const lastSiteStorageKey = "mdshelf-last-site";

  // The remembered page can disappear when content changes. If we land on a 404
  // whose path is the remembered page, forget it so "Home" cannot loop back here.
  if (document.querySelector(".error-shell")) {
    try {
      if (localStorage.getItem(lastSiteStorageKey) === window.location.pathname) {
        localStorage.removeItem(lastSiteStorageKey);
      }
    } catch {
      /* ignore */
    }
  }

  if (window.location.pathname === "/") {
    try {
      const lastSite = localStorage.getItem(lastSiteStorageKey);
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
    localStorage.setItem(lastSiteStorageKey, window.location.pathname);
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

  const siteMount = shell.dataset.siteMount || "";
  const sidebarList = document.getElementById("sidebar-list");
  const sidebarLabelButtons = Array.from(
    document.querySelectorAll("[data-sidebar-label-mode]"),
  );
  const sidebarSortAxisButtons = Array.from(
    document.querySelectorAll("[data-sidebar-sort-axis]"),
  );
  const sidebarSortDirectionButton = document.getElementById("sidebar-sort-direction");

  const sidebarLabelStorageKey = `mdshelf-sidebar-label:${siteMount}`;
  const sidebarSortStorageKey = `mdshelf-sidebar-sort:${siteMount}`;

  function readSidebarLabelMode() {
    try {
      const stored = localStorage.getItem(sidebarLabelStorageKey);
      if (stored === "filename" || stored === "title") {
        return stored;
      }
    } catch {
      /* ignore */
    }
    return "title";
  }

  function readSidebarSortMode() {
    try {
      const stored = localStorage.getItem(sidebarSortStorageKey);
      if (stored === "tree") {
        return "name-asc";
      }
      if (
        stored === "date-asc" ||
        stored === "date-desc" ||
        stored === "name-asc" ||
        stored === "name-desc"
      ) {
        return stored;
      }
    } catch {
      /* ignore */
    }
    return "name-asc";
  }

  function writeSidebarLabelMode(mode) {
    try {
      localStorage.setItem(sidebarLabelStorageKey, mode);
    } catch {
      /* ignore */
    }
  }

  function writeSidebarSortMode(mode) {
    try {
      localStorage.setItem(sidebarSortStorageKey, mode);
    } catch {
      /* ignore */
    }
  }

  function parseSortMode(mode) {
    const [axis, direction] = mode.split("-");
    return {
      axis: axis === "date" ? "date" : "name",
      direction: direction === "desc" ? "desc" : "asc",
    };
  }

  function composeSortMode(axis, direction) {
    return `${axis}-${direction}`;
  }

  function sortDirectionAriaLabel(axis, direction) {
    if (axis === "date") {
      return direction === "desc" ? "Newest first" : "Oldest first";
    }
    return direction === "asc" ? "A to Z" : "Z to A";
  }

  function sortArrowPointsDown(axis, direction) {
    void axis;
    return direction === "asc";
  }

  function sidebarItemDepth(item) {
    return parseInt(item.dataset.sidebarDepth || "0", 10);
  }

  function sidebarSubtreeEnd(items, startIndex) {
    const depth = sidebarItemDepth(items[startIndex]);
    let endIndex = startIndex + 1;
    while (endIndex < items.length && sidebarItemDepth(items[endIndex]) > depth) {
      endIndex += 1;
    }
    return endIndex;
  }

  function sidebarLabelKey(labelMode) {
    return labelMode === "filename" ? "sidebarFilename" : "sidebarTitle";
  }

  function compareSidebarOrder(leftItem, rightItem) {
    return (
      parseInt(leftItem.dataset.sidebarOrder || "0", 10) -
      parseInt(rightItem.dataset.sidebarOrder || "0", 10)
    );
  }

  function compareAlphabetical(leftItem, rightItem, labelMode) {
    const labelKey = sidebarLabelKey(labelMode);
    const nameCompare = (leftItem.dataset[labelKey] || "")
      .toLowerCase()
      .localeCompare((rightItem.dataset[labelKey] || "").toLowerCase(), undefined, {
        sensitivity: "base",
        numeric: true,
      });
    if (nameCompare !== 0) {
      return nameCompare;
    }
    return compareSidebarOrder(leftItem, rightItem);
  }

  function compareSidebarFiles(leftItem, rightItem, sortMode, labelMode) {
    if (sortMode.startsWith("date-")) {
      const leftDate = parseInt(leftItem.dataset.sidebarDate || "0", 10);
      const rightDate = parseInt(rightItem.dataset.sidebarDate || "0", 10);
      const dateCompare = leftDate - rightDate;
      if (dateCompare !== 0) {
        return sortMode === "date-desc" ? -dateCompare : dateCompare;
      }
    } else {
      const labelKey = sidebarLabelKey(labelMode);
      const nameCompare = (leftItem.dataset[labelKey] || "")
        .toLowerCase()
        .localeCompare((rightItem.dataset[labelKey] || "").toLowerCase(), undefined, {
          sensitivity: "base",
          numeric: true,
        });
      if (nameCompare !== 0) {
        return sortMode === "name-desc" ? -nameCompare : nameCompare;
      }
    }
    return compareSidebarOrder(leftItem, rightItem);
  }

  function isSidebarFolderRange(items, range) {
    return range.end > range.start + 1;
  }

  function sortSidebarSiblings(items, parentStartIndex, parentDepth, sortMode, labelMode) {
    let cursor = parentStartIndex + 1;
    const childRanges = [];
    while (cursor < items.length && sidebarItemDepth(items[cursor]) > parentDepth) {
      if (sidebarItemDepth(items[cursor]) !== parentDepth + 1) {
        cursor += 1;
        continue;
      }
      const rangeStart = cursor;
      const rangeEnd = sidebarSubtreeEnd(items, rangeStart);
      childRanges.push({ start: rangeStart, end: rangeEnd });
      cursor = rangeEnd;
    }
    if (childRanges.length === 0) {
      return;
    }
    if (childRanges.length >= 1) {
      const folderRanges = [];
      const fileRanges = [];
      childRanges.forEach((range) => {
        if (isSidebarFolderRange(items, range)) {
          folderRanges.push(range);
        } else {
          fileRanges.push(range);
        }
      });
      folderRanges.sort((leftRange, rightRange) =>
        compareAlphabetical(items[leftRange.start], items[rightRange.start], labelMode),
      );
      fileRanges.sort((leftRange, rightRange) =>
        compareSidebarFiles(items[leftRange.start], items[rightRange.start], sortMode, labelMode),
      );
      const orderedRanges = [...folderRanges, ...fileRanges];
      const sortedChunks = orderedRanges.flatMap((range) =>
        items.slice(range.start, range.end),
      );
      const insertAt = childRanges[0].start;
      const removeCount = childRanges[childRanges.length - 1].end - insertAt;
      items.splice(insertAt, removeCount, ...sortedChunks);
    }
    let recurseAt = parentStartIndex + 1;
    while (recurseAt < items.length && sidebarItemDepth(items[recurseAt]) > parentDepth) {
      if (sidebarItemDepth(items[recurseAt]) === parentDepth + 1) {
        const subtreeEnd = sidebarSubtreeEnd(items, recurseAt);
        sortSidebarSiblings(items, recurseAt, parentDepth + 1, sortMode, labelMode);
        recurseAt = subtreeEnd;
      } else {
        recurseAt += 1;
      }
    }
  }

  function reorderSidebarList(sortMode, labelMode) {
    if (!sidebarList) {
      return;
    }
    const items = Array.from(sidebarList.querySelectorAll(".sidebar-item"));
    if (items.length < 2) {
      return;
    }
    sortSidebarSiblings(items, -1, -1, sortMode, labelMode);
    sidebarList.replaceChildren(...items);
  }

  function applySidebarLabels(labelMode) {
    document.querySelectorAll(".sidebar-item").forEach((item) => {
      const label =
        labelMode === "filename"
          ? item.dataset.sidebarFilename || item.dataset.sidebarTitle || ""
          : item.dataset.sidebarTitle || "";
      const link = item.querySelector(".sidebar-link");
      if (link) {
        link.textContent = label;
      }
      const toggle = item.querySelector(".sidebar-folder-toggle");
      if (toggle) {
        const labelSpan = toggle.querySelector("span");
        if (labelSpan) {
          labelSpan.textContent = label;
        }
      }
    });
  }

  const sidebarOptionsButton = document.getElementById("sidebar-options-button");
  const sidebarOptionsMenu = document.getElementById("sidebar-options-menu");
  const sidebarOptionsPopoverWidth = 248;
  const sidebarOptionsPopoverGap = 10;
  let sidebarOptionsPopoverPortaled = false;

  function ensureSidebarOptionsPopoverPortal() {
    if (!sidebarOptionsMenu || sidebarOptionsPopoverPortaled) {
      return;
    }
    document.body.appendChild(sidebarOptionsMenu);
    sidebarOptionsPopoverPortaled = true;
  }

  function positionSidebarOptionsPopover() {
    if (!sidebarOptionsButton || !sidebarOptionsMenu) {
      return;
    }
    const anchor = sidebarOptionsButton.getBoundingClientRect();
    const menuHeight = sidebarOptionsMenu.offsetHeight;
    const headerOffset =
      parseInt(getComputedStyle(document.documentElement).getPropertyValue("--header-h"), 10) || 60;
    const edge = 12;

    let top = anchor.bottom + sidebarOptionsPopoverGap;
    let left = anchor.right - sidebarOptionsPopoverWidth;

    if (left < edge) {
      left = edge;
    }
    if (left + sidebarOptionsPopoverWidth > window.innerWidth - edge) {
      left = window.innerWidth - sidebarOptionsPopoverWidth - edge;
    }
    if (top + menuHeight > window.innerHeight - edge) {
      top = anchor.top - menuHeight - sidebarOptionsPopoverGap;
    }
    top = Math.max(top, headerOffset + edge);

    sidebarOptionsMenu.style.top = `${Math.round(top)}px`;
    sidebarOptionsMenu.style.left = `${Math.round(left)}px`;
  }

  function setSidebarOptionsOpen(open) {
    if (!sidebarOptionsButton || !sidebarOptionsMenu) {
      return;
    }
    if (open) {
      ensureSidebarOptionsPopoverPortal();
      sidebarOptionsMenu.hidden = false;
      positionSidebarOptionsPopover();
    } else {
      sidebarOptionsMenu.hidden = true;
    }
    sidebarOptionsButton.setAttribute("aria-expanded", open ? "true" : "false");
  }

  function syncSidebarToolbar(labelMode, sortMode) {
    sidebarLabelButtons.forEach((button) => {
      const active = button.dataset.sidebarLabelMode === labelMode;
      button.classList.toggle("is-active", active);
      button.setAttribute("aria-pressed", active ? "true" : "false");
    });
    const { axis, direction } = parseSortMode(sortMode);
    sidebarSortAxisButtons.forEach((button) => {
      const active = button.dataset.sidebarSortAxis === axis;
      button.classList.toggle("is-active", active);
      button.setAttribute("aria-pressed", active ? "true" : "false");
    });
    if (sidebarSortDirectionButton) {
      sidebarSortDirectionButton.classList.toggle(
        "sort-arrow-up",
        !sortArrowPointsDown(axis, direction),
      );
      sidebarSortDirectionButton.setAttribute(
        "aria-label",
        sortDirectionAriaLabel(axis, direction),
      );
    }
  }

  function applySidebarSortMode(nextSortMode) {
    if (!nextSortMode || nextSortMode === sidebarSortMode) {
      return;
    }
    sidebarSortMode = nextSortMode;
    writeSidebarSortMode(nextSortMode);
    syncSidebarToolbar(sidebarLabelMode, sidebarSortMode);
    reorderSidebarList(sidebarSortMode, sidebarLabelMode);
    initSidebarFolders();
  }

  let sidebarLabelMode = readSidebarLabelMode();
  let sidebarSortMode = readSidebarSortMode();
  syncSidebarToolbar(sidebarLabelMode, sidebarSortMode);
  reorderSidebarList(sidebarSortMode, sidebarLabelMode);
  applySidebarLabels(sidebarLabelMode);

  sidebarLabelButtons.forEach((button) => {
    button.addEventListener("click", () => {
      const nextMode = button.dataset.sidebarLabelMode;
      if (!nextMode || nextMode === sidebarLabelMode) {
        return;
      }
      sidebarLabelMode = nextMode;
      writeSidebarLabelMode(nextMode);
      syncSidebarToolbar(sidebarLabelMode, sidebarSortMode);
      applySidebarLabels(sidebarLabelMode);
      reorderSidebarList(sidebarSortMode, sidebarLabelMode);
      initSidebarFolders();
    });
  });

  sidebarSortAxisButtons.forEach((button) => {
    button.addEventListener("click", () => {
      const nextAxis = button.dataset.sidebarSortAxis;
      if (!nextAxis) {
        return;
      }
      const { direction } = parseSortMode(sidebarSortMode);
      applySidebarSortMode(composeSortMode(nextAxis, direction));
    });
  });

  sidebarSortDirectionButton?.addEventListener("click", () => {
    const { axis, direction } = parseSortMode(sidebarSortMode);
    const nextDirection = direction === "asc" ? "desc" : "asc";
    applySidebarSortMode(composeSortMode(axis, nextDirection));
  });

  if (sidebarOptionsButton && sidebarOptionsMenu) {
    sidebarOptionsButton.addEventListener("click", (event) => {
      event.stopPropagation();
      setSidebarOptionsOpen(sidebarOptionsMenu.hidden);
    });

    document.addEventListener("click", (event) => {
      if (
        sidebarOptionsMenu.hidden ||
        sidebarOptionsButton.contains(event.target) ||
        sidebarOptionsMenu.contains(event.target)
      ) {
        return;
      }
      setSidebarOptionsOpen(false);
    });

    window.addEventListener("resize", () => {
      if (!sidebarOptionsMenu.hidden) {
        positionSidebarOptionsPopover();
      }
    });

    window.addEventListener(
      "scroll",
      () => {
        if (!sidebarOptionsMenu.hidden) {
          positionSidebarOptionsPopover();
        }
      },
      true,
    );
  }

  const chevronSvg = `<svg class="sidebar-chevron" xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9"/></svg>`;
  const allSidebarItems = () => Array.from(document.querySelectorAll(".sidebar-item"));

  function initSidebarFolders() {
    document.querySelectorAll(".sidebar-folder-toggle").forEach((toggle) => {
      const item = toggle.closest(".sidebar-item");
      if (!item) {
        return;
      }
      const sectionSpan = document.createElement("span");
      sectionSpan.className = "sidebar-section";
      sectionSpan.textContent = toggle.querySelector("span")?.textContent || "";
      toggle.replaceWith(sectionSpan);
    });

    const items = allSidebarItems();
    const folders = [];
    items.forEach((item, index) => {
      const depth = sidebarItemDepth(item);
      const descendants = [];
      for (let childIndex = index + 1; childIndex < items.length; childIndex += 1) {
        const childDepth = sidebarItemDepth(items[childIndex]);
        if (childDepth <= depth) {
          break;
        }
        descendants.push(items[childIndex]);
      }
      if (descendants.length === 0) {
        return;
      }

      const sectionSpan = item.querySelector(".sidebar-section");
      const linkAnchor = item.querySelector(".sidebar-link");
      if (!sectionSpan && !linkAnchor) {
        return;
      }

      const folderKey = item.dataset.sidebarFolderKey || `depth-${depth}-${index}`;
      const storageKey = `mdshelf-folder:${siteMount}:${folderKey}`;
      const hasActive = descendants.some((child) => child.querySelector(".is-active"));

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

      const label =
        sidebarLabelMode === "filename"
          ? item.dataset.sidebarFilename || item.dataset.sidebarTitle || ""
          : item.dataset.sidebarTitle || "";

      folders.push({
        item,
        index,
        depth,
        descendants,
        sectionSpan,
        linkAnchor,
        label,
        storageKey,
        isExpanded,
      });
    });

    items.forEach((item) => {
      item.dataset.hiddenBy = "0";
    });
    folders.forEach(({ isExpanded, descendants }) => {
      if (!isExpanded) {
        descendants.forEach((descendant) => {
          descendant.dataset.hiddenBy = String(parseInt(descendant.dataset.hiddenBy, 10) + 1);
        });
      }
    });
    items.forEach((item) => {
      item.classList.toggle("sidebar-folder-hidden", parseInt(item.dataset.hiddenBy, 10) > 0);
    });

    function applyToggle(toggleEl, folder) {
      const next = toggleEl.getAttribute("aria-expanded") !== "true";
      toggleEl.setAttribute("aria-expanded", next ? "true" : "false");
      folder.isExpanded = next;
      folder.descendants.forEach((descendant) => {
        const count = parseInt(descendant.dataset.hiddenBy, 10) + (next ? -1 : 1);
        descendant.dataset.hiddenBy = String(Math.max(0, count));
        descendant.classList.toggle(
          "sidebar-folder-hidden",
          parseInt(descendant.dataset.hiddenBy, 10) > 0,
        );
      });
      try {
        localStorage.setItem(folder.storageKey, String(next));
      } catch {
        /* ignore */
      }
    }

    folders.forEach((folder) => {
      const { sectionSpan, isExpanded, label } = folder;
      if (!sectionSpan) {
        return;
      }
      const toggle = document.createElement("button");
      toggle.className = "sidebar-folder-toggle";
      toggle.setAttribute("aria-expanded", isExpanded ? "true" : "false");
      const labelSpan = document.createElement("span");
      labelSpan.textContent = label;
      toggle.append(labelSpan);
      toggle.insertAdjacentHTML("beforeend", chevronSvg);
      sectionSpan.replaceWith(toggle);
      toggle.addEventListener("click", () => applyToggle(toggle, folder));
    });
  }

  initSidebarFolders();
  documentElement.classList.remove("sidebar-loading");

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
      setSidebarOptionsOpen(false);
      setPageActionsOpen(false);
    }
  });

  /* ---------------------------------------------------------------- page actions */

  const pageActionsButton = document.getElementById("page-actions-button");
  const pageActionsMenu = document.getElementById("page-actions-menu");
  const pageActionsStatus = document.getElementById("page-actions-status");
  const pageActionsFallback = document.getElementById("page-actions-fallback");
  const pageActionsFallbackText = document.getElementById("page-actions-fallback-text");
  const pageCopyButton = document.getElementById("page-copy-md");
  const pageDownloadButton = document.getElementById("page-download-md");
  const pageSourceEl = document.getElementById("page-source");
  const pageActionsPopoverWidth = 248;
  const pageActionsPopoverGap = 10;
  let pageActionsPortaled = false;
  let pageActionsCloseTimer = 0;

  // The inverse of the server-side escaper: on a backslash, take the next character
  // literally. `textContent` hands back the script body unparsed, so this is the only
  // transformation between the file on disk and the clipboard.
  function decodePageSource(text) {
    let out = "";
    for (let i = 0; i < text.length; i += 1) {
      if (text[i] === "\\" && i + 1 < text.length) {
        i += 1;
      }
      out += text[i];
    }
    return out;
  }

  function readPageSource() {
    return pageSourceEl ? decodePageSource(pageSourceEl.textContent) : "";
  }

  function ensurePageActionsPortal() {
    if (!pageActionsMenu || pageActionsPortaled) {
      return;
    }
    document.body.appendChild(pageActionsMenu);
    pageActionsPortaled = true;
  }

  function positionPageActionsPopover() {
    if (!pageActionsButton || !pageActionsMenu) {
      return;
    }
    const anchor = pageActionsButton.getBoundingClientRect();
    const menuHeight = pageActionsMenu.offsetHeight;
    const headerOffset =
      parseInt(getComputedStyle(document.documentElement).getPropertyValue("--header-h"), 10) || 60;
    const edge = 12;

    let top = anchor.bottom + pageActionsPopoverGap;
    let left = anchor.right - pageActionsPopoverWidth;

    if (left < edge) {
      left = edge;
    }
    if (left + pageActionsPopoverWidth > window.innerWidth - edge) {
      left = window.innerWidth - pageActionsPopoverWidth - edge;
    }
    if (top + menuHeight > window.innerHeight - edge) {
      top = anchor.top - menuHeight - pageActionsPopoverGap;
    }
    top = Math.max(top, headerOffset + edge);

    pageActionsMenu.style.top = `${Math.round(top)}px`;
    pageActionsMenu.style.left = `${Math.round(left)}px`;
  }

  function resetPageActionsFeedback() {
    window.clearTimeout(pageActionsCloseTimer);
    if (pageActionsStatus) {
      pageActionsStatus.textContent = "";
    }
    if (pageActionsFallback) {
      pageActionsFallback.hidden = true;
    }
    if (pageCopyButton) {
      pageCopyButton.classList.remove("is-done");
      const label = pageCopyButton.querySelector(".page-actions-label");
      if (label) {
        label.textContent = "Copy as Markdown";
      }
    }
  }

  function setPageActionsOpen(open) {
    if (!pageActionsButton || !pageActionsMenu) {
      return;
    }
    if (open) {
      resetPageActionsFeedback();
      ensurePageActionsPortal();
      pageActionsMenu.hidden = false;
      positionPageActionsPopover();
    } else {
      resetPageActionsFeedback();
      pageActionsMenu.hidden = true;
    }
    pageActionsButton.setAttribute("aria-expanded", open ? "true" : "false");
  }

  function confirmCopied() {
    if (pageActionsStatus) {
      pageActionsStatus.textContent = "Copied";
    }
    if (pageCopyButton) {
      pageCopyButton.classList.add("is-done");
      const label = pageCopyButton.querySelector(".page-actions-label");
      if (label) {
        label.textContent = "Copied";
      }
    }
    // Long enough to read, short enough not to sit on top of the page being read.
    pageActionsCloseTimer = window.setTimeout(() => setPageActionsOpen(false), 1200);
  }

  // Tier 3: neither clipboard API worked, so hand the text over and let the reader take
  // it. Nothing else in this feature has a floor that does not depend on a browser API.
  function offerManualCopy(source) {
    if (!pageActionsFallback || !pageActionsFallbackText) {
      return;
    }
    pageActionsFallbackText.value = source;
    pageActionsFallback.hidden = false;
    positionPageActionsPopover();
    pageActionsFallbackText.focus();
    pageActionsFallbackText.select();
  }

  // Tier 2: `navigator.clipboard` is undefined on a plain-HTTP non-loopback origin,
  // which is exactly how mdshelf is read over Tailscale. `execCommand` is deprecated
  // but still the only thing that works there.
  function copyViaExecCommand(source) {
    const scratch = document.createElement("textarea");
    scratch.value = source;
    scratch.setAttribute("readonly", "");
    scratch.style.position = "fixed";
    scratch.style.top = "-1000px";
    scratch.style.opacity = "0";
    document.body.appendChild(scratch);
    scratch.select();
    let copied = false;
    try {
      copied = document.execCommand("copy");
    } catch (error) {
      copied = false;
    }
    document.body.removeChild(scratch);
    return copied;
  }

  async function copyPageSource() {
    const source = readPageSource();
    if (!source) {
      return;
    }
    // Read synchronously above so tier 1 still runs inside this click's own user
    // gesture: a fetch first would lose the gesture and be rejected on iOS.
    if (navigator.clipboard && window.isSecureContext) {
      try {
        await navigator.clipboard.writeText(source);
        confirmCopied();
        return;
      } catch (error) {
        // Permission denied or a non-secure context that still exposes the object.
      }
    }
    if (copyViaExecCommand(source)) {
      confirmCopied();
      return;
    }
    offerManualCopy(source);
  }

  // iOS and iPadOS Safari accept `a[download]` but frequently open a Blob URL in a
  // viewer instead of saving it, so those get the server route, which sets the filename
  // and MIME type itself. `maxTouchPoints` catches an iPad reporting a desktop UA.
  function isIosWebkit() {
    const ua = navigator.userAgent || "";
    return (
      /iPad|iPhone|iPod/.test(navigator.platform || "") ||
      /iP(ad|hone|od)/.test(ua) ||
      (navigator.maxTouchPoints > 1 && /Mac/.test(ua))
    );
  }

  function downloadViaBlob(source, filename) {
    const url = URL.createObjectURL(new Blob([source], { type: "text/markdown" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = filename;
    document.body.appendChild(link);
    link.click();
    document.body.removeChild(link);
    window.setTimeout(() => URL.revokeObjectURL(url), 0);
  }

  async function downloadPageSource() {
    const source = readPageSource();
    if (!source || !pageSourceEl) {
      return;
    }
    const filename = pageSourceEl.dataset.filename || "page.md";
    const routeUrl = pageSourceEl.dataset.mdUrl;

    if (routeUrl && isIosWebkit()) {
      try {
        const probe = await fetch(routeUrl, { method: "HEAD" });
        if (probe.ok) {
          window.location.href = routeUrl;
          setPageActionsOpen(false);
          return;
        }
      } catch (error) {
        // No server behind this page — an exported static bundle. Fall through.
      }
    }
    downloadViaBlob(source, filename);
    setPageActionsOpen(false);
  }

  if (pageActionsButton && pageActionsMenu) {
    pageActionsButton.addEventListener("click", (event) => {
      event.stopPropagation();
      setPageActionsOpen(pageActionsMenu.hidden);
    });

    document.addEventListener("click", (event) => {
      if (
        pageActionsMenu.hidden ||
        pageActionsButton.contains(event.target) ||
        pageActionsMenu.contains(event.target)
      ) {
        return;
      }
      setPageActionsOpen(false);
    });

    window.addEventListener("resize", () => {
      if (!pageActionsMenu.hidden) {
        positionPageActionsPopover();
      }
    });

    window.addEventListener(
      "scroll",
      () => {
        if (!pageActionsMenu.hidden) {
          positionPageActionsPopover();
        }
      },
      true,
    );

    pageCopyButton?.addEventListener("click", () => {
      copyPageSource();
    });
    pageDownloadButton?.addEventListener("click", () => {
      downloadPageSource();
    });
  }

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

  function headingFragmentId(heading) {
    return heading.querySelector("a.anchor")?.id || heading.id || "";
  }

  // Add anchor links to headings
  document.querySelectorAll(".prose :is(h2, h3, h4, h5, h6)").forEach(heading => {
    const fragmentId = headingFragmentId(heading);
    if (!fragmentId) {
      return;
    }
    const anchor = document.createElement("a");
    anchor.className = "heading-anchor";
    anchor.href = `#${fragmentId}`;
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

  const headings = Array.from(document.querySelectorAll(".prose h2, .prose h3")).filter(
    (heading) => headingFragmentId(heading),
  );
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
