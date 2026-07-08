import { defineConfig, mergeConfig } from 'vitest/config';
import viteConfig from '../vite.config';

export default defineConfig(async (env) => {
  // @ts-ignore
  const config = viteConfig.default || viteConfig;
  const resolvedViteConfig = await (typeof config === 'function' ? config(env) : config);
  return mergeConfig(resolvedViteConfig, {
    test: {
      environment: 'jsdom',
      setupFiles: ['./vitest.setup.ts'],
    },
  });
});
