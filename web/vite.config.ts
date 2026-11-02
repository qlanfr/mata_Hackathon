import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In development the API is the local aiopsd; in production the Worker serves both.
const api = process.env.AIOPS_API ?? "http://127.0.0.1:18090";

export default defineConfig({
  plugins: [react()],
  server: { proxy: { "/api": api } },
  preview: { proxy: { "/api": api } },
  build: { outDir: "dist", sourcemap: false },
});
