"use strict";

// A button that opens a listbox: a native select that can show icons. The landing page
// installers and the docs agent picker share it; each owns its selection and display.
// entries: [{ id, label, icon, group }]; selected() returns the chosen index; choose(index) applies one.
function listbox(prefix, label, listId, entries, selected, choose) {
  const button = document.createElement("button");
  const list = document.createElement("ul");
  const items = [];
  let active = 0;
  let heading;
  button.type = "button";
  button.className = `${prefix}-button`;
  button.setAttribute("aria-haspopup", "listbox");
  button.setAttribute("aria-expanded", "false");
  button.setAttribute("aria-label", label);
  button.setAttribute("aria-controls", listId);
  list.className = `${prefix}-list`;
  list.id = listId;
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", label);
  list.tabIndex = -1;
  list.hidden = true;

  for (const entry of entries) {
    if (entry.group && entry.group !== heading) {
      heading = entry.group;
      const title = document.createElement("li");
      title.className = `${prefix}-group`;
      title.setAttribute("role", "presentation");
      title.textContent = heading;
      list.append(title);
    }
    const item = document.createElement("li");
    item.id = entry.id;
    item.className = `${prefix}-option`;
    item.setAttribute("role", "option");
    item.append(entry.icon, entry.label);
    item.addEventListener("click", () => pick(items.indexOf(item)));
    item.addEventListener("mousemove", () => highlight(items.indexOf(item)));
    list.append(item);
    items.push(item);
  }

  function highlight(index) {
    active = Math.max(0, Math.min(items.length - 1, index));
    items.forEach((item, position) => item.classList.toggle("is-active", position === active));
    list.setAttribute("aria-activedescendant", items[active].id);
    items[active].scrollIntoView({ block: "nearest" });
  }

  function open() {
    list.hidden = false;
    button.setAttribute("aria-expanded", "true");
    highlight(selected());
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
    choose(index);
  }

  button.addEventListener("click", () => (list.hidden ? open() : close()));
  button.addEventListener("keydown", (event) => {
    if (!["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) return;
    event.preventDefault();
    open();
  });
  list.addEventListener("keydown", (event) => {
    const moves = { ArrowDown: active + 1, ArrowUp: active - 1, Home: 0, End: items.length - 1, PageDown: active + 8, PageUp: active - 8 };
    if (event.key in moves) highlight(moves[event.key]);
    else if (event.key === "Enter" || event.key === " ") pick(active);
    else if (event.key === "Escape") close();
    else if (event.key === "Tab") return close(false);
    else if (event.key.length === 1) {
      // Jump to the next item starting with the typed letter.
      const letter = event.key.toLowerCase();
      const starts = (item) => item.textContent.toLowerCase().startsWith(letter);
      const next = items.findIndex((item, position) => position > active && starts(item));
      const first = items.findIndex(starts);
      if (next >= 0 || first >= 0) highlight(next >= 0 ? next : first);
    } else return;
    event.preventDefault();
  });
  document.addEventListener("click", (event) => {
    if (!button.contains(event.target) && !list.contains(event.target)) close(false);
  });
  return { button, list, items };
}
