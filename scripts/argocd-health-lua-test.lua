-- SPDX-License-Identifier: FSL-1.1-Apache-2.0
--
-- Fixtures for the custom resource-health Lua that `component_argocd.cue`
-- ships into `argocd-cm`. Driven by `scripts/check-argocd-health-lua.sh`,
-- which exports each script out of the CUE and hands this file the directory
-- they landed in (`HEALTH_DIR`).
--
-- ## Why these scripts get a test at all
--
-- They are the only thing standing between a held resource and a tile that
-- says nothing is wrong, and they fail QUIETLY in both directions: a Lua
-- syntax error makes Argo CD log and fall back, and a wrong branch simply
-- reports the wrong colour. Nothing else in this repository executes them.
--
-- The record is not hypothetical. `argoproj.io_Application` was removed as
-- dead in chart 0.2.30 and every sync wave in the chart silently stopped
-- ordering anything until 0.2.75 found it — 45 versions. And the
-- `apprafter.io_Application` fall-through reported three held phases as
-- "Awaiting controller reconcile" until 0.2.77.
--
-- ## The coverage contract
--
-- `COVERED` below must name every `resource.customizations.health.*` key in
-- the chart. The runner compares the two sets and fails on either
-- direction — a new health script with no fixture is a failure, and so is a
-- fixture for a script that no longer ships. That is the half of this gate
-- which catches the NEXT kind, rather than the ones already written.

local COVERED = {
    "ConfigMap",
    "apprafter.io_Application",
    "apprafter.io_MigrationPlan",
    "apprafter.io_SharedDatabase",
    "apprafter.io_SharedVolume",
    "argoproj.io_Application",
}

local dir = assert(os.getenv("HEALTH_DIR"), "HEALTH_DIR is not set")

-- Argo CD evaluates the script body with a global `obj` in scope and reads
-- back `hs`. Reproduce exactly that, so a script relying on the ambient
-- global (every one of them does — `hs = {}` is not local) behaves here as
-- it does there.
local function run(key, o)
    local f = assert(io.open(dir .. "/" .. key .. ".lua"), "no exported script for " .. key)
    local src = f:read("*a")
    f:close()
    local chunk, err = load("local obj = ...\n" .. src, key)
    assert(chunk, "Lua will not compile for " .. key .. ": " .. tostring(err))
    local ok, hs = pcall(chunk, o)
    assert(ok, "Lua raised for " .. key .. ": " .. tostring(hs))
    return hs
end

local failures = 0
local checks = 0

-- `want_fragment` is matched PLAIN (no patterns): these messages carry `->`,
-- `(` and `%` and a pattern match would quietly stop meaning what it reads.
local function check(label, got, want_status, want_fragment)
    checks = checks + 1
    local ok = got ~= nil and got.status == want_status
    if ok and want_fragment then
        ok = got.message ~= nil and string.find(got.message, want_fragment, 1, true) ~= nil
    end
    if ok then
        print(string.format("ok   %s", label))
    else
        failures = failures + 1
        print(string.format("FAIL %s\n       want %s + %q\n       got  %s + %q",
            label, want_status, want_fragment or "",
            tostring(got and got.status), tostring(got and got.message)))
    end
end

local function ready_false(reason, message)
    return {type = "Ready", status = "False", reason = reason, message = message}
end

-- ----------------------------------------------------- apprafter.io_Application
local APP = "apprafter.io_Application"

check("app: a CR with no status yet is genuinely in flight",
    run(APP, {}), "Progressing", "Awaiting controller reconcile")

check("app: phase=Ready is Healthy",
    run(APP, {status = {phase = "Ready"}}), "Healthy", "Reconcile complete")

check("app: AwaitingMigrationApproval stays Degraded and quotes the plan",
    run(APP, {status = {phase = "AwaitingMigrationApproval", conditions = {
        {type = "MigrationPending", status = "True",
         message = "plan atm-api-abc123 awaiting approval"}}}}),
    "Degraded", "atm-api-abc123")

check("app: a rollback pin still reads Suspended above every hold",
    run(APP, {status = {phase = "AwaitingResourceClaim",
        image = {tag = "latest", pinned = {resolved = "ghcr.io/x@sha256:ab"}}}}),
    "Suspended", "Pinned to")

-- The three phases that fell through to "Awaiting controller reconcile"
-- before 0.2.77. Each is the operator DELIBERATELY holding, with a
-- `Ready=False` condition whose message names the missing thing.
check("app: AwaitingResourceClaim is Progressing and names the claims",
    run(APP, {status = {phase = "AwaitingResourceClaim", conditions = {
        ready_false("ResourceClaimPending",
            "paused awaiting ResourceClaim provisioning: atm-api-pg, atm-api-redis"),
        {type = "ResourceClaimPending", status = "True", message = "awaiting"}}}}),
    "Progressing", "atm-api-pg, atm-api-redis")

