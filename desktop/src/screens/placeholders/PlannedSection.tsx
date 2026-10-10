// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A section whose screen a later slice brings: the design's page header, then which slice and
// the CLI command that does it meanwhile — nothing pretends to show data it does not have.
// The command must reach the tab's target, and every planned section's command runs on the
// CLI's active target (sections.test.ts), so the hint says to switch to this one first.
import { StatePanel } from '../../components/StatePanel';
import { sectionInfo } from '../../shell/sections';
import type { Section } from '../../state/session';

export function PlannedSection({ section, target }: { section: Section; target: string }) {
  const info = sectionInfo(section);
  return (
    <div className="page">
      <header className="page-header">
        <h1 className="page-title">{info.label}</h1>
        {info.sub !== undefined && <p className="page-sub">{info.sub(target)}</p>}
      </header>
      <StatePanel
        icon={info.icon}
        title={info.planned ?? ''}
        text={
          <>
            Until then the CLI does it, on its active target: run{' '}
            <code>{`apprafter target use ${target}`}</code> first.
          </>
        }
        meta={`apprafter ${info.leaf}`}
      />
    </div>
  );
}
