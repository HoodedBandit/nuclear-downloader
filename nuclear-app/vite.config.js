/// <reference types="node" />

import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import { execSync } from "node:child_process";

const DEV_PORT = 1420;

// Kill any stale process on our port before starting
function killStalePort() {
  try {
    const result = execSync(
      `netstat -ano | findstr :${DEV_PORT} | findstr LISTENING`,
      { encoding: "utf-8", stdio: ["pipe", "pipe", "pipe"] }
    );
    const lines = result.trim().split("\n");
    for (const line of lines) {
      const pid = line.trim().split(/\s+/).pop();
      if (pid && pid !== "0") {
        try {
          execSync(`taskkill /PID ${pid} /F`, { stdio: "ignore" });
          console.log(`Killed stale process on port ${DEV_PORT} (PID ${pid})`);
        } catch {}
      }
    }
  } catch {
    // No process on port — good
  }
}

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(({ command }) => {
  if (command === "serve") {
    killStalePort();
  }

  return {
    plugins: [sveltekit()],

    // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
    clearScreen: false,
    server: {
      port: DEV_PORT,
      strictPort: true,
      host: host || false,
      hmr: host
        ? {
            protocol: "ws",
            host,
            port: 1421,
          }
        : undefined,
      watch: {
        ignored: ["**/src-tauri/**"],
      },
    },
  };
});
