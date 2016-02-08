/**
 * Build and release configuration for OpenZipper.
 *
 * This file is pure data so it can be parsed statically. There are no command
 * strings and no conditional scripts here; the build order is executed by the
 * workspace scripts.
 */
export default {
    native: {
        crate: 'open-zipper-napi',
        targets: ['win32-x64', 'darwin-x64', 'darwin-arm64', 'linux-x64', 'linux-arm64'],
        cdylib: 'open_zipper_napi',
    },
    packages: {
        native: [
            { target: 'win32-x64', dir: 'packages/open-zipper-win32-x64', os: ['win32'], cpu: ['x64'] },
            { target: 'darwin-x64', dir: 'packages/open-zipper-darwin-x64', os: ['darwin'], cpu: ['x64'] },
            { target: 'darwin-arm64', dir: 'packages/open-zipper-darwin-arm64', os: ['darwin'], cpu: ['arm64'] },
            { target: 'linux-x64', dir: 'packages/open-zipper-linux-x64', os: ['linux'], cpu: ['x64'] },
            { target: 'linux-arm64', dir: 'packages/open-zipper-linux-arm64', os: ['linux'], cpu: ['arm64'] },
        ],
    },
    build: {
        order: ['native', 'typescript', 'ui', 'skills'],
    },
    skills: {
        dir: 'packages/open-zipper-skills/skills',
        check: 'scripts/check-skills.ts',
    },
    release: {
        packages: [
            '@wae/open-zipper',
            '@wae/open-zipper-skills',
            '@wae/open-zipper-win32-x64',
            '@wae/open-zipper-darwin-x64',
            '@wae/open-zipper-darwin-arm64',
            '@wae/open-zipper-linux-x64',
            '@wae/open-zipper-linux-arm64',
        ],
        tag: 'vX.Y.Z',
    },
    ci: {
        workflows: ['check-rust.yml', 'check-typescript.yml'],
    },
    audit: {
        app: 'open-zipper',
        actorSources: ['cli', 'environment', 'gui', 'plugin'],
    },
};
