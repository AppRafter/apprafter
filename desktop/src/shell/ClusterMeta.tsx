// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The sidebar's line under a tab's target name: provider · region · tier, from the target store's
// list — the Targets view's query, with no interval of its own (a mutation refreshes it). Nothing
// for a name the list does not hold; a rename refreshes the list, so the new name finds its line.
import { useQuery } from '@tanstack/react-query';
import * as api from '../ipc/api';
import { metaLine } from '../screens/targets/labels';
import { TARGETS_KEY } from '../state/targets';

export function ClusterMeta({ target }: { target: string }) {
  const list = useQuery({ queryKey: TARGETS_KEY, queryFn: api.targetList });
  const summary = list.data?.targets.find((t) => t.name === target);
  if (summary === undefined) return null;
  return <div className="cluster-meta">{metaLine(summary)}</div>;
}
