import { defineConfig } from "vite";

// The Go bridge embeds a dist directory inside its own package, so the web
// build lands there for local `go test` / `go run`. Docker does the same.
export default defineConfig({
  build: {
    target: "es2022",
    assetsInlineLimit: 0,
    outDir: "../bridge/web/dist",
    emptyOutDir: false,
  },
});
