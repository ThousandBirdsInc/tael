import {defineConfig} from 'vite'

export default defineConfig({
  clearScreen: false,
  server: {
    strictPort: true,
    port: 1420,
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2022', 'chrome108', 'safari15'],
    outDir: 'src-tauri/dist',
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
})
