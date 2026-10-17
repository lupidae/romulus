// A discussion, as the comments panel, the graph's pins and the SQL pane
// use it: the threads, who may write, and what writing does. Two sources: a
// draft (its Y.Doc, live, checked by the server's room) and a version (REST:
// its draft's review, frozen, and threads started on its page).

import { queryOptions, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { api, succeed, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";
import type { MentionPerson } from "@/components/ui/mention-textarea";
import { membersQuery } from "@/features/orgs/queries";
import type { DraftSession, Me } from "@/features/draft/session";
import { usePeers, useSessionState, useYMap } from "@/features/draft/use-session";

import { type Anchor, lastActivity, threadsOf, type ThreadView } from "./model";

export type VersionThread = components["schemas"]["VersionThread"];

export interface Discussion {
  /** Null while they load. */
  threads: ThreadView[] | null;
  error: Error | null;
  retry: () => void;
  me: Me;
  /** Who `@` suggests. */
  people: MentionPerson[];
  /** May start threads, reply and resolve (frozen threads aside). */
  canComment: boolean;
  /** May delete anyone's comments and threads (a draft's developers and admins). */
  canModerate: boolean;
  /** Up to when this person read a thread (null: never). */
  readAt: (thread: string) => string | null;
  markRead: (thread: ThreadView) => void;
  /** Who else is writing in a thread now (`new`: starting one). */
  typing: (thread: string) => string[];
  setTyping: (thread: string | null) => void;
  start: (anchor: Anchor, body: string, mentions: string[]) => Promise<string>;
  reply: (thread: string, body: string, mentions: string[]) => Promise<void>;
  edit: (thread: string, comment: string, body: string, mentions: string[]) => Promise<void>;
  remove: (thread: string, comment: string) => Promise<void>;
  /** A thread and every comment in it (absent: one comment at a time). */
  removeThread?: (thread: string) => Promise<void>;
  resolve: (thread: string, resolved: boolean) => Promise<void>;
}

export const commentKeys = {
  draftReads: (org: string, db: string, draft: string) =>
    ["drafts", org, db, draft, "reads"] as const,
  versionThreads: (org: string, db: string, number: number) =>
    ["orgs", org, "databases", db, "versions", number, "threads"] as const,
};

const NO_PEOPLE: MentionPerson[] = [];

function usePeople(org: string, enabled: boolean): MentionPerson[] {
  const members = useQuery({ ...membersQuery(org), enabled, staleTime: 60_000 });
  return React.useMemo(
    () => members.data?.map((m) => ({ id: m.user_id, name: m.name, detail: m.role })) ?? NO_PEOPLE,
    [members.data],
  );
}

/** Where this person read each thread, and marking more: the server's, optimistic here. */
function useReads(
  initial: (thread: string) => string | null,
  save: ((thread: string, at: string) => Promise<void>) | null,
) {
  const [mine, setMine] = React.useState<Record<string, string>>({});
  const readAt = React.useCallback(
    (thread: string) => {
      const local = mine[thread] ?? null;
      const known = initial(thread);
      return local && (!known || local > known) ? local : known;
    },
    [mine, initial],
  );
  const markRead = React.useCallback(
    (thread: ThreadView) => {
      const at = lastActivity(thread);
      const now = readAt(thread.id);
      if (now && now >= at) return;
      setMine((m) => ({ ...m, [thread.id]: at }));
      void save?.(thread.id, at).catch(() => {
        // An unread marker that comes back is no harm: nothing to tell.
      });
    },
    [readAt, save],
  );
  return { readAt, markRead };
}

export const draftReadsQuery = (org: string, db: string, draft: string) =>
  queryOptions({
    queryKey: commentKeys.draftReads(org, db, draft),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/drafts/{id}/reads", {
          params: { path: { org, db, id: draft } },
        }),
      ),
    staleTime: 60_000,
  });

/**
 * A draft's discussion: its doc's threads, live. `local`: a draft without a
 * server (dev screens): no read marks or members to fetch.
 */
