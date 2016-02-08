/**
 * Copy the compiled N-API cdylib into a platform package as `<name>.<target>.node`.
 *
 * Target facts live here so `build-napi` and CI agree on file names.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export type NativeTarget = {
    target: string;
    os: string;
    cpu: string;
    /** cdylib file name produced by cargo under `target/release`. */
    lib: string;
};

export const NATIVE_TARGETS: NativeTarget[] = [
    { target: 'win32-x64', os: 'win32', cpu: 'x64', lib: 'open_zipper_napi.dll' },
    { target: 'darwin-x64', os: 'darwin', cpu: 'x64', lib: 'libopen_zipper_napi.dylib' },
    { target: 'darwin-arm64', os: 'darwin', cpu: 'arm64', lib: 'libopen_zipper_napi.dylib' },
    { target: 'linux-x64', os: 'linux', cpu: 'x64', lib: 'libopen_zipper_napi.so' },
    { target: 'linux-arm64', os: 'linux', cpu: 'arm64', lib: 'libopen_zipper_napi.so' },
];

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

export function findTarget(target: string): NativeTarget {
    const found = NATIVE_TARGETS.find((entry) => entry.target === target);
    if (!found) throw new Error(`unknown native target \`${target}\``);
    return found;
}

export function currentTarget(): NativeTarget {
    const host = `${process.platform}-${process.arch}`;
    return findTarget(host);
}

/** Absolute path of the cargo cdylib, whether or not it has been built. */
export function napiArtifactPath(targetName: string, targetDir = path.join(root, 'target', 'release')): string {
    return path.join(targetDir, findTarget(targetName).lib);
}

/** Same as {@link napiArtifactPath} but fails when the artifact is missing. */
export function resolveNapiArtifact(targetName: string, targetDir?: string): string {
    const artifact = napiArtifactPath(targetName, targetDir);
    if (!fs.existsSync(artifact)) {
        throw new Error(`native artifact missing at ${artifact}; run \`pnpm run build:napi\` first`);
    }
    return artifact;
}

export function platformPackageDir(targetName: string): string {
    return path.join(root, 'packages', `open-zipper-${targetName}`);
}

/** Copy the cdylib into the platform package as a loadable `.node` module. */
export function stageNative(targetName: string, targetDir?: string): string {
    const artifact = resolveNapiArtifact(targetName, targetDir);
    const destination = path.join(platformPackageDir(targetName), `open-zipper.${targetName}.node`);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.copyFileSync(artifact, destination);
    return destination;
}

function isEntryPoint(): boolean {
    if (!process.argv[1]) return false;
    const invoked = path.resolve(process.argv[1]);
    const modulePath = fileURLToPath(import.meta.url);
    return process.platform === 'win32' ? invoked.toLowerCase() === modulePath.toLowerCase() : invoked === modulePath;
}

if (isEntryPoint()) {
    const target = process.argv[2] ?? currentTarget().target;
    const destination = stageNative(target);
    console.log(`staged ${target} -> ${path.relative(root, destination)}`);
}
