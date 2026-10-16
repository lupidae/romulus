import { ApiError } from "@/api/client";

import { failureOf, type ImportEvent, lines } from "./api";

function stream(chunks: string[]) {
  const encoder = new TextEncoder();
  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
}

describe("an import's progress", () => {
  it("is read one event per line, whatever the chunks", async () => {
    const events: ImportEvent[] = [];
    for await (const event of lines(
      stream([
        '{"event":"chec',
        'ked"}\n{"event":"replayed","index":0,',
        '"number":1,"changes":"1 table added"}\n\n',
      ]),
    )) {
      events.push(event);
    }
    expect(events).toEqual([
      { event: "checked" },
      { event: "replayed", index: 0, number: 1, changes: "1 table added" },
    ]);
  });

  it("names the failing migration of an import_failed", () => {
    const error = new ApiError(422, {
      code: "import_failed",
      message: "x.sql:3: oops",
      details: { index: 2, id: "3", name: "3_x.sql", path: "3_x.sql", errors: [] },
    });
    expect(failureOf(error)?.index).toBe(2);
    expect(failureOf(new ApiError(409, { code: "has_versions", message: "" }))).toBeNull();
  });
});