export function useDraftDiscussion({
  session,
  org,
  db,
  draftId,
  me,
  canComment,
  canModerate = false,
  local = false,
  people: given,
}: {
  session: DraftSession | null;
  org: string;
  db: string;
  draftId: string;
  me: Me;
  canComment: boolean;
  /** Developers and admins: delete anyone's comments (the server records it). */
  canModerate?: boolean;
  local?: boolean;
  people?: MentionPerson[];
}): Discussion {
  const synced = useSessionState(session, (s) => s.synced, false);
  const threadsMap = useYMap(session?.threads);
  const commentsMap = useYMap(session?.comments);
  const threads = React.useMemo(
    () => (synced ? threadsOf(threadsMap, commentsMap) : null),
    [synced, threadsMap, commentsMap],
  );
  const peers = usePeers(session);
  const fetched = usePeople(org, !local && !given);
  const people = given ?? fetched;

  const reads = useQuery({ ...draftReadsQuery(org, db, draftId), enabled: !local });
  const known = React.useMemo(
    () => new Map(reads.data?.map((r) => [r.thread, r.read_at]) ?? []),
    [reads.data],
  );
  const initial = React.useCallback((thread: string) => known.get(thread) ?? null, [known]);
  const save = React.useMemo(
    () =>
      local
        ? null
        : (thread: string, at: string) =>
            succeed(
              api.PUT("/api/orgs/{org}/databases/{db}/drafts/{id}/reads/{thread}", {
                params: { path: { org, db, id: draftId, thread } },
                body: { at },
              }),
            ),
    [local, org, db, draftId],
  );
  const { readAt, markRead } = useReads(initial, save);

  const typing = React.useCallback(
    (thread: string) => {
      const names = peers.filter((p) => p.typing === thread).map((p) => p.user.name);
      return [...new Set(names)];
    },
    [peers],
  );

  return React.useMemo<Discussion>(
    () => ({
      threads,
      error: null,
      retry: () => {},
      me,
      people,
      canComment: canComment && !!session,
      canModerate: canModerate && canComment && !!session,
      readAt,
      markRead,
      typing,
      setTyping: (thread) => session?.setTyping(thread),
      start: (anchor, body, mentions) =>
        session ? session.startThread(anchor, body, mentions, me) : notOpen(),
      reply: (thread, body, mentions) =>
        session ? session.reply(thread, body, mentions, me) : notOpen(),
      edit: (_thread, comment, body, mentions) =>
        session ? session.editComment(comment, body, mentions, me) : notOpen(),
      remove: (_thread, comment) => (session ? session.deleteComment(comment, me) : notOpen()),
      removeThread: (thread) => (session ? session.deleteThread(thread, me) : notOpen()),
      resolve: (thread, resolved) =>
        session ? session.setResolved(thread, resolved, me) : notOpen(),
    }),
    [threads, me, people, canComment, canModerate, session, readAt, markRead, typing],
  );
}

function notOpen(): Promise<never> {
  return Promise.reject(new Error("The draft is still opening."));
}

// ---- A version's ----------------------------------------------------------------

export const versionThreadsQuery = (org: string, db: string, number: number) =>
  queryOptions({
    queryKey: commentKeys.versionThreads(org, db, number),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/versions/{number}/threads", {
          params: { path: { org, db, number } },
        }),
      ),
    // No live channel on a version's page: others' comments come in a while.
    refetchInterval: 20_000,
  });

function anchorOf(anchor: VersionThread["anchor"]): Anchor {
  switch (anchor.kind) {
    case "table":
      return anchor.statement
        ? { kind: "table", table: anchor.table, statement: anchor.statement }
        : { kind: "table", table: anchor.table };
    case "column":
      return anchor.statement
        ? {
            kind: "column",
            table: anchor.table,
            column: anchor.column,
            statement: anchor.statement,
          }
        : { kind: "column", table: anchor.table, column: anchor.column };
    case "point":
      return { kind: "point", x: anchor.x, y: anchor.y };
    case "statement":
      return { kind: "statement", statement: anchor.statement };
  }
}

export function versionThreadView(t: VersionThread): ThreadView {
  return {
    id: t.id,
    anchor: anchorOf(t.anchor),
    by: t.created_by ?? null,
    at: t.created_at,
    resolved: t.resolved
      ? { by: t.resolved.by ?? null, name: t.resolved.name, at: t.resolved.at }
      : null,
    comments: t.comments.map((c) => ({
      id: c.id,
      authorId: c.author ?? null,
      authorName: c.author_name,
      body: c.body,
      mentions: c.mentions,
      at: c.created_at,
      edited: c.edited_at ?? null,
    })),
    frozen: t.frozen,
  };
}