check("app: EnvSecretMissing is Degraded and names the variable",
    run(APP, {status = {phase = "EnvSecretMissing", conditions = {
        ready_false("EnvSecretMissing",
            "env GITHUB_APP_ID -> Secret atm/github-app-id: not found")}}}),
    "Degraded", "GITHUB_APP_ID")

check("app: InvalidEffectiveSpec is Degraded and carries the diagnostic",
    run(APP, {status = {phase = "InvalidEffectiveSpec", conditions = {
        ready_false("InvalidEffectiveSpec",
            "expose.port is required to render a probe")}}}),
    "Degraded", "expose.port")

check("app: an unknown held reason fails CLOSED rather than reading in-flight",
    run(APP, {status = {phase = "SomethingAddedLater", conditions = {
        ready_false("SomethingAddedLater", "a state this script predates")}}}),
    "Degraded", "predates")

check("app: Ready=False with no message falls back to the reason",
    run(APP, {status = {phase = "EnvSecretMissing", conditions = {
        {type = "Ready", status = "False", reason = "EnvSecretMissing"}}}}),
    "Degraded", "EnvSecretMissing")

check("app: a Ready=True condition is never read as a hold",
    run(APP, {status = {phase = "Ready", conditions = {
        {type = "Ready", status = "True", reason = "Reconciled"}}}}),
    "Healthy", "Reconcile complete")

-- --------------------------------------------------- apprafter.io_SharedDatabase
local DB = "apprafter.io_SharedDatabase"

check("db: a CR with no status yet is provisioning",
    run(DB, {}), "Progressing", "Awaiting provisioning")

check("db: ready names the backing database",
    run(DB, {status = {ready = true, database = "shd_atm_orders"}}),
    "Healthy", "shd_atm_orders")

check("db: a ready cache names the pool instance instead",
    run(DB, {status = {ready = true, instance = "platform-redis-000", dbnum = 7}}),
    "Healthy", "platform-redis-000")

check("db: ready with neither field still reads Healthy",
    run(DB, {status = {ready = true}}), "Healthy", "Ready")

check("db: AwaitingCluster clears itself, so Progressing",
    run(DB, {status = {ready = false, conditions = {
        ready_false("AwaitingCluster", "shared Postgres cluster is not answering yet")}}}),
    "Progressing", "not answering")

check("db: AwaitingDatabase clears itself, so Progressing",
    run(DB, {status = {ready = false, conditions = {
        ready_false("AwaitingDatabase", "CNPG has not reported the Database reconciled")}}}),
    "Progressing", "CNPG")

check("db: ExtensionUnavailable needs a person, so Degraded",
    run(DB, {status = {ready = false, conditions = {
        ready_false("ExtensionUnavailable",
            "extension vector is not provided by the running operand image")}}}),
    "Degraded", "vector")

check("db: NoProvider needs a person, so Degraded",
    run(DB, {status = {ready = false, conditions = {
        ready_false("NoProvider", "no pg ServiceProvider available")}}}),
    "Degraded", "ServiceProvider")

check("db: a delete held by live consumers is Degraded, not silent",
    run(DB, {status = {ready = false, refCount = 2, conditions = {
        ready_false("InUse", "deletion is held while 2 consumer(s) are still bound")}}}),
    "Degraded", "still bound")

-- ----------------------------------------------------- apprafter.io_SharedVolume
local VOL = "apprafter.io_SharedVolume"

check("vol: a CR with no status yet is provisioning",
    run(VOL, {}), "Progressing", "Awaiting provisioning")

check("vol: ready names the PVC",
    run(VOL, {status = {ready = true, pvcRef = "shared-uploads"}}),
    "Healthy", "shared-uploads")

check("vol: NoProvider is Degraded",
    run(VOL, {status = {ready = false, conditions = {
        ready_false("NoProvider", "no shared-disk ServiceProvider available")}}}),
    "Degraded", "shared-disk")

check("vol: CapacityWarning is about the future and does not unready a volume",
    run(VOL, {status = {ready = true, pvcRef = "shared-uploads", conditions = {
        {type = "CapacityWarning", status = "True", reason = "VolumeNearlyFull",
         message = "volume 91.2% full (> 85% threshold)"}}}}),
    "Healthy", "shared-uploads")

-- --------------------------------------------------- apprafter.io_MigrationPlan
local PLAN = "apprafter.io_MigrationPlan"

