/**
 * Build and release configuration for OpenZipper.
 *
 * Pure literals only: `wae` parses this file statically and never executes it.
 * Build order is fixed: native → ts → ui → skills → staging.
 */
export default {
    native: {
        crate: 'open-zipper-napi',
        cdylib: 'open_zipper_napi',
        targets: [
            {
                platform: 'win32',
                arch: 'x64',
                triple: 'x86_64-pc-windows-msvc',
                package: '@wae/open-zipper-win32-x64',
                nodeFile: 'open-zipper.win32-x64.node',
                dir: 'packages/open-zipper-win32-x64',
            },
            {
                platform: 'darwin',
                arch: 'x64',
                triple: 'x86_64-apple-darwin',
                package: '@wae/open-zipper-darwin-x64',
                nodeFile: 'open-zipper.darwin-x64.node',
                dir: 'packages/open-zipper-darwin-x64',
            },
            {
                platform: 'darwin',
                arch: 'arm64',
                triple: 'aarch64-apple-darwin',
                package: '@wae/open-zipper-darwin-arm64',
                nodeFile: 'open-zipper.darwin-arm64.node',
                dir: 'packages/open-zipper-darwin-arm64',
            },
            {
                platform: 'linux',
                arch: 'x64',
                triple: 'x86_64-unknown-linux-gnu',
                package: '@wae/open-zipper-linux-x64',
                nodeFile: 'open-zipper.linux-x64.node',
                dir: 'packages/open-zipper-linux-x64',
            },
            {
                platform: 'linux',
                arch: 'arm64',
                triple: 'aarch64-unknown-linux-gnu',
                package: '@wae/open-zipper-linux-arm64',
                nodeFile: 'open-zipper.linux-arm64.node',
                dir: 'packages/open-zipper-linux-arm64',
            },
        ],
    },
    packages: [
        {
            name: '@wae/open-zipper-win32-x64',
            dir: 'packages/open-zipper-win32-x64',
            os: ['win32'],
            cpu: ['x64'],
            main: 'open-zipper.win32-x64.node',
        },
        {
            name: '@wae/open-zipper-darwin-x64',
            dir: 'packages/open-zipper-darwin-x64',
            os: ['darwin'],
            cpu: ['x64'],
            main: 'open-zipper.darwin-x64.node',
        },
        {
            name: '@wae/open-zipper-darwin-arm64',
            dir: 'packages/open-zipper-darwin-arm64',
            os: ['darwin'],
            cpu: ['arm64'],
            main: 'open-zipper.darwin-arm64.node',
        },
        {
            name: '@wae/open-zipper-linux-x64',
            dir: 'packages/open-zipper-linux-x64',
            os: ['linux'],
            cpu: ['x64'],
            main: 'open-zipper.linux-x64.node',
        },
        {
            name: '@wae/open-zipper-linux-arm64',
            dir: 'packages/open-zipper-linux-arm64',
            os: ['linux'],
            cpu: ['arm64'],
            main: 'open-zipper.linux-arm64.node',
        },
    ],
    build: {
        ts: { packages: ['@wae/open-zipper'] },
        ui: { package: '@wae/open-zipper-ui' },
        stage: [
            { from: 'packages/open-zipper-ui/dist/static', to: 'packages/open-zipper/dist/gui' },
        ],
        outputs: ['packages/open-zipper/dist/cli.js', 'packages/open-zipper/dist/gui/index.html'],
    },
    skills: {
        packages: [
            {
                dir: 'packages/open-zipper-skills/skills',
                packageJson: 'packages/open-zipper-skills/package.json',
                appPackageJson: 'packages/open-zipper/package.json',
                cliEntry: 'packages/open-zipper/dist/cli.js',
                commandName: 'open-zipper',
            },
        ],
    },
    release: {
        registry: 'https://registry.npmjs.org',
        tagPattern: '^v(\\d+\\.\\d+\\.\\d+)$',
        packages: [
            { name: '@wae/open-zipper', dir: 'packages/open-zipper' },
            { name: '@wae/open-zipper-skills', dir: 'packages/open-zipper-skills' },
            { name: '@wae/open-zipper-win32-x64', dir: 'packages/open-zipper-win32-x64' },
            { name: '@wae/open-zipper-darwin-x64', dir: 'packages/open-zipper-darwin-x64' },
            { name: '@wae/open-zipper-darwin-arm64', dir: 'packages/open-zipper-darwin-arm64' },
            { name: '@wae/open-zipper-linux-x64', dir: 'packages/open-zipper-linux-x64' },
            { name: '@wae/open-zipper-linux-arm64', dir: 'packages/open-zipper-linux-arm64' },
        ],
    },
    ci: {
        repo: 'nyar-os/OpenZipper',
        workflows: ['check-rust.yml', 'check-typescript.yml', 'publish-npm.yml'],
        requiredJobs: ['core-tests', 'gates', 'check-typescript'],
    },
    install: {
        shortcutDefault: true,
    },
    audit: {
        actor: { kind: 'flag', id: 'flag', source: 'flag' },
    },
    window: { mode: 'single', secondLaunch: 'focus' },
};
