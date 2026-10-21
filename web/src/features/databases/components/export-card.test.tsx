import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { vi } from "vitest";

import { toast, Toaster } from "@/components/ui/toaster";

import { ExportButton } from "./export-card";

function renderButton() {
  // The toaster follows the theme; jsdom has no media queries.
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <ExportButton href="/api/orgs/acme/export" label="Export organization (ZIP)" />
      <Toaster />
    </QueryClientProvider>,
  );
}

/** The names of the files handed to the browser as downloads. */
function watchSaves() {
  const saved: string[] = [];
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:export");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    saved.push(this.download);
  });
  return saved;
}

afterEach(() => {
  // Toasts live in a store of their own, which outlives each test's Toaster.
  toast.dismiss();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("ExportButton", () => {
  it("says how long to wait when the export is rate limited, and saves no file", async () => {
    const saved = watchSaves();
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              code: "rate_limited",
              message: "Too many requests. Try again in 30 s.",
              details: { retry_after: 30 },
            }),
            {
              status: 429,
              headers: { "content-type": "application/json", "retry-after": "30" },
            },
          ),
        ),
      ),
    );
    renderButton();

    await userEvent.click(screen.getByRole("button", { name: "Export organization (ZIP)" }));

    expect(await screen.findByText("Too many exports for now")).toBeInTheDocument();
    expect(screen.getByText("Try again in 30 seconds.")).toBeInTheDocument();
    expect(saved).toEqual([]);
  });

  it("reads the wait from Retry-After when the body has none", async () => {
    const saved = watchSaves();
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve(
          new Response("<html>Too Many Requests</html>", {
            status: 429,
            headers: { "content-type": "text/html", "retry-after": "120" },
          }),
        ),
      ),
    );
    renderButton();

    await userEvent.click(screen.getByRole("button", { name: "Export organization (ZIP)" }));

    expect(await screen.findByText("Try again in 2 minutes.")).toBeInTheDocument();
    expect(saved).toEqual([]);
  });

  it("saves the ZIP under the name the API gives", async () => {
    const saved = watchSaves();
    const fetch = vi.fn(() =>
      Promise.resolve(
        new Response(new Blob(["PK"]), {
          status: 200,
          headers: {
            "content-type": "application/zip",
            "content-disposition": 'attachment; filename="acme-export-2026-10-21.zip"',
          },
        }),
      ),
    );
    vi.stubGlobal("fetch", fetch);
    renderButton();

    await userEvent.click(screen.getByRole("button", { name: "Export organization (ZIP)" }));

    await vi.waitFor(() => expect(saved).toEqual(["acme-export-2026-10-21.zip"]));
    expect(fetch).toHaveBeenCalledWith("/api/orgs/acme/export", { credentials: "same-origin" });
    expect(screen.queryByText("Too many exports for now")).not.toBeInTheDocument();
  });
});
