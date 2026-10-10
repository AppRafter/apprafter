// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The settings Rust keeps (settings.json, which the lock machine also reads): read once, saved
// optimistically, one save at a time.
//
// settings_set takes the whole object, so two saves in flight at once could each carry the
// other's old value, and whichever Rust answered last would win. Saves therefore run in one
// mutation scope, one after another, and each request is built when it goes out: the settings
// Rust last CONFIRMED (settings_get, or a save's answer) plus that save's one change — never
// the optimistic view, which may hold changes Rust has not taken. Meanwhile the view shows the
// confirmed settings with every save not yet answered laid over them, in order, so the dialog
// shows the owner's last choices at once. A refusal (e.g. switching the lock on with nothing to
// verify the owner) says why in a toast, drops its change from the view at once — the confirmed
// settings with the saves still waiting, so a read that fails cannot keep it — and reads the
// settings again: what Rust holds then, with the saves still waiting laid over it, is what
// shows. No snapshot from before the save is put back.
import {
  type QueryClient,
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

/** One save: its change, by identity, so two equal changes stay two saves. */
interface Save {
  readonly change: Partial<Settings>;
}

/** What Rust confirmed, and the saves it has not answered yet, oldest first; per client. */
interface Ledger {
  confirmed: Settings | undefined;
  waiting: Save[];
}

const ledgers = new WeakMap<QueryClient, Ledger>();

function ledgerOf(client: QueryClient): Ledger {
  let ledger = ledgers.get(client);
  if (ledger === undefined) {
    ledger = { confirmed: undefined, waiting: [] };
    ledgers.set(client, ledger);
  }
  return ledger;
}

/** The confirmed settings with every waiting save laid over them. */
function shown(confirmed: Settings, waiting: readonly Save[]): Settings {
  return Object.assign({}, confirmed, ...waiting.map((save) => save.change));
}

export function useSettings(): UseQueryResult<Settings> {
  const client = useQueryClient();
  return useQuery({
    queryKey: SETTINGS_KEY,
    queryFn: async () => {
      const ledger = ledgerOf(client);
      ledger.confirmed = await settingsGet();
      return shown(ledger.confirmed, ledger.waiting);
    },
    staleTime: Infinity,
  });
}

/** Saves one change; see the module docs for the order and what a refusal does. */
export function useSaveSettings(): (change: Partial<Settings>) => void {
  const client = useQueryClient();
  const toast = useToast();
  const mutation = useMutation<Settings, Error, Save>({
    scope: { id: 'settings' },
    mutationFn: (save) => {
      const ledger = ledgerOf(client);
      const base = ledger.confirmed ?? client.getQueryData<Settings>(SETTINGS_KEY);
      if (base === undefined) return Promise.reject(new Error('the settings are not read yet'));
      return settingsSet({ ...base, ...save.change });
    },
    // No cancelQueries: a read in flight lays the waiting saves over its answer too, and the
    // read a refusal starts must not be cancelled by the next change.
    onMutate: (save) => {
      const ledger = ledgerOf(client);
      ledger.waiting.push(save);
      if (ledger.confirmed !== undefined) {
        client.setQueryData(SETTINGS_KEY, shown(ledger.confirmed, ledger.waiting));
      }
    },
    onSuccess: (inUse, save) => {
      const ledger = ledgerOf(client);
      ledger.waiting = ledger.waiting.filter((waiting) => waiting !== save);
      ledger.confirmed = inUse;
      client.setQueryData(SETTINGS_KEY, shown(inUse, ledger.waiting));
    },
    onError: async (error, save) => {
      const ledger = ledgerOf(client);
      ledger.waiting = ledger.waiting.filter((waiting) => waiting !== save);
      // The refused change leaves the view now, not when the read answers: a read that fails
      // keeps the entry's data, and the refused theme would stay applied.
      if (ledger.confirmed !== undefined) {
        client.setQueryData(SETTINGS_KEY, shown(ledger.confirmed, ledger.waiting));
      }
      toast({ message: uiErrorOf(error).message, icon: WarningCircleIcon });
      // The next save waits for this: it goes out from what Rust holds now.
      await client.refetchQueries({ queryKey: SETTINGS_KEY });
    },
  });
  return (change) => mutation.mutate({ change });
}
