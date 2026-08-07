import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The Rust crate embeds `frontend/dist` via rust-embed, so emit a
// self-contained static bundle with relative asset URLs.
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
