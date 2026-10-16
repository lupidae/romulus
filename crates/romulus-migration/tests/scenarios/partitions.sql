-- Bounds moved, partitions added and dropped (one overlapping the new
-- one), sub-partitioning added, a partition moved to another parent.
CREATE TABLE logs (at date NOT NULL, region text NOT NULL, line text) PARTITION BY RANGE (at);
CREATE TABLE logs_2025 PARTITION OF logs FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
CREATE TABLE logs_2026h1 PARTITION OF logs FOR VALUES FROM ('2026-01-01') TO ('2026-07-01');
CREATE TABLE logs_2026h2 PARTITION OF logs FOR VALUES FROM ('2026-07-01') TO ('2027-01-01');
CREATE TABLE logs_old PARTITION OF logs DEFAULT;
CREATE TABLE sales (region text NOT NULL, total numeric) PARTITION BY LIST (region);
CREATE TABLE sales_eu PARTITION OF sales FOR VALUES IN ('fr', 'de');
CREATE TABLE sales_us PARTITION OF sales FOR VALUES IN ('us');
CREATE TABLE other_sales (region text NOT NULL, total numeric) PARTITION BY LIST (region);
-- ==== new ====
CREATE TABLE logs (at date NOT NULL, region text NOT NULL, line text) PARTITION BY RANGE (at);
CREATE TABLE logs_2025 PARTITION OF logs FOR VALUES FROM ('2025-01-01') TO ('2026-02-01');
CREATE TABLE logs_2026 PARTITION OF logs FOR VALUES FROM ('2026-02-01') TO ('2027-01-01') PARTITION BY LIST (region);
CREATE TABLE logs_2026_eu PARTITION OF logs_2026 FOR VALUES IN ('eu');
CREATE TABLE logs_2026_rest PARTITION OF logs_2026 DEFAULT;
CREATE TABLE logs_old PARTITION OF logs DEFAULT;
CREATE TABLE sales (region text NOT NULL, total numeric) PARTITION BY LIST (region);
CREATE TABLE sales_eu PARTITION OF sales FOR VALUES IN ('fr', 'de', 'it');
CREATE TABLE other_sales (region text NOT NULL, total numeric) PARTITION BY LIST (region);
CREATE TABLE sales_us PARTITION OF other_sales FOR VALUES IN ('us');
