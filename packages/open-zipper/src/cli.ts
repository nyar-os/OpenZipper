#!/usr/bin/env node
/**
 * OpenZipper CLI — archive listing, extraction, creation and verification.
 *
 * Exit codes are stable for automation:
 * - 0: success
 * - 1: runtime failure
 * - 2: usage / argument failure
 * - 3: capability unavailable (`unsupported_platform`, `not_implemented`, `dependency_blocked`)
 */
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createCli, type ParsedOptions } from '@wae/commander';
import {
    auditQuery,
    capabilities,
    configureAudit,
    createArchive,
    extract,
    listEntries,
    runGui,
    testArchive,
    version,
} from './index.js';
import type { ArchiveFormat, ArchiveListing, AuditRecord, Capabilities, CreateReport, ExtractReport, OverwritePolicy, TestReport } from './index.js';

const EXIT_OK = 0;
const EXIT_FAILURE = 1;
const EXIT_USAGE = 2;
const EXIT_CAPABILITY = 3;

const CAPABILITY_CODES = ['unsupported_platform', 'not_implemented', 'dependency_blocked'];
const OVERWRITE_POLICIES: OverwritePolicy[] = ['error', 'skip', 'rename'];
const FORMATS: ArchiveFormat[] = ['zip', 'tar', 'tar-gz', 'tar-bz2', 'tar-xz'];

const here = path.dirname(fileURLToPath(import.meta.url));
const localesDir = path.join(here, '..', 'locales');

const cli = createCli('open-zipper')
    .locales(localesDir)
    .intro('cli.intro')
    .option('--actor <id>', 'cli.opt.actor')
    .option('--actor-kind <kind>', 'cli.opt.actorKind')
    .option('--actor-source <source>', 'cli.opt.actorSource');

function text(options: ParsedOptions, key: string): string | undefined {
    const value = options[key];
    return typeof value === 'string' ? value : undefined;
}

/** Attribute every capability call in this run to an explicit actor. */
function applyActor(options: ParsedOptions): void {
    const kindRaw = text(options, 'actor-kind');
    configureAudit({
        actorId: text(options, 'actor'),
        actorKind: kindRaw === 'user' || kindRaw === 'agent' ? kindRaw : undefined,
        actorSource: text(options, 'actor-source') ?? 'cli',
    });
}

function errorText(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}

function exitCodeFor(error: unknown): number {
    const message = errorText(error);
    if (CAPABILITY_CODES.some((code) => message.includes(code))) return EXIT_CAPABILITY;
    return EXIT_FAILURE;
}

function reportError(error: unknown): number {
    console.error(errorText(error));
    return exitCodeFor(error);
}

function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 ** 3) return `${(bytes / 1024).toFixed(1)} MB`;
    return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

function parseOverwrite(raw: string | undefined): OverwritePolicy | null {
    if (raw === undefined) return 'error';
    const policy = raw.trim().toLowerCase() as OverwritePolicy;
    return OVERWRITE_POLICIES.includes(policy) ? policy : null;
}

function parseFormat(raw: string | undefined): ArchiveFormat | null | undefined {
    if (raw === undefined) return undefined;
    const format = raw.trim().toLowerCase() as ArchiveFormat;
    return FORMATS.includes(format) ? format : null;
}

function printListing(listing: ArchiveListing, limit: number): void {
    console.log(`archive: ${listing.path}`);
    console.log(`format: ${listing.format}`);
    console.log(`entries: ${listing.entryCount}`);
    console.log(`size: ${formatBytes(listing.totalSizeBytes)}`);
    if (listing.unsafeEntryCount > 0) console.log(`unsafe entries: ${listing.unsafeEntryCount}`);
    for (const entry of listing.entries.slice(0, limit)) {
        const compressed = entry.compressedSizeBytes === undefined ? '' : ` (${formatBytes(entry.compressedSizeBytes)} stored)`;
        const method = entry.compressionMethod === undefined ? '' : ` [${entry.compressionMethod}]`;
        const modified = entry.modifiedAtUnixMs === undefined ? '' : ` ${new Date(entry.modifiedAtUnixMs).toISOString()}`;
        const unsafe = entry.safe ? '' : `  UNSAFE: ${entry.unsafeReason ?? 'rejected'}`;
        console.log(
            `  ${String(entry.index).padStart(5)}  ${entry.kind.padEnd(9)}  ${String(entry.sizeBytes).padStart(10)}  ${entry.name}${compressed}${method}${modified}${unsafe}`,
        );
    }
    if (listing.entries.length > limit) console.log(`  ... ${listing.entries.length - limit} more (use --limit or --json)`);
}

