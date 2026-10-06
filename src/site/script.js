"use strict";

// Every locale (index.html, pt-br/, es/) shares this file: interface text comes from the
// page's #ui-strings JSON, and site files resolve from this script's URL.
const ui = JSON.parse(document.getElementById("ui-strings").textContent);

// Browsers block manifest requests from file://, while the page itself works offline.
if (location.protocol !== "file:") {
  const manifest = document.createElement("link");
  manifest.rel = "manifest";
  manifest.href = new URL("site.webmanifest", document.currentScript.src).href;
  document.head.append(manifest);
}

// Close the language menu like a native select.
const languages = document.querySelector("details.languages");
document.addEventListener("click", (event) => {
  if (!languages.contains(event.target)) languages.open = false;
});
languages.addEventListener("keydown", (event) => {
  if (event.key !== "Escape" || !languages.open) return;
  languages.open = false;
  languages.querySelector("summary").focus();
});

const status = document.getElementById("copy-status");

// Native selects cannot show icons in their options, so each one gets a listbox button.
// The select stays the source of truth: choosing an item sets its value and fires "change".
function enhanceSelect(select, icon) {
  const current = document.createElement("span");
  const options = [...select.options];
  const entries = options.map((option) => {
    const image = document.createElement("img");
    image.src = icon.src.replace(/[^/]+$/, option.dataset.icon);
    image.alt = "";
    image.width = image.height = 20;
    if (option.dataset.iconClass) image.className = option.dataset.iconClass;
    return { id: `${select.id}-${option.value}`, label: option.textContent, icon: image, group: option.parentElement.label };
  });
  const label = document.querySelector(`label[for="${select.id}"]`).textContent;
  const { button, list, items } = listbox("method", label, `${select.id}-list`, entries, () => select.selectedIndex, (index) => {
    if (select.selectedIndex === index) return;
    select.selectedIndex = index;
    select.dispatchEvent(new Event("change"));
  });
  button.append(current);

  function sync() {
    current.textContent = select.selectedOptions[0].textContent;
    items.forEach((item, position) => item.setAttribute("aria-selected", String(options[position].selected)));
  }

  select.addEventListener("change", sync);
  select.classList.add("visually-hidden");
  select.tabIndex = -1;
  select.setAttribute("aria-hidden", "true");
  select.after(button, list);
  sync();
}

// Each installer (CLI, AI agents, SDKs, test frameworks) owns a dropdown, a command
// panel and a copy button; SDK and framework installers also switch their example.
for (const installer of document.querySelectorAll(".installer")) {
  const select = installer.querySelector(".method-select");
  const icon = installer.querySelector(".method-icon");
  const panel = installer.querySelector(".install-panel");
  const code = panel.querySelector("code");
  const button = panel.querySelector(".copy-button");
  const prompt = panel.querySelector(".prompt");
  const download = panel.querySelector(".download");
  const hint = installer.querySelector(".install-hint");
  const example = document.getElementById(installer.dataset.example);
  let reset;

  function resetCopy() {
    clearTimeout(reset);
    delete button.dataset.copied;
    button.setAttribute("aria-label", ui.copyCommand);
    button.title = ui.copyCommand;
    status.textContent = "";
  }

  function selectMethod() {
    const option = select.selectedOptions[0];
    const command = option.dataset.command;
    code.textContent = command || "";
    prompt.textContent = option.dataset.prompt ?? "$";
    hint.textContent = option.dataset.hint || "";
    // Icons sit beside this page's assets, including on localized pages.
    icon.src = icon.src.replace(/[^/]+$/, option.dataset.icon);
    icon.className = `method-icon ${option.dataset.iconClass || ""}`.trim();
    code.parentElement.hidden = !command;
    button.hidden = !command;
    if (download) download.hidden = Boolean(command);
    for (const sample of example?.querySelectorAll("[data-for]") ?? []) sample.hidden = sample.dataset.for !== option.value;
    resetCopy();
  }

  select.addEventListener("change", selectMethod);
  enhanceSelect(select, icon);
  // Browsers may restore a previous choice on reload; show its command and example.
  selectMethod();

  button.addEventListener("click", async () => {
    const command = code.textContent;
    resetCopy();
    let copied = false;
    try {
      await navigator.clipboard.writeText(command);
      copied = true;
    } catch {
      // Report clipboard failure locally; the command remains selectable.
    }
    // A pending clipboard request must not mark a different method as copied.
    if (command !== code.textContent || button.hidden) return;
    if (copied) {
      button.dataset.copied = "";
      button.setAttribute("aria-label", ui.commandCopied);
      button.title = ui.copiedTitle;
      status.textContent = ui.copiedStatus;
    } else {
      button.setAttribute("aria-label", ui.copyFailed);
      button.title = ui.copyUnavailable;
      status.textContent = button.title;
    }
    button.focus({ preventScroll: true });
    reset = setTimeout(resetCopy, 1500);
  });
}