check("plan: pending-approval is Suspended and quotes the approve command",
    run(PLAN, {metadata = {name = "platform-0-2-75-to-0-2-76"}, status = {phase = "pending-approval"},
        spec = {trigger = {from = "0.2.75", to = "0.2.76"},
                risks = {classification = "requires-restart"}}}),
    "Suspended", "apprafter migration approve platform-0-2-75-to-0-2-76")

check("plan: an absent phase is treated as pending, not as unknown",
    run(PLAN, {metadata = {name = "p"}, spec = {trigger = {from = "a", to = "b"}}}),
    "Suspended", "awaiting approval")

check("plan: approved is Progressing",
    run(PLAN, {metadata = {name = "p"}, status = {phase = "approved"},
        spec = {trigger = {from = "a", to = "b"}}}),
    "Progressing", "applying")

check("plan: completed is Healthy",
    run(PLAN, {metadata = {name = "p"}, status = {phase = "completed"},
        spec = {trigger = {from = "a", to = "b"}}}),
    "Healthy", "complete")

check("plan: rejected is Degraded",
    run(PLAN, {metadata = {name = "p"}, status = {phase = "rejected"},
        spec = {trigger = {from = "a", to = "b"}}}),
    "Degraded", "rejected")

check("plan: the ADR 0052 security rollup reaches the approver's message",
    run(PLAN, {metadata = {name = "p"}, status = {phase = "pending-approval"},
        spec = {trigger = {from = "a", to = "b"},
                risks = {classification = "breaking",
                         classifications = {"security-boundary", "data-migration"}},
                changes = {{type = "jetstream-foreign-subject", field = "needs.jetstream.streams",
                            classification = "security-boundary", from = "none", to = "4 publishers"}}}}),
    "Suspended", "[security-boundary]")

check("plan: a legacy plan carrying no rollup falls back to the headline",
    run(PLAN, {metadata = {name = "p"}, status = {phase = "pending-approval"},
        spec = {trigger = {from = "0.1.0", to = "0.1.1"}, risks = {classification = "safe"}}}),
    "Suspended", "0.1.0->0.1.1")

-- ------------------------------------------------------ argoproj.io_Application
--
-- The wave-ordering script. gitops-engine counts a resource with no health
-- assessment as finished the instant its apply returns, so this existing at
-- all is what makes a `syncWave` on a child Application mean anything.
local CHILD = "argoproj.io_Application"

check("child app: health is passed through so a wave can gate on it",
    run(CHILD, {status = {health = {status = "Healthy", message = "all good"}}}),
    "Healthy", "all good")

check("child app: Degraded is passed through, which is what holds later waves",
    run(CHILD, {status = {health = {status = "Degraded", message = "pod CrashLoopBackOff"}}}),
    "Degraded", "CrashLoopBackOff")

check("child app: no health yet is Progressing, NOT an instant pass",
    run(CHILD, {status = {}}), "Progressing")

check("child app: no status at all is Progressing",
    run(CHILD, {}), "Progressing")

-- ---------------------------------------------------------------- ConfigMap
--
-- ADR 0048's approval anchor. Every OTHER ConfigMap in the cluster is
-- evaluated by this script too, so the unannotated case is the one that
-- matters most.
local CM = "ConfigMap"

check("cm: an ordinary ConfigMap is Healthy",
    run(CM, {metadata = {name = "kube-root-ca.crt"}}), "Healthy")

check("cm: a ConfigMap with no metadata at all does not raise",
    run(CM, {}), "Healthy")

check("cm: the upgrade anchor is Suspended and quotes the approve command",
    run(CM, {metadata = {name = "platform-migration-anchor", annotations = {
        ["apprafter.io/upgrade-pending"] = "true",
        ["apprafter.io/upgrade-from"] = "0.2.75",
        ["apprafter.io/upgrade-to"] = "0.2.76",
        ["apprafter.io/upgrade-class"] = "safe",
        ["apprafter.io/upgrade-plan"] = "platform-0-2-76"}}}),
    "Suspended", "apprafter migration approve platform-0-2-76")

check("cm: a half-written anchor still renders rather than raising",
    run(CM, {metadata = {annotations = {["apprafter.io/upgrade-pending"] = "true"}}}),
    "Suspended", "?->?")

-- ------------------------------------------------------------------- report
print("")
print(string.format("%d assertion(s) over %d health script(s)", checks, #COVERED))
if failures > 0 then
    print(string.format("FAILED: %d assertion(s)", failures))
    os.exit(1)
end

-- Hand the covered set back to the runner, which holds the other half of the
-- contract: that it matches the keys the chart actually ships.
local out = assert(io.open(dir .. "/covered.txt", "w"))
for _, k in ipairs(COVERED) do out:write(k, "\n") end
out:close()
print("all health-script assertions passed")
