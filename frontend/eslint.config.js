import js from "@eslint/js";
import tseslint from "typescript-eslint";

// Flat config for the frontend web lane's `lint` gate. Kept minimal: the
// recommended JS + typescript-eslint rule sets over the TS/TSX sources, with the
// browser and test globals the served-page plumbing and its smoke test rely on.
export default tseslint.config(
  { ignores: ["dist", "node_modules"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      globals: {
        window: "readonly",
        document: "readonly",
        atob: "readonly",
        globalThis: "readonly",
      },
    },
  },
);
