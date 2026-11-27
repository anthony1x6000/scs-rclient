// Guards the npm/Rust Tauri version mapping: the tauri CLI refuses to build
// when the npm @tauri-apps packages drift from the crates, and Dependabot
// cannot edit package.json from a Cargo pull request.
// Run with `node --test scripts/sync-tauri-npm.test.mjs`.
import test from "node:test";
import assert from "node:assert/strict";
import { applyChanges, crateVersions, planPackageJson, specMinor } from "./sync-tauri-npm.mjs";

const lock = `version = 4

[[package]]
name = "serde"
version = "1.0.154"

[[package]]
name = "tauri"
version = "2.12.6"

[[package]]
name = "tauri-plugin-store"
version = "2.5.0"

[[package]]
name = "tauri-utils"
version = "2.7.0"
`;

test("reads only the tauri crates that map to an npm package", () => {
  assert.deepEqual(crateVersions(lock), {
    tauri: "2.12.6",
    "tauri-plugin-store": "2.5.0"
  });
});

test("maps crate versions to the matching npm minor", () => {
  const pkg = {
    dependencies: {
      "@tauri-apps/api": "^2",
      "@tauri-apps/plugin-store": "^2",
      react: "^19.1.0"
    }
  };
  assert.deepEqual(planPackageJson(pkg, crateVersions(lock)), [
    { name: "@tauri-apps/api", from: "^2", to: "^2.12" },
    { name: "@tauri-apps/plugin-store", from: "^2", to: "^2.5" }
  ]);
});

test("reads the major.minor out of a range, whatever its precision", () => {
  assert.equal(specMinor("^2"), "2");
  assert.equal(specMinor("^2.12"), "2.12");
  assert.equal(specMinor("^2.12.4"), "2.12");
  assert.equal(specMinor("~2.12.4"), "2.12");
  assert.equal(specMinor("2.12.4"), "2.12");
  assert.equal(specMinor("latest"), null);
});

test("a patch-qualified range on the right minor is already aligned", () => {
  const pkg = {
    dependencies: {
      "@tauri-apps/api": "^2.12.4",
      "@tauri-apps/plugin-store": "^2.5.0"
    }
  };
  assert.deepEqual(planPackageJson(pkg, crateVersions(lock)), []);
});

test("an older minor is rewritten to the crate minor", () => {
  const pkg = {
    dependencies: {
      "@tauri-apps/api": "^2.11.0",
      "@tauri-apps/plugin-store": "^2.5.0"
    }
  };
  assert.deepEqual(planPackageJson(pkg, crateVersions(lock)), [
    { name: "@tauri-apps/api", from: "^2.11.0", to: "^2.12" }
  ]);
});

test("an aligned manifest plans nothing", () => {
  const pkg = {
    dependencies: {
      "@tauri-apps/api": "^2.12",
      "@tauri-apps/plugin-store": "^2.5"
    }
  };
  assert.deepEqual(planPackageJson(pkg, crateVersions(lock)), []);
});

test("packages this repo does not depend on are never added", () => {
  const pkg = { dependencies: { "@tauri-apps/api": "^2.12" } };
  assert.deepEqual(planPackageJson(pkg, crateVersions(lock)), []);
});

test("applying the plan is idempotent and leaves other deps alone", () => {
  const pkg = {
    dependencies: {
      "@tauri-apps/api": "^2",
      react: "^19.1.0"
    }
  };
  const changes = planPackageJson(pkg, crateVersions(lock));
  const next = applyChanges(pkg, changes);
  assert.equal(next.dependencies["@tauri-apps/api"], "^2.12");
  assert.equal(next.dependencies.react, "^19.1.0");
  assert.deepEqual(planPackageJson(next, crateVersions(lock)), []);
});