function printExtractReport(report: ExtractReport): void {
    const verb = report.dryRun ? 'would write' : 'wrote';
    console.log(`archive: ${report.archive}`);
    console.log(`destination: ${report.destination}`);
    console.log(`format: ${report.format}`);
    console.log(`${verb}: ${report.written} files, ${report.directories} directories, ${formatBytes(report.bytesWritten)}`);
    if (report.skipped > 0) console.log(`skipped: ${report.skipped}`);
    if (report.renamed > 0) console.log(`renamed: ${report.renamed}`);
    if (report.nestedArchives > 0) console.log(`nested archives: ${report.nestedArchives}`);
    const notable = report.entries.filter((entry) => entry.action !== 'written' && entry.action !== 'directory');
    for (const entry of notable.slice(0, 20)) {
        const target = entry.renamedTo ? `${entry.name} -> ${entry.renamedTo}` : entry.name;
        console.log(`  ${entry.action}: ${target}`);
    }
    if (notable.length > 20) console.log(`  ... ${notable.length - 20} more (use --json)`);
}

function printCreateReport(report: CreateReport): void {
    console.log(`archive: ${report.archive}`);
    console.log(`format: ${report.format}`);
    console.log(`entries: ${report.entryCount}`);
    console.log(`source: ${formatBytes(report.sourceBytes)} -> archive: ${formatBytes(report.archiveBytes)}`);
}

function printTestReport(report: TestReport): void {
    console.log(`archive: ${report.archive}`);
    console.log(`format: ${report.format}`);
    console.log(`entries: ${report.entryCount}`);
    console.log(`verified: ${formatBytes(report.totalBytes)}`);
    console.log(`result: ${report.ok ? 'ok' : 'damaged'}`);
}

function printCapabilities(report: Capabilities): void {
    console.log(`platform: ${report.platform}-${report.architecture}`);
    for (const entry of report.entries) {
        const state = entry.available ? 'available' : (entry.code ?? 'blocked');
        console.log(`  ${entry.capability.padEnd(22)} ${state.padEnd(20)} ${entry.detail}`);
    }
}

function printAuditRecord(entry: AuditRecord): void {
    console.log(JSON.stringify(entry));
}

const limitOption = {
    coerce: (raw: string | boolean | string[]): unknown => {
        const n = Number(raw);
        if (!Number.isFinite(n) || n < 1) throw new Error(`invalid --limit: ${String(raw)}`);
        return String(Math.floor(n));
    },
};

cli.command('extract', 'cli.cmd.extract')
    .option('--dest <dir>', 'cli.opt.dest', { required: true })
    .option('--overwrite <policy>', 'cli.opt.overwrite')
    .option('--dry-run', 'cli.opt.dryRun')
    .option('--recurse', 'cli.opt.recurse')
    .option('--json', 'cli.opt.json')
    .actionCtx(async (options, args) => {
        applyActor(options);
        const archive = args[0];
        const destination = text(options, 'dest');
        if (!archive || !destination) {
            console.error('usage: open-zipper extract <archive> --dest <dir> [--overwrite error|skip|rename] [--dry-run] [--recurse] [--json]');
            return EXIT_USAGE;
        }
        const overwrite = parseOverwrite(text(options, 'overwrite'));
        if (overwrite === null) {
            console.error(`invalid --overwrite: expected one of ${OVERWRITE_POLICIES.join(', ')}`);
            return EXIT_USAGE;
        }
        try {
            const report = extract({ path: archive, destination, overwrite, dryRun: options['dry-run'] === true, recurse: options.recurse === true });
            if (options.json === true) console.log(JSON.stringify(report, null, 2));
            else printExtractReport(report);
            return EXIT_OK;
        }
        catch (error) {
            return reportError(error);
        }
    });

