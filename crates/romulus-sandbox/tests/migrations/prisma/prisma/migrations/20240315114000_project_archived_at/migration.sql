/*
  Warnings:

  - You are about to drop the column `archived` on the `Project` table. All the data in the column will be lost.

*/
-- AlterTable
ALTER TABLE "Project" ADD COLUMN     "archivedAt" TIMESTAMP(3);

-- Edited: keep when projects were archived.
UPDATE "Project" SET "archivedAt" = CURRENT_TIMESTAMP WHERE "archived";

-- AlterTable
ALTER TABLE "Project" DROP COLUMN "archived";

-- AlterTable
ALTER TABLE "User" ALTER COLUMN "displayName" SET DATA TYPE VARCHAR(200);
