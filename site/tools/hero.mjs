#!/usr/bin/env node
// Builds the homepage's live hero into site/public/hero/, which is committed
// (Cloudflare Pages serves the site as files, with no build step):
//
//   1. crates/romulus-wasm for wasm32 (the `wasm` profile, `--locked`),
//      through wasm-bindgen (the CLI must match the crate's wasm-bindgen
//      version) and wasm-opt (binaryen, the version below) into
//      web/.hero/romulus-wasm/;
//   2. web/src/hero with vite.hero.config.ts: hero.js, hero.css, chunks and
//      the two wasm files (romulus-wasm's, and libpg-query's parser);
//   3. manifest.json: every file with its SHA-256 and sizes, and hashes of
//      what the build is made of, so `--check` tells when it lags behind.
//
//   node site/tools/hero.mjs           build
//   node site/tools/hero.mjs --check   exit 1 if a built file differs from the
//                                      manifest (edited, missing, extra) or
//                                      the hero's own inputs changed since;
//                                      a note if the engine's dependencies or
//                                      the app's components did
//
// Inputs, and what a change to them does to `--check`:
//   own     the hero's sources, crates/romulus-wasm, this file, the `wasm`
//           profile: fails (rebuild with `just site-hero` and commit);
//   engine  what else goes into romulus-wasm: romulus-diff, romulus-migration
//           and every crate they and the engine resolve to in Cargo.lock
//           (remus-core…): a note;
//   app     web's components, features, styles and its lockfile: a note.
// The last two are shared with the app and the API: failing on them would
// make every PR touching them rebuild and commit the hero's binaries (and
// conflict with each other on them). The note says the hero is behind.
// site/package-lock.json is not an input: it holds the site's test tools only.
//
// Needs: rustup's wasm32-unknown-unknown target, `cargo install
// wasm-bindgen-cli --version <the crate's> --locked`, binaryen's wasm-opt
// (version WASM_OPT below: another version writes other bytes).

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { brotliCompressSync, gzipSync } from "node:zlib";

const WASM_OPT = "133";

const repo = join(dirname(fileURLToPath(import.meta.url)), "../..");
const out = join(repo, "site/public/hero");
const check = process.argv.includes("--check");

