"use strict";

const PLATFORM_PACKAGES = {
  "darwin-arm64": "@duduclaw/pro-darwin-arm64",
  "darwin-x64": "@duduclaw/pro-darwin-x64",
};

const key = `${process.platform}-${process.arch}`;
const pkg = PLATFORM_PACKAGES[key];

if (!pkg) {
  console.warn(
    `\n[duduclaw-pro] Warning: unsupported platform ${key}.\n` +
      `DuDuClaw Pro currently supports: macOS (ARM64 / x64).\n` +
      `For other platforms, use the Community Edition: npm install -g duduclaw\n`
  );
  process.exit(0);
}

try {
  require.resolve(`${pkg}/package.json`);
} catch {
  console.warn(
    `\n[duduclaw-pro] Warning: platform package ${pkg} was not installed.\n` +
      `This may happen with --no-optional. Try:\n` +
      `  npm install -g duduclaw-pro\n`
  );
}
