// SPDX-License-Identifier: MIT
//
// Drilldown view for a single Application — base config (image,
// replicas, expose, env), per-environment overrides, status
// (phase, observedGeneration, conditions), and the endpoint URL.

import type { Application } from '../types.ts';

export interface ApplicationDetailProps {
  application: Application;
}

export function ApplicationDetail({
  application,
}: ApplicationDetailProps): JSX.Element {
  const base = application.spec.base;
  const status = application.status;
  return (
    <article className="apprafter-application-detail">
      <header>
        <h2>{application.metadata.name}</h2>
        {application.metadata.namespace && (
          <span className="apprafter-namespace">
            {application.metadata.namespace}
          </span>
        )}
      </header>

      <section className="apprafter-section">
        <h3>Base</h3>
        <dl>
          {base?.image && (
            <>
              <dt>Image</dt>
              <dd>{base.image}</dd>
            </>
          )}
          {base?.replicas !== undefined && (
            <>
              <dt>Replicas</dt>
              <dd>{base.replicas}</dd>
            </>
          )}
          {base?.expose && (
            <>
              <dt>Expose</dt>
              <dd>
                port {base.expose.port}
                {base.expose.public ? ', public' : ''}
                {base.expose.network ? `, ${base.expose.network}` : ''}
              </dd>
            </>
          )}
          {base?.env && Object.keys(base.env).length > 0 && (
            <>
              <dt>Env</dt>
              <dd>
                <ul>
                  {Object.entries(base.env).map(([k, v]) => (
                    <li key={k}>
                      <code>{k}</code>={v}
                    </li>
                  ))}
                </ul>
              </dd>
            </>
          )}
        </dl>
      </section>

      {application.spec.environments &&
        Object.keys(application.spec.environments).length > 0 && (
          <section className="apprafter-section">
            <h3>Environments</h3>
            <ul>
              {Object.entries(application.spec.environments).map(
                ([env, override]) => (
                  <li key={env}>
                    <strong>{env}</strong>:{' '}
                    {override.image && `image=${override.image} `}
                    {override.replicas !== undefined &&
                      `replicas=${override.replicas} `}
                  </li>
                ),
              )}
            </ul>
          </section>
        )}

      {status && (
        <section className="apprafter-section">
          <h3>Status</h3>
          <dl>
            {status.phase && (
              <>
                <dt>Phase</dt>
                <dd>{status.phase}</dd>
              </>
            )}
            {status.observedGeneration !== undefined && (
              <>
                <dt>Observed generation</dt>
                <dd>{status.observedGeneration}</dd>
              </>
            )}
            {status.endpointURL && (
              <>
                <dt>Endpoint</dt>
                <dd>
                  <a
                    href={status.endpointURL}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {status.endpointURL}
                  </a>
                </dd>
              </>
            )}
          </dl>
          {status.conditions && status.conditions.length > 0 && (
            <>
              <h4>Conditions</h4>
              <table className="apprafter-conditions-table">
                <thead>
                  <tr>
                    <th>Type</th>
                    <th>Status</th>
                    <th>Reason</th>
                    <th>Last transition</th>
                    <th>Message</th>
                  </tr>
                </thead>
                <tbody>
                  {status.conditions.map((c) => (
                    <tr key={`${c.type}-${c.lastTransitionTime}`}>
                      <td>{c.type}</td>
                      <td>{c.status}</td>
                      <td>{c.reason}</td>
                      <td>{c.lastTransitionTime}</td>
                      <td>{c.message}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          )}
        </section>
      )}
    </article>
  );
}
