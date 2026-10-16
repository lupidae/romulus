-- Edited: partitioned by month (Prisma models the parent table only).
CREATE TABLE "Event" (
    "id" BIGSERIAL NOT NULL,
    "organizationId" TEXT NOT NULL,
    "kind" TEXT NOT NULL,
    "payload" JSONB NOT NULL DEFAULT '{}',
    "occurredAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "Event_pkey" PRIMARY KEY ("id", "occurredAt")
) PARTITION BY RANGE ("occurredAt");

CREATE TABLE "Event_2024_03" PARTITION OF "Event" FOR VALUES FROM ('2024-03-01') TO ('2024-04-01');
CREATE TABLE "Event_2024_04" PARTITION OF "Event" FOR VALUES FROM ('2024-04-01') TO ('2024-05-01');

-- CreateIndex
CREATE INDEX "Event_organizationId_occurredAt_idx" ON "Event"("organizationId", "occurredAt" DESC);
