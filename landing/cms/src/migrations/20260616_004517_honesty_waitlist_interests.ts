// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import { sql } from '@payloadcms/db-postgres'
import type { MigrateDownArgs, MigrateUpArgs } from '@payloadcms/db-postgres'

export async function up({ db, payload, req }: MigrateUpArgs): Promise<void> {
  await db.execute(sql`
   CREATE TYPE "public"."enum_waitlist_signups_interests" AS ENUM('tier2', 'observability', 'managed', 'tier3', 'tier4');
  CREATE TABLE "waitlist_signups_interests" (
  	"order" integer NOT NULL,
  	"parent_id" integer NOT NULL,
  	"value" "enum_waitlist_signups_interests",
  	"id" serial PRIMARY KEY NOT NULL
  );
  
  CREATE TABLE "waitlist_form_copy_interests" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"key" varchar NOT NULL
  );
  
  CREATE TABLE "waitlist_form_copy_interests_locales" (
  	"label" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  ALTER TABLE "waitlist_form_copy_locales" ALTER COLUMN "storage_note" SET DEFAULT 'Stored only for launch announcements.';
  ALTER TABLE "waitlist_form_copy_locales" ADD COLUMN "interests_label" varchar DEFAULT 'What are you waiting for? (pick any)' NOT NULL;
  ALTER TABLE "waitlist_signups_interests" ADD CONSTRAINT "waitlist_signups_interests_parent_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."waitlist_signups"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "waitlist_form_copy_interests" ADD CONSTRAINT "waitlist_form_copy_interests_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."waitlist_form_copy"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "waitlist_form_copy_interests_locales" ADD CONSTRAINT "waitlist_form_copy_interests_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."waitlist_form_copy_interests"("id") ON DELETE cascade ON UPDATE no action;
  CREATE INDEX "waitlist_signups_interests_order_idx" ON "waitlist_signups_interests" USING btree ("order");
  CREATE INDEX "waitlist_signups_interests_parent_idx" ON "waitlist_signups_interests" USING btree ("parent_id");
  CREATE INDEX "waitlist_form_copy_interests_order_idx" ON "waitlist_form_copy_interests" USING btree ("_order");
  CREATE INDEX "waitlist_form_copy_interests_parent_id_idx" ON "waitlist_form_copy_interests" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "waitlist_form_copy_interests_locales_locale_parent_id_unique" ON "waitlist_form_copy_interests_locales" USING btree ("_locale","_parent_id");`)
}

export async function down({ db, payload, req }: MigrateDownArgs): Promise<void> {
  await db.execute(sql`
   ALTER TABLE "waitlist_signups_interests" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "waitlist_form_copy_interests" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "waitlist_form_copy_interests_locales" DISABLE ROW LEVEL SECURITY;
  DROP TABLE "waitlist_signups_interests" CASCADE;
  DROP TABLE "waitlist_form_copy_interests" CASCADE;
  DROP TABLE "waitlist_form_copy_interests_locales" CASCADE;
  ALTER TABLE "waitlist_form_copy_locales" ALTER COLUMN "storage_note" SET DEFAULT 'Stored only for launch announcement.';
  ALTER TABLE "waitlist_form_copy_locales" DROP COLUMN "interests_label";
  DROP TYPE "public"."enum_waitlist_signups_interests";`)
}
