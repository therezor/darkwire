import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    name: 'i18n',
    include: ['test/**/*.test.ts'],
  },
});
