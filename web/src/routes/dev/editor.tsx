import { createFileRoute, notFound } from "@tanstack/react-router";
import * as React from "react";

import { Button } from "@/components/ui/button";
import { Workspace } from "@/features/draft/components/draft-editor";
import type { Diagnostic, Draft } from "@/features/draft/queries";
import { DraftSession } from "@/features/draft/session";
import { useSessionState } from "@/features/draft/use-session";
import { applyEdit } from "@/features/schema/edits/apply";
import blogJson from "@/features/schema/fixtures/blog.json";
import blogSql from "@/features/schema/fixtures/blog.sql?raw";
import { parseSchema, type Schema } from "@/features/schema/model";
import { devRoutes } from "@/lib/dev-routes";

// The draft editor on a draft held in this tab, with no server: graph edits,
// undo, Tidy's button, empty and reviewer states, for building and QA while
// the stack is down. The "compile" replays the graph edits found in the text
// onto the blog fixture. Dev builds only.
//   ?role=reviewer       read-only
//   ?fixture=empty       no SQL yet

interface Search {
  role?: "reviewer";
  fixture?: "empty";
}

export const Route = createFileRoute("/dev/editor")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    role: search.role === "reviewer" ? "reviewer" : undefined,
    fixture: search.fixture === "empty" ? "empty" : undefined,
  }),
  beforeLoad: () => {
    if (!devRoutes) throw notFound();
  },
  head: () => ({ meta: [{ title: "Draft editor – romulus" }] }),
  component: DevEditor,
});

const BLOG = parseSchema(blogJson);
const EMPTY: Schema = { ...BLOG, entities: [], enums: [], domains: [] };

const draft: Draft = {
  id: "dev-draft",
  name: "Draft from v3",
  status: "open",
  database: { id: "dev-db", slug: "blog", name: "blog" },
  base_version: 3,
  latest_version: 3,
  stale: false,
  published_version: null,
  created_by: { id: "u-alice", name: "Alice Martin" },
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
  present: [],
};

function error(from: number, to: number, message: string): Diagnostic {
  return {
    code: "sql",
    severity: "error",
    message,
    span: {
      start: from,
      end: to,
      utf16_start: from,
      utf16_end: to,
      from: { line: 1, column: 1, offset: from },
      to: { line: 1, column: 1, offset: to },
    },
  } as unknown as Diagnostic;
}

function DevEditor() {
  const { role, fixture } = Route.useSearch();
  const [session, setSession] = React.useState<DraftSession | null>(null);
  const [failNext, setFailNext] = React.useState(false);
  const failNextRef = React.useRef(false);
  React.useEffect(() => {
    failNextRef.current = failNext;
  }, [failNext]);

  React.useEffect(() => {
    const base = fixture === "empty" ? EMPTY : BLOG;
    const failing = new Set<number>();
    const holder: { session?: DraftSession } = {};
    const created = new DraftSession("acme", "dev", {
      local: {
        sql: fixture === "empty" ? "" : blogSql,
        role: role ?? "developer",
        compile: (sql) => {
          let model = base;
          const diagnostics: Diagnostic[] = [];
          for (const entry of holder.session?.getState().edits ?? []) {
            const at = sql.lastIndexOf(entry.sql);
            if (at < 0) continue;
            if (failing.has(entry.id))
              diagnostics.push(
                error(
                  at,
                  at + entry.sql.length,
                  "Postgres refused it here (a failure asked for on the dev screen).",
                ),
              );
            else model = applyEdit(model, entry.edit);
          }
          const typo = sql.indexOf("ERROR");
          if (typo >= 0) diagnostics.push(error(typo, typo + 5, 'syntax error at or near "ERROR"'));
          return { model: diagnostics.length ? null : model, diagnostics };
        },
      },
    });
    holder.session = created;
    let known = 0;
    const unsubscribe = created.subscribe(() => {
      const edits = created.getState().edits;
      for (const entry of edits.slice(known)) {
        if (!failNextRef.current) continue;
        failing.add(entry.id);
        setFailNext(false);
      }
      known = edits.length;
    });
    // The session owns a doc and timers: made here, handed to render.
    setSession(created);
    return () => {
      unsubscribe();
      created.destroy();
      setSession(null);
    };
  }, [role, fixture]);

  const state = useSessionState(session, (s) => s, null);
  return (
    <>
      <Workspace
        key={session?.key ?? 0}
        org="acme"
        db="blog"
        draft={draft}
        meId="u-alice"
        isWriter={role !== "reviewer"}
        session={session}
        state={state}
        onPublished={() => {}}
        onDeleted={() => {}}
      />
      <div
        role="group"
        aria-label="Dev controls"
        className="fixed right-3 bottom-36 z-(--z-popover) flex flex-col gap-1.5 rounded-lg border border-dashed border-border-strong bg-card/90 p-2 shadow-raised"
      >
        <Button
          size="sm"
          variant={failNext ? "primary" : "secondary"}
          onClick={() => setFailNext(!failNext)}
        >
          {failNext ? "Next edit fails…" : "Fail the next edit"}
        </Button>
        <Button
          size="sm"
          onClick={() =>
            session?.doc.transact(
              () => session.sql.insert(0, "-- typed by someone else\n"),
              "remote",
            )
          }
        >
          Someone types
        </Button>
      </div>
    </>
  );
}
