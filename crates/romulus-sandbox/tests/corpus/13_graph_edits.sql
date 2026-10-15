CREATE TABLE users (
    id     bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email  text NOT NULL
);
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
CREATE POLICY users_self ON users TO app_user USING (id = current_setting('app.user_id')::bigint);

CREATE TABLE posts (
    id       bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id  bigint NOT NULL,
    body     text NOT NULL,
    draft    boolean NOT NULL DEFAULT true
);
CREATE POLICY posts_author ON posts TO app_user, moderator USING (user_id = current_setting('app.user_id')::bigint);

CREATE TABLE tmp_import (id int);

-- edits from the graph
ALTER TABLE users RENAME TO members;
ALTER TABLE members RENAME COLUMN email TO login_email;
ALTER TABLE posts RENAME COLUMN user_id TO member_id;
ALTER TABLE posts ADD COLUMN published_at timestamptz;
ALTER TABLE posts ADD CONSTRAINT posts_member_id_fkey FOREIGN KEY (member_id) REFERENCES members (id) ON DELETE CASCADE;
COMMENT ON TABLE members IS 'People with an account';
COMMENT ON COLUMN posts.published_at IS 'Null while a draft';
ALTER TABLE posts DROP COLUMN draft;
DROP TABLE tmp_import;
ALTER POLICY posts_author ON posts RENAME TO posts_by_author;
