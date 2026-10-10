// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Target › Machine › Change (spec §7, target machine): the machine picker on the target's own
// catalogue (read with its stored token), opened on the machine the target is set to. "Apply
// machine" is the Bounded plan's plain confirm (spec §4.4); a Destructive plan would open D.3d's
// PlanConfirm first. A provisioned target never gets here (D.3d's Machine row offers the rebuild
// recipe instead); if one was provisioned meanwhile, the core's refusal is shown as it is.
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { HardDrivesIcon, SpinnerGapIcon } from '../../components/icons';
import { PlanConfirm } from '../../components/PlanConfirm';
import { StatePanel } from '../../components/StatePanel';
import { useToast } from '../../components/Toast';
import { Wizard } from '../../components/Wizard';
import * as api from '../../ipc/api';
import type { MachineCatalogue } from '../../ipc/generated/MachineCatalogue';
import type { MachineSet } from '../../ipc/generated/MachineSet';
import type { PlanView } from '../../ipc/generated/PlanView';
import type { RegionLatency } from '../../ipc/generated/RegionLatency';
import type { UiError } from '../../ipc/generated/UiError';
import { failureOf, runPlan } from '../../ipc/plans';
import { usePlatform } from '../../state/platform';
import { useRead } from '../../state/read';
import { TARGETS_KEY, targetKey } from '../../state/targets';
import { choosable, defaultRegion, defaultSku, latencyView, offerIn } from './catalogue';
import { MachinePicker } from './MachinePicker';

/** The machine the target is set to now (its TargetReport's region and server type). */
export interface MachineNow {
  readonly region: string | null;
  readonly serverType: string | null;
}

interface Choice {
  readonly region: string | null;
  readonly sku: string | null;
}

/** Where the picker opens: the target's own region and type where the catalogue offers them. */
function startingChoice(cat: MachineCatalogue, now: MachineNow): Choice {
  const offered = new Set(cat.offers.map((o) => o.location));
  const region = now.region !== null && offered.has(now.region) ? now.region : defaultRegion(cat);
  if (region === null) return { region: null, sku: null };
  const sku = choosable(offerIn(cat, region, now.serverType))
    ? now.serverType
    : defaultSku(cat, region);
  return { region, sku };
}

export interface ChangeMachineDialogProps {
  readonly target: string;
  readonly now: MachineNow;
  readonly onClose: () => void;
}

export function ChangeMachineDialog({ target, now, onClose }: ChangeMachineDialogProps) {
  const info = usePlatform();
  const client = useQueryClient();
  const toast = useToast();
  const catalogueRead = useRead<MachineCatalogue>();
  const latencyRead = useRead<RegionLatency[]>();
  const [choice, setChoice] = useState<Choice | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<UiError | null>(null);
  const [confirm, setConfirm] = useState<PlanView | null>(null);
  // Whether the dialog is still there to show the end (a lock or a closed tab takes it).
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // The target's catalogue, read when the dialog opens and again after Try again (reset → idle).
  const idle = catalogueRead.state.status === 'idle';
  const runCatalogue = catalogueRead.run;
  const runLatencies = latencyRead.run;
  const nowRegion = now.region;
  const nowType = now.serverType;
  useEffect(() => {
    if (!idle) return;
    void runCatalogue(() => api.opStartMachineCatalogue({ kind: 'target', name: target })).then(
      (found) => {
        if (found !== null) {
          setChoice(startingChoice(found, { region: nowRegion, serverType: nowType }));
        }
      },
    );
  }, [idle, runCatalogue, target, nowRegion, nowType]);

  const cat = catalogueRead.state.status === 'done' ? catalogueRead.state.data : null;

  // The latencies of the catalogue's regions, once it is in, and again after Try again.
  const latencyIdle = latencyRead.state.status === 'idle';
  useEffect(() => {
    if (cat === null || !latencyIdle) return;
    const regions = cat.regions.map((r) => r.code);
    void runLatencies(() => api.opStartRegionLatencies(regions));
  }, [cat, latencyIdle, runLatencies]);
  const region = choice?.region ?? null;
  const sku = choice?.sku ?? null;
  const chosen = cat !== null && region !== null && choosable(offerIn(cat, region, sku));
  const unchanged = sku === now.serverType && region === now.region;

  const done = (set: MachineSet) => {
    // D.3d's keys, never literals: the Targets list and this target's report read again.
    void client.invalidateQueries({ queryKey: TARGETS_KEY });
    void client.invalidateQueries({ queryKey: targetKey(target) });
    toast({
      message: `Machine for “${set.name}”: ${set.sku}${set.region === null ? '' : ` in ${set.region}`}.`,
      icon: HardDrivesIcon,
    });
    onClose();
  };

  const apply = async () => {
    if (!chosen || sku === null || region === null) return;
    setSaving(true);
    setError(null);
    try {
      const plan = await api.opPlanTargetMachine(target, sku, region);
      if (plan.class === 'destructive') {
        setSaving(false);
        setConfirm(plan); // its dialog first
        return;
      }
      // Bounded: the Apply click was its plain confirm.
      const set = await runPlan<MachineSet>(plan.opId, undefined, {
        title: plan.title,
        shown: () => alive.current,
      });
      if (alive.current) done(set);
    } catch (reason) {
      if (!alive.current) return;
      setSaving(false);
      setError(failureOf(reason)); // a refused plan (IpcError) or a failed run (OperationFailed)
    }
  };

  const { latencies, latencyProblem } = latencyView(latencyRead.state, latencyRead.reset);
  const readState = catalogueRead.state;
  const body =
    cat !== null ? (
      <MachinePicker
        catalogue={cat}
        latencies={latencies}
        latencyProblem={latencyProblem}
        region={region ?? ''}
        sku={sku}
        onRegion={(value) =>
          setChoice({
            region: value,
            sku: choosable(offerIn(cat, value, sku)) ? sku : defaultSku(cat, value),
          })
        }
        onSku={(value) => setChoice({ region, sku: value })}
      />
    ) : readState.status === 'failed' ? (
      <>
        <ErrorPanel error={readState.error} />
        <Button onClick={catalogueRead.reset}>Try again</Button>
      </>
    ) : readState.status === 'cancelled' ? (
      <StatePanel
        title="Reading the catalogue was cancelled."
        actions={<Button onClick={catalogueRead.reset}>Try again</Button>}
      />
    ) : (
      <StatePanel icon={SpinnerGapIcon} spin title="Reading the provider's catalogue…" />
    );

  return (
    <>
      <Wizard
        title={`Change machine · ${target}`}
        hint="Prices from the provider, excl. VAT"
        nextLabel={saving ? 'Applying…' : 'Apply machine'}
        nextDisabled={!chosen || unchanged}
        busy={saving}
        onNext={() => {
          void apply();
        }}
        onClose={onClose}
      >
        {body}
        {error !== null && <ErrorPanel error={error} />}
      </Wizard>
      {/* Beside the frame, not in it: the frame is a form, and the confirm's own form would sit
          inside it, its submit bubbling up to Apply (GOTCHA-144). */}
      {confirm !== null && (
        <PlanConfirm
          view={confirm}
          title={confirm.title}
          confirmLabel="Apply machine"
          auth={info.auth}
          onDone={(result) => done(result as MachineSet)}
          onFailed={setError}
          onClose={() => setConfirm(null)}
        />
      )}
    </>
  );
}
