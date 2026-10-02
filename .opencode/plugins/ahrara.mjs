import { fileURLToPath } from 'node:url';

export default async () => ({
  config: async (config) => {
    const skills = fileURLToPath(new URL('../../skills', import.meta.url));
    config.skills ??= {};
    config.skills.paths ??= [];
    if (!config.skills.paths.includes(skills)) config.skills.paths.push(skills);
    config.mcp ??= {};
    config.mcp.ahrara ??= {
      type: 'local',
      command: [process.execPath, fileURLToPath(new URL('../../plugin/ahrara.mjs', import.meta.url))],
      enabled: true,
      timeout: 330000,
    };
  },
});
