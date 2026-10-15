-- Tables that reference each other: foreign keys added once both exist.
CREATE TABLE departments (
    id          integer PRIMARY KEY,
    name        text NOT NULL UNIQUE,
    head_id     integer
);

CREATE TABLE employees (
    id             integer PRIMARY KEY,
    department_id  integer NOT NULL,
    mentor_id      integer,
    name           text NOT NULL
);

ALTER TABLE departments
    ADD CONSTRAINT departments_head_fkey FOREIGN KEY (head_id) REFERENCES employees (id)
    DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE employees
    ADD CONSTRAINT employees_department_fkey FOREIGN KEY (department_id) REFERENCES departments (id)
    DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE employees
    ADD CONSTRAINT employees_mentor_fkey FOREIGN KEY (mentor_id) REFERENCES employees (id) ON DELETE SET NULL;

CREATE TABLE categories (
    id         integer PRIMARY KEY,
    parent_id  integer REFERENCES categories (id) ON DELETE CASCADE,
    name       text NOT NULL,
    UNIQUE (parent_id, name)
);

CREATE TABLE assignments (
    department_id  integer NOT NULL,
    employee_id    integer NOT NULL,
    since          date NOT NULL,
    PRIMARY KEY (department_id, employee_id)
);
CREATE TABLE assignment_notes (
    department_id  integer NOT NULL,
    employee_id    integer NOT NULL,
    note           text NOT NULL,
    FOREIGN KEY (employee_id, department_id) REFERENCES assignments (employee_id, department_id) ON UPDATE CASCADE
);
ALTER TABLE assignments ADD CONSTRAINT assignments_unique_pair UNIQUE (employee_id, department_id);
