import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

const DEV_DAEMON_URL = "http://127.0.0.1:7676";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": DEV_DAEMON_URL,
      "/ws": { target: DEV_DAEMON_URL, ws: true },
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
