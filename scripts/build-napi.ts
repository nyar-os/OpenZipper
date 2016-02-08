/**
 * Build the N-API cdylib with cargo and stage it into the host platform package.
 *
 * Build-time only. The product never shells out to archive tools, and neither
 * does this script: cargo compiles `crates/napi`, nothing else runs.
 */
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { currentTarget, stageNative } from './stage-native.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const target = process.argv[2] ?? currentTarget().target;

execFileSync('cargo', ['build', '--release', '--package', 'open-zipper-napi'], {
    cwd: root,
    stdio: 'inherit',
    env: {
        ...process.env,
        // Keep one compiler line so local builds and CI do not fight for memory.
        CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '2',
    },
});

const destination = stageNative(target, path.join(root, 'target', 'release'));
console.log(`staged ${target} -> ${path.relative(root, destination)}`);
