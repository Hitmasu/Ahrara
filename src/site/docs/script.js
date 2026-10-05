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
  const KEYWORDS = "\\b(?:import|from|const|let|var|await|async|try|finally|with|as|for|in|if|return|using|new|class|static|void|throws|export|default|function|package|func|defer|use|fn|pub|Some|None|true|false|null|True|False)\\b";
  function tokenPattern(syntax) {
    const parts = [];
    if (["typescript", "javascript", "csharp", "go", "java", "rust"].includes(syntax)) parts.push("(?<comment>(?<!:)//[^\\n]*)");
    if (["python", "robot", "shell"].includes(syntax)) parts.push("(?<comment>(?:^|(?<=\\s))# [^\\n]*)");
    parts.push("(?<string>\"(?:\\\\.|[^\"\\\\])*\"|'(?:\\\\.|[^'\\\\])*'|`[^`]*`)");
    if (!["shell", "json", "toml", "xml", "text", "robot"].includes(syntax)) parts.push(`(?<keyword>${KEYWORDS})`, "(?<number>\\b\\d[\\d_]*\\b)");
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
  function copy(text, button) {
    const label = button.querySelector("span");
    const done = () => {
      button.dataset.copied = "";
      label.textContent = ui.copied;
      announce(ui.copiedStatus);
      setTimeout(() => { delete button.dataset.copied; label.textContent = ui.copy; }, 1600);
    };
    const fallback = () => {
      const area = document.createElement("textarea");
      area.value = text;
      area.setAttribute("readonly", "");
      area.style.cssText = "position:fixed;top:0;left:0;opacity:0";
      document.body.append(area);
      area.select();
      let ok = false;
      try { ok = document.execCommand("copy"); } catch { ok = false; }
      area.remove();
      if (ok) done(); else announce(ui.copyFailed);
    };
    if (navigator.clipboard?.writeText) navigator.clipboard.writeText(text).then(done, fallback);
    else fallback();
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
    const button = document.createElement("button");
    const current = document.createElement("span");
    const list = document.createElement("ul");
    const options = [];
    let active = 0;
    let heading;
    wrap.className = "picker";
    button.type = "button";
    button.className = "picker-button";
    button.setAttribute("aria-haspopup", "listbox");
    button.setAttribute("aria-expanded", "false");
    button.setAttribute("aria-label", group.dataset.label);
    button.append(current);
    list.className = "picker-list";
    list.id = "picker-" + (++uid);
    list.setAttribute("role", "listbox");
    list.setAttribute("aria-label", group.dataset.label);
    list.tabIndex = -1;
    list.hidden = true;
    button.setAttribute("aria-controls", list.id);
    for (const panel of panelsOf(group)) {
      if (panel.dataset.group && panel.dataset.group !== heading) {
        heading = panel.dataset.group;
        const title = document.createElement("li");
        title.className = "picker-group";
        title.setAttribute("role", "presentation");
        title.textContent = heading;
        list.append(title);
      }
      const id = "tab-" + (++uid);
      const option = document.createElement("li");
      option.id = id + "-tab";
      option.className = "picker-option";
      option.setAttribute("role", "option");
      option.append(iconFor(panel, false), panel.dataset.label);
      panel.id = id;
      panel.setAttribute("role", "region");
      panel.setAttribute("aria-labelledby", option.id);
      option.addEventListener("click", () => pick(options.indexOf(option)));
      option.addEventListener("mousemove", () => highlight(options.indexOf(option)));
      list.append(option);
      options.push(option);
    }
    const panels = panelsOf(group);
    function highlight(index) {
      active = Math.max(0, Math.min(options.length - 1, index));
      options.forEach((option, position) => option.classList.toggle("is-active", position === active));
      list.setAttribute("aria-activedescendant", options[active].id);
      options[active].scrollIntoView({ block: "nearest" });
    }
    function open() {
      list.hidden = false;
      button.setAttribute("aria-expanded", "true");
      highlight(panels.findIndex((panel) => !panel.hidden));
      list.focus({ preventScroll: true });
    }
    function close(focus = true) {
      if (list.hidden) return;
      list.hidden = true;
      button.setAttribute("aria-expanded", "false");
      if (focus) button.focus({ preventScroll: true });
    }
    function pick(index) {
      close();
      choose(group.dataset.sync, panels[index].dataset.key, button);
    }
    button.addEventListener("click", () => (list.hidden ? open() : close()));
    button.addEventListener("keydown", (event) => {
      if (!["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) return;
      event.preventDefault();
      open();
    });
    list.addEventListener("keydown", (event) => {
      const moves = { ArrowDown: active + 1, ArrowUp: active - 1, Home: 0, End: options.length - 1, PageDown: active + 8, PageUp: active - 8 };
      if (event.key in moves) highlight(moves[event.key]);
      else if (event.key === "Enter" || event.key === " ") pick(active);
      else if (event.key === "Escape") close();
      else if (event.key === "Tab") return close(false);
      else if (event.key.length === 1) {
        // Jump to the next agent starting with the typed letter.
        const letter = event.key.toLowerCase();
        const starts = (option) => option.textContent.toLowerCase().startsWith(letter);
        const next = options.findIndex((option, position) => position > active && starts(option));
        const first = options.findIndex(starts);
        if (next >= 0 || first >= 0) highlight(next >= 0 ? next : first);
      } else return;
      event.preventDefault();
    });
    document.addEventListener("click", (event) => {
      if (!wrap.contains(event.target)) close(false);
    });
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

  /* Language menu: plain links that carry the current hash, closed like a native select. */
  const lang = document.querySelector("details.lang");
  const langLinks = $$(".lang-menu a");
  document.addEventListener("click", (event) => {
    if (!lang.contains(event.target)) lang.open = false;
  });
  lang.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || !lang.open) return;
    event.stopPropagation();
    lang.open = false;
    lang.querySelector("summary").focus();
  });

  /* Links into the previous docs keep working. Each new ID lists the old IDs it replaces;
     "|sync=key" also selects that tab everywhere, as clicking it would. */
  const LEGACY = new Map(Object.entries({
    "overview": "overview-project",
    "paths": "overview-uses",
    "qs-install": "quick-start-install",
    "qs-start": "quick-start-listen",
    "qs-copy": "quick-start-address",
    "qs-send": "quick-start-send",
    "qs-read": "quick-start-read quick-start-next",
    "install-client": "install-methods install-availability",
    "install-client|install=shell": "install-shell",
    "install-client|install=homebrew": "install-homebrew",
    "install-client|install=powershell": "install-powershell",
    "install-client|install=binaries": "install-binaries",
    "install-sdk": "sdks-choose",
    "install-sdk|lang=node": "install-typescript install-javascript",
    "install-sdk|lang=python": "install-python",
    "install-sdk|lang=csharp": "install-csharp",
    "install-sdk|lang=go": "install-go",
    "install-java|lang=java": "install-java",
    "install-rust|lang=rust": "install-rust",
    "install-k6|lang=k6": "install-k6",
    "install-frameworks|lang=playwright": "install-playwright testing-playwright testing-playwright-setup testing-playwright-run",
    "install-frameworks|lang=cypress": "install-cypress testing-cypress testing-cypress-run",
    "install-frameworks|lang=robot": "install-robot testing-robot testing-robot-setup testing-robot-run",
    "install-update": "install-updates update-shell update-homebrew update-powershell update-binaries update-typescript update-javascript update-python update-csharp update-go update-java update-rust update-k6 update-playwright update-robot update-cypress testing-playwright-update testing-cypress-update testing-robot-update sdk-updates",
    "receive": "receiving",
    "receive-terminal": "receiving-manual",
    "receive-web": "web quick-start-web web-reading local-inbox inbox-read inbox-history",
    "receive-addresses": "addresses addresses-manage",
    "receive-domain": "addresses-domain",
    "receive-profiles": "profiles inbox-lifetime profiles-keep",
    "receive-backup": "inbox-backups profiles-restore storage-history",
    "sdk": "sdks sdks-start receiving-automate",
    "sdk-cypress|lang=cypress": "testing-cypress-setup",
    "sdk-receive": "sdk-wait sdk-wait-order sdk-receive-example sdk-receive-result",
    "sdk-filter": "filters filters-simple filters-examples sdk-wait-matches",
    "sdk-code": "email-verification email-verification-prepare verification-example email-verification-result testing testing-flow testing-deadlines testing-example testing-examples testing-example-check",
    "sdk-link": "password-reset password-reset-prepare reset-extract password-reset-result",
    "sdk-attachments": "attachments attachments-prepare attachments-read attachments-files",
    "sdk-reuse": "sdks-reuse sdks-session sdk-credentials",
    "agents": "agents-capabilities",
    "agents-install": "agents-integration agents-installation",
    "agents-ask": "quick-start-agents",
    "agents-update": "agents-maintenance agents-removal",
    "api-start": "http-api-start",
    "api-list": "http-api-request",
    "api-read": "http-api-read",
    "delivery": "delivery-next",
    "delivery-path": "architecture architecture-concept architecture-delivery architecture-self-host",
    "delivery-zero": "zero-retention zero-retention-server zero-retention-local storage-retention architecture-retention",
    "delivery-offline": "receiving-connected storage-offline zero-retention-delivery",
    "delivery-data": "data-storage storage-location storage-close",
    "delivery-identity": "security",
    "delivery-encryption": "security-identity security-transport security-limits",
    "delivery-access": "security-local architecture-interfaces architecture-trust",
    "delivery-untrusted": "security-input",
    "delivery-report": "security-report",
    "ref-cli": "cli",
    "cli-commands": "cli-terminal quick-start-script cli-maintenance",
    "cli-dashboard": "cli-status",
    "ref-config": "configuration",
    "config-file": "configuration-location",
    "config-profile": "configuration-profile",
    "config-settings": "configuration-options configuration-server",
    "config-web": "authentication authentication-web",
    "config-token": "authentication-api",
    "config-mcp": "authentication-mcp",
    "config-https": "authentication-https",
    "config-lan": "authentication-lan web-access",
    "ref-sdk": "sdk-reference",
    "sdkref-calls": "sdk-language-reference sdk-history sdk-extraction",
    "sdk-rules": "filters-rules sdk-filters",
    "sdk-behavior": "sdks-lifecycle",
    "ref-mcp": "mcp",
    "mcp-connections": "mcp-plugin mcp-stdio mcp-http",
    "mcp-sessions": "mcp-lifetime",
    "ref-api": "api",
    "api-basics": "api-authentication api-contract",
    "errors-timeouts": "sdk-wait-timeout",
    "errors-sdk": "sdk-errors",
    "trouble-mail": "troubleshooting-mail",
    "trouble-agent": "troubleshooting-agent",
    "trouble-cypress": "troubleshooting-cypress",
    "trouble-api": "troubleshooting-api",
    "trouble-report": "troubleshooting-report",
    "contrib-start": "contributing-start contributing-docs",
    "contrib-license": "contributing-license",
  }).flatMap(([to, from]) => from.split(" ").map((id) => [id, to])));
  // The old installer listed agents as #install-agent-0…25 and #update-agent-0…25.
  ["codex", "claude", "copilot", "gemini", ...Array(22)].forEach((agent, n) => {
    LEGACY.set("install-agent-" + n, agent ? "agents-install|agent=" + agent : "agents-other");
    LEGACY.set("update-agent-" + n, "agents-update");
  });

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
    const [targetId, tab] = (LEGACY.get(id) || id).split("|");
    if (tab) selectAll(...tab.split("="));
    const target = targetId ? document.getElementById(targetId) : null;
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
    lang.open = false;
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