cli.command('create', 'cli.cmd.create')
    .option('--out <archive>', 'cli.opt.out', { required: true })
    .option('--format <name>', 'cli.opt.format')
    .option('--force', 'cli.opt.force')
    .option('--json', 'cli.opt.json')
    .actionCtx(async (options, args) => {
        applyActor(options);
        const destination = text(options, 'out');
        if (args.length === 0 || !destination) {
            console.error('usage: open-zipper create <source...> --out <archive> [--format zip|tar|tar-gz|tar-bz2|tar-xz] [--force] [--json]');
            return EXIT_USAGE;
        }
        const format = parseFormat(text(options, 'format'));
        if (format === null) {
            console.error(`invalid --format: expected one of ${FORMATS.join(', ')}`);
            return EXIT_USAGE;
        }
        try {
            const report = createArchive({ destination, sources: args, format, force: options.force === true });
            if (options.json === true) console.log(JSON.stringify(report, null, 2));
            else printCreateReport(report);
            return EXIT_OK;
        }
        catch (error) {
            return reportError(error);
        }
    });

cli.command('list', 'cli.cmd.list')
    .option('--limit <n>', 'cli.opt.limit', limitOption)
    .option('--json', 'cli.opt.json')
    .actionCtx(async (options, args) => {
        applyActor(options);
        const archive = args[0];
        if (!archive) {
            console.error('usage: open-zipper list <archive> [--limit <n>] [--json]');
            return EXIT_USAGE;
        }
        const limitRaw = text(options, 'limit');
        const limit = limitRaw ? Number(limitRaw) : 50;
        try {
            const listing = listEntries({ path: archive });
            if (options.json === true) console.log(JSON.stringify(listing, null, 2));
            else printListing(listing, limit);
            return EXIT_OK;
        }
        catch (error) {
            return reportError(error);
        }
    });

cli.command('test', 'cli.cmd.test')
    .option('--json', 'cli.opt.json')
    .actionCtx(async (options, args) => {
        applyActor(options);
        const archive = args[0];
        if (!archive) {
            console.error('usage: open-zipper test <archive> [--json]');
            return EXIT_USAGE;
        }
        try {
            const report = testArchive({ path: archive });
            if (options.json === true) console.log(JSON.stringify(report, null, 2));
            else printTestReport(report);
            return EXIT_OK;
        }
        catch (error) {
            return reportError(error);
        }
    });

cli.command('capabilities', 'cli.cmd.capabilities')
    .option('--json', 'cli.opt.json')
    .actionCtx(async (options) => {
        applyActor(options);
        try {
            const report = capabilities();
            if (options.json === true) console.log(JSON.stringify(report, null, 2));
            else printCapabilities(report);
            return EXIT_OK;
        }
        catch (error) {
            return reportError(error);
        }
    });

cli.command('audit', 'cli.cmd.audit')
    .option('--filter-actor <id>', 'cli.opt.actorFilter')
    .option('--capability <name>', 'cli.opt.capability')
    .option('--limit <n>', 'cli.opt.auditLimit', limitOption)
    .option('--json', 'cli.opt.json')
    .actionCtx(async (options) => {
        applyActor(options);
        const limitRaw = text(options, 'limit');
        try {
            const entries = auditQuery({
                actorId: text(options, 'filter-actor'),
                capability: text(options, 'capability'),
                limit: limitRaw ? Number(limitRaw) : undefined,
            });
            if (options.json === true) console.log(JSON.stringify(entries, null, 2));
            else for (const entry of entries) printAuditRecord(entry);
            return EXIT_OK;
        }
        catch (error) {
            return reportError(error);
        }
    });

cli.command('gui', 'cli.cmd.gui')
    .option('--title <text>', 'cli.opt.title', { default: 'OpenZipper' })
    .option('--content-dir <path>', 'cli.opt.contentDir')
    .actionCtx(async (options) => {
        applyActor(options);
        const title = typeof options.title === 'string' ? options.title : 'OpenZipper';
        try {
            await runGui({ title, contentDir: text(options, 'content-dir') });
            return EXIT_OK;
        }
        catch (error) {
            return reportError(error);
        }
    });

cli.command('version', 'cli.cmd.version').actionCtx(async (options) => {
    applyActor(options);
    try {
        console.log(version());
        return EXIT_OK;
    }
    catch (error) {
        return reportError(error);
    }
});

const code = await cli.parse(process.argv);
process.exitCode = code;
