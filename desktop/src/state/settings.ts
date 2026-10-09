// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The settings Rust keeps (settings.json, which the lock machine also reads): read once,
// saved optimistically. A save shows at once; Rust answers with the settings in use, and a
// refusal (e.g. switching the lock on with nothing to verify the owner) puts the old ones back
// and says why in a toast.
import {
  type UseMutationResult,
  type UseQueryResult,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { WarningCircleIcon } from '../components/icons';
import { useToast } from '../components/Toast';
import { settingsGet, settingsSet, uiErrorOf } from '../ipc/api';
import type { Settings } from '../ipc/generated/Settings';

export const SETTINGS_KEY = ['settings'] as const;

export function useSettings(): UseQueryResult<Settings> {
  return useQuery({ queryKey: SETTINGS_KEY, queryFn: settingsGet, staleTime: Infinity });
}

/** Saves a change to the settings as they are now (not as the caller last rendered them). */
export function useSaveSettings(): (change: Partial<Settings>) => void {
  const client = useQueryClient();
  const toast = useToast();
  const mutation: UseMutationResult<Settings, Error, Settings, { previous?: Settings }> =
    useMutation({
      mutationFn: settingsSet,
      onMutate: async (next) => {
        await client.cancelQueries({ queryKey: SETTINGS_KEY });
        const previous = client.getQueryData<Settings>(SETTINGS_KEY);
        client.setQueryData(SETTINGS_KEY, next);
        return previous === undefined ? {} : { previous };
      },
      onError: (error, _next, context) => {
        if (context?.previous !== undefined) client.setQueryData(SETTINGS_KEY, context.previous);
        toast({ message: uiErrorOf(error).message, icon: WarningCircleIcon });
      },
      onSuccess: (inUse) => {
        client.setQueryData(SETTINGS_KEY, inUse);
      },
    });
  return (change) => {
    const current = client.getQueryData<Settings>(SETTINGS_KEY);
    if (current !== undefined) mutation.mutate({ ...current, ...change });
  };
}
