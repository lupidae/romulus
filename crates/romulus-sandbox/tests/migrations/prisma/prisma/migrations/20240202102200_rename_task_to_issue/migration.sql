-- Edited: Prisma would drop and recreate; rename instead, keeping the data.
ALTER TABLE "User" RENAME COLUMN "fullName" TO "displayName";

-- RenameTable
ALTER TABLE "Task" RENAME TO "Issue";

-- RenameIndex
ALTER INDEX "Task_projectId_idx" RENAME TO "Issue_projectId_idx";

-- RenamePrimaryKey
ALTER TABLE "Issue" RENAME CONSTRAINT "Task_pkey" TO "Issue_pkey";

-- RenameForeignKey
ALTER TABLE "Issue" RENAME CONSTRAINT "Task_projectId_fkey" TO "Issue_projectId_fkey";

-- RenameForeignKey
ALTER TABLE "Issue" RENAME CONSTRAINT "Task_assigneeId_fkey" TO "Issue_assigneeId_fkey";
