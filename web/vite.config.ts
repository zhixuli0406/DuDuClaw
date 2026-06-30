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
    // Bind IPv4 127.0.0.1 explicitly so the Tauri desktop shell's dev poller
    // (which waits on http://127.0.0.1:5173) matches — the Vite default
    // "localhost" can resolve to IPv6 ::1 and leave Tauri waiting forever.
    // strictPort fails loudly instead of silently moving off 5173 (which would
    // break tauri.conf.json devUrl). Use `npm run dev -- --host` to expose on LAN.
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy: {
      '/ws': {
        target: gatewayWs,
        ws: true,
      },
      '/health': {
        target: gatewayUrl,
      },
      // Dev-only: forward REST API calls (e.g. /api/login, /api/refresh,
      // /api/me) to the gateway so `npm run dev` works without CORS issues.
      '/api': {
        target: gatewayUrl,
      },
    },
  },
  build: {
    outDir: '../crates/duduclaw-dashboard/dist',
    emptyOutDir: true,
  },
});
