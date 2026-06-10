# --- оператор ДОЛЖЕН быть v0.2.11 (иначе тестируешь старый баг) ---
kubectl -n apprafter-system get deploy apprafter-operator -o
jsonpath='{.spec.template.spec.containers[0].image}{"\n"}'

PRIMARY=$(kubectl -n cnpg-system get pod -l cnpg.io/cluster=platform-postgres,role=primary -o
jsonpath='{.items[0].metadata.name}')
wait_ready(){ until [ "$(kubectl -n demo get resourceclaim.apprafter.io gc-probe-pg -o
jsonpath='{.status.ready}' 2>/dev/null)" = true ]; do sleep 3; done; }

# --- cleanup debris + свежий gc-probe + sentinel ---
kubectl delete application.apprafter.io gc-probe -n demo --ignore-not-found
kubectl -n apprafter-system delete retainedclaim claim-demo-gc-probe-pg --ignore-not-found
kubectl -n cnpg-system delete database.postgresql.cnpg.io claim-demo-gc-probe-pg --ignore-not-found
kubectl exec "$PRIMARY" -n cnpg-system -- psql -U postgres -c 'DROP DATABASE IF EXISTS
claim_demo_gc_probe_pg; DROP ROLE IF EXISTS claim_demo_gc_probe_pg;'
kubectl apply -f test.yaml; wait_ready
kubectl exec "$PRIMARY" -n cnpg-system -- psql -U postgres -d claim_demo_gc_probe_pg -c 'CREATE TABLE
probe(v int); INSERT INTO probe VALUES (42);'

# === TEST 1 — recovery: re-claim гасит RetainedClaim (нет time-bomb) ===
kubectl delete application.apprafter.io gc-probe -n demo; sleep 8
kubectl apply -f test.yaml; wait_ready
kubectl -n apprafter-system get retainedclaim claim-demo-gc-probe-pg
# ОЖИДАЮ: NotFound
kubectl exec "$PRIMARY" -n cnpg-system -- psql -U postgres -d claim_demo_gc_probe_pg -tAc 'SELECT v
FROM probe'    # ОЖИДАЮ: 42

# === TEST 2 — full-delete + force-GC: роль реально дропнута, запись спрунена (нет leak) ===
kubectl delete application.apprafter.io gc-probe -n demo; sleep 8
kubectl -n apprafter-system get retainedclaim claim-demo-gc-probe-pg -o json \
| jq '{apiVersion,kind,metadata:{name:.metadata.name,namespace:.metadata.namespace},spec:(.spec+{reta
inUntil:"2000-01-01T00:00:00Z"})}' > /tmp/rc.json
kubectl -n apprafter-system delete retainedclaim claim-demo-gc-probe-pg; kubectl apply -f /tmp/rc.json
# фазовый drain (DB→role ensure:absent → CNPG дропает → prune) — ждём финализации, до 3 мин
for i in $(seq 1 12); do kubectl -n apprafter-system get retainedclaim claim-demo-gc-probe-pg
>/dev/null 2>&1 || break; sleep 15; done
kubectl -n apprafter-system get retainedclaim claim-demo-gc-probe-pg
# ОЖИДАЮ: NotFound
kubectl exec "$PRIMARY" -n cnpg-system -- psql -U postgres -tAc "SELECT 1 FROM pg_roles WHERE
rolname='claim_demo_gc_probe_pg'"   # ОЖИДАЮ: (пусто)
kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres \
-o jsonpath='{range .spec.managed.roles[?(@.name=="claim_demo_gc_probe_pg")]}{.name}{end}{"\n"}'
# ОЖИДАЮ: (пусто)

Четыре ключевых проверки: TEST 1 → NotFound + 42; TEST 2 → NotFound + два (пусто). Если все четыре
сошлись — оба бага закрыты эмпирически, и можно к закрытию 2.4g. Скинь вывод — гляну.
