/*
  Warnings:

  - A unique constraint covering the columns `[projectId,number]` on the table `Issue` will be added. If there are existing duplicate values, this will fail.
  - Added the required column `number` to the `Issue` table without a default value. This is not possible if the table is not empty.

*/
-- AlterTable
ALTER TABLE "Issue" ADD COLUMN     "number" INTEGER;

-- Edited: number the existing issues before making the column required.
UPDATE "Issue" AS i
SET "number" = numbered.n
FROM (SELECT "id", row_number() OVER (PARTITION BY "projectId" ORDER BY "id") AS n FROM "Issue") AS numbered
WHERE numbered."id" = i."id";

ALTER TABLE "Issue" ALTER COLUMN "number" SET NOT NULL;

-- CreateIndex
CREATE UNIQUE INDEX "Issue_projectId_number_key" ON "Issue"("projectId", "number");
