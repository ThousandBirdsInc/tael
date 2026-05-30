import {defineConfig} from 'vite'

export default defineConfig({
  plugins: [
    {
      name: 'strip-crossorigin-for-tauri-assets',
      enforce: 'post',
      transformIndexHtml(html) {
        return html.replace(/\s+crossorigin(?:=(["']).*?\1)?/g, '')
      },
    },
  ],
  base: './',
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
    rollupOptions: {
      output: {
        entryFileNames: 'assets/index.js',
        assetFileNames: assetInfo => assetInfo.names?.includes('index.css') ? 'assets/index.css' : 'assets/[name][extname]',
      },
    },
  },
})
