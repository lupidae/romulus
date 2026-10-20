import { useMutation, useQueryClient } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";

import {
  type MigrationTest,
  type StartOptions,
  type TestSource,
  createBody,
  testKeys,
} from "./queries";

/** Starts a test of `source`: answers at once with it, queued. */
export function useStartTest(org: string, db: string, source: TestSource) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (options: StartOptions = {}) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/migration-tests", {
          params: { path: { org, db } },
          body: createBody(source, options),
        }),
      ),
    onSuccess: (test) => {
      queryClient.setQueryData<MigrationTest>(testKeys.one(org, db, test.id), test);
      void queryClient.invalidateQueries({ queryKey: testKeys.of(org, db, source) });
    },
  });
}

/** Stops a test and deletes its playground. */
export function useCancelTest(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/migration-tests/{id}/cancel", {
          params: { path: { org, db, id } },
        }),
      ),
    onSuccess: (test) => {
      queryClient.setQueryData<MigrationTest>(testKeys.one(org, db, id), test);
      void queryClient.invalidateQueries({ queryKey: testKeys.all(org, db) });
    },
  });
}

/** Acknowledges the data loss a test found that its migration does not declare. */
export function useAcknowledgeTest(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/migration-tests/{id}/acknowledge", {
          params: { path: { org, db, id } },
        }),
      ),
    onSuccess: (test) => {
      queryClient.setQueryData<MigrationTest>(testKeys.one(org, db, id), test);
      void queryClient.invalidateQueries({ queryKey: testKeys.all(org, db) });
    },
  });
}
