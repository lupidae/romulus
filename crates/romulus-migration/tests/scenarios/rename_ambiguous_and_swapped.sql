-- Two equally good targets for one column: none is chosen (dropped and
-- added). Two columns swapped with ALTER (known): through a temporary name.
CREATE TABLE contacts (
    note text,
    id integer PRIMARY KEY,
    a integer,
    b text
);
-- ==== new ====
CREATE TABLE contacts (
    note text,
    id integer PRIMARY KEY,
    a integer,
    b text
);
ALTER TABLE contacts DROP COLUMN note;
ALTER TABLE contacts ADD COLUMN first_remark text;
ALTER TABLE contacts ADD COLUMN second_remark text;
ALTER TABLE contacts RENAME COLUMN a TO tmp;
ALTER TABLE contacts RENAME COLUMN b TO a;
ALTER TABLE contacts RENAME COLUMN tmp TO b;
