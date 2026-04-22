import { defineConfig } from 'vite';

export default defineConfig({
  base: '/blue-marble-front/',
  server: {
    port: 5173,
    host: true,
  },
  build: {
    outDir: '../dist',
    emptyOutDir: true,
  },
});
