import { useQuery } from "@tanstack/react-query";

import { OpenNewWindowIcon, PageIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatDate } from "@/lib/time";

import { euros } from "../format";
import { type InvoiceSummary, invoicesQuery } from "../queries";

const statusBadge: Record<string, { label: string; variant: "success" | "warning" | "neutral" }> = {
  paid: { label: "Paid", variant: "success" },
  open: { label: "Due", variant: "warning" },
  uncollectible: { label: "Unpaid", variant: "warning" },
  void: { label: "Void", variant: "neutral" },
};

function amount(invoice: InvoiceSummary): string {
  return invoice.currency.toLowerCase() === "eur"
    ? euros(invoice.total_cents)
    : `${(invoice.total_cents / 100).toFixed(2)} ${invoice.currency.toUpperCase()}`;
}

/** Stripe's invoices for the organization, newest first (admins). */
export function InvoicesCard({ org }: { org: string }) {
  const invoices = useQuery(invoicesQuery(org));
  return (
    <Card aria-labelledby="invoices-title">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle id="invoices-title">Invoices</CardTitle>
          <p className="text-sm text-muted-foreground">
            Each one opens on Stripe, to read, download or pay.
          </p>
        </div>
      </CardHeader>
      {invoices.isPending ? (
        <div aria-hidden className="divide-y divide-border">
          {[0, 1].map((i) => (
            <div key={i} className="flex items-center gap-4 px-4 py-3">
              <Skeleton className="h-4 w-20" />
              <Skeleton className="h-4 flex-1" />
              <Skeleton className="h-4 w-14" />
            </div>
          ))}
        </div>
      ) : invoices.isError ? (
        <div className="p-4">
          <Alert
            variant="destructive"
            title="Could not load the invoices"
            action={
              <Button
                size="sm"
                loading={invoices.isFetching}
                onClick={() => void invoices.refetch()}
              >
                Retry
              </Button>
            }
          >
            {invoices.error.message}
          </Alert>
        </div>
      ) : invoices.data.length === 0 ? (
        <EmptyState
          icon={<PageIcon />}
          title="No invoices yet"
          description="Each payment's invoice appears here, starting with the first plan you choose."
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Date</TableHead>
              <TableHead className="hidden sm:table-cell">Number</TableHead>
              <TableHead className="text-right">Amount</TableHead>
              <TableHead>Status</TableHead>
              <TableHead className="w-12">
                <span className="sr-only">Open</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {invoices.data.map((invoice) => {
              const status = statusBadge[invoice.status ?? ""];
              return (
                <TableRow key={invoice.id}>
                  <TableCell className="whitespace-nowrap">{formatDate(invoice.created)}</TableCell>
                  <TableCell className="hidden font-mono text-sm sm:table-cell">
                    {invoice.number ?? "—"}
                  </TableCell>
                  <TableCell className="text-right tabular-nums">{amount(invoice)}</TableCell>
                  <TableCell>
                    {status && <Badge variant={status.variant}>{status.label}</Badge>}
                  </TableCell>
                  <TableCell>
                    {invoice.url && (
                      <Button variant="ghost" size="sm" asChild>
                        <a href={invoice.url} target="_blank" rel="noreferrer">
                          View
                          <OpenNewWindowIcon aria-hidden />
                          <span className="sr-only">
                            {" "}
                            invoice {invoice.number ?? ""} (opens in a new tab)
                          </span>
                        </a>
                      </Button>
                    )}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
    </Card>
  );
}
