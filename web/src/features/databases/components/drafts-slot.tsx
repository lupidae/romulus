import { DraftsSection } from "@/features/draft/components/drafts-section";
import { PlaygroundsSection } from "@/features/playground/components/entry";

/**
 * Where a database's open drafts are listed, above its versions: who is in
 * each, where it started. "New draft" is in the database's header. Its
 * playgrounds follow.
 */
export function DraftsSlot({ org, db }: { org: string; db: string }) {
  return (
    <>
      <DraftsSection org={org} db={db} withNewButton={false} />
      <PlaygroundsSection org={org} db={db} />
    </>
  );
}
