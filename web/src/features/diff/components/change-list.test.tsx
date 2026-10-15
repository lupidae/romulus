import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { vi } from "vitest";

import { TooltipProvider } from "@/components/ui/tooltip";
import blogDiff from "@/features/schema/fixtures/blog-diff.json";

import { type ChangeFilter, changeSections, type SchemaDiff } from "../changes";
import { ChangeList } from "./change-list";

const sections = changeSections(blogDiff as unknown as SchemaDiff);

function List({ onSelect = vi.fn() }: { onSelect?: (id: string) => void }) {
  const [filter, setFilter] = React.useState<ChangeFilter>("all");
  const [selected, setSelected] = React.useState<string | null>(null);
  return (
    <TooltipProvider>
      <ChangeList
        sections={sections}
        summary="2 tables added, 3 changed, 1 removed; 1 enum added"
        filter={filter}
        onFilterChange={setFilter}
        selected={selected}
        onSelect={(item) => {
          setSelected(item.id);
          onSelect(item.id);
        }}
        multiSchema={false}
      />
    </TooltipProvider>
  );
}

describe("ChangeList", () => {
  beforeAll(() => {
    // jsdom lays nothing out.
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("lists every changed object under its section, with its lines", () => {
    render(<List />);
    expect(screen.getByRole("heading", { name: /Changes/ })).toHaveTextContent("Changes7");
    const tables = screen.getByRole("region", { name: "Tables" });
    const posts = within(tables).getByRole("button", { name: /^posts, changed/ });
    expect(posts).toHaveAccessibleDescription(/^column posts\.body removed/);
    // An added table's only line would repeat its status.
    const added = within(tables).getByRole("button", { name: /^post_reactions, added/ });
    expect(added).not.toHaveTextContent("table post_reactions added");
    expect(screen.getByRole("region", { name: "Enums" })).toHaveTextContent("reaction_kind");
  });

  it("selects an object on click, and moves with the arrow keys", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<List onSelect={onSelect} />);
    const posts = screen.getByRole("button", { name: /^posts,/ });
    await user.click(posts);
    expect(onSelect).toHaveBeenCalledWith("table:public.posts");
    expect(posts).toHaveAttribute("aria-current", "true");
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("button", { name: /^profiles,/ })).toHaveFocus();
    await user.keyboard("{ArrowUp}{ArrowUp}");
    expect(screen.getByRole("button", { name: /^post_reactions,/ })).toHaveFocus();
  });

  it("filters by status, and says when nothing is left", async () => {
    const user = userEvent.setup();
    render(<List />);
    await user.click(screen.getByRole("radio", { name: "Removed: 1" }));
    const buttons = screen.getAllByRole("button", { name: /, (added|removed|changed)/ });
    expect(buttons.map((b) => b.textContent)).toEqual([expect.stringMatching(/^−profiles/)]);
    expect(screen.queryByRole("region", { name: "Enums" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "All: 7" }));
    expect(screen.getAllByRole("button", { name: /, (added|removed|changed)/ })).toHaveLength(7);
  });
});
