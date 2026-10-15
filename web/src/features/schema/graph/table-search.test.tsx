import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { TooltipProvider } from "@/components/ui/tooltip";

import blog from "../fixtures/blog.json";
import { parseSchema } from "../model";
import { buildGraph } from "./build";
import { TableSearch } from "./table-search";

const graph = buildGraph({ schema: parseSchema(blog) });

function setup() {
  const onPick = vi.fn();
  const onOpenChange = vi.fn();
  render(
    <TooltipProvider>
      <TableSearch open onOpenChange={onOpenChange} graph={graph} onPick={onPick} />
    </TooltipProvider>,
  );
  return { onPick, onOpenChange, user: userEvent.setup() };
}

describe("TableSearch", () => {
  test("is a labelled combobox listing every table before anything is typed", () => {
    setup();
    expect(screen.getByRole("dialog", { name: "Find a table" })).toBeInTheDocument();
    const field = screen.getByRole("combobox", { name: "Table or column name" });
    expect(field).toHaveFocus();
    expect(screen.getAllByRole("option")).toHaveLength(7);
    expect(screen.getAllByRole("option")[0]).toHaveAttribute("aria-selected", "true");
  });

  test("filters as you type, moves with the arrows and picks with Enter", async () => {
    const { user, onPick, onOpenChange } = setup();
    await user.keyboard("post");
    const options = screen.getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual(
      expect.arrayContaining([
        expect.stringContaining("posts"),
        expect.stringContaining("post_tags"),
      ]),
    );
    await user.keyboard("{ArrowDown}");
    const field = screen.getByRole("combobox");
    expect(field).toHaveAttribute("aria-activedescendant", screen.getAllByRole("option")[1]!.id);
    await user.keyboard("{Enter}");
    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(onPick).toHaveBeenCalledWith(expect.stringMatching(/^public\.post/), undefined);
  });

  test("finds a column and focuses its table", async () => {
    const { user, onPick } = setup();
    await user.keyboard("users.ema{Enter}");
    expect(onPick).toHaveBeenCalledWith("public.users", "email");
  });

  test("says when nothing matches", async () => {
    const { user } = setup();
    await user.keyboard("nothing-like-this");
    expect(screen.queryAllByRole("option")).toHaveLength(0);
    expect(screen.getByText("No table or column matches “nothing-like-this”.")).toBeInTheDocument();
  });

  test("a click picks the option under the pointer", async () => {
    const { user, onPick } = setup();
    await user.click(screen.getByRole("option", { name: /^tags/ }));
    expect(onPick).toHaveBeenCalledWith("public.tags", undefined);
  });
});
