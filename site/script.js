"use strict";

// Browsers block manifest requests from file://, while the page itself works offline.
if (location.protocol !== "file:") {
  const manifest = document.createElement("link");
  manifest.rel = "manifest";
  manifest.href = "site.webmanifest";
  document.head.append(manifest);
}

function copyFallback(text) {
  const field = document.createElement("textarea");
  field.value = text;
  field.className = "visually-hidden";
  field.setAttribute("readonly", "");
  document.body.append(field);
  field.select();
  try {
    if (!document.execCommand("copy")) throw new Error("Clipboard unavailable");
  } finally {
    field.remove();
  }
}

const tabs = [...document.querySelectorAll(".method-tab")];
const panel = document.getElementById("install-panel");
const code = document.getElementById("install-command");
const button = panel.querySelector(".copy-button");
const status = document.getElementById("copy-status");
let reset;

function resetCopy() {
  clearTimeout(reset);
  delete button.dataset.copied;
  button.setAttribute("aria-label", "Copy command");
  button.title = "Copy command";
  status.textContent = "";
}

function selectMethod(selected) {
  for (const tab of tabs) {
    const active = tab === selected;
    tab.setAttribute("aria-selected", String(active));
    tab.tabIndex = active ? 0 : -1;
  }
  const command = selected.dataset.command;
  code.textContent = command || "";
  code.parentElement.hidden = !command;
  button.hidden = !command;
  panel.querySelector(".download").hidden = Boolean(command);
  panel.setAttribute("aria-labelledby", selected.id);
  resetCopy();
  selected.scrollIntoView({ block: "nearest", inline: "nearest" });
}

for (const [index, tab] of tabs.entries()) {
  tab.addEventListener("click", () => selectMethod(tab));
  tab.addEventListener("keydown", (event) => {
    let next;
    if (event.key === "ArrowRight") next = (index + 1) % tabs.length;
    else if (event.key === "ArrowLeft") next = (index - 1 + tabs.length) % tabs.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = tabs.length - 1;
    else return;
    event.preventDefault();
    tabs[next].focus({ preventScroll: true });
    selectMethod(tabs[next]);
  });
}

button.addEventListener("click", async () => {
  const command = code.textContent;
  resetCopy();
  let copied = false;
  try {
    try {
      await navigator.clipboard.writeText(command);
    } catch {
      copyFallback(command);
    }
    copied = true;
  } catch {
    // Report clipboard failure locally; the command remains selectable.
  }
  // A pending clipboard request must not mark a different method as copied.
  if (command !== code.textContent || button.hidden) return;
  if (copied) {
    button.dataset.copied = "";
    button.setAttribute("aria-label", "Command copied");
    button.title = "Copied!";
    status.textContent = "Command copied to clipboard.";
  } else {
    button.setAttribute("aria-label", "Copy failed. Select the command to copy manually.");
    button.title = "Copy unavailable. Select the command to copy manually.";
    status.textContent = button.title;
  }
  button.focus({ preventScroll: true });
  reset = setTimeout(resetCopy, 1500);
});

const terminal = document.querySelector(".terminal-preview");
const terminalCommand = terminal.querySelector(".terminal-prompt code");
const terminalStatus = terminal.querySelector("dd.terminal-connected");
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
  terminalStatus.textContent = "[+] Connected";
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
    terminalStatus.textContent = "[~] Connecting…";
  });
  later(1700, () => {
    terminal.dataset.stage = "receiving";
    terminalStatus.textContent = "[+] Connected";
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
