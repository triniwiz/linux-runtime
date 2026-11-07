#!/usr/bin/env node
// Release identity for npm_release.yml's `setup` job, written to $GITHUB_OUTPUT:
// NPM_VERSION, NPM_TAG, PACKAGES (JSON matrix), ENGINES (Node-API engines for pack-npm.sh).
//
//   workflow_dispatch with a version -> that version (leading "v" allowed)
//   push of a v* tag                 -> the tag's version
//   workflow_dispatch without one    -> rolling "next" off the Cargo workspace version
import fs from "node:fs";
import path from "node:path";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";

// engine -> npm package; every package carries both x64 and arm64.
// napi is the linux-napi addon (pack-npm.sh treats it as an engine).
const PACKAGES = {
  classic: "@nativescript/linux",
  quickjs: "@nativescript/linux-quickjs",
  v8: "@nativescript/linux-v8",
  jsc: "@nativescript/linux-jsc",
  hermes: "@nativescript/linux-hermes",
  napi: "@nativescript/linux-napi",
};
const ENGINES = Object.keys(PACKAGES);

const { values } = parseArgs({
  options: {
    version: { type: "string", default: "" },
    engine: { type: "string", default: "all" },
  },
});

const fail = (message) => {
  console.error(`::error::${message}`);
  process.exit(1);
};

const SEMVER = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/;

const engine = values.engine.trim() || "all";
if (engine !== "all" && !ENGINES.includes(engine)) {
  fail(`Unknown engine "${engine}". Expected: all, ${ENGINES.join(", ")}`);
}
const engines = engine === "all" ? ENGINES : [engine];

let version = values.version.trim().replace(/^v/, "");
const ref = process.env.GITHUB_REF || "";
if (!version && ref.startsWith("refs/tags/")) {
  version = ref.slice("refs/tags/".length).replace(/^v/, "");
}
if (!version) {
  const root = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
  const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
  const declared = /\[workspace\.package\][^[]*?version\s*=\s*"([^"]+)"/.exec(cargo)?.[1];
  const m = SEMVER.exec(declared || "");
  if (!m) fail(`Cargo.toml workspace version is not semver: ${declared}`);
  // a released base takes a patch bump so "next" never trails "latest"
  const patch = m[4] ? Number(m[3]) : Number(m[3]) + 1;
  const date = new Date().toISOString().slice(0, 10);
  version = `${m[1]}.${m[2]}.${patch}-next.${date}-${process.env.GITHUB_RUN_ID || 0}`;
}

const parsed = SEMVER.exec(version);
if (!parsed) fail(`Release version is not valid semver: ${version}`);
// dist-tag: the first prerelease identifier, or "latest"
const id = parsed[4]?.split(".")[0];
if (id !== undefined && !/^[a-zA-Z]+$/.test(id)) {
  fail(`Version ${version} has no usable npm dist-tag (prerelease "${id}").`);
}
const tag = id ?? "latest";

const packages = engines.map((e) => {
  const name = PACKAGES[e];
  const dir = name.slice("@nativescript/".length);
  return { engine: e, npm_name: name, dir, tarball: `nativescript-${dir}-${version}.tgz` };
});

const out = [
  `NPM_VERSION=${version}`,
  `NPM_TAG=${tag}`,
  `PACKAGES=${JSON.stringify({ include: packages })}`,
  `ENGINES=${engines.filter((e) => e !== "classic").join(" ")}`,
].join("\n");
console.log(out);
if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, out + "\n");
