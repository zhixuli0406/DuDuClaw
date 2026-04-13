"use strict";

// Verify the platform-specific binary was installed via optionalDependencies.
// If not, print a helpful message. The binary itself is in the platform package.

const PLATFORM_PACKAGES = {
  "darwin-arm64": "@duduclaw/darwin-arm64",
  "darwin-x64": "@duduclaw/darwin-x64",
  "linux-x64": "@duduclaw/linux-x64",
  "linux-arm64": "@duduclaw/linux-arm64",
};

const key = `${process.platform}-${process.arch}`;
const pkg = PLATFORM_PACKAGES[key];

if (!pkg) {
  console.warn(
    `\n[duduclaw] Warning: unsupported platform ${key}.\n` +
      `Supported: darwin-arm64, darwin-x64, linux-x64, linux-arm64.\n` +
      `You can install from source: https://github.com/zhixuli0406/DuDuClaw\n`
  );
  process.exit(0);
}

try {
  require.resolve(`${pkg}/package.json`);
} catch {
  console.warn(
    `\n[duduclaw] Warning: platform package ${pkg} was not installed.\n` +
      `This may happen with --no-optional. Try:\n` +
      `  npm install -g duduclaw\n`
  );
}
