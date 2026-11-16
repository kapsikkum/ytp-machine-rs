import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

const backend = 'http://127.0.0.1:8765';

export default defineConfig({
  plugins: [
    sveltekit({
      adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html', precompress: false, strict: false })
    })
  ],
  server: {
    proxy: {
      '/api': { target: backend, changeOrigin: false, ws: true }
    }
  },
  // The same API proxy for `vite preview`, so a production build can be
  // exercised against the running server without rebuilding the Rust binary.
  preview: {
    proxy: {
      '/api': { target: backend, changeOrigin: false, ws: true }
    }
  }
});