import { fileURLToPath } from 'node:url';

import { defineConfig } from 'vite';

// The output is embedded in the galata-desk binary and served from the root of
// a loopback port, so every path is relative and nothing is fetched from a CDN.
export default defineConfig({
  base: './',
  resolve: {
    alias: [
      // See src/no-modal.ts: the modal is never used, so it is not shipped.
      {
        find: /^@reown\/appkit(\/.*)?$/,
        replacement: fileURLToPath(new URL('./src/no-modal.ts', import.meta.url)),
      },
    ],
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2022',
    chunkSizeWarningLimit: 1500,
  },
});
