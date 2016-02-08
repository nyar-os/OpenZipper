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
    type ListOptions,
    type OpenGuiOptions,
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
    ListOptions,
    OpenGuiOptions,
    OverwritePolicy,
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

/** Report which sinks are available on the current build target. */
export function capabilities(): Capabilities {
    return loadNative().capabilities();
}

export function version(): string {
    return loadNative().version();
}

export function openGui(options: OpenGuiOptions = {}): void {
    const native = loadNative();
    native.openGui({
        title: options.title ?? 'OpenZipper',
        contentDir: options.contentDir ?? (options.url ? undefined : defaultGuiContentDir()),
        url: options.url,
        width: options.width,
        height: options.height,
    });
}

/** Pump the host message loop. Returns false when the GUI has closed. */
export function pumpEvents(): boolean {
    return loadNative().pumpEvents();
}

export function closeGui(): void {
    loadNative().closeGui();
}

export function isGuiOpen(): boolean {
    return loadNative().isGuiOpen();
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

export function runGui(options: OpenGuiOptions = {}, intervalMs = 16): Promise<void> {
    openGui(options);
    return new Promise((resolve, reject) => {
        let frames = 0;
        const timer = setInterval(() => {
            try {
                frames += 1;
                if (frames === 1 && !isGuiOpen()) {
                    clearInterval(timer);
                    reject(new Error('GUI failed to stay open after openGui()'));
                    return;
                }
                if (!pumpEvents()) {
                    clearInterval(timer);
                    resolve();
                }
            }
            catch (err) {
                clearInterval(timer);
                reject(err);
            }
        }, intervalMs);
    });
}
