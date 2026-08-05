import { readFile, readdir, stat } from 'node:fs/promises';
import { dirname, extname, join, relative, resolve } from 'node:path';
import process from 'node:process';
import YAML from 'yaml';

const root = process.cwd();
const wikiRoot = join(root, 'docs', 'wiki');
const reserved = new Set(['index.md', 'log.md']);
const errors = [];

async function walk(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = await Promise.all(
    entries.map((entry) => {
      const path = join(directory, entry.name);
      return entry.isDirectory() ? walk(path) : [path];
    }),
  );
  return files.flat();
}

function withoutFences(markdown, file) {
  let fence;
  const visible = [];

  markdown.split('\n').forEach((line, index) => {
    const marker = line.match(/^\s*(`{3,}|~{3,})/u)?.[1];
    if (marker) {
      if (!fence) {
        fence = marker;
      } else if (marker[0] === fence[0] && marker.length >= fence.length) {
        fence = undefined;
      }
      return;
    }
    if (!fence) visible.push(line);
    if (/\s$/u.test(line)) errors.push(`${relative(root, file)}:${index + 1} trailing whitespace`);
  });

  if (fence) errors.push(`${relative(root, file)} has an unclosed code fence`);
  return visible.join('\n');
}

async function checkLink(file, target) {
  if (!target || target.startsWith('#') || /^[a-z][a-z\d+.-]*:/iu.test(target)) return;
  const cleanTarget = target.replace(/^<|>$/gu, '').split(/[?#]/u, 1)[0];
  if (!cleanTarget) return;
  const destination = resolve(dirname(file), cleanTarget);
  try {
    await stat(destination);
  } catch {
    errors.push(`${relative(root, file)} has broken link ${target}`);
  }
}

const wikiFiles = (await walk(wikiRoot)).filter((file) => extname(file) === '.md').sort();
const linkedFiles = [
  join(root, 'AGENTS.md'),
  join(root, 'PRODUCT.md'),
  join(root, 'README.md'),
  join(root, 'docs', 'PRD-v0.1.md'),
  ...wikiFiles,
];

for (const file of linkedFiles) {
  const markdown = await readFile(file, 'utf8');
  const visible = withoutFences(markdown, file);

  if (file.startsWith(wikiRoot) && !reserved.has(file.split('/').at(-1))) {
    const frontmatter = markdown.match(/^---\n([\s\S]*?)\n---\n/u)?.[1];
    if (!frontmatter) {
      errors.push(`${relative(root, file)} is missing YAML frontmatter`);
    } else {
      try {
        const data = YAML.parse(frontmatter);
        if (!data?.type || !data?.status) {
          errors.push(`${relative(root, file)} requires non-empty type and status`);
        }
      } catch (error) {
        errors.push(`${relative(root, file)} has invalid YAML: ${error.message}`);
      }
    }
  }

  const links = [...visible.matchAll(/(?<!!)\[[^\]]*\]\(([^)]+)\)/gu)];
  await Promise.all(links.map((match) => checkLink(file, match[1].trim())));
}

if (errors.length) {
  console.error(
    `Documentation validation failed:\n${errors.map((error) => `- ${error}`).join('\n')}`,
  );
  process.exit(1);
}

console.log(`Documentation validation passed for ${wikiFiles.length} Wiki files.`);