/** Every file under the paths, sorted, skipping build output and dependencies. */
function files(paths) {
  const list = [];
  const walk = (path) => {
    if (!existsSync(path)) return;
    if (statSync(path).isDirectory()) {
      for (const name of readdirSync(path)) {
        if (["node_modules", "target", ".hero", "dist"].includes(name)) continue;
        walk(join(path, name));
      }
    } else list.push(path);
  };
  paths.forEach((p) => walk(join(repo, p)));
  return list.sort();
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** One hash of the files under `paths` and of `extra` strings. */
function hash(paths, extra = []) {
  const h = createHash("sha256");
  for (const file of files(paths)) {
    h.update(relative(repo, file));
    h.update(readFileSync(file));
  }
  extra.forEach((e) => h.update(e));
  return h.digest("hex").slice(0, 16);
}

// ---- Inputs -----------------------------------------------------------------

/** `[profile.wasm]` of the workspace's Cargo.toml: how the engine is compiled. */
function wasmProfile() {
  const toml = readFileSync(join(repo, "Cargo.toml"), "utf8");
  return toml.match(/^\[profile\.wasm\][^[]*/m)?.[0] ?? "";
}

/** Cargo.lock's packages, by name. */
function lockfile() {
  const packages = new Map();
  for (const block of readFileSync(join(repo, "Cargo.lock"), "utf8").split("[[package]]").slice(1)) {
    const field = (key) => block.match(new RegExp(`^${key} = "([^"]*)"`, "m"))?.[1];
    const deps = [...(block.match(/^dependencies = \[([^\]]*)\]/m)?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    const pkg = { name: field("name"), version: field("version"), source: field("source"), checksum: field("checksum"), deps };
    packages.set(pkg.name, [...(packages.get(pkg.name) ?? []), pkg]);
  }
  return packages;
}

/** The names a workspace crate depends on to build (not its dev-dependencies). */
function buildDependencies(dir) {
  const names = new Set();
  let section = "";
  for (const line of readFileSync(join(repo, dir, "Cargo.toml"), "utf8").split("\n")) {
    const header = line.match(/^\[(.+)\]\s*$/);
    if (header) section = header[1];
    else if (/(^|\.)(dependencies|build-dependencies)$/.test(section) && !section.endsWith("dev-dependencies")) {
      const name = line.match(/^([A-Za-z0-9_-]+)\s*(\.|=)/)?.[1];
      if (name) names.add(name);
    }
  }
  return names;
}

/**
 * What romulus-wasm is compiled from besides its own sources: the workspace
 * crates it uses (their files) and every registry crate in their closure
 * (name, version, checksum), as Cargo.lock resolves them.
 */
function engineInputs() {
  const lock = lockfile();
  const find = (spec) => {
    const [name, version] = spec.split(" ");
    const all = lock.get(name) ?? [];
    return version ? all.find((p) => p.version === version) : all[0];
  };
  const seen = new Set();
  const paths = [];
  const crates = [];
  const visit = (pkg) => {
    const key = `${pkg.name} ${pkg.version}`;
    if (seen.has(key)) return;
    seen.add(key);
    let deps = pkg.deps;
    if (!pkg.source) {
      // A workspace crate: its own files, and its build dependencies only.
      const dir = `crates/${pkg.name}`;
      if (pkg.name !== "romulus-wasm") paths.push(`${dir}/src`, `${dir}/Cargo.toml`);
      const wanted = buildDependencies(dir);
      deps = deps.filter((d) => wanted.has(d.split(" ")[0]));
    } else crates.push(`${key} ${pkg.checksum ?? pkg.source}`);
    deps.map(find).filter(Boolean).forEach(visit);
  };
  visit(find("romulus-wasm"));
  return { paths, crates: crates.sort() };
}

function inputs() {
  const own = hash(
    ["crates/romulus-wasm/src", "crates/romulus-wasm/Cargo.toml", "web/src/hero", "web/vite.hero.config.ts", "site/tools/hero.mjs"],
    [wasmProfile()],
  );
  const { paths, crates } = engineInputs();
  const engine = hash(paths, crates);
  const app = hash(["web/src/components", "web/src/features", "web/src/lib", "web/src/styles", "web/package.json", "web/package-lock.json"]);
  return { own, engine, app };
}

// ---- Check --------------------------------------------------------------------

const built = () => files(["site/public/hero"]).filter((f) => !f.endsWith("manifest.json"));

if (check) {
  const path = join(out, "manifest.json");
  if (!existsSync(path)) {
    console.error("hero: site/public/hero is not built; run `just site-hero` and commit");
    process.exit(1);
  }
  const manifest = JSON.parse(readFileSync(path, "utf8"));
  const listed = manifest.files ?? {};
  const problems = [];
  for (const file of built()) {
    const name = relative(out, file);
    if (!listed[name]) problems.push(`${name}: not in the manifest`);
    else if (listed[name].sha256 !== sha256(readFileSync(file))) problems.push(`${name}: differs from the build`);
  }
  for (const name of Object.keys(listed)) {
    if (!existsSync(join(out, name))) problems.push(`${name}: missing`);
  }
  if (problems.length) {
    console.error(`hero: site/public/hero is not what the build wrote:\n  ${problems.join("\n  ")}\nrun \`just site-hero\` and commit everything it writes`);
    process.exit(1);
  }
  const now = inputs();
  if (manifest.inputs?.own !== now.own) {
    console.error("hero: the hero's sources changed since site/public/hero was built; run `just site-hero` and commit");
    process.exit(1);
  }
  if (manifest.inputs.engine !== now.engine)
    console.log("hero: note: romulus-diff, romulus-migration or a crate the engine uses changed since the hero was built (`just site-hero` brings them in)");
  if (manifest.inputs.app !== now.app)
    console.log("hero: note: the app's components or packages changed since the hero was built (`just site-hero` brings them in)");
  process.exit(0);
}

// ---- Build --------------------------------------------------------------------

const run = (command, args, options = {}) =>
  execFileSync(command, args, { stdio: "inherit", cwd: repo, ...options });

/** A tool on the PATH, or in cargo's bin directory. */
function tool(name, hint) {
  const cargoBin = join(process.env.CARGO_HOME ?? join(homedir(), ".cargo"), "bin", name);
  for (const candidate of [name, cargoBin]) {
    try {
      execFileSync(candidate, ["--version"], { stdio: "ignore" });
      return candidate;
    } catch {
      // Not this one.
    }
  }
  console.error(`hero: ${name} is missing: ${hint}`);
  process.exit(1);
}

const lock = readFileSync(join(repo, "Cargo.lock"), "utf8");
const bindgenVersion = lock.match(/name = "wasm-bindgen"\nversion = "([^"]+)"/)?.[1];
const wasmBindgen = tool("wasm-bindgen", `cargo install wasm-bindgen-cli --version ${bindgenVersion} --locked`);
const cliVersion = execFileSync(wasmBindgen, ["--version"]).toString().trim().split(" ").pop();
if (cliVersion !== bindgenVersion) {
  console.error(`hero: wasm-bindgen ${cliVersion} does not match the crate's ${bindgenVersion}: cargo install wasm-bindgen-cli --version ${bindgenVersion} --locked`);
  process.exit(1);
}
const optHint = `install binaryen ${WASM_OPT} (https://github.com/WebAssembly/binaryen/releases/tag/version_${WASM_OPT}; brew install binaryen)`;
const wasmOpt = tool("wasm-opt", optHint);
const optVersion = execFileSync(wasmOpt, ["--version"]).toString().match(/version (\d+)/)?.[1];
if (optVersion !== WASM_OPT) {
  console.error(`hero: wasm-opt ${optVersion} is not ${WASM_OPT}, which the committed build uses: ${optHint}`);
  process.exit(1);
}

run("cargo", ["build", "-q", "--locked", "-p", "romulus-wasm", "--target", "wasm32-unknown-unknown", "--profile", "wasm"]);
const pkg = join(repo, "web/.hero/romulus-wasm");
run(wasmBindgen, [
  "--target", "web",
  "--no-typescript",
  // `__wbg_reset_state`: a fresh instance after a trap (engine.ts).
  "--experimental-reset-state-function",
  "--out-dir", pkg,
  join(repo, "target/wasm32-unknown-unknown/wasm/romulus_wasm.wasm"),
]);
const wasm = join(pkg, "romulus_wasm_bg.wasm");
// The features the module declares (its target_features section), not
// `--all-features`: that enables proposals browsers do not ship (compact
// imports), which wasm-opt then uses.
run(wasmOpt, ["-Oz", "--strip-debug", "--strip-producers", "-o", wasm, wasm]);
run("npx", ["vite", "build", "--config", "vite.hero.config.ts", "--logLevel", "warn"], { cwd: join(repo, "web") });

// Every file, its hash, and its sizes as sent (Pages compresses with brotli,
// gzip for older clients).
const listed = Object.fromEntries(
  built().map((f) => {
    const bytes = readFileSync(f);
    return [
      relative(out, f),
      { sha256: sha256(bytes), raw: bytes.length, gzip: gzipSync(bytes, { level: 9 }).length, brotli: brotliCompressSync(bytes).length },
    ];
  }),
);
const total = (key) => Object.values(listed).reduce((n, s) => n + s[key], 0);
writeFileSync(
  join(out, "manifest.json"),
  JSON.stringify(
    {
      note: "Built by site/tools/hero.mjs: do not edit. `--check` compares the files with `files` and the inputs with `inputs`.",
      inputs: inputs(),
      total: { raw: total("raw"), gzip: total("gzip"), brotli: total("brotli") },
      files: listed,
    },
    null,
    2,
  ) + "\n",
);
const kb = (n) => `${(n / 1024).toFixed(0)} KB`;
console.log(`hero: built site/public/hero (${kb(total("raw"))} raw, ${kb(total("gzip"))} gzip, ${kb(total("brotli"))} brotli)`);
