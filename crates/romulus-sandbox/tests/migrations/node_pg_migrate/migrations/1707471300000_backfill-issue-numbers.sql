-- Up Migration
ALTER TABLE issues ADD COLUMN number integer;

-- Number the existing issues per project, oldest first.
UPDATE issues AS i
SET number = numbered.n
FROM (
    SELECT id, row_number() OVER (PARTITION BY project_id ORDER BY id) AS n
    FROM issues
) AS numbered
WHERE numbered.id = i.id;

ALTER TABLE issues ALTER COLUMN number SET NOT NULL;
ALTER TABLE issues ADD CONSTRAINT issues_project_number_key UNIQUE (project_id, number);

-- Down Migration
ALTER TABLE issues DROP COLUMN number;
