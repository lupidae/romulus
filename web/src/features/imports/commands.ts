import alembic from "../../../../scripts/export-migrations/alembic.sh?raw";
import django from "../../../../scripts/export-migrations/django.sh?raw";
import rails from "../../../../scripts/export-migrations/rails.sh?raw";

/** The command CI runs to import a migrations folder, zipped on the fly. */
export function importCommand(databaseId: string, origin = window.location.origin) {
  return [
    "cd migrations && zip -qr - . | curl -fsS \\",
    '  -H "Authorization: Bearer $ROMULUS_TOKEN" -H "Content-Type: application/zip" \\',
    `  --data-binary @- ${origin}/api/databases/${databaseId}/imports`,
  ].join("\n");
}

/** The tools whose migrations are code: their exporter, and how to run it. */
export const exporters = [
  {
    tool: "alembic",
    label: "Alembic",
    file: "alembic.sh",
    script: alembic,
    run: "bash alembic.sh > alembic.sql",
    note: "Next to alembic.ini, with the project's virtualenv active. Uses Alembic's offline mode: no database.",
  },
  {
    tool: "django",
    label: "Django",
    file: "django.sh",
    script: django,
    run: "DATABASE_URL=postgres://localhost/romulus_export bash django.sh --database default > django.sql",
    note: "Next to manage.py. --database names the settings' alias of an empty scratch database: each migration is applied to it.",
  },
  {
    tool: "rails",
    label: "Rails",
    file: "rails.sh",
    script: rails,
    run: "DATABASE_URL=postgres://localhost/romulus_export bash rails.sh > rails.sql",
    note: "From the app's root, pointed at an empty scratch database: each migration is applied to it.",
  },
] as const;
