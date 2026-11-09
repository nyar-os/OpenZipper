import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export type ArchiveFormat = 'zip' | 'tar' | 'tar-gz' | 'tar-bz2' | 'tar-xz' | '7z';

export type EntryKind = 'file' | 'directory' | 'symlink' | 'hardlink' | 'other';

/** One member of an archive, as read from the archive index. */
export type EntryInfo = {
    index: number;
    name: string;
    kind: EntryKind;
    sizeBytes: number;
    compressedSizeBytes?: number;
    /** Decoder label from the archive index, for example `lzma2` or `deflated`. */
    compressionMethod?: string;
    /** Absolute modification time in Unix milliseconds. Absent when the format only stores local wall-clock time. */
    modifiedAtUnixMs?: number;
    /** False when the entry would escape the destination directory. */
    safe: boolean;
    unsafeReason?: string;
};

export type ArchiveListing = {
    path: string;
    format: ArchiveFormat;
    entries: EntryInfo[];
    entryCount: number;
    totalSizeBytes: number;
    unsafeEntryCount: number;
};

export type OverwritePolicy = 'error' | 'skip' | 'rename';

export type ExtractAction = 'written' | 'would-write' | 'directory' | 'skipped-exists' | 'renamed';

export type ExtractOutcome = {
    name: string;
    action: ExtractAction;
    bytesWritten: number;
    renamedTo?: string;
};

export type ExtractReport = {
    archive: string;
    destination: string;
    format: ArchiveFormat;
    dryRun: boolean;
    written: number;
    skipped: number;
    directories: number;
    renamed: number;
    bytesWritten: number;
    nestedArchives: number;
    entries: ExtractOutcome[];
};

export type CreateReport = {
    archive: string;
    format: ArchiveFormat;
    entryCount: number;
    sourceBytes: number;
    archiveBytes: number;
};

export type TestReport = {
    archive: string;
    format: ArchiveFormat;
    entryCount: number;
    totalBytes: number;
    ok: boolean;
};

export type CapabilityCode = 'unsupported_platform' | 'not_implemented' | 'dependency_blocked';

export type CapabilityEntry = {
    capability: string;
    available: boolean;
    code?: CapabilityCode;
    detail: string;
};

export type ArchiveLimits = {
    maxEntries?: number;
    maxTotalBytes?: number;
    maxEntryBytes?: number;
    maxCompressionRatio?: number;
    maxNestedDepth?: number;
};

export type Capabilities = {
    platform: string;
    architecture: string;
    entries: CapabilityEntry[];
    defaultLimits: Required<ArchiveLimits>;
};

export type ActorKind = 'user' | 'agent';

export type AuditActor = {
    kind: ActorKind;
    id: string;
    source: string;
};

/** One append-only record from the shared `wae.audit/v0` history. */
export type AuditRecord = {
    schema: string;
    timestampMs: number;
    actor: AuditActor;
    app: string;
    appVersion: string;
    capability: string;
    mode: 'dry_run' | 'execute';
    params: unknown;
    targets: {
        count: number;
        summary: string;
    };
    result: 'success' | 'failure' | 'rejected';
    errorCode?: string;
    durationMs: number;
    host: string;
    session: string;
};

export type AuditConfigureOptions = {
    actorKind?: ActorKind;
    actorId?: string;
    actorSource?: string;
    session?: string;
    host?: string;
};

export type AuditQueryOptions = {
    actorKind?: ActorKind;
    actorId?: string;
    /** Inclusive lower bound, Unix milliseconds. */
    fromMs?: number;
    /** Inclusive upper bound, Unix milliseconds. */
    toMs?: number;
    capability?: string;
    limit?: number;
};

export type ListOptions = {
    path: string;
    limits?: ArchiveLimits;
};

export type ExtractOptions = {
    path: string;
    destination: string;
    /** Defaults to `error`, so nothing is replaced silently. */
    overwrite?: OverwritePolicy;
    dryRun?: boolean;
    /** Extract archives found inside this archive into `<file>.extracted`. */
    recurse?: boolean;
    limits?: ArchiveLimits;
};

export type CreateOptions = {
    destination: string;
    sources: string[];
    /** Inferred from the destination extension when omitted. */
    format?: ArchiveFormat;
    force?: boolean;
    limits?: ArchiveLimits;
};

export type TestOptions = {
    path: string;
    limits?: ArchiveLimits;
};

export type PreviewKind = 'text' | 'image' | 'binary' | 'empty';

/** One bounded entry read, classified in the core so every host agrees. */
export type PreviewResult = {
    name: string;
    index: number;
    kind: PreviewKind;
    mime?: string;
    encoding?: string;
    text?: string;
    dataBase64?: string;
    truncated: boolean;
    previewedBytes: number;
    totalBytes: number;
};

export type PreviewOptions = {
    path: string;
    index: number;
    /** Decoded byte budget; the core clamps it to `1..=1 MiB`. */
    capBytes?: number;
    limits?: ArchiveLimits;
};

