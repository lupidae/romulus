ALTER TABLE draft_compiles
    DROP CONSTRAINT draft_compiles_good_tables_with_model,
    DROP COLUMN good_tables,
    DROP COLUMN good_model;
