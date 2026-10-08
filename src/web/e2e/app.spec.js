import { expect, test } from '@playwright/test';
import { readFileSync } from 'node:fs';
import nodemailer from 'nodemailer';
import { app } from './playwright.config.js';

const mailer = nodemailer.createTransport({
  host: '127.0.0.1',
  port: 22525,
  requireTLS: true,
  tls: { ca: readFileSync(new URL('../../../.dev/certs/ca.pem', import.meta.url)) },
});
const notes = Buffer.from('attachment bytes \u0000ÿ');
const pixel = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jZ2kAAAAASUVORK5CYII=', 'base64');

// The relay refuses mail until the browser holding the address is connected.
async function send(to, subject, { retry = true } = {}) {
  for (const started = Date.now(); ;) {
    try {
      return await mailer.sendMail({
        from: 'sender@example.test',
        to,
        subject,
        text: 'Your code is 123456.',
        html: '<p>Your code is 123456.</p><img src="https://example.test/pixel.png" alt="Remote"><img src="cid:pixel" alt="Inline">',
        attachments: [
          { filename: 'notes.txt', content: notes },
          { filename: 'pixel.png', content: pixel, cid: 'pixel' },
        ],
      });
    } catch (error) {
      if (!retry || Date.now() - started > 60_000) throw error;
      await new Promise(resolve => setTimeout(resolve, 500));
    }
  }
}

// Firefox reports every redirect a Service Worker answers to a navigation, then follows it.
const followedRedirect = /^\[JavaScript Error: "Failed to load ‘’\. A ServiceWorker intercepted the request/;

function errors(page) {
  const seen = [];
  page.on('pageerror', error => seen.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error' && !followedRedirect.test(message.text())) seen.push(message.text());
  });
  return seen;
}

// Every page lives in the shell's frame.
const ui = page => page.frameLocator('iframe');

async function onboard(page) {
  await page.goto('/');
  await ui(page).getByRole('button', { name: 'Create a new identity' }).click();
  await expect(page).toHaveURL(/\/backup$/);
  const key = (await ui(page).locator('code').textContent()).trim();
  await ui(page).getByLabel('I saved my Recovery Key somewhere safe').check();
  await ui(page).getByRole('button', { name: 'Continue' }).click();
  await expect(page).toHaveURL(/\/addresses#create-address$/);
  return key;
}

async function createAddress(page) {
  await ui(page).getByRole('button', { name: 'Create address' }).click();
  const address = ui(page).locator('.addresses-table .address-text').first();
  await expect(address).toHaveText(/@ahrara\.dev$/);
  return (await address.textContent()).trim();
}

test('receives and reads mail like the CLI page', async ({ page }) => {
  const seen = errors(page);
  page.on('dialog', dialog => dialog.accept());
  expect(await onboard(page)).toMatch(/^ahrara_sk_v1_/);
  // Once saved, the key is shown again only from Settings, after a confirmation.
  await page.goto('/backup');
  await expect(page).not.toHaveURL(/\/backup$/);
  await expect(ui(page).getByText(/^ahrara_sk_v1_/)).toHaveCount(0);
  await page.goto('/addresses');
  const address = await createAddress(page);
  await expect(ui(page).locator('[data-connection]')).toHaveText('Connected', { timeout: 60_000 });

  await ui(page).getByRole('link', { name: /^Inbox/ }).click();
  await expect(page).toHaveURL(/:28080\/$/);
  await send(address, 'Browser app check');
  await send(address, 'Another message');
  // The open inbox updates without a reload.
  await expect(ui(page).locator('.subject-cell')).toHaveCount(2);
  await ui(page).getByLabel('Search sender or subject').fill('app check');
  await ui(page).getByRole('button', { name: 'Search' }).click();
  await expect(ui(page).locator('.subject-cell')).toHaveText(['Browser app check']);
  await ui(page).getByRole('link', { name: 'Browser app check' }).click();
  await expect(page).toHaveURL(/\/emails\/\d+$/);
  await expect(ui(page).locator('.plain-text')).toHaveText('Your code is 123456.');
  await expect(ui(page).locator('iframe[name="ahrara-email-html"]')).toHaveAttribute('sandbox', '');
  await expect(ui(page).getByRole('button', { name: 'Load images' })).toBeVisible();
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    ui(page).locator('.attachments li', { hasText: 'notes.txt' }).getByRole('link').click(),
  ]);
  expect(download.suggestedFilename()).toBe('notes.txt');
  expect(readFileSync(await download.path()).equals(notes)).toBe(true);

  await ui(page).getByText('Raw EML & metadata').click();
  await ui(page).getByRole('link', { name: 'View raw EML' }).click();
  await expect(ui(page).locator('pre.raw')).toContainText('Subject: Browser app check');
  await page.goBack();
  await expect(page).toHaveURL(/\/emails\/\d+$/);
  await page.goForward();
  await expect(page).toHaveURL(/\/emails\/\d+\/raw$/);
  await page.reload();
  await expect(ui(page).locator('pre.raw')).toContainText('Subject: Browser app check');

  await ui(page).getByRole('link', { name: 'View message' }).click();
  await ui(page).getByRole('button', { name: 'Delete email' }).click();
  await expect(page).toHaveURL(/:28080\/$/);
  await expect(ui(page).locator('.subject-cell')).toHaveText(['Another message']);

  // A disabled address refuses delivery while the browser stays connected.
  await ui(page).getByRole('link', { name: 'Addresses' }).click();
  await ui(page).getByTitle(`Disable ${address}`).click();
  await expect(ui(page).getByTitle(`Enable ${address}`)).toHaveText('Disabled');
  await expect(send(address, 'Refused', { retry: false })).rejects.toMatchObject({ responseCode: 550 });
  expect(seen).toEqual([]);
});

