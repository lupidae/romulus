CREATE OR REPLACE VIEW issue_counts AS
SELECT project_id, count(*) AS issues FROM issues GROUP BY project_id;
