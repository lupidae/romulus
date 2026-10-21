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
import { useLocale } from "@/features/legal/locale";

import { billingCopy } from "../copy";
import { formatShortDate, money } from "../format";
import { invoicesQuery } from "../queries";

const statusVariant: Record<string, "success" | "warning" | "neutral"> = {
  paid: "success",
  open: "warning",
  uncollectible: "warning",
  void: "neutral",
};

/** Stripe's invoices for the organization, newest first (admins). */
export function InvoicesCard({ org }: { org: string }) {
  const locale = useLocale();
  const t = billingCopy[locale].invoices;
  const labels: Record<string, string> = {
    paid: t.paid,
    open: t.due,
    uncollectible: t.unpaid,
    void: t.void,
  };
  const invoices = useQuery(invoicesQuery(org));
  return (
    // min-w-0: the table scrolls inside the card rather than widening the page.
    <Card aria-labelledby="invoices-title" lang={locale} className="min-w-0">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle id="invoices-title">{t.title}</CardTitle>
          <p className="text-sm text-muted-foreground">{t.description}</p>
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
            title={t.loadFailed}
            action={
              <Button
                size="sm"
                loading={invoices.isFetching}
                onClick={() => void invoices.refetch()}
              >
                {billingCopy[locale].page.retry}
              </Button>
            }
          >
            {invoices.error.message}
          </Alert>
        </div>
      ) : invoices.data.length === 0 ? (
        <EmptyState icon={<PageIcon />} title={t.empty} description={t.emptyBody} />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t.date}</TableHead>
              <TableHead className="hidden sm:table-cell">{t.number}</TableHead>
              <TableHead className="text-right">{t.amount}</TableHead>
              <TableHead>{t.status}</TableHead>
              <TableHead className="w-12">
                <span className="sr-only">{t.open}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {invoices.data.map((invoice) => {
              const status = invoice.status ?? "";
              const variant = statusVariant[status];
              return (
                <TableRow key={invoice.id}>
                  <TableCell className="whitespace-nowrap">
                    {formatShortDate(invoice.created, locale)}
                  </TableCell>
                  <TableCell className="hidden font-mono text-sm sm:table-cell">
                    {invoice.number ?? "—"}
                  </TableCell>
                  <TableCell className="text-right tabular-nums">
                    {money(invoice.total_cents, invoice.currency, locale)}
                  </TableCell>
                  <TableCell>
                    {variant && <Badge variant={variant}>{labels[status]}</Badge>}
                  </TableCell>
                  <TableCell>
                    {invoice.url && (
                      <Button variant="ghost" size="sm" asChild>
                        <a href={invoice.url} target="_blank" rel="noreferrer">
                          {t.view}
                          <OpenNewWindowIcon aria-hidden />
                          <span className="sr-only"> {t.viewLabel(invoice.number ?? "")}</span>
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
