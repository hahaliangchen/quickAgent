// Restore architecture-specific optional packages omitted by an x64-generated
// npm lockfile. Install the parent's exact declared version without resolving
// or changing the rest of the application's locked dependency tree.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
if (process.platform !== 'linux' || process.arch !== 'arm64') throw Error('ARM64 Linux is required');
const root = process.cwd();
const modules = path.join(root, 'node_modules');
const parents = [];
for (const entry of fs.readdirSync(modules)) {
  if (entry.startsWith('.')) continue;
  const directory = path.join(modules, entry);
  if (entry.startsWith('@')) {
    for (const child of fs.readdirSync(directory)) parents.push(path.join(directory, child));
  } else parents.push(directory);
}
const wanted = new Map();
for (const parent of parents) {
  const file = path.join(parent, 'package.json');
  if (!fs.existsSync(file)) continue;
  const pkg = JSON.parse(fs.readFileSync(file));
  for (const [name, version] of Object.entries(pkg.optionalDependencies || {})) {
    if (!/linux.*(?:arm64|aarch64)/.test(name) || /musl|android/.test(name)) continue;
    if (!/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(version)) throw Error('Native dependency must be exact: ' + name + '@' + version);
    if (wanted.has(name) && wanted.get(name) !== version) throw Error('Conflicting native dependency: ' + name);
    wanted.set(name, version);
  }
}
const records = [];
for (const [name, version] of wanted) {
  const destination = path.join(modules, name);
  const manifest = path.join(destination, 'package.json');
  if (fs.existsSync(manifest) && JSON.parse(fs.readFileSync(manifest)).version === version) {
    records.push({ name, version, source: 'npm-ci' });
    continue;
  }
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'arm64-npm-'));
  try {
    const packed = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--registry=https://registry.npmjs.org', '--pack-destination', temp, name + '@' + version], { encoding: 'utf8' }))[0];
    const archive = path.join(temp, packed.filename);
    fs.mkdirSync(destination, { recursive: true });
    execFileSync('tar', ['-xzf', archive, '-C', destination, '--strip-components=1']);
    if (JSON.parse(fs.readFileSync(manifest)).version !== version) throw Error('Native package version mismatch');
    records.push({ name, version, integrity: packed.integrity, sha256: crypto.createHash('sha256').update(fs.readFileSync(archive)).digest('hex') });
    console.log('Restored ARM64 dependency: ' + name + '@' + version);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
}
fs.writeFileSync(path.join(root, 'arm64-native-dependencies.json'), JSON.stringify(records, null, 2) + '\n');