/** A version's discussion over REST: its draft's review (frozen), and threads since. */
export function useVersionDiscussion({
  org,
  db,
  number,
  me,
  canModerate = false,
}: {
  org: string;
  db: string;
  number: number;
  me: Me;
  /** Developers and admins: delete anyone's comments (the server records it). */
  canModerate?: boolean;
}): Discussion {
  const queryClient = useQueryClient();
  const key = commentKeys.versionThreads(org, db, number);
  const query = useQuery(versionThreadsQuery(org, db, number));
  const people = usePeople(org, true);
  const threads = React.useMemo(() => query.data?.map(versionThreadView) ?? null, [query.data]);
  const known = React.useMemo(
    () => new Map(query.data?.map((t) => [t.id, t.read_at ?? null]) ?? []),
    [query.data],
  );
  const initial = React.useCallback((thread: string) => known.get(thread) ?? null, [known]);
  const save = React.useCallback(
    (thread: string, at: string) =>
      succeed(
        api.PUT("/api/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/read", {
          params: { path: { org, db, number, thread } },
          body: { at },
        }),
      ),
    [org, db, number],
  );
  const { readAt, markRead } = useReads(initial, save);

  /** Puts the thread the API answered in place (or first, when new). */
  const put = React.useCallback(
    (thread: VersionThread) =>
      queryClient.setQueryData<VersionThread[]>(key, (list) => {
        if (!list) return [thread];
        return list.some((t) => t.id === thread.id)
          ? list.map((t) => (t.id === thread.id ? thread : t))
          : [...list, thread];
      }),
    // `key` is derived from these.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [queryClient, org, db, number],
  );
  const path = { org, db, number };
  const start = useMutation({
    mutationFn: (input: { anchor: Anchor; body: string; mentions: string[] }) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/versions/{number}/threads", {
          params: { path },
          body: input,
        }),
      ),
    onSuccess: put,
  });
  const reply = useMutation({
    mutationFn: ({
      thread,
      body,
      mentions,
    }: {
      thread: string;
      body: string;
      mentions: string[];
    }) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/comments", {
          params: { path: { ...path, thread } },
          body: { body, mentions },
        }),
      ),
    onSuccess: put,
  });
  const edit = useMutation({
    mutationFn: (input: { thread: string; comment: string; body: string; mentions: string[] }) =>
      unwrap(
        api.PATCH(
          "/api/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/comments/{comment}",
          {
            params: { path: { ...path, thread: input.thread, comment: input.comment } },
            body: { body: input.body, mentions: input.mentions },
          },
        ),
      ),
    onSuccess: put,
  });
  const remove = useMutation({
    mutationFn: ({ thread, comment }: { thread: string; comment: string }) =>
      succeed(
        api.DELETE(
          "/api/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/comments/{comment}",
          { params: { path: { ...path, thread, comment } } },
        ),
      ),
    onSuccess: (_, { thread, comment }) =>
      queryClient.setQueryData<VersionThread[]>(key, (list) =>
        list
          ?.map((t) =>
            t.id === thread ? { ...t, comments: t.comments.filter((c) => c.id !== comment) } : t,
          )
          .filter((t) => t.comments.length > 0),
      ),
  });
  const resolve = useMutation({
    mutationFn: ({ thread, resolved }: { thread: string; resolved: boolean }) =>
      unwrap(
        resolved
          ? api.POST("/api/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/resolve", {
              params: { path: { ...path, thread } },
            })
          : api.POST("/api/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/reopen", {
              params: { path: { ...path, thread } },
            }),
      ),
    onSuccess: put,
  });

  const { mutateAsync: startAsync } = start;
  const { mutateAsync: replyAsync } = reply;
  const { mutateAsync: editAsync } = edit;
  const { mutateAsync: removeAsync } = remove;
  const { mutateAsync: resolveAsync } = resolve;
  const { refetch } = query;
  return React.useMemo<Discussion>(
    () => ({
      threads,
      error: query.error,
      retry: () => void refetch(),
      me,
      people,
      canComment: true,
      canModerate,
      readAt,
      markRead,
      typing: () => [],
      setTyping: () => {},
      start: async (anchor, body, mentions) => (await startAsync({ anchor, body, mentions })).id,
      reply: async (thread, body, mentions) => {
        await replyAsync({ thread, body, mentions });
      },
      edit: async (thread, comment, body, mentions) => {
        await editAsync({ thread, comment, body, mentions });
      },
      remove: (thread, comment) => removeAsync({ thread, comment }),
      resolve: async (thread, resolved) => {
        await resolveAsync({ thread, resolved });
      },
    }),
    [
      threads,
      query.error,
      refetch,
      me,
      people,
      canModerate,
      readAt,
      markRead,
      startAsync,
      replyAsync,
      editAsync,
      removeAsync,
      resolveAsync,
    ],
  );
}
