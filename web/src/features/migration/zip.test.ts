import { describe, expect, it } from "vitest";

import { crc32, zip } from "./zip";

/** Reads a stored (uncompressed) zip back: names, contents and CRCs from the central directory. */
async function unzip(blob: Blob) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const view = new DataView(bytes.buffer);
  const end = bytes.length - 22;
  expect(view.getUint32(end, true)).toBe(0x06054b50);
  const count = view.getUint16(end + 10, true);
  let at = view.getUint32(end + 16, true);
  const decoder = new TextDecoder();
  const out: { path: string; content: string; crc: number }[] = [];
  for (let i = 0; i < count; i++) {
    expect(view.getUint32(at, true)).toBe(0x02014b50);
    const crc = view.getUint32(at + 16, true);
    const size = view.getUint32(at + 20, true);
    const nameLength = view.getUint16(at + 28, true);
    const local = view.getUint32(at + 42, true);
    const path = decoder.decode(bytes.slice(at + 46, at + 46 + nameLength));
    const localName = view.getUint16(local + 26, true);
    const start = local + 30 + localName;
    out.push({ path, content: decoder.decode(bytes.slice(start, start + size)), crc });
    at += 46 + nameLength;
  }
  return out;
}

describe("zip", () => {
  it("computes the standard CRC-32", () => {
    expect(crc32(new TextEncoder().encode("123456789"))).toBe(0xcbf43926);
  });

  it("stores files under their folders, readable back", async () => {
    const files = [
      { path: "20261016093000_add/migration.sql", content: "CREATE TABLE t ();\n" },
      { path: "notes é.sql", content: "-- café\n" },
    ];
    const entries = await unzip(zip(files));
    expect(entries.map((e) => e.path)).toEqual(files.map((f) => f.path));
    expect(entries.map((e) => e.content)).toEqual(files.map((f) => f.content));
    expect(entries[0]!.crc).toBe(crc32(new TextEncoder().encode(files[0]!.content)));
  });
});
