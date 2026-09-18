---
description: "Telling the platform when your application is ready for traffic and when it has stopped working — the one line that is usually enough, the check you already have without asking, the defaults behind each field, and the mistake that turns a slow start into a restart loop."
---

# Health checks

Two different questions, and the platform can only answer them if you say
how:

- **Should this instance receive traffic?** A new instance is not ready the
  moment its process starts — it may still be opening a database connection
  or warming a cache. Sending requests to it produces errors that look like
  your application is broken.
- **Has this instance stopped working without exiting?** A process that has
  deadlocked is still a running process. Nothing restarts it, because from
  the outside it looks alive.

You answer both with a `probes` block.

## The short version

```cue
spec: base: {
    image: "ghcr.io/example/orders:v1"
    expose: port: 8080

    probes: readiness: path: "/healthz"
}
```

That is a complete, working health check. The port comes from `expose.port`,
and every timing has a default. Add a second line when you also want a
deadlocked process restarted:

```cue
    probes: {
        readiness: path: "/healthz"
        liveness:  path: "/livez"
    }
```

## You already have a readiness check

An application with an `expose.port` and **no** `probes` block still gets
one: the platform waits for something to accept a connection on that port
before sending it traffic. It is not looking at your application's health —
only that the process is listening — but it closes the window in which a
rolling update sends requests to an instance that has not finished starting.

Nothing is guessed here. The port is the one your own manifest declares.

Two consequences worth knowing:

- If your `expose.port` is wrong, your application will now show as not
  ready. That was always broken — traffic was being sent to a port nothing
  answered on — and it is now visible instead of silent.
- You can turn it off: `probes: readiness: enabled: false`. Do that if your
  application legitimately does not listen on the exposed port at startup.

## The three probes

| | Question it answers | What happens when it fails |
| --- | --- | --- |
| `readiness` | Should this instance receive traffic? | It is taken out of the load balancer. It keeps running. |
| `liveness` | Has this instance stopped working? | **It is restarted.** |
| `startup` | Is it still starting? | While it runs, liveness is held off entirely. |

Point `liveness` at something cheap that a deadlocked process could not
answer. Pointing it at a path that checks your database is a common and
expensive mistake: the database going away then restarts every instance of
your application, which is the opposite of what you want.

## Slow starts are already handled

The mistake this catches is worth naming, because it is the usual one.
A liveness probe with a thirty-second budget on an application that takes
forty seconds to warm a cache restarts it at thirty seconds — forever, and
the logs show a healthy start every time.

You do not have to plan for it. **Declaring a liveness probe and no startup
probe gives you a startup probe**, against the same endpoint, with five
minutes to come up. Liveness only begins once the application has answered
once.

Declare `startup` yourself when five minutes is the wrong number:

```cue
    probes: {
        liveness: path: "/livez"
        startup: {
            path:             "/livez"
            periodSeconds:    10
            failureThreshold: 90    // fifteen minutes
        }
    }
```

## Every field, and what it defaults to

```cue
    probes: readiness: {
        path:   "/healthz"   // present => HTTP GET; absent => a TCP connect
        port:   8080         // default: your expose.port
        scheme: "https"      // default: http
        headers: "X-Probe": "apprafter"

        initialDelaySeconds: 0    // wait before the first check
        periodSeconds:       10   // how often
        timeoutSeconds:      2    // how long one check may take
        failureThreshold:    3    // consecutive failures before acting
        successThreshold:    1    // readiness only; must be 1 for the others
    }
```

Omit `path` to get a plain TCP connect, which is what you want for something
that does not speak HTTP:

```cue
    probes: readiness: port: 5432
```

Timings are whole seconds. Sub-second values are not expressible, because
the underlying check is not.

## Per environment

A `probes` block merges field by field, so an environment carries only its
difference:

```cue
spec: {
    base: probes: readiness: {path: "/healthz", periodSeconds: 10}
    environments: dev: probes: readiness: periodSeconds: 3
}
```

`dev` keeps `/healthz` and checks three times as often.

## Seeing what is actually running

The defaults are applied when your application is deployed, so they are not
in your manifest and not in the stored object. `apprafter app status` prints
what the instance actually got, including the two probes you may not have
written:

```text
Probes:          readiness http /healthz:8080 every 10s, liveness http /livez:8080 every 30s, startup http /livez:8080 every 5s, up to 5m (derived)
```

`(default)` marks the readiness check you get for free; `(derived)` marks the
startup check that came from your liveness probe.

`up to 5m` is how long that probe keeps failing before it acts — its period
multiplied by how many failures it tolerates. It appears only where that
allowance is not the usual three failures, so a line without it is a probe
that acts on the third miss. On the derived startup probe it is the number
worth knowing: five seconds between checks, sixty of them, five minutes for
your application to come up before the liveness probe takes over.

## What the platform refuses

Each of these is rejected when you deploy, not ignored:

- a `path` that does not start with `/`;
- `scheme` or `headers` on a probe with no `path` — those belong to an HTTP
  check, and a probe without a path is a TCP connect;
- a probe with no port and no `expose.port` to inherit;
- `successThreshold` other than 1 on `liveness` or `startup`;
- `timeoutSeconds` equal to or larger than `periodSeconds`, which would let
  one check still be running when the next starts.

---

The mechanism — what is rendered, why the defaults are what they are, and
why the startup probe is derived rather than inherited — is [Health checks
on a running application](../how-it-works/health-checks.md).
