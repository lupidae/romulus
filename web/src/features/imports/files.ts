/**
 * The files of an import, as the browser hands them over: a folder picked
 * or dropped (each file with its path inside it), a zip, or loose files.
 * Only what the import can use is sent: `.sql` files, zips, and Prisma's
 * `migration_lock.toml`.
 */

export interface PickedFile {
  /** Its path inside what was picked: `migrations/0001_init.sql`. */
  path: string;
  file: File;
}

export interface Picked {
  files: PickedFile[];
  /** What was picked, in a few words: the folder's or the zip's name. */
  label: string;
  /** Files left out before sending (not SQL). */
  left: number;
  bytes: number;
}

/** The API's limits (crates/romulus-api/src/imports/upload.rs). */
export const MAX_BYTES = 32 * 1024 * 1024;
export const MAX_FILES = 5000;

/** Folders that never hold migrations but may hold thousands of files. */
const NEVER = /(^|\/)(node_modules|\.git|\.venv|venv|__pycache__)\//;

export function sendable(path: string): boolean {
  if (NEVER.test(path)) return false;
  const name = path.split("/").pop() ?? path;
  return /\.(sql|zip)$/i.test(name) || name === "migration_lock.toml";
}

/** Why the files cannot be sent, in a sentence; `null` when they can. */
export function tooMuch(picked: Picked): string | null {
  if (picked.files.length === 0) {
    return picked.left > 0
      ? `No .sql file in ${picked.label}: pick the folder that holds the migrations.`
      : "Nothing was picked.";
  }
  if (picked.files.length > MAX_FILES) {
    return `That is ${picked.files.length} files; an import reads at most ${MAX_FILES}.`;
  }
  if (picked.bytes > MAX_BYTES) {
    return `That is ${(picked.bytes / 1024 / 1024).toFixed(0)} MB of SQL; an import reads at most 32 MB.`;
  }
  return null;
}

/** From a file input (a folder's files carry `webkitRelativePath`). */
export function fromInput(files: File[]): Picked {
  return collect(files.map((file) => ({ path: file.webkitRelativePath || file.name, file })));
}

/**
 * From a drop: folders are walked (their entries must be taken during the
 * drop event, so call this from the handler, then await it).
 */
export function fromDrop(data: DataTransfer): Promise<Picked> {
  const entries = Array.from(data.items)
    .filter((item) => item.kind === "file")
    .map((item) => item.webkitGetAsEntry?.() ?? null);
  const loose = Array.from(data.files);
  return (async () => {
    if (entries.some((e) => e === null) || entries.length === 0) {
      return collect(loose.map((file) => ({ path: file.name, file })));
    }
    const out: PickedFile[] = [];
    for (const entry of entries) await walk(entry!, out);
    return collect(out);
  })();
}

async function walk(entry: FileSystemEntry, out: PickedFile[]): Promise<void> {
  const path = entry.fullPath.replace(/^\//, "");
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) =>
      (entry as FileSystemFileEntry).file(resolve, reject),
    );
    out.push({ path, file });
    return;
  }
  if (!entry.isDirectory || NEVER.test(`${path}/`)) return;
  const reader = (entry as FileSystemDirectoryEntry).createReader();
  // A directory reader hands entries over in batches until an empty one.
  for (;;) {
    const batch = await new Promise<FileSystemEntry[]>((resolve, reject) =>
      reader.readEntries(resolve, reject),
    );
    if (batch.length === 0) break;
    for (const child of batch) await walk(child, out);
  }
}

function collect(all: PickedFile[]): Picked {
  const files = all.filter((f) => sendable(f.path));
  const top = new Set(all.map((f) => (f.path.includes("/") ? f.path.split("/")[0] : f.path)));
  const label =
    top.size === 1
      ? ([...top][0] ?? "")
      : all.length === 1
        ? (all[0]?.path ?? "")
        : `${all.length} files`;
  return {
    files,
    label,
    left: all.length - files.length,
    bytes: files.reduce((sum, f) => sum + f.file.size, 0),
  };
}
