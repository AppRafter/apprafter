# providers/pg-integrated/

PostgreSQL ServiceProvider backed by CloudNativePG. A single shared CNPG cluster serves multiple tenants; each `ResourceClaim` gets its own database and role. Default backend for Tier 1–3.
