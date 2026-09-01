import { defineWorkersConfig } from "@cloudflare/vitest-pool-workers/config";

export default defineWorkersConfig({
  test: {
    poolOptions: {
      workers: {
        wrangler: { configPath: "./wrangler.jsonc" },
        isolatedStorage: false,
        miniflare: {
          bindings: { STREAM_TOKEN: "secret", EDGE_PUBLIC: "on" },
        },
      },
    },
  },
});
