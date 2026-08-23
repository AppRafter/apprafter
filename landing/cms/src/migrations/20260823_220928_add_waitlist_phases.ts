// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import { sql } from '@payloadcms/db-postgres'
import type { MigrateDownArgs, MigrateUpArgs } from '@payloadcms/db-postgres'

export async function up({ db, payload, req }: MigrateUpArgs): Promise<void> {
  await db.execute(sql`
   CREATE TYPE "public"."enum_waitlist_signups_phases" AS ENUM('tier2', 'managed', 'tier3', 'tier4', 'federation');
  CREATE TABLE "waitlist_signups_phases" (
  	"order" integer NOT NULL,
  	"parent_id" integer NOT NULL,
  	"value" "enum_waitlist_signups_phases",
  	"id" serial PRIMARY KEY NOT NULL
  );

  ALTER TABLE "waitlist_signups_phases" ADD CONSTRAINT "waitlist_signups_phases_parent_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."waitlist_signups"("id") ON DELETE cascade ON UPDATE no action;
  CREATE INDEX "waitlist_signups_phases_order_idx" ON "waitlist_signups_phases" USING btree ("order");
  CREATE INDEX "waitlist_signups_phases_parent_idx" ON "waitlist_signups_phases" USING btree ("parent_id");`)
}

export async function down({ db, payload, req }: MigrateDownArgs): Promise<void> {
  await db.execute(sql`
   ALTER TABLE "waitlist_signups_phases" DISABLE ROW LEVEL SECURITY;
  DROP TABLE "waitlist_signups_phases" CASCADE;
  DROP TYPE "public"."enum_waitlist_signups_phases";`)
}
