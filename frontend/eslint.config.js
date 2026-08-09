import js from "@eslint/js";
import tseslint from "typescript-eslint";

// Flat config for the frontend web lane's `lint` gate. Kept minimal: the
// recommended JS + typescript-eslint rule sets over the TS/TSX sources. No
// browser globals need declaring — typescript-eslint turns `no-undef` off for
// TS files and leaves undefined-name checking to `tsc`.
export default tseslint.config(
  { ignores: ["dist", "node_modules"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
);
