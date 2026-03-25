import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

// Gateway backend URL — configurable via DUDUCLAW_GATEWAY env var (FE-L6)
const gatewayUrl = process.env.DUDUCLAW_GATEWAY ?? 'http://localhost:18789';
const gatewayWs = gatewayUrl.replace(/^http/, 'ws');

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
        target: gatewayWs,
        ws: true,
      },
      '/health': {
        target: gatewayUrl,
      },
    },
  },
  build: {
    outDir: '../crates/duduclaw-dashboard/dist',
    emptyOutDir: true,
  },
});
