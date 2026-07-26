#!/usr/bin/env node
// Thin launcher: exec the vendored fbx-agent binary for this platform, passing through argv + stdio.
// The package ships a prebuilt linux-x64 binary because fbx-agent only ever runs inside a Sprite
// (Debian/glibc, x86-64). On any other platform it fails loudly rather than silently doing nothing.
'use strict';

const path = require('path');
const { spawnSync } = require('child_process');

const platform = process.platform; // 'linux' on a Sprite
const arch = process.arch; // 'x64' on a Sprite

if (platform !== 'linux' || arch !== 'x64') {
  console.error(
    `fbx-agent: no prebuilt binary for ${platform}-${arch}. This package ships a linux-x64 build ` +
      `(it runs inside a Sprite VM). Install it on a linux-x64 host.`
  );
  process.exit(1);
}

const binary = path.join(__dirname, '..', 'vendor', 'linux-x64', 'fbx-agent');
const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });

if (result.error) {
  console.error(`fbx-agent: failed to launch ${binary}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
