import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/ws': {
        target: 'ws://localhost:18789',
        ws: true,
      },
      '/health': {
        target: 'http://localhost:18789',
      },
    },
  },
  build: {
    outDir: '../crates/duduclaw-dashboard/dist',
    emptyOutDir: true,
  },
});
