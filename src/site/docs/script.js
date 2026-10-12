"use strict";

// Progressive enhancement only: without this file every page, example and nav link still works.
// Every locale (index.html, pt-br/, es/) shares this file. Interface text comes from the page's
// #ui-strings JSON, and assets resolve from this script's own URL, never from the page's location.
(() => {
  const root = document.documentElement;
  const ICONS = new URL("../assets/icons/", document.currentScript.src);
  const ui = JSON.parse(document.getElementById("ui-strings").textContent);
  const $$ = (selector, scope = document) => [...scope.querySelectorAll(selector)];
  const store = {
    get(key) { try { return localStorage.getItem(key); } catch { return null; } },
    set(key, value) { try { localStorage.setItem(key, value); } catch { /* storage unavailable */ } },
  };
  const status = document.getElementById("status");
  let statusTimer;
  function announce(message, delay = 0) {
    clearTimeout(statusTimer);
    statusTimer = setTimeout(() => { status.textContent = message; }, delay);
  }
  function icon(id) {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
    svg.setAttribute("aria-hidden", "true");
    use.setAttribute("href", "#" + id);
    svg.append(use);
    return svg;
  }

  /* Theme: follow the system until the reader picks one. */
  const darkQuery = matchMedia("(prefers-color-scheme: dark)");
  const isDark = () => (root.dataset.theme ? root.dataset.theme === "dark" : darkQuery.matches);
  const themeButtons = $$(".theme-toggle");
  function paintTheme() {
    for (const button of themeButtons) {
      button.hidden = false;
      button.setAttribute("aria-label", isDark() ? ui.themeLight : ui.themeDark);
    }
  }
  for (const button of themeButtons) {
    button.addEventListener("click", () => {
      root.dataset.theme = isDark() ? "light" : "dark";
      store.set("ahrara-docs-theme", root.dataset.theme);
      paintTheme();
    });
  }
  darkQuery.addEventListener("change", paintTheme);
  paintTheme();

  /* Small lexical highlights, as in the current docs; text nodes keep code inert. */
  const WORDS = {
    javascript: "import|from|export|default|const|let|var|function|async|await|return|if|else|for|of|in|new|class|try|catch|finally|throw|typeof|true|false|null|undefined",
    csharp: "using|namespace|public|private|internal|protected|static|readonly|async|await|var|new|class|record|void|string|int|bool|return|if|else|foreach|for|in|try|catch|finally|throw|null|true|false|this|out|is|not",
    go: "package|import|func|return|if|else|for|range|defer|go|var|const|type|struct|interface|map|nil|true|false",
    java: "import|package|public|private|protected|static|final|void|class|new|return|if|else|for|try|catch|finally|throw|throws|var|null|true|false|this",
    python: "import|from|as|def|class|async|await|with|return|if|elif|else|for|in|try|except|finally|raise|assert|lambda|not|and|or|None|True|False",
    rust: "use|fn|pub|let|mut|async|await|move|match|if|else|for|in|loop|while|return|impl|struct|enum|mod|Some|None|Ok|Err|true|false",
  };
  WORDS.typescript = `${WORDS.javascript}|type|interface`;
  // Robot Framework: section headers, settings, variables, and the keyword each step calls.
  const ROBOT = [
    "(?<keyword>^\\*{3}[^*]+\\*{3}$|^(?:Library|Resource|Variables|Documentation|Suite Setup|Suite Teardown|Test Setup|Test Teardown|Test Timeout)\\b|(?<=^ {4})\\[[A-Za-z][\\w ]*\\]|\\b(?:WITH NAME|AND)\\b)",
    "(?<variable>[$@&%]\\{[^}]*\\})",
    "(?<function>(?:(?<=^ {4})|(?<== {2,}))[A-Za-z][\\w.]*(?: [\\w.]+)*)",
  ];
  function tokenPattern(syntax) {
    const parts = [];
    if (["typescript", "javascript", "csharp", "go", "java", "rust"].includes(syntax)) parts.push("(?<comment>(?<!:)//[^\\n]*)");
    if (["python", "robot", "shell"].includes(syntax)) parts.push("(?<comment>(?:^|(?<=\\s))# [^\\n]*)");
    if (syntax === "robot") parts.push(...ROBOT);
    parts.push("(?<string>\"(?:\\\\.|[^\"\\\\])*\"|'(?:\\\\.|[^'\\\\])*'|`[^`]*`)");
    if (WORDS[syntax]) parts.push(`(?<keyword>\\b(?:${WORDS[syntax]})\\b)`, "(?<number>\\b\\d[\\d_]*\\b)", "(?<function>\\b[A-Za-z_]\\w*(?=\\())");
    return new RegExp(parts.join("|"), "g");
  }
  for (const pre of $$("pre[data-syntax]")) {
    if (pre.dataset.syntax === "text") continue;
    const code = pre.querySelector("code");
    const pattern = tokenPattern(pre.dataset.syntax);
    const lines = code.textContent.split("\n");
    code.replaceChildren();
    lines.forEach((line, index) => {
      if (index) code.append("\n");
      let end = 0;
      for (const match of line.matchAll(pattern)) {
        code.append(line.slice(end, match.index));
        const token = document.createElement("span");
        token.className = "token-" + Object.keys(match.groups).find((name) => match.groups[name] !== undefined);
        token.textContent = match[0];
        code.append(token);
        end = match.index + match[0].length;
      }
      code.append(line.slice(end));
    });
  }

  /* Copy buttons */
  async function copy(text, button) {
    const label = button.querySelector("span");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      return announce(ui.copyFailed);
    }
    button.dataset.copied = "";
    label.textContent = ui.copied;
    announce(ui.copiedStatus);
    setTimeout(() => { delete button.dataset.copied; label.textContent = ui.copy; }, 1600);
  }
  function copyButton(getText) {
    const button = document.createElement("button");
    const label = document.createElement("span");
    button.type = "button";
    button.className = "copy";
    button.setAttribute("aria-label", ui.copyCode);
    button.title = ui.copyCode;
    label.textContent = ui.copy;
    button.append(icon("i-copy"), label);
    button.addEventListener("click", () => copy(getText(), button));
    return button;
  }
  for (const block of $$(".code")) {
    const pre = block.querySelector("pre");
    block.querySelector(".code-bar").append(copyButton(() => pre.textContent));
  }

  /* Tabs: one choice per kind (language, install method, agent) applies to the whole site. */
  const ALIAS = { playwright: "node", cypress: "node", robot: "python" };
  const groups = $$(".tabs");
  let uid = 0;
  const panelsOf = (group) => $$(":scope > .tab-panel", group);
  function select(group, key) {
    const panels = panelsOf(group);
    const target = panels.find((panel) => panel.dataset.key === key) || panels.find((panel) => panel.dataset.key === ALIAS[key]);
    if (!target) return;
    for (const panel of panels) {
      const on = panel === target;
      const tab = document.getElementById(panel.getAttribute("aria-labelledby"));
      panel.hidden = !on;
      tab.setAttribute("aria-selected", String(on));
      tab.tabIndex = on ? 0 : -1;
    }
    const button = group.querySelector(":scope > .code-bar .copy");
    if (button) button.hidden = !target.querySelector("pre");
    group.showChoice?.(target);
  }
  function selectAll(sync, key) {
    for (const group of groups) if (group.dataset.sync === sync) select(group, key);
    store.set("ahrara-docs-tab-" + sync, key);
  }
  function choose(sync, key, tab) {
    const before = tab.getBoundingClientRect().top;
    selectAll(sync, key);
    // Keep the clicked tab still while examples above it change height.
    window.scrollBy(0, tab.getBoundingClientRect().top - before);
    tab.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
  function iconFor(panel, light) {
    const image = document.createElement("img");
    image.src = new URL(panel.dataset.icon, ICONS).href;
    image.alt = "";
    image.width = image.height = 16;
    if ("invert" in panel.dataset) image.className = light ? "icon-ink" : "icon-invert";
    if ("plate" in panel.dataset) image.className = "icon-plate";
    return image;
  }

  // Long choices (every supported agent) use a listbox instead of a row of tabs.
  function buildPicker(group, bar) {
    const wrap = document.createElement("div");
    const current = document.createElement("span");
    const panels = panelsOf(group);
    const listId = "picker-" + (++uid);
    const entries = panels.map((panel) => {
      panel.id = "tab-" + (++uid);
      panel.setAttribute("role", "region");
      panel.setAttribute("aria-labelledby", panel.id + "-tab");
      return { id: panel.id + "-tab", label: panel.dataset.label, icon: iconFor(panel, false), group: panel.dataset.group };
    });
    const { button, list } = listbox("picker", group.dataset.label, listId, entries, () => panels.findIndex((panel) => !panel.hidden), (index) => choose(group.dataset.sync, panels[index].dataset.key, button));
    wrap.className = "picker";
    button.append(current);
    group.showChoice = (panel) => current.replaceChildren(iconFor(panel, false), panel.dataset.label);
    wrap.append(button, list);
    bar.append(wrap);
  }

  for (const group of groups) {
    const bar = document.createElement("div");
    const list = document.createElement("div");
    const light = group.classList.contains("tabs-light");
    bar.className = "code-bar";
    if ("picker" in group.dataset) {
      buildPicker(group, bar);
      bar.append(copyButton(() => group.querySelector(".tab-panel:not([hidden]) pre")?.textContent ?? ""));
      group.prepend(bar);
      group.dataset.ready = "";
      select(group, panelsOf(group)[0].dataset.key);
      const saved = store.get("ahrara-docs-tab-" + group.dataset.sync);
      if (saved) select(group, saved);
      continue;
    }
    list.className = "tab-list";
    list.setAttribute("role", "tablist");
    list.setAttribute("aria-label", group.dataset.label);
    for (const panel of panelsOf(group)) {
      const id = "tab-" + (++uid);
      const tab = document.createElement("button");
      tab.type = "button";
      tab.className = "tab";
      tab.id = id + "-tab";
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-controls", id);
      tab.append(iconFor(panel, light), panel.dataset.label);
      panel.id = id;
      panel.setAttribute("role", "tabpanel");
      panel.setAttribute("aria-labelledby", tab.id);
      tab.addEventListener("click", () => choose(group.dataset.sync, panel.dataset.key, tab));
      list.append(tab);
    }
    list.addEventListener("keydown", (event) => {
      const tabs = $$("[role=tab]", list);
      const index = tabs.indexOf(document.activeElement);
      const moves = { ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: tabs.length - 1 };
      if (!(event.key in moves)) return;
      event.preventDefault();
      const next = tabs[(moves[event.key] + tabs.length) % tabs.length];
      next.focus();
      next.click();
    });
    bar.append(list);
    if (!light) bar.append(copyButton(() => group.querySelector(".tab-panel:not([hidden]) pre")?.textContent ?? ""));
    group.prepend(bar);
    group.dataset.ready = "";
    select(group, panelsOf(group)[0].dataset.key);
    const saved = store.get("ahrara-docs-tab-" + group.dataset.sync);
    if (saved) select(group, saved);
  }

  /* Mobile navigation */
  const menu = document.querySelector(".menu-toggle");
  const main = document.getElementById("main");
  const narrow = matchMedia("(max-width: 959px)");
  root.classList.add("js-nav");
  menu.hidden = false;
  function setNav(open) {
    document.body.classList.toggle("nav-open", open);
    menu.setAttribute("aria-expanded", String(open));
    menu.setAttribute("aria-label", open ? ui.menuClose : ui.menuOpen);
    main.inert = open && narrow.matches;
  }
  menu.addEventListener("click", () => {
    const open = !document.body.classList.contains("nav-open");
    setNav(open);
    if (open) (document.querySelector('.nav a[aria-current="page"]') || document.querySelector(".nav a")).focus();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && document.body.classList.contains("nav-open")) {
      setNav(false);
      menu.focus();
    }
  });
  narrow.addEventListener("change", () => setNav(false));

  /* Language menus (top bar and sidebar): plain links that carry the current hash, closed like a native select. */
  const langs = $$("details.lang");
  const langLinks = $$(".lang-menu a");
  document.addEventListener("click", (event) => {
    for (const lang of langs) if (!lang.contains(event.target)) lang.open = false;
  });
  for (const lang of langs) {
    lang.addEventListener("keydown", (event) => {
      if (event.key !== "Escape" || !lang.open) return;
      event.stopPropagation();
      lang.open = false;
      lang.querySelector("summary").focus();
    });
  }

  /* Pages: one article at a time, chosen by the URL hash. */
  const pages = $$(".page");
  const navLinks = $$(".nav a");
  let current = null;
  function focusQuietly(element) {
    if (!element.hasAttribute("tabindex")) element.setAttribute("tabindex", "-1");
    element.focus({ preventScroll: true });
  }
  /* Right-hand topics: on wide screens the current page's "On this page" list moves to a
     sticky column and follows the scroll. Narrow screens keep the list inside the page. */
  const toc = document.createElement("aside");
  let tocLinks = [];
  toc.className = "toc";
  main.append(toc);
  function spy() {
    let active = tocLinks[0];
    for (const link of tocLinks) {
      const target = document.getElementById(link.getAttribute("href").slice(1));
      if (target && target.getBoundingClientRect().top < 140) active = link;
    }
    for (const link of tocLinks) link.classList.toggle("is-current", link === active);
  }
  function fillToc(page) {
    const source = page.querySelector(":scope > .onpage");
    toc.hidden = !source;
    if (!source) return toc.replaceChildren();
    const copy = source.cloneNode(true);
    copy.className = "toc-nav";
    copy.removeAttribute("aria-labelledby");
    copy.setAttribute("aria-label", source.querySelector("p").textContent);
    for (const element of copy.querySelectorAll("[id]")) element.removeAttribute("id");
    toc.replaceChildren(copy);
    tocLinks = [...copy.querySelectorAll("a")];
    spy();
  }
  window.addEventListener("scroll", spy, { passive: true });

  function route(navigated) {
    let id = "";
    try { id = decodeURIComponent(location.hash.slice(1)); } catch { /* malformed hash: show the current page */ }
    const target = id ? document.getElementById(id) : null;
    const page = target?.closest(".page") || current || pages[0];
    for (const candidate of pages) candidate.hidden = candidate !== page;
    current = page;
    fillToc(page);
    for (const link of navLinks) {
      if (link.getAttribute("href") === "#" + page.id) link.setAttribute("aria-current", "page");
      else link.removeAttribute("aria-current");
    }
    document.title = page.id === "overview" ? ui.siteTitle : ui.pageTitle.replace("{page}", page.dataset.title);
    for (const link of langLinks) link.hash = location.hash;
    for (const lang of langs) lang.open = false;
    setNav(false);
    const folded = target?.closest("details");
    if (folded) folded.open = true;
    if (target && target !== page) {
      target.scrollIntoView();
      if (navigated) focusQuietly(target);
    } else {
      window.scrollTo(0, 0);
      if (navigated) focusQuietly(page.querySelector("h1"));
    }
  }
  window.addEventListener("hashchange", () => route(true));
  route(false);

  /* Search over page titles and headings. */
  const search = document.querySelector(".search");
  const input = document.getElementById("search");
  const results = document.getElementById("search-results");
  const empty = document.querySelector(".search-empty");
  const headingText = (heading) => {
    const clone = heading.cloneNode(true);
    for (const extra of clone.querySelectorAll(".step, .visually-hidden")) extra.remove();
    return clone.textContent.trim();
  };
  const index = pages.flatMap((page) => $$("h1, h2[id], h3[id]", page).map((heading) => ({
    text: headingText(heading),
    page: page.dataset.title,
    href: "#" + (heading.tagName === "H1" ? page.id : heading.id),
    isPage: heading.tagName === "H1",
  })));
  search.hidden = false;
  function render() {
    const words = input.value.toLowerCase().split(/\s+/).filter(Boolean);
    results.replaceChildren();
    if (!words.length) {
      results.hidden = true;
      empty.hidden = true;
      return;
    }
    const hits = index
      .filter((entry) => words.every((word) => (entry.text + " " + entry.page).toLowerCase().includes(word)))
      .sort((a, b) => Number(b.text.toLowerCase().includes(words[0])) - Number(a.text.toLowerCase().includes(words[0])) || Number(b.isPage) - Number(a.isPage))
      .slice(0, 8);
    for (const hit of hits) {
      const item = document.createElement("li");
      const link = document.createElement("a");
      link.href = hit.href;
      link.textContent = hit.text;
      if (!hit.isPage) {
        const where = document.createElement("span");
        where.textContent = hit.page;
        link.append(where);
      }
      item.append(link);
      results.append(item);
    }
    results.hidden = !hits.length;
    empty.hidden = Boolean(hits.length);
    announce(hits.length === 1 ? ui.resultOne : hits.length ? ui.resultMany.replace("{count}", hits.length) : ui.noResults, 500);
  }
  function clearSearch() {
    input.value = "";
    render();
  }
  input.addEventListener("input", render);
  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      results.querySelector("a")?.focus();
    } else if (event.key === "Enter") {
      const first = results.querySelector("a");
      if (!first) return;
      event.preventDefault();
      location.hash = first.getAttribute("href");
      clearSearch();
    } else if (event.key === "Escape" && input.value) {
      event.stopPropagation();
      clearSearch();
    }
  });
  results.addEventListener("keydown", (event) => {
    const links = $$("a", results);
    const at = links.indexOf(document.activeElement);
    if (event.key === "ArrowDown") {
      event.preventDefault();
      links[Math.min(at + 1, links.length - 1)].focus();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      if (at <= 0) input.focus(); else links[at - 1].focus();
    } else if (event.key === "Escape") {
      event.stopPropagation();
      input.focus();
    }
  });
  results.addEventListener("click", (event) => {
    if (event.target.closest("a")) setTimeout(clearSearch);
  });
  document.addEventListener("keydown", (event) => {
    const active = document.activeElement;
    const typing = active && (/^(INPUT|TEXTAREA|SELECT)$/.test(active.tagName) || active.isContentEditable);
    if ((event.key.toLowerCase() === "k" && (event.ctrlKey || event.metaKey)) || (event.key === "/" && !typing)) {
      event.preventDefault();
      if (narrow.matches) setNav(true);
      input.focus();
      input.select();
    }
  });
})();
