-- Shared by every table that has an updated_at column.
CREATE FUNCTION set_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END
$$;

COMMENT ON FUNCTION set_updated_at() IS
    'BEFORE UPDATE trigger: keeps updated_at honest without every query having to set it.';
