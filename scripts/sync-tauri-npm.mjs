#!/usr/bin/env node
// Keeps the npm @tauri-apps packages on the same major.minor as the Rust
// crates they talk to. `tauri build` refuses to run when they drift, and
// Dependabot cannot edit package.json from a Cargo pull request, so without
// this the first Cargo-only Tauri bump leaves main unbuildable until a human
// fixes the npm side by hand.
//
// Usage: node scripts/sync-tauri-npm.mjs [--check]
//   default  rewrite package.json in place, printing what changed
//   --check  exit 1 when a rewrite is pending, without touching the file
import fs from "node:fs";

const PACKAGE_JSON = "package.json";
const CARGO_LOCK = "src-tauri/Cargo.lock";

// Only crates with a published npm counterpart are mapped. @tauri-apps/cli is
// published separately from the CLI crate and stays a plain Dependabot bump.
export function crateToNpmPackage(crate) {
  if (crate === "tauri") return "@tauri-apps/api";
  const plugin = "tauri-plugin-";
  if (crate.startsWith(plugin)) return `@tauri-apps/plugin-${crate.slice(plugin.length)}`;
  return null;
}

export function crateVersions(lockText) {
  const versions = {};
  for (const block of lockText.split("[[package]]").slice(1)) {
    const name = /^name = "([^"]+)"$/m.exec(block)?.[1];
    const version = /^version = "([^"]+)"$/m.exec(block)?.[1];
    if (!name || !version) continue;
    if (crateToNpmPackage(name)) versions[name] = version;
  }
  return versions;
}

// The CLI compares major.minor only, so the range is pinned to that pair: the
// package manager still picks the newest patch inside it.
export function desiredSpec(version) {
  const [major, minor] = String(version).split(".");
  return `^${major}.${minor}`;
}

export function planPackageJson(pkg, versions) {
  const deps = pkg.dependencies || {};
  const changes = [];
  for (const [crate, version] of Object.entries(versions)) {
    const name = crateToNpmPackage(crate);
    if (!name || !(name in deps)) continue;
    const to = desiredSpec(version);
    if (deps[name] !== to) changes.push({ name, from: deps[name], to });
  }
  return changes.sort((a, b) => a.name.localeCompare(b.name));
}

export function applyChanges(pkg, changes) {
  const next = structuredClone(pkg);
  next.dependencies = next.dependencies || {};
  for (const change of changes) next.dependencies[change.name] = change.to;
  return next;
}

function main(argv) {
  const checkOnly = argv.includes("--check");
  if (!fs.existsSync(CARGO_LOCK)) {
    console.error(`${CARGO_LOCK} is missing; nothing to align.`);
    process.exit(2);
  }
  const pkg = JSON.parse(fs.readFileSync(PACKAGE_JSON, "utf8"));
  const versions = crateVersions(fs.readFileSync(CARGO_LOCK, "utf8"));
  const changes = planPackageJson(pkg, versions);
  if (changes.length === 0) {
    console.log("npm @tauri-apps packages already match the Rust crates.");
    return;
  }
  for (const change of changes) {
    console.log(`${change.name}: ${change.from} -> ${change.to}`);
  }
  if (checkOnly) process.exit(1);
  fs.writeFileSync(PACKAGE_JSON, `${JSON.stringify(applyChanges(pkg, changes), null, 2)}\n`);
  console.log("package.json updated; run `pnpm install --lockfile-only` to refresh the lockfile.");
}

if (process.argv[1] && process.argv[1].endsWith("sync-tauri-npm.mjs")) {
  main(process.argv.slice(2));
}
