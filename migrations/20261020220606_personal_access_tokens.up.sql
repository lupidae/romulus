-- Personal access tokens: how a person's AI agent (an MCP client: Claude Code,
-- Claude Desktop, Cursor…) calls romulus as them, at /api/mcp
-- (auth::access_tokens, the mcp module). A token acts as its person, with
-- their roles in every organization; `read` ones never write. Shown once,
-- only its sha256 kept, like api_tokens; revoking keeps the row.
CREATE TYPE access_token_scope AS ENUM ('read', 'write');

CREATE TABLE personal_access_tokens (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    name text NOT NULL CHECK (btrim(name) <> '' AND char_length(name) <= 100),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    scope access_token_scope NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz CHECK (expires_at IS NULL OR expires_at > created_at),
    last_used_at timestamptz,
    last_client text CHECK (last_client IS NULL OR char_length(last_client) <= 200),
    revoked_at timestamptz
);

COMMENT ON TABLE personal_access_tokens IS
    'A person''s tokens for their AI agent (the MCP endpoint, /api/mcp): each acts as its person, with their roles. `rmls_pat_` + a random token, only its sha256 stored.';
COMMENT ON COLUMN personal_access_tokens.scope IS
    '`read`: reading tools only, never a write; `write`: everything the person''s roles allow.';
COMMENT ON COLUMN personal_access_tokens.expires_at IS 'NULL: never expires (until revoked).';
COMMENT ON COLUMN personal_access_tokens.last_used_at IS 'The last request it authenticated, written at most once a minute.';
COMMENT ON COLUMN personal_access_tokens.last_client IS
    'The MCP client that last said who it is with it (its clientInfo: `claude-code 2.1.4`), as it said it.';

CREATE INDEX personal_access_tokens_user_id_idx ON personal_access_tokens (user_id, created_at DESC);

-- What a person's agent did with a write token, in the database's Activity
-- (`details`: the token, the tool, what it touched).
ALTER TYPE audit_action ADD VALUE 'agent_action';
