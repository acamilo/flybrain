import { readFile } from 'node:fs/promises';

const minimumAgeMs = 30 * 24 * 60 * 60 * 1000;
const cutoff = new Date(Date.now() - minimumAgeMs);
const lock = JSON.parse(await readFile(new URL('../package-lock.json', import.meta.url), 'utf8'));
const dependencies = new Map();

for (const [path, metadata] of Object.entries(lock.packages)) {
  if (!path.startsWith('node_modules/') || !metadata.version) continue;
  const name = path.split('node_modules/').at(-1);
  dependencies.set(`${name}@${metadata.version}`, { name, version: metadata.version });
}

const failures = [];
for (const { name, version } of dependencies.values()) {
  const response = await fetch(`https://registry.npmjs.org/${name.replace('/', '%2f')}`);
  if (!response.ok) throw new Error(`Unable to inspect ${name}: HTTP ${response.status}`);
  const packument = await response.json();
  const published = packument.time?.[version];
  if (!published) failures.push(`${name}@${version}: publication date unavailable`);
  else if (new Date(published) > cutoff) failures.push(`${name}@${version}: published ${published}`);
}

if (failures.length) {
  console.error(`Packages newer than the 30-day cutoff (${cutoff.toISOString()}):\n${failures.join('\n')}`);
  process.exitCode = 1;
} else {
  console.log(`${dependencies.size} locked packages were published before ${cutoff.toISOString()}`);
}
