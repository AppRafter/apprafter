// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import { type MigrateDownArgs, type MigrateUpArgs, sql } from '@payloadcms/db-postgres'

export async function up({ db, payload, req }: MigrateUpArgs): Promise<void> {
  await db.execute(sql`
   ALTER TABLE "site_settings_locales" ALTER COLUMN "home_seo_title" SET DEFAULT 'AppRafter — One manifest. From a €5 VPS to production. Source Available.';
  ALTER TABLE "site_settings_locales" ALTER COLUMN "home_seo_description" SET DEFAULT 'AppRafter is an opinionated PaaS on Kubernetes. Describe your applications in a single CUE manifest — the same one runs from a single VDS to a multi-node production cluster. Source available (FSL-1.1-Apache-2.0).';
  ALTER TABLE "landing_hero" ALTER COLUMN "primary_c_t_a_href" SET DEFAULT 'https://docs.apprafter.dev/operator-guide/quickstart/';
  ALTER TABLE "users" ADD COLUMN "reset_password_requested_at" timestamp(3) with time zone;`)
}

export async function down({ db, payload, req }: MigrateDownArgs): Promise<void> {
  await db.execute(sql`
   ALTER TABLE "site_settings_locales" ALTER COLUMN "home_seo_title" SET DEFAULT 'AppRafter — One manifest. From a €5 VPS to production. Open source.';
  ALTER TABLE "site_settings_locales" ALTER COLUMN "home_seo_description" SET DEFAULT 'AppRafter is an opinionated PaaS on Kubernetes. Describe your applications in a single CUE manifest — the same one runs from a single VDS to a multi-node production cluster. Open source (FSL-1.1-Apache-2.0).';
  ALTER TABLE "landing_hero" ALTER COLUMN "primary_c_t_a_href" DROP DEFAULT;
  ALTER TABLE "users" DROP COLUMN "reset_password_requested_at";`)
}
