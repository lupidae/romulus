/** The command CI runs to push a database's schema: remus JSON, through curl. */
export function pushCommand(databaseId: string, origin = window.location.origin) {
  return [
    'remus -f json | curl -fsS -H "Authorization: Bearer $ROMULUS_TOKEN" \\',
    `  --data-binary @- ${origin}/api/databases/${databaseId}/versions`,
  ].join("\n");
}
