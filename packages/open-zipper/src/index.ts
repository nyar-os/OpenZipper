import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import {
    defaultGuiContentDir,
    loadNative,
    type ArchiveListing,
    type AuditConfigureOptions,
    type AuditQueryOptions,
    type AuditRecord,
    type Capabilities,
    type CreateOptions,
    type CreateReport,
    type ExtractOptions,
    type ExtractReport,
    type GuiCloseResult,
    type GuiOpenResult,
    type GuiStatus,
    type ListOptions,
    type OpenGuiOptions,
    type PreviewOptions,
    type PreviewResult,
    type TestOptions,
    type TestReport,
} from './native.js';

export type {
    ActorKind,
    ArchiveFormat,
    ArchiveLimits,
    ArchiveListing,
    AuditActor,
    AuditConfigureOptions,
    AuditQueryOptions,
    AuditRecord,
    Capabilities,
    CapabilityCode,
    CapabilityEntry,
    CreateOptions,
    CreateReport,
    EntryInfo,
    EntryKind,
    ExtractAction,
    ExtractOptions,
    ExtractOutcome,
    ExtractReport,
    GuiCloseResult,
    GuiOpenResult,
    GuiState,
    GuiStatus,
    ListOptions,
    OpenGuiOptions,
    OverwritePolicy,
    PreviewKind,
    PreviewOptions,
    PreviewResult,
    TestOptions,
    TestReport,
} from './native.js';
export { supportedPlatforms } from './native.js';

/** Enumerate archive members without writing anything to disk. */
export function listEntries(options: ListOptions): ArchiveListing {
    return loadNative().listEntries(options);
}

/**
 * Extract an archive through the audited `extract` capability. Path safety and
 * bomb guards run inside the native core before the first byte is written.
 */
export function extract(options: ExtractOptions): ExtractReport {
    return loadNative().extract(options);
}

/** Create an archive from files and directories, all inside the native core. */
export function createArchive(options: CreateOptions): CreateReport {
    return loadNative().create(options);
}

/** Verify every entry can be read back; zip CRCs are checked by the decoder. */
export function testArchive(options: TestOptions): TestReport {
    return loadNative().testArchive(options);
}

/**
 * Read one entry's payload into memory for previewing. The core bounds the
 * read, decodes text with the same fallback chain as entry names, and refuses
 * encrypted members; over-length payloads arrive truncated, not failed.
 */
export function previewEntry(options: PreviewOptions): PreviewResult {
    return loadNative().previewEntry(options);
}

/** Report which sinks are available on the current build target. */
export function capabilities(): Capabilities {
    return loadNative().capabilities();
}

export function version(): string {
    return loadNative().version();
}

/**
 * Read `window.policy.secondLaunch` from the contract `wae build` writes next
 * to the CLI entry. A missing or unreadable contract means the default policy:
 * focus the existing window instead of opening a second one.
 */
function secondLaunchFromContract(): 'focus' | 'reject' {
    try {
        const url = new URL('./wae-runtime.json', import.meta.url);
        const value: unknown = JSON.parse(fs.readFileSync(fileURLToPath(url), 'utf8'));
        if (typeof value !== 'object' || value === null || Array.isArray(value)) return 'focus';
        const window = (value as { window?: unknown }).window;
        if (typeof window !== 'object' || window === null) return 'focus';
        const policy = (window as { policy?: unknown }).policy;
        if (typeof policy !== 'object' || policy === null) return 'focus';
        return (policy as { secondLaunch?: unknown }).secondLaunch === 'reject' ? 'reject' : 'focus';
    }
    catch {
        return 'focus';
    }
}

/**
 * Open the native window. Failures are returned instead of thrown so callers
 * keep the host code, stage, HRESULT and probed locations.
 */
export function openGui(options: OpenGuiOptions = {}): GuiOpenResult {
    const native = loadNative();
    return native.openGui({
        title: options.title ?? 'OpenZipper',
        contentDir: options.contentDir ?? (options.url ? undefined : defaultGuiContentDir()),
        url: options.url,
        width: options.width,
        height: options.height,
        secondLaunch: options.secondLaunch ?? secondLaunchFromContract(),
    });
}

/** Pump the host message loop once and report the observed lifecycle state. */
export function pumpEvents(): GuiStatus {
    return loadNative().pumpEvents();
}

/** Close the window; the result distinguishes "already closed" from a failure. */
export function closeGui(): GuiCloseResult {
    return loadNative().closeGui();
}

export function isGuiOpen(): boolean {
    return loadNative().isGuiOpen();
}

/** Structured GUI failure so scripts can branch on `code` without parsing text. */
export class GuiError extends Error {
    readonly code: string;
    readonly status: GuiStatus;

    constructor(status: GuiStatus) {
        super(status.message ?? `GUI ${status.state} (${status.code ?? 'unknown'})`);
        this.name = 'GuiError';
        this.code = status.code ?? 'gui_unknown';
        this.status = status;
    }
}

/**
 * Tag subsequent capability calls with an explicit actor. Without an override
 * `WAE_ACTOR_KIND` / `WAE_ACTOR_ID` / `WAE_ACTOR_SOURCE` supply the identity.
 */
export function configureAudit(options: AuditConfigureOptions = {}): void {
    loadNative().configureAudit(options);
}

/** Read the append-only capability history written by the shared auditor. */
export function auditQuery(options: AuditQueryOptions = {}): AuditRecord[] {
    return loadNative().auditQuery(options);
}

/**
 * Open the window and pump until it closes. Resolves with the terminal
 * `gui_closed` status, or rejects with a `GuiError` that carries the host code
 * so a host that dies right after opening cannot look like a clean shutdown.
 */
export function runGui(options: OpenGuiOptions = {}, intervalMs = 16): Promise<GuiStatus> {
    const opened = openGui(options);
    if (!opened.ok) {
        return Promise.reject(new GuiError(opened));
    }
    return new Promise((resolve, reject) => {
        const timer = setInterval(() => {
            let status: GuiStatus;
            try {
                status = pumpEvents();
            }
            catch (error) {
                clearInterval(timer);
                reject(error);
                return;
            }
            if (status.state === 'open') return;
            clearInterval(timer);
            if (status.code === 'gui_closed') resolve(status);
            else reject(new GuiError(status));
        }, intervalMs);
    });
}
