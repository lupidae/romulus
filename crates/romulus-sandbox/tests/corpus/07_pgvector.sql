-- Retrieval-augmented search over documents.
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE documents (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source    text NOT NULL,
    content   text NOT NULL,
    embedding vector(1536),
    summary_embedding halfvec(256)
);
CREATE INDEX documents_embedding_idx ON documents USING hnsw (embedding vector_cosine_ops) WITH (m = 16, ef_construction = 64);
CREATE INDEX documents_summary_idx ON documents USING hnsw (summary_embedding halfvec_l2_ops);

CREATE TABLE chunks (
    document_id bigint NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    ordinal     integer NOT NULL,
    embedding   vector(1536) NOT NULL,
    PRIMARY KEY (document_id, ordinal)
);
COMMENT ON COLUMN chunks.embedding IS 'text-embedding-3-small';
