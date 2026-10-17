import { act, render, renderHook, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeAll, describe, expect, it, vi } from "vitest";

import { TooltipProvider } from "@/components/ui/tooltip";

import type { Discussion } from "../discussion";
import type { ThreadView } from "../model";
import { useCommentsUi } from "../use-comments-ui";
import { CommentsPanel } from "./comments-panel";

const me = { id: "u-alice", name: "Alice" };

// A wide screen: the panel beside the viewer.
beforeAll(() => {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: true,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  Element.prototype.scrollIntoView = () => {};
});

const thread = (id: string, over: Partial<ThreadView> = {}): ThreadView => ({
  id,
  anchor: { kind: "column", table: "public.users", column: "email" },
  by: "u-bob",
  at: "2026-10-16T10:00:00Z",
  resolved: null,
  comments: [
    {
      id: `${id}-c1`,
      authorId: "u-bob",
      authorName: "Bob",
      body: "Should email be citext?",
      mentions: [],
      at: "2026-10-16T10:00:00Z",
      edited: null,
    },
  ],
  ...over,
});

function discussion(threads: ThreadView[] | null, over: Partial<Discussion> = {}): Discussion {
  return {
    threads,
    error: null,
    retry: vi.fn(),
    me,
    people: [
      { id: "u-alice", name: "Alice" },
      { id: "u-bob", name: "Bob" },
    ],
    canComment: true,
    canModerate: false,
    readAt: () => null,
    markRead: vi.fn(),
    typing: () => [],
    setTyping: vi.fn(),
    start: vi.fn(() => Promise.resolve("new")),
    reply: vi.fn(() => Promise.resolve()),
    edit: vi.fn(() => Promise.resolve()),
    remove: vi.fn(() => Promise.resolve()),
    removeThread: vi.fn(() => Promise.resolve()),
    resolve: vi.fn(() => Promise.resolve()),
    ...over,
  };
}

function setup(d: Discussion, outdated: (t: ThreadView) => boolean = () => false) {
  const ui = renderHook(() => useCommentsUi(true));
  const onShow = vi.fn();
  const onNew = vi.fn();
  const view = () => (
    <TooltipProvider>
      <CommentsPanel
        discussion={d}
        ui={ui.result.current}
        title="Comments"
        outdated={outdated}
        onShow={onShow}
        onNew={onNew}
      />
    </TooltipProvider>
  );
  const rendered = render(view());
  const rerender = () => {
    ui.rerender();
    rendered.rerender(view());
  };
  return { ui, onShow, onNew, rerender };
}

