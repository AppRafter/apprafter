// SPDX-License-Identifier: MIT
//
// Plain-HTML React table that renders ApplicationRow[]. No
// Backstage / Material-UI dependency — operators wrap it in
// `<InfoCard>` or whatever their host provides.

import type { ApplicationRow } from '../transforms.ts';

export interface ApplicationsTableProps {
  rows: ApplicationRow[];
  /** Optional click handler — operators wire this to drilldown navigation. */
  onSelect?: (row: ApplicationRow) => void;
}

export function ApplicationsTable({
  rows,
  onSelect,
}: ApplicationsTableProps): JSX.Element {
  if (rows.length === 0) {
    return (
      <div className="apprafter-applications-empty">
        No Applications declared.
      </div>
    );
  }
  return (
    <table className="apprafter-applications-table">
      <thead>
        <tr>
          <th>Name</th>
          <th>Namespace</th>
          <th>Image</th>
          <th>Replicas</th>
          <th>Phase</th>
          <th>Ready</th>
          <th>Endpoint</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr
            key={`${row.namespace}/${row.name}`}
            onClick={onSelect ? () => onSelect(row) : undefined}
            style={{ cursor: onSelect ? 'pointer' : undefined }}
          >
            <td>{row.name}</td>
            <td>{row.namespace}</td>
            <td>{row.image}</td>
            <td>{row.replicas}</td>
            <td>{row.phase}</td>
            <td>{row.ready}</td>
            <td>
              {row.endpointURL ? (
                <a href={row.endpointURL} target="_blank" rel="noreferrer">
                  {row.endpointURL}
                </a>
              ) : (
                ''
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
