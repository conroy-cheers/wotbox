import { defineConfig, loadEnv } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig(({ mode }) => {
  const environment = loadEnv(mode, ".", "");
  const backend = environment.WOTBOX_DEV_BACKEND_URL ?? "http://127.0.0.1:8780";
  return {
    base: "./",
    plugins: [tailwindcss(), svelte()],
    server: {
      port: 5173,
      proxy: {
        "/api": backend,
        "/health": backend
      }
    },
    test: {
      environment: "jsdom"
    }
  };
});