export type OpenGuiOptions = {
    title?: string;
    /** Local folder containing index.html (served via https://app.local/). */
    contentDir?: string;
    /** Escape hatch; prefer contentDir. */
    url?: string;
    width?: number;
    height?: number;
    /**
     * What a second open request does in single-window mode. Callers pass the
     * value from `wae-runtime.json#window.policy`; it defaults to `focus`.
     */
    secondLaunch?: 'focus' | 'reject';
};

/** Lifecycle state reported by the native GUI host. */
export type GuiState = 'open' | 'closed' | 'failed' | 'not_open';

/**
 * One structured GUI lifecycle observation. `code` is stable for scripts:
 * `gui_closed`, `gui_host_exited`, `gui_not_open`, `gui_close_failed`,
 * `gui_open_failed`, `window_second_launch_rejected`,
 * `window_config_invalid` or `unsupported_platform`.
 */
export type GuiStatus = {
    state: GuiState;
    code: string | null;
    backend: string | null;
    platform: string;
    uptimeMs: number;
    message: string | null;
};

/** Structured result of `openGui`, including host failure detail when known. */
export type GuiOpenResult = GuiStatus & {
    ok: boolean;
    /** Host failure stage, for example `resolve_runtime` or `create_window`. */
    stage: string | null;
    /** HRESULT or Win32 error code reported by the host layer. */
    hresult: number | null;
    /** Locations or probes the host already tried. */
    attempted: string[];
};

export type GuiCloseResult = GuiStatus & { closed: boolean };

type NativeBinding = {
    listEntries(options: ListOptions): ArchiveListing;
    extract(options: ExtractOptions): ExtractReport;
    create(options: CreateOptions): CreateReport;
    testArchive(options: TestOptions): TestReport;
    previewEntry(options: PreviewOptions): PreviewResult;
    capabilities(): Capabilities;
    version(): string;
    openGui(options?: OpenGuiOptions): GuiOpenResult;
    pumpEvents(): GuiStatus;
    closeGui(): GuiCloseResult;
    isGuiOpen(): boolean;
    configureAudit(options?: AuditConfigureOptions): void;
    auditQuery(options?: AuditQueryOptions): AuditRecord[];
};

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));

type PlatformArch = 'win32-x64' | 'darwin-x64' | 'darwin-arm64' | 'linux-x64' | 'linux-arm64';

/**
 * Archive work is plain filesystem work, so every desktop host has a build.
 * A missing host entry means no native artifact exists for it.
 */
const PLATFORM_PACKAGES: Record<PlatformArch, string> = {
    'win32-x64': '@wae/open-zipper-win32-x64',
    'darwin-x64': '@wae/open-zipper-darwin-x64',
    'darwin-arm64': '@wae/open-zipper-darwin-arm64',
    'linux-x64': '@wae/open-zipper-linux-x64',
    'linux-arm64': '@wae/open-zipper-linux-arm64',
};

export function supportedPlatforms(): PlatformArch[] {
    return Object.keys(PLATFORM_PACKAGES) as PlatformArch[];
}

function currentPlatformArch(): PlatformArch | null {
    const key = `${process.platform}-${process.arch}`;
    return (Object.hasOwn(PLATFORM_PACKAGES, key) ? key : null) as PlatformArch | null;
}

function candidateNodePaths(): string[] {
    const pa = currentPlatformArch();
    if (!pa) {
        throw new Error(
            `unsupported_platform: OpenZipper native addon targets ${supportedPlatforms().join(', ')}, ` +
                `current host is ${process.platform}-${process.arch}. No compatibility build exists.`,
        );
    }

    const pkg = PLATFORM_PACKAGES[pa];
    const fileName = `open-zipper.${pa}.node`;
    const paths: string[] = [];

    try {
        const pkgJson = require.resolve(`${pkg}/package.json`);
        const root = path.dirname(pkgJson);
        paths.push(path.join(root, fileName));
    }
    catch {
        // optional dependency may be missing during monorepo dev
    }

    // packages/open-zipper/dist → ../../open-zipper-<platform>-<arch>
    paths.push(path.resolve(here, `../../open-zipper-${pa}/${fileName}`));
    paths.push(path.resolve(here, '../../../target/release/open_zipper_napi.node'));
    return paths;
}

let cached: NativeBinding | null = null;

export function loadNative(): NativeBinding {
    if (cached) return cached;
    const tried: string[] = [];
    for (const candidate of candidateNodePaths()) {
        tried.push(candidate);
        if (!fs.existsSync(candidate)) continue;
        cached = require(candidate) as NativeBinding;
        return cached;
    }
    throw new Error(`Failed to load open-zipper native addon. Tried:\n${tried.map((p) => `  - ${p}`).join('\n')}`);
}

/** GUI assets ship inside the published/built `dist/gui` tree. */
export function defaultGuiContentDir(): string {
    const dir = path.resolve(here, 'gui');
    if (!fs.existsSync(path.join(dir, 'index.html'))) {
        throw new Error(`GUI folder missing at ${dir} (expected packages/open-zipper/dist/gui)`);
    }
    return dir;
}