const terminal = document.querySelector(".terminal-preview");
const terminalCommand = terminal.querySelector(".terminal-prompt code");
const terminalStatus = terminal.querySelector("dd.terminal-connected");
const connected = terminalStatus.textContent;
const emailRows = [...terminal.querySelectorAll("tbody tr")];
const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
const terminalTimers = [];
let terminalObserver;

function dateEmail(row, date) {
  const time = row.querySelector("time");
  const pad = (value) => String(value).padStart(2, "0");
  time.dateTime = date.toISOString();
  time.textContent = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

function showTerminal() {
  terminalTimers.forEach(clearTimeout);
  terminalObserver?.disconnect();
  terminalCommand.textContent = "ahrara";
  terminalStatus.textContent = connected;
  terminal.dataset.stage = "done";
  // Keep the newest email first, including when motion is disabled mid-demo.
  for (const [index, row] of emailRows.entries()) {
    dateEmail(row, new Date(Date.now() - index * 1500));
    delete row.dataset.pending;
    delete row.dataset.received;
    row.parentElement.append(row);
  }
}

function animateTerminal() {
  terminalObserver.disconnect();
  terminal.dataset.stage = "typing";
  const later = (delay, action) => terminalTimers.push(setTimeout(action, delay));
  for (let length = 1; length <= 6; length++) {
    later(200 + length * 100, () => { terminalCommand.textContent = "ahrara".slice(0, length); });
  }
  later(1100, () => {
    terminal.dataset.stage = "connecting";
    terminalStatus.textContent = ui.connecting;
  });
  later(1700, () => {
    terminal.dataset.stage = "receiving";
    terminalStatus.textContent = connected;
  });
  // An older email arrives first; the next one is inserted above it.
  for (const [index, row] of [...emailRows].reverse().entries()) {
    later(2200 + index * 1500, () => {
      dateEmail(row, new Date());
      row.parentElement.prepend(row);
      delete row.dataset.pending;
      row.dataset.received = "";
      if (index === emailRows.length - 1) terminal.dataset.stage = "done";
    });
  }
}

if (reducedMotion.matches || !("IntersectionObserver" in window)) {
  showTerminal();
} else {
  terminalCommand.textContent = "";
  terminal.dataset.stage = "idle";
  emailRows.forEach((row) => { row.dataset.pending = ""; });
  terminalObserver = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) animateTerminal();
  }, { threshold: 0.25 });
  terminalObserver.observe(terminal);
}
reducedMotion.addEventListener("change", () => {
  if (reducedMotion.matches) showTerminal();
});

const chat = document.querySelector(".agent-chat");
if (!reducedMotion.matches && "IntersectionObserver" in window) {
  chat.dataset.state = "idle";
  const observer = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) {
      chat.dataset.state = "playing";
      observer.disconnect();
    }
  }, { threshold: 0.25 });
  observer.observe(chat);
  chat.addEventListener("animationend", (event) => {
    if (event.target.classList.contains("chat-result")) chat.dataset.state = "done";
  });
  reducedMotion.addEventListener("change", () => {
    if (reducedMotion.matches) {
      observer.disconnect();
      chat.dataset.state = "done";
    }
  });
}