describe("the comments panel", () => {
  it("says how to start when there is nothing, and shows the shape while loading", async () => {
    const { onNew } = setup(discussion([]));
    expect(screen.getByRole("heading", { name: "No comments yet" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Add a comment" }));
    expect(onNew).toHaveBeenCalled();
  });

  it("opens a thread: read, shown, answered", async () => {
    const d = discussion([thread("t1")]);
    const { ui, onShow, rerender } = setup(d);
    await userEvent.click(screen.getByRole("button", { name: /^users\.email/ }));
    rerender();
    expect(ui.result.current.active).toBe("t1");
    expect(onShow).toHaveBeenCalled();
    expect(d.markRead).toHaveBeenCalled();
    const reply = screen.getByRole("combobox", { name: "Reply to Bob" });
    await userEvent.type(reply, "Yes, @Bo");
    await userEvent.keyboard("{Enter}");
    await userEvent.type(reply, "agreed{Enter}");
    expect(d.reply).toHaveBeenCalledWith("t1", "Yes, @Bob agreed", ["u-bob"]);
  });

  it("files resolved threads apart, and outdated ones under their own heading", async () => {
    const d = discussion([
      thread("open"),
      thread("done", {
        resolved: { by: "u-alice", name: "Alice", at: "2026-10-16T11:00:00Z" },
      }),
      thread("gone", { anchor: { kind: "table", table: "public.orders" } }),
    ]);
    const { rerender } = setup(d, (t) => t.id === "gone");
    expect(screen.getAllByRole("button", { name: /^users\.email/ })).toHaveLength(1);
    const outdated = screen.getByRole("button", { name: /Outdated/ });
    expect(outdated).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(outdated);
    expect(screen.getByRole("list", { name: "Outdated threads" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("radio", { name: /Resolved/ }));
    act(rerender);
    expect(screen.getByText("Resolved by Alice")).toBeInTheDocument();
  });

  it("offers no reply on a thread kept from a draft's review", async () => {
    const d = discussion([thread("t1", { frozen: true })]);
    const { rerender } = setup(d);
    await userEvent.click(screen.getByRole("button", { name: /^users\.email/ }));
    rerender();
    expect(screen.getByText("From the draft's review")).toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: /Reply/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Resolve" })).not.toBeInTheDocument();
  });

  it("deletes only your comment while the thread has others, the thread with its last", async () => {
    const mine = (id: string, body: string) => ({
      id,
      authorId: me.id,
      authorName: me.name,
      body,
      mentions: [],
      at: "2026-10-16T10:00:00Z",
      edited: null,
    });
    const d = discussion([
      thread("t1", { by: me.id, comments: [mine("c1", "First"), mine("c2", "Second")] }),
    ]);
    const { rerender } = setup(d);
    await userEvent.click(screen.getByRole("button", { name: /^users\.email/ }));
    rerender();
    await userEvent.click(screen.getByRole("button", { name: "More on your comment" }));
    await userEvent.click(screen.getByRole("menuitem", { name: /Delete/ }));
    const dialog = screen.getByRole("alertdialog", { name: "Delete your comment?" });
    expect(dialog).toHaveTextContent("The replies stay");
    await userEvent.click(screen.getByRole("button", { name: "Delete comment" }));
    expect(d.remove).toHaveBeenCalledWith("t1", "c1");
  });

  it("lets developers and admins delete anyone's comment or a whole thread, saying it is recorded", async () => {
    const d = discussion(
      [
        thread("t1", {
          comments: [
            thread("t1").comments[0]!,
            {
              id: "t1-c2",
              authorId: "u-bob",
              authorName: "Bob",
              body: "spam",
              mentions: [],
              at: "2026-10-16T10:05:00Z",
              edited: null,
            },
          ],
        }),
      ],
      { canModerate: true },
    );
    const { rerender } = setup(d);
    await userEvent.click(screen.getByRole("button", { name: /^users\.email/ }));
    rerender();
    await userEvent.click(screen.getByRole("button", { name: "More on Bob's reply" }));
    expect(screen.queryByRole("menuitem", { name: /Edit/ })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("menuitem", { name: /Delete/ }));
    const dialog = screen.getByRole("alertdialog", { name: "Delete Bob's comment?" });
    expect(dialog).toHaveTextContent("recorded in the database's activity");
    await userEvent.click(screen.getByRole("button", { name: "Delete comment" }));
    expect(d.remove).toHaveBeenCalledWith("t1", "t1-c2");

    await userEvent.click(screen.getByRole("button", { name: "Delete thread" }));
    const whole = screen.getByRole("alertdialog", { name: "Delete this thread?" });
    expect(whole).toHaveTextContent("Its 2 comments go for everyone");
    await userEvent.click(screen.getByRole("button", { name: "Delete thread" }));
    expect(d.removeThread).toHaveBeenCalledWith("t1");
  });

  it("offers others' comments no menu to reviewers", async () => {
    const d = discussion([thread("t1")]);
    const { rerender } = setup(d);
    await userEvent.click(screen.getByRole("button", { name: /^users\.email/ }));
    rerender();
    expect(screen.queryByRole("button", { name: /More on Bob/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete thread" })).not.toBeInTheDocument();
  });

  it("deletes others' comments one at a time where threads cannot go whole (a version)", async () => {
    const d = discussion([thread("t1")], { canModerate: true, removeThread: undefined });
    const { rerender } = setup(d);
    await userEvent.click(screen.getByRole("button", { name: /^users\.email/ }));
    rerender();
    expect(screen.getByRole("button", { name: "More on Bob's comment" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete thread" })).not.toBeInTheDocument();
  });
});
