// Search dialog: `/` or Cmd/Ctrl+K opens it, results update as you type, arrow keys
// move through them, Enter opens the selected one (or the full results page).
//
// Results are built with DOM APIs and textContent only: every string here came from a
// page body, and none of it may be interpreted as markup.
(() => {
  const dialog = document.getElementById("search-dialog");
  const opener = document.getElementById("search-open");
  if (!dialog || !opener || typeof dialog.showModal !== "function") {
    return;
  }
  const input = document.getElementById("search-input");
  const list = document.getElementById("search-results");
  const hint = document.getElementById("search-hint");
  const status = document.getElementById("search-status");
  const allLink = document.getElementById("search-all");
  const closer = document.getElementById("search-close");
  const form = dialog.querySelector("form");

  const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  let active = -1;
  let timer = 0;
  let controller = null;
  let lastQuery = "";

  function open() {
    if (dialog.open) {
      return;
    }
    dialog.showModal();
    input.select();
  }

  function close() {
    if (dialog.open) {
      dialog.close();
    }
  }

  function options() {
    return Array.from(list.querySelectorAll("[role='option']"));
  }

  function setActive(index) {
    const items = options();
    if (!items.length) {
      active = -1;
      input.removeAttribute("aria-activedescendant");
      return;
    }
    active = (index + items.length) % items.length;
    items.forEach((item, i) => item.setAttribute("aria-selected", i === active ? "true" : "false"));
    input.setAttribute("aria-activedescendant", items[active].id);
    items[active].scrollIntoView({ block: "nearest" });
  }

  function setStatus(text) {
    status.textContent = text;
    status.hidden = !text;
  }

  function render(query, payload) {
    list.replaceChildren();
    active = -1;
    allLink.href = `/__search?q=${encodeURIComponent(query)}`;
    hint.hidden = Boolean(query);
    if (!query) {
      setStatus("");
      input.setAttribute("aria-expanded", "false");
      return;
    }
    if (payload.unavailable) {
      setStatus("Search is unavailable right now. Try again in a minute.");
      input.setAttribute("aria-expanded", "false");
      return;
    }
    if (!payload.results.length) {
      setStatus(`No pages match “${query}”.`);
      input.setAttribute("aria-expanded", "false");
      return;
    }
    setStatus("");
    payload.results.forEach((result, index) => {
      const item = document.createElement("li");
      item.className = "search-result";
      item.id = `search-option-${index}`;
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", "false");
      item.style.setProperty("--accent", result.color);

      const link = document.createElement("a");
      link.className = "search-result-link";
      link.href = result.url;
      link.tabIndex = -1;

      const title = document.createElement("span");
      title.className = "search-result-title";
      title.textContent = result.title;

      const meta = document.createElement("span");
      meta.className = "search-result-meta";
      const site = document.createElement("span");
      site.className = "search-result-site";
      site.textContent = result.site;
      meta.append(site);
      if (result.section) {
        meta.append(` · ${result.section}`);
      }

      link.append(title, meta);
      if (result.snippet.length) {
        const snippet = document.createElement("span");
        snippet.className = "search-result-snippet";
        result.snippet.forEach((segment) => {
          if (segment.match) {
            const mark = document.createElement("mark");
            mark.textContent = segment.text;
            snippet.append(mark);
          } else {
            snippet.append(segment.text);
          }
        });
        link.append(snippet);
      }
      item.append(link);
      item.addEventListener("mousemove", () => {
        if (active !== index) {
          setActive(index);
        }
      });
      list.append(item);
    });
    input.setAttribute("aria-expanded", "true");
    setActive(0);
  }

  async function search(query) {
    if (controller) {
      controller.abort();
    }
    if (!query) {
      dialog.classList.remove("is-loading");
      render("", { results: [] });
      return;
    }
    controller = new AbortController();
    dialog.classList.add("is-loading");
    try {
      const response = await fetch(`/__search.json?q=${encodeURIComponent(query)}`, {
        signal: controller.signal,
        headers: { Accept: "application/json" },
        credentials: "same-origin",
      });
      if (!response.ok) {
        // A lapsed session answers with the sign-in page; reloading shows it.
        if (response.status === 401) {
          window.location.reload();
          return;
        }
        throw new Error(`search answered ${response.status}`);
      }
      const payload = await response.json();
      if (query === lastQuery) {
        render(query, payload);
      }
    } catch (error) {
      if (error.name !== "AbortError") {
        render(query, { unavailable: true, results: [] });
      }
    } finally {
      if (query === lastQuery) {
        dialog.classList.remove("is-loading");
      }
    }
  }

  input.addEventListener("input", () => {
    const query = input.value.trim();
    lastQuery = query;
    window.clearTimeout(timer);
    timer = window.setTimeout(() => search(query), 140);
  });

  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive(active + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive(active - 1);
    }
  });

  form.addEventListener("submit", (event) => {
    const items = options();
    if (active >= 0 && items[active]) {
      event.preventDefault();
      window.location.assign(items[active].querySelector("a").href);
    }
    // Otherwise the form submits to the full results page.
  });

  opener.addEventListener("click", open);
  closer.addEventListener("click", close);
  // A click on the backdrop (the dialog element itself, outside its content) closes it.
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) {
      close();
    }
  });

  const keyHint = opener.querySelector(".search-trigger-key");
  if (keyHint) {
    keyHint.textContent = isMac ? "⌘K" : "Ctrl K";
  }

  window.addEventListener("keydown", (event) => {
    const target = event.target;
    const typing =
      target instanceof HTMLElement &&
      (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));
    const commandK = (isMac ? event.metaKey : event.ctrlKey) && event.key.toLowerCase() === "k";
    if (commandK || (event.key === "/" && !typing && !event.metaKey && !event.ctrlKey && !event.altKey)) {
      event.preventDefault();
      open();
    }
  });
})();
