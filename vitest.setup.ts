import { beforeAll, afterEach } from "vitest";
import { randomFillSync } from "crypto";
import { clearMocks } from "@tauri-apps/api/mocks";

// jsdom doesn't come with a WebCrypto implementation
beforeAll(() => {
  Object.defineProperty(window, 'crypto', {
    value: {
      getRandomValues: (buffer: any) => {
        return randomFillSync(buffer);
      },
    },
  });
});

afterEach(() => {
  clearMocks();
});
