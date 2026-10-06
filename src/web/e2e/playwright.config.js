import { defineConfig } from '@playwright/test';

// Runs against make web-app-e2e's build in .dev/web-e2e, an isolated relay and the dev certificates.
const root = new URL('../../../', import.meta.url).pathname;
export const app = 'http://127.0.0.1:28080';

export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.js',
  timeout: 180_000,
  workers: 1,
  reporter: 'list',
  use: { baseURL: app, ignoreHTTPSErrors: true, actionTimeout: 15_000 },
  projects: [
    { name: 'chromium', use: { browserName: 'chromium', launchOptions: { args: ['--ignore-certificate-errors'] } } },
    { name: 'firefox', use: { browserName: 'firefox' } },
  ],
  webServer: [
    { command: 'node serve.mjs', url: `${app}/`, reuseExistingServer: false },
    {
      command: `${root}dist/ahrara-server`,
      port: 28443,
      reuseExistingServer: false,
      env: {
        AHRARA_SMTP_BIND: '127.0.0.1:22525',
        AHRARA_HTTPS_BIND: '127.0.0.1:28443',
        AHRARA_TLS_CERT: `${root}.dev/certs/server.pem`,
        AHRARA_TLS_KEY: `${root}.dev/certs/server-key.pem`,
        AHRARA_TRANSPORT_KEY_FILE: `${root}.dev/certs/transport.key`,
        AHRARA_WEB_ORIGINS: app,
      },
    },
  ],
});
