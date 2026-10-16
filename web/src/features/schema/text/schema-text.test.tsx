import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Button } from "@/components/ui/button";
import { TooltipProvider } from "@/components/ui/tooltip";

import { SchemaText, type TextTab } from "./schema-text";

const migration = (position: TextTab["position"]): TextTab => ({
  id: "migration",
  label: "Migration",
  position,
  render: (active) => <p>{active ? "the migration, shown" : "the migration, kept"}</p>,
  actions: <Button aria-label="Download the migration">Download</Button>,
});

function renderText(props: Partial<React.ComponentProps<typeof SchemaText>>) {
  return render(
    <TooltipProvider>
      <SchemaText texts={{ sql: "CREATE TABLE a (x int);" }} {...props} />
    </TooltipProvider>,
  );
}

describe("SchemaText's own tabs", () => {
  test("sit before the formats or after the SQL", () => {
    const { unmount } = renderText({ tabs: [migration("start")] });
    const names = () => screen.getAllByRole("tab").map((tab) => tab.textContent);
    expect(names()).toEqual(["Migration", "SQL", "JSON", "DBML", "Mermaid"]);
    unmount();
    renderText({ tabs: [migration("after-sql")] });
    expect(names()).toEqual(["SQL", "Migration", "JSON", "DBML", "Mermaid"]);
  });

  test("show their content and actions when picked, and stay mounted", async () => {
    renderText({ tabs: [migration("after-sql")] });
    expect(screen.getByText("the migration, kept")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Download the migration" })).toBeNull();
    expect(screen.getByRole("button", { name: "Copy the SQL" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("tab", { name: "Migration" }));
    const panel = screen.getByRole("tabpanel", { name: "Migration" });
    expect(within(panel).getByText("the migration, shown")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Download the migration" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Copy the SQL" })).toBeNull();
  });

  test("say why the SQL is read-only, and what it is", () => {
    renderText({
      sqlTab: { locked: "Derived from the migration", notice: <p>Read-only: derived.</p> },
    });
    expect(screen.getByText("Read-only: derived.")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "SQL" })).toBeInTheDocument();
  });
});
