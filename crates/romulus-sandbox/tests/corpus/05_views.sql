-- Views over views, a recursive view, a materialized view.
CREATE TABLE staff (
    id          integer PRIMARY KEY,
    manager_id  integer REFERENCES staff (id),
    name        text NOT NULL,
    salary      numeric(10, 2) NOT NULL,
    department  text NOT NULL,
    active      boolean NOT NULL DEFAULT true
);

CREATE VIEW active_staff AS
    SELECT id, manager_id, name, department FROM staff WHERE active;

CREATE VIEW department_heads WITH (security_barrier = true) AS
    SELECT s.department, s.name
    FROM active_staff s
    WHERE s.manager_id IS NULL;

CREATE VIEW well_paid AS
    SELECT id, name, salary FROM staff WHERE salary > 100000
    WITH CASCADED CHECK OPTION;

CREATE RECURSIVE VIEW reporting_chain (id, manager_id, depth) AS
    SELECT id, manager_id, 0 FROM staff WHERE manager_id IS NULL
    UNION ALL
    SELECT s.id, s.manager_id, c.depth + 1 FROM staff s JOIN reporting_chain c ON s.manager_id = c.id;

CREATE VIEW salary_ranks AS
    SELECT name, department, rank() OVER (PARTITION BY department ORDER BY salary DESC) AS rank
    FROM staff;

CREATE MATERIALIZED VIEW department_payroll AS
    SELECT department, count(*) AS headcount, sum(salary) AS payroll
    FROM staff
    GROUP BY department;
COMMENT ON MATERIALIZED VIEW department_payroll IS 'Refreshed nightly';
COMMENT ON VIEW active_staff IS 'Who is still here';