test('moves an Identity to another browser and hands the connection between tabs', async ({ browser }) => {
  const first = await browser.newContext();
  const page = await first.newPage();
  const key = await onboard(page);
  const address = await createAddress(page);
  await ui(page).getByRole('link', { name: 'Settings' }).click();
  await ui(page).getByLabel('Show it on this screen').check();
  await ui(page).getByRole('button', { name: 'Show Recovery Key' }).click();
  await expect(ui(page).locator('section', { hasText: 'Recovery Key' }).locator('code')).toHaveText(key);
  const fingerprint = await ui(page).locator('dd.fingerprint').first().textContent();
  await first.close();

  const second = await browser.newContext();
  const tab = await second.newPage();
  const seen = errors(tab);
  await tab.goto('/');
  await ui(tab).getByLabel('Recovery Key').fill(key);
  await ui(tab).getByRole('button', { name: 'Import Recovery Key' }).click();
  await expect(tab).toHaveURL(/\/addresses$/);
  await ui(tab).getByText('Add an existing address').click();
  await ui(tab).getByLabel('Address', { exact: true }).fill(address);
  await ui(tab).getByRole('button', { name: 'Add address' }).click();
  await expect(ui(tab).locator('.addresses-table .address-text')).toHaveText([address]);
  await ui(tab).getByRole('link', { name: 'Settings' }).click();
  await expect(ui(tab).locator('dd.fingerprint').first()).toHaveText(fingerprint);

  const other = await second.newPage();
  await other.goto('/');
  await expect(ui(other).locator('[data-connection]')).toHaveText('Connected', { timeout: 60_000 });
  // Closing the tab that holds the connection lets the other one connect.
  await tab.close();
  await send(address, 'After the handover');
  await expect(ui(other).getByRole('link', { name: 'After the handover' })).toBeVisible({ timeout: 60_000 });
  expect(seen).toEqual([]);
  await second.close();
});

test('Settings chooses the language', async ({ page }) => {
  await onboard(page);
  await ui(page).getByRole('link', { name: 'Settings' }).click();
  await ui(page).getByLabel('Language').selectOption('es');
  await ui(page).getByRole('button', { name: 'Save', exact: true }).click();
  await expect(ui(page).getByRole('heading', { name: 'Configuración', level: 1 })).toBeVisible();
  await expect(page).toHaveTitle('Configuración · Ahrara');
});

test('follows the browser language', async ({ browserName, playwright }) => {
  // Playwright's locale emulation does not reach Service Workers; Firefox's own preference does.
  test.skip(browserName !== 'firefox', 'headless Chromium cannot change the Service Worker language');
  const browser = await playwright.firefox.launch({ firefoxUserPrefs: { 'intl.accept_languages': 'pt-BR, pt' } });
  const page = await browser.newPage({ baseURL: app });
  await page.goto('/');
  await expect(ui(page).getByRole('heading', { name: 'Sua caixa de entrada, neste navegador.' })).toBeVisible();
  await browser.close();
});

test('marks what the browser version lacks', async ({ page }) => {
  await page.goto('/');
  const notice = ui(page).locator('.browser-notice');
  await expect(notice).toContainText("doesn't include every feature of the installed Ahrara");
  await expect(notice.getByRole('link', { name: 'See more' }))
    .toHaveAttribute('href', 'https://ahrara.dev/docs/#receive-app-support');
  await onboard(page);
  await expect(notice).toBeVisible();
  await ui(page).getByRole('link', { name: 'Settings' }).click();
  // The installed app's options stay visible, disabled, with the reason on hover.
  for (const [name, reason] of [['Enable LAN access', /local server/], ['Set password', /local server/], ['Enable MCP', /AI agents/]]) {
    const option = ui(page).getByRole('button', { name });
    await expect(option).toBeDisabled();
    await expect(option).toHaveAttribute('title', reason);
  }
  await ui(page).getByRole('button', { name: 'Enable MCP' }).click({ force: true });
  await expect(page).toHaveURL(/\/settings$/);
});
