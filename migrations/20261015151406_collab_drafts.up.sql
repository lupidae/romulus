-- What the collab server keeps besides the last compile result: the last
-- model that compiled, so the graph has something to show while the text has
-- errors (also to a client that connects then), and where each of its tables
-- was created, so a rename typed in the text keeps the table's place.
ALTER TABLE draft_compiles
    ADD COLUMN good_model jsonb,
    ADD COLUMN good_tables jsonb,
    ADD CONSTRAINT draft_compiles_good_tables_with_model CHECK ((good_model IS NULL) = (good_tables IS NULL));

COMMENT ON COLUMN draft_compiles.good_model IS
    'The model of the last compile that succeeded (equal to model when the last one did).';
COMMENT ON COLUMN draft_compiles.good_tables IS
    'For good_model: qualified table name -> index of its CREATE statement in the text it came from.';
