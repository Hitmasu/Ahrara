// Run with playwright-cli run-code --filename=tests/site.smoke.js after opening /docs/.
// The server must serve src/site/ at its root.
async (page) => {
  const check = (value, message) => { if (!value) throw new Error(message); };
  const origin = new URL(page.url()).origin;
  const docs = `${origin}/docs/`;
  const browser = page.context().browser();
  const errors = [];
  const notes = [];
  const onError = (error) => errors.push(error.message);
  const onConsole = (message) => { if (message.type() === "error") errors.push(message.text()); };
  page.on("pageerror", onError);
  page.on("console", onConsole);

  const strings = () => page.evaluate(() => JSON.parse(document.getElementById("ui-strings").textContent));
  // A full load each time, so deep links are tested as a visitor would open them.
  const visit = async (url) => {
    await page.goto("about:blank");
    await page.goto(url);
  };
  const visiblePage = () => page.evaluate(() => [...document.querySelectorAll(".page:not([hidden])")].map((article) => article.id).join());
  const expectPage = (id) => page.waitForFunction((id) => [...document.querySelectorAll(".page:not([hidden])")].map((article) => article.id).join() === id, id);
  const selectedKey = (selector) => page.evaluate((selector) => {
    const tab = document.querySelector(`${selector} [role=tab][aria-selected="true"], ${selector} [role=option][aria-selected="true"]`);
    // Tabs point at their panel; picker options are named by it.
    const panel = tab && (document.getElementById(tab.getAttribute("aria-controls")) || document.querySelector(`[aria-labelledby="${tab.id}"]`));
    return panel?.dataset.key;
  }, selector);
  // Opens a docs URL and checks that it shows the page holding `target`, scrolled into view.
  const landsOn = async (hash, target, tabs = {}) => {
    await visit(`${docs}${hash ? "#" + hash : ""}`);
    const result = await page.evaluate((target) => {
      const element = document.getElementById(target);
      const shown = document.querySelector(".page:not([hidden])");
      const top = element?.getBoundingClientRect().top;
      return { found: Boolean(element), inside: Boolean(element && shown?.contains(element)), pages: document.querySelectorAll(".page:not([hidden])").length, inView: top > -2 && top < innerHeight };
    }, target);
    check(result.found && result.inside && result.pages === 1, `#${hash} must open the page holding #${target}.`);
    check(result.inView, `#${hash} must scroll #${target} into view.`);
    for (const [sync, key] of Object.entries(tabs)) {
      check(await selectedKey(`.tabs[data-sync="${sync}"]`) === key, `#${hash} must select the ${key} tab.`);
    }
  };

  try {
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);

    // Landing page: language menu, copy text from the HTML, and every docs link.
    await visit(`${origin}/`);
    const lpText = await strings();
    check(["copyCommand", "commandCopied", "copiedTitle", "copiedStatus", "copyFailed", "copyUnavailable", "connecting"].every((key) => typeof lpText[key] === "string" && lpText[key]), "The landing page provides its interface text in #ui-strings.");
    const lpLanguages = await page.locator("details.languages a").evaluateAll((links) => links.map((link) => [new URL(link.href).pathname, link.hreflang, link.getAttribute("aria-current")]));
    check(lpLanguages.map(([path]) => path).join() === "/,/pt-br/,/es/", "The landing page links to its English, Portuguese and Spanish versions.");
    check(lpLanguages.map(([, lang]) => lang).join() === "en,pt-BR,es" && lpLanguages[0][2] === "page", "Language links declare their language and mark the current one.");
    await page.locator("details.languages summary").click();
    check(await page.locator("details.languages a").first().isVisible(), "The landing page language menu opens.");
    await page.keyboard.press("Escape");
    check(!await page.locator("details.languages").evaluate((menu) => menu.open), "Escape closes the landing page language menu.");
    await page.locator("#install-panel .copy-button").click();
    await page.waitForFunction((message) => document.getElementById("copy-status").textContent === message, lpText.copiedStatus);
    check(await page.evaluate(() => navigator.clipboard.readText()) === await page.locator("#install-command").textContent(), "The landing page copies the selected command.");
    check(await page.locator("#install-panel .copy-button").getAttribute("aria-label") === lpText.commandCopied, "The copy button uses the page's interface text.");
    // Each installer is a listbox over a native select and keeps its own choice.
    const chooseMethod = async (select, value) => {
      await page.locator(`button[aria-controls="${select}-list"]`).click();
      await page.locator(`#${select}-${value}`).click();
      check(await page.locator(`#${select}`).inputValue() === value && await page.locator(`#${select}-list`).isHidden(), `Choosing ${value} updates #${select} and closes its list.`);
    };
    const cliCommand = await page.locator("#install-command").textContent();
    await chooseMethod("agent-method", "codex");
    check((await page.locator("#agent-command").textContent()).includes("codex plugin") && await page.locator("#install-command").textContent() === cliCommand, "Each landing page installer keeps its own choice.");
    await page.locator("#agent-panel .copy-button").click();
    await page.waitForFunction(() => document.querySelector("#agent-panel .copy-button").hasAttribute("data-copied"));
    check(await page.evaluate(() => navigator.clipboard.readText()) === await page.locator("#agent-command").textContent(), "The agent installer copies its own command.");
    for (const [installer, value, command] of [["sdk", "go", "go get "], ["framework", "robot", "pip install ahrara"]]) {
      await chooseMethod(`${installer}-method`, value);
      check((await page.locator(`#${installer}-command`).textContent()).startsWith(command), `The ${installer} installer shows the ${value} command.`);
      const examples = await page.locator(`#${installer}-example pre[data-for]`).evaluateAll((samples) => samples.filter((sample) => !sample.hidden).map((sample) => sample.dataset.for));
      check(examples.join() === value, `The ${installer} example follows the ${value} choice.`);
    }
    const agentButton = page.locator('button[aria-controls="agent-method-list"]');
    await agentButton.focus();
    await page.keyboard.press("ArrowDown");
    check(await agentButton.getAttribute("aria-expanded") === "true", "The keyboard opens an installer list.");
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    check(await page.locator("#agent-method").inputValue() === "mcp" && await page.locator("#agent-method-mcp").getAttribute("aria-selected") === "true", "End and Enter choose the last agent.");
    check(await agentButton.evaluate((button) => button === document.activeElement && button.getAttribute("aria-expanded") === "false"), "Choosing closes the list and returns focus.");
    await agentButton.press("Enter");
    await page.keyboard.press("Escape");
    check(await page.locator("#agent-method-list").isHidden(), "Escape closes an installer list.");
    await chooseMethod("install-method", "binaries");
    check(await page.locator("#install-panel .download").isVisible() && await page.locator("#install-panel .copy-button").isHidden(), "Binaries link to the downloads instead of a command.");
    const lpDocsLinks = [...new Set(await page.locator('a[href^="docs/"]').evaluateAll((links) => links.map((link) => link.getAttribute("href"))))];
    await page.locator(".navigation").getByRole("link", { name: "Docs", exact: true }).click();
    await page.waitForURL(docs);
    await expectPage("overview");

    const landing = {
      "": ["overview"],
      install: ["install"],
      agents: ["agents"],
      sdk: ["sdk"],
      "receive-app-support": ["receive-app-support"],
    };
    const lpHashes = lpDocsLinks.map((href) => href.split("#")[1] || "");
    for (const hash of lpHashes) check(hash in landing, `Map the landing page link docs/#${hash} in this test.`);
    for (const hash of Object.keys(landing)) await landsOn(hash, ...landing[hash]);

    // Docs structure, interface text and static assets.
    await visit(docs);
    const text = await strings();
    const keys = Object.keys(text).sort().join();
    check(keys === ["siteTitle", "pageTitle", "themeLight", "themeDark", "menuOpen", "menuClose", "copy", "copied", "copyCode", "copiedStatus", "copyFailed", "resultOne", "resultMany", "noResults"].sort().join(), "The docs provide every interface string in #ui-strings.");
    const integrity = await page.evaluate(() => {
      const ids = [...document.querySelectorAll("[id]")].map((element) => element.id);
      return {
        unique: ids.length === new Set(ids).size,
        missing: [...document.querySelectorAll('a[href^="#"]')].filter((link) => !document.getElementById(decodeURIComponent(link.hash.slice(1)))).map((link) => link.hash),
        assets: [...document.querySelectorAll("script[src], img[src], link[rel=stylesheet], link[rel=icon], link[rel=apple-touch-icon]")].map((element) => element.src || element.href),
        pages: document.querySelectorAll("article.page").length,
      };
    });
    check(integrity.unique, "Element IDs must be unique.");
    check(!integrity.missing.length, `Every fragment must resolve: ${integrity.missing.join(" ")}`);
    check(integrity.pages === 16, "The docs have 16 pages.");
    for (const asset of new Set(integrity.assets)) {
      check(new URL(asset).origin === origin, `The docs must not load remote assets: ${asset}`);
      check((await page.request.get(asset)).ok(), `Missing static asset: ${new URL(asset).pathname}`);
    }
    check(await page.locator(".tab img").evaluateAll((images) => images.length > 0 && images.every((image) => image.complete && image.naturalWidth > 0)), "Tab icons resolve from the shared script's location.");

    // Navigation reaches every page.
    const routes = await page.locator(".nav a").evaluateAll((links) => links.map((link) => link.hash));
    check(routes.length === 16, "The sidebar lists every page.");
    for (const route of routes) {
      await page.locator(`.nav a[href="${route}"]`).click();
      await expectPage(route.slice(1));
      check(await page.locator('.nav a[aria-current="page"]').getAttribute("href") === route, `${route} is marked as the current page.`);
      const title = await page.locator(route).getAttribute("data-title");
      check(await page.title() === (route === "#overview" ? text.siteTitle : text.pageTitle.replace("{page}", title)), `${route} sets the document title from #ui-strings.`);
    }

    await landsOn("sdk-cypress", "sdk-cypress");
    check(await page.locator("#sdk-cypress").evaluate((details) => details.open), "Links to the Cypress setup open its disclosure.");
    check(await page.locator("pre code").filter({ hasText: "setupNodeEvents(on, config)" }).count() === 1, "The Cypress tasks are documented once.");

    // The delivery diagram and the complete MCP tool list.
    await visit(`${docs}#delivery-path`);
    check(await page.locator(".diagram img").evaluate((image) => image.complete && image.naturalWidth > 0 && image.alt.length > 40), "The delivery diagram loads with a description.");
    await visit(`${docs}#ref-mcp`);
    const tools = await page.locator("#ref-mcp td:first-child code").allTextContents();
    for (const tool of ["start", "stop", "status", "wait_for_email", "list_emails", "get_email", "read_eml", "list_attachments", "save_attachment", "delete_email", "clear_inbox", "create_address", "list_addresses", "get_address", "enable_address", "disable_address", "delete_address"]) {
      check(tools.includes(`ahrara_${tool}`), `The MCP reference documents ahrara_${tool}.`);
    }

    // Tabs: one choice per kind applies everywhere and is remembered.
    await visit(`${docs}#sdk`);
    const languageGroups = page.locator('#sdk .tabs[data-sync="lang"]');
    check(await languageGroups.count() > 1, "The SDK guide has synchronized examples.");
    await languageGroups.first().getByRole("tab", { name: "Python", exact: true }).click();
    const allPython = () => page.evaluate(() => [...document.querySelectorAll('#sdk .tabs[data-sync="lang"]')].every((group) => {
      const panels = group.querySelectorAll(":scope > .tab-panel:not([hidden])");
      return panels.length === 1 && panels[0].dataset.key === "python";
    }));
    check(await allPython(), "Choosing a language updates every example.");
    await languageGroups.first().getByRole("tab", { name: "Python", exact: true }).focus();
    await page.keyboard.press("ArrowRight");
    check(await selectedKey('#sdk .tabs[data-sync="lang"]') === "csharp", "Tabs support arrow keys.");
    await page.keyboard.press("ArrowLeft");
    await page.reload();
    check(await allPython(), "The chosen language survives a reload.");
    check(await selectedKey('#ref-sdk .tabs[data-sync="lang"]') === "python", "Reference tabs follow the same choice.");
    await visit(`${docs}#install-client`);
    await page.locator("#install .tabs").first().getByRole("tab", { name: "PowerShell", exact: true }).click();
    check(await selectedKey('#quick-start .tabs[data-sync="install"]') === "powershell", "Install method tabs stay synchronized across pages.");
    // The agent picker is the landing page's listbox: keyboard and pointer choices show one agent.
    await visit(`${docs}#agents-install`);
    const picker = page.locator('.tabs[data-sync="agent"] .picker-button');
    await picker.focus();
    await page.keyboard.press("ArrowDown");
    check(await picker.getAttribute("aria-expanded") === "true", "The keyboard opens the agent picker.");
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    check(await picker.evaluate((button) => button === document.activeElement && button.getAttribute("aria-expanded") === "false"), "Choosing an agent closes the picker and returns focus.");
    check(await selectedKey('.tabs[data-sync="agent"]') === await page.locator('.tabs[data-sync="agent"] > .tab-panel').last().getAttribute("data-key"), "End and Enter choose the last agent.");
    await picker.click();
    await page.locator('.tabs[data-sync="agent"] [role=option]', { hasText: "Cline" }).click();
    check(await selectedKey('.tabs[data-sync="agent"]') === "cline" && await page.locator('.tabs[data-sync="agent"] [role=listbox]').isHidden(), "Clicking an agent selects it and closes the picker.");

    // Copy to clipboard.
    await visit(`${docs}#install-manual`);
    const block = page.locator("#install .code").filter({ hasText: "chmod +x ahrara" });
    await block.getByRole("button", { name: text.copyCode, exact: true }).click();
    await page.waitForFunction((message) => document.getElementById("status").textContent === message, text.copiedStatus);
    check(await page.evaluate(() => navigator.clipboard.readText()) === await block.locator("pre").textContent(), "Copy preserves the code exactly.");
    check(await block.locator(".copy span").textContent() === text.copied, "The copy button confirms with the page's interface text.");
    const group = page.locator("#install .tabs").first();
    await group.locator(".code-bar .copy").click();
    check(await page.evaluate(() => navigator.clipboard.readText()) === await group.locator(".tab-panel:not([hidden]) pre").textContent(), "Tabbed examples copy the visible panel.");

    // Search.
    await page.evaluate(() => document.activeElement?.blur());
    await page.keyboard.press("/");
    check(await page.evaluate(() => document.activeElement.id === "search"), "Slash focuses search.");
    const search = page.locator("#search");
    await search.fill("zz-no-such-guide");
    check(await page.locator(".search-empty").isVisible(), "Search reports no matches.");
    await page.waitForFunction((message) => document.getElementById("status").textContent === message, text.noResults);
    await search.fill("<img src=x onerror=alert(1)>");
    check(await page.locator("#search-results img").count() === 0, "Search text stays inert.");
    await search.fill("mcp");
    const hits = await page.locator("#search-results a").count();
    check(hits > 1, "Search finds pages and sections.");
    await page.waitForFunction((message) => document.getElementById("status").textContent === message, text.resultMany.replace("{count}", hits));
    await page.keyboard.press("ArrowDown");
    check(await page.evaluate(() => document.activeElement.matches("#search-results a")), "Search results support the keyboard.");
    const destination = await page.evaluate(() => document.activeElement.getAttribute("href"));
    await page.keyboard.press("Enter");
    await page.waitForFunction((hash) => location.hash === hash && document.querySelector(".page:not([hidden])").contains(document.querySelector(hash)), destination);
    await page.waitForFunction(() => document.getElementById("search").value === "");

    // Theme toggle.
    const toggle = page.locator(".sidebar-head .theme-toggle");
    const dark = await page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches);
    check(await toggle.getAttribute("aria-label") === (dark ? text.themeLight : text.themeDark), "The theme toggle names the theme it switches to.");
    await toggle.click();
    check(await page.evaluate(() => document.documentElement.dataset.theme) === (dark ? "light" : "dark"), "The toggle switches the theme.");
    check(await toggle.getAttribute("aria-label") === (dark ? text.themeDark : text.themeLight), "The toggle label follows the theme.");
    await page.reload();
    check(await page.evaluate(() => document.documentElement.dataset.theme) === (dark ? "light" : "dark"), "The chosen theme is remembered.");
    await page.evaluate(() => localStorage.removeItem("ahrara-docs-theme"));

    // Docs language menu keeps the current page.
    await visit(`${docs}#ref-mcp`);
    const languageLinks = await page.locator(".lang-menu a").evaluateAll((links) => links.map((link) => ({ href: link.href, lang: link.hreflang, current: link.getAttribute("aria-current") })));
    check(languageLinks.map((link) => link.lang).join() === "en,pt-BR,es", "The docs offer English, Portuguese and Spanish.");
    check(languageLinks.every((link) => new URL(link.href).hash === "#ref-mcp"), "Language links keep the current page.");
    await page.locator(".nav a[href='#ref-api']").click();
    await expectPage("ref-api");
    check(await page.locator(".lang-menu a").evaluateAll((links) => links.every((link) => link.hash === "#ref-api")), "Language links follow navigation.");
    await page.locator("details.lang summary").click();
    check(await page.locator(".lang-menu a").first().isVisible(), "The docs language menu opens.");
    await page.keyboard.press("Escape");
    check(!await page.locator("details.lang").evaluate((menu) => menu.open), "Escape closes the docs language menu.");

    // Narrow screens: drawer navigation and no horizontal scrolling.
    await page.setViewportSize({ width: 390, height: 844 });
    await visit(docs);
    const menu = page.locator(".menu-toggle");
    check(await menu.getAttribute("aria-label") === text.menuOpen, "The menu button names its action.");
    await menu.click();
    check(await menu.getAttribute("aria-expanded") === "true" && await menu.getAttribute("aria-label") === text.menuClose, "The menu opens.");
    check(await page.locator("main").evaluate((element) => element.inert), "The open drawer takes focus away from the article.");
    await page.locator("details.lang summary").scrollIntoViewIfNeeded();
    check(await page.locator("details.lang summary").isVisible(), "The language menu is reachable on mobile.");
    await page.locator('.nav a[href="#ref-mcp"]').click();
    await expectPage("ref-mcp");
    check(await menu.getAttribute("aria-expanded") === "false", "Choosing a page closes the drawer.");
    await menu.click();
    await page.keyboard.press("Escape");
    check(await menu.getAttribute("aria-expanded") === "false" && await menu.evaluate((element) => element === document.activeElement), "Escape closes the drawer and restores focus.");
    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 844 });
      for (const route of routes) {
        await page.goto(`${docs}${route}`);
        await expectPage(route.slice(1));
        check(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `No horizontal scrolling on ${route} at ${width}px.`);
      }
    }
    for (const width of [320, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await visit(`${origin}/`);
      check(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `No horizontal scrolling on the landing page at ${width}px.`);
    }
    await page.setViewportSize({ width: 1440, height: 1000 });

    // Every language: same IDs, correct lang, readable without JavaScript.
    const noScript = await browser.newContext({ javaScriptEnabled: false });
    const checked = [];
    try {
      const staticPage = await noScript.newPage();
      let englishIds;
      for (const locale of languageLinks) {
        const url = locale.href.split("#")[0];
        const response = await page.request.get(url);
        const html = response.ok() ? await response.text() : "";
        if (!html.includes('id="ui-strings"')) {
          notes.push(`${locale.lang} ${response.ok() ? "still uses the old format" : "not present yet"}, skipped`);
          continue;
        }
        await staticPage.goto(url);
        const shape = await staticPage.evaluate(() => ({
          lang: document.documentElement.lang,
          ids: [...document.querySelectorAll("[id]")].map((element) => element.id).sort().join(),
          keys: Object.keys(JSON.parse(document.getElementById("ui-strings").textContent)).sort().join(),
          pages: [...document.querySelectorAll("article.page")].filter((article) => article.offsetHeight > 0 && article.querySelector("h1").textContent.trim()).length,
          languages: [...document.querySelectorAll(".lang-menu a")].map((link) => link.href.split("#")[0]),
          shared: [...document.querySelectorAll("script[src], link[rel=stylesheet]")].map((element) => new URL(element.src || element.href).pathname).sort().join(),
          images: [...document.images].every((image) => image.complete && image.naturalWidth > 0),
          search: document.querySelector(".search").hidden,
        }));
        englishIds ??= shape.ids;
        check(shape.lang === locale.lang, `${url} must declare lang="${locale.lang}".`);
        check(shape.ids === englishIds, `${url} must keep the same element IDs as English.`);
        check(shape.keys === keys, `${url} must provide the same #ui-strings keys.`);
        check(shape.pages === 16, `${url} must show all 16 pages without JavaScript.`);
        check(shape.languages.join() === languageLinks.map((link) => link.href.split("#")[0]).join(), `${url} must link to the same three languages.`);
        check(shape.shared === "/docs/script.js,/docs/styles.css,/listbox.js", `${url} must share the docs scripts and stylesheet.`);
        check(shape.images, `${url} must load its images without JavaScript.`);
        check(shape.search, `${url} must not offer search without JavaScript.`);
        // With JavaScript, a section link opens its page in this language.
        await visit(`${url}#config-file`);
        await expectPage("ref-config");
        check(await page.evaluate(() => document.documentElement.lang) === locale.lang, `${url} keeps its language with JavaScript.`);
        checked.push(locale.lang);
      }
      // Landing pages: same IDs and shared files in every language, readable without JavaScript.
      let landingIds;
      for (const [path, lang] of lpLanguages) {
        const url = origin + path;
        const response = await page.request.get(url);
        if (!response.ok()) {
          notes.push(`landing page ${lang} not present yet, skipped`);
          continue;
        }
        await staticPage.goto(url);
        const shape = await staticPage.evaluate(() => ({
          lang: document.documentElement.lang,
          ids: [...document.querySelectorAll("[id]")].map((element) => element.id).sort().join(),
          keys: Object.keys(JSON.parse(document.getElementById("ui-strings").textContent)).sort().join(),
          shared: [...document.querySelectorAll("script[src], link[rel=stylesheet]")].map((element) => new URL(element.src || element.href).pathname).sort().join(),
          readable: Boolean(document.querySelector("#hero-title").offsetHeight && document.querySelector("#install-command").textContent.trim()),
          images: [...document.images].every((image) => image.complete && image.naturalWidth > 0),
        }));
        landingIds ??= shape.ids;
        check(shape.lang === lang, `${url} must declare lang="${lang}".`);
        check(shape.ids === landingIds, `${url} must keep the same element IDs as English.`);
        check(shape.keys === Object.keys(lpText).sort().join(), `${url} must provide the same #ui-strings keys.`);
        check(shape.shared === "/listbox.js,/script.js,/styles.css", `${url} must share the landing page scripts and stylesheet.`);
        check(shape.readable && shape.images, `${url} must be readable without JavaScript.`);
        checked.push(`landing ${lang}`);
      }
    } finally {
      await noScript.close();
    }
    check(checked.includes("en") && checked.includes("landing en"), "English must pass the locale checks.");

    // Switching language keeps the page, for every converted locale.
    await visit(`${docs}#ref-mcp`);
    for (const lang of checked.filter((lang) => lang !== "en" && !lang.startsWith("landing"))) {
      await page.locator("details.lang summary").click();
      await page.locator(`.lang-menu a[hreflang="${lang}"]`).click();
      await page.waitForURL((url) => url.hash === "#ref-mcp" && url.pathname !== "/docs/");
      await expectPage("ref-mcp");
      check(await page.evaluate(() => document.documentElement.lang) === lang, `Switching to ${lang} opens that language.`);
      await page.locator("details.lang summary").click();
      await page.locator('.lang-menu a[hreflang="en"]').click();
      await page.waitForURL(`${docs}#ref-mcp`);
      await expectPage("ref-mcp");
    }

    check(!errors.length, `Browser errors: ${errors.join("; ")}`);
    return `PASS: landing page and docs links, 16 pages, tabs, clipboard, search, theme, mobile and languages (${checked.join(", ")})${notes.length ? "; " + notes.join("; ") : ""}.`;
  } finally {
    page.off("pageerror", onError);
    page.off("console", onConsole);
  }
}
