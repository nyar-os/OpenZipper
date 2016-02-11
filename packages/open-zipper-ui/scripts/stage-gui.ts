/**
 * Copy the VMZ static delivery into the OpenZipper WebView content root.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const uiRoot = path.resolve(here, '..');
const from = path.join(uiRoot, 'dist', 'static');
const to = path.resolve(uiRoot, '..', 'open-zipper', 'dist', 'gui');

/**
 * Markers that only exist in a development server. The GUI content root is a
 * shipped artifact, so any of them means the delivery was built wrong and the
 * page would request a module the WebView cannot serve.
 */
const DEV_CLIENT_MARKERS = ['/@vite/client', '@react-refresh', 'import.meta.hot'];

/** Text assets that could carry a dev-client reference. */
const TEXT_EXTENSIONS = new Set(['.html', '.js', '.mjs', '.cjs', '.css', '.json', '.svg', '.map']);

function scanForDevClient(dir: string, violations: string[]): void {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) {
            scanForDevClient(full, violations);
            continue;
        }
        if (!TEXT_EXTENSIONS.has(path.extname(entry.name).toLowerCase())) continue;
        const text = fs.readFileSync(full, 'utf8');
        for (const marker of DEV_CLIENT_MARKERS) {
            const at = text.indexOf(marker);
            if (at === -1) continue;
            const line = text.slice(0, at).split('\n').length;
            violations.push(`${path.relative(uiRoot, full)}:${line} references ${marker}`);
        }
    }
}

function copyDir(src: string, dest: string): void {
    fs.mkdirSync(dest, { recursive: true });
    for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
        const s = path.join(src, entry.name);
        const d = path.join(dest, entry.name);
        if (entry.isDirectory()) copyDir(s, d);
        else fs.copyFileSync(s, d);
    }
}

if (!fs.existsSync(path.join(from, 'index.html'))) {
    console.error(`missing static delivery at ${from} (run vmz build first)`);
    process.exit(1);
}

const violations: string[] = [];
scanForDevClient(from, violations);
if (violations.length > 0) {
    console.error(`development-server client found in ${path.relative(uiRoot, from)}:`);
    for (const violation of violations) console.error(`  ${violation}`);
    console.error('the GUI content root must be a static build without dev-client references');
    process.exit(1);
}

fs.rmSync(to, { recursive: true, force: true });
copyDir(from, to);
console.log(`staged ${from} → ${to}`);
