// SPDX-License-Identifier: MIT
//
// Controlled per-environment tab strip. Stateless — operators own
// the selected-env state in their parent component (matches
// React's controlled-component pattern). Pair with
// `applicationsForEnvironment` from `helpers.ts` to filter the
// table per tab.

export interface EnvironmentTabsProps {
  environments: string[];
  /** Currently-selected env, or `null` for "All". */
  selected: string | null;
  onSelect: (environment: string | null) => void;
}

export function EnvironmentTabs({
  environments,
  selected,
  onSelect,
}: EnvironmentTabsProps): JSX.Element {
  return (
    <nav className="apprafter-environment-tabs" aria-label="Environments">
      <button
        type="button"
        aria-pressed={selected === null}
        onClick={() => onSelect(null)}
        className={selected === null ? 'apprafter-tab-active' : 'apprafter-tab'}
      >
        All
      </button>
      {environments.map((env) => (
        <button
          key={env}
          type="button"
          aria-pressed={selected === env}
          onClick={() => onSelect(env)}
          className={
            selected === env ? 'apprafter-tab-active' : 'apprafter-tab'
          }
        >
          {env}
        </button>
      ))}
    </nav>
  );
}
