// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import { sql } from '@payloadcms/db-postgres'
import type { MigrateDownArgs, MigrateUpArgs } from '@payloadcms/db-postgres'

export async function up({ db, payload, req }: MigrateUpArgs): Promise<void> {
  await db.execute(sql`
   CREATE TABLE "site_settings_locales" (
  	"nav_docs_label" varchar DEFAULT 'Docs' NOT NULL,
  	"nav_spec_label" varchar DEFAULT 'Spec' NOT NULL,
  	"soon_badge_label" varchar DEFAULT 'Soon' NOT NULL,
  	"home_seo_title" varchar DEFAULT 'AppRafter — One manifest. From a €5 VPS to production. Open source.' NOT NULL,
  	"home_seo_description" varchar DEFAULT 'AppRafter is an opinionated PaaS on Kubernetes. Describe your applications in a single CUE manifest — the same one runs from a single VDS to a multi-node production cluster. Open source (FSL-1.1-Apache-2.0).' NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "value_props_locales" (
  	"eyebrow" varchar DEFAULT 'Why AppRafter' NOT NULL,
  	"title" varchar DEFAULT 'Three things you don''t have to fight anymore.' NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "legal_terms" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "legal_terms_locales" (
  	"seo_title" varchar NOT NULL,
  	"seo_description" varchar NOT NULL,
  	"eyebrow" varchar DEFAULT '/ Legal' NOT NULL,
  	"heading" varchar DEFAULT 'Terms.' NOT NULL,
  	"body_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "legal_privacy" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "legal_privacy_locales" (
  	"seo_title" varchar NOT NULL,
  	"seo_description" varchar NOT NULL,
  	"eyebrow" varchar DEFAULT '/ Legal' NOT NULL,
  	"heading" varchar DEFAULT 'Privacy.' NOT NULL,
  	"body_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "not_found" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "not_found_locales" (
  	"seo_title" varchar NOT NULL,
  	"seo_description" varchar NOT NULL,
  	"eyebrow" varchar DEFAULT '/ 404' NOT NULL,
  	"heading" varchar DEFAULT 'Page not found.' NOT NULL,
  	"body_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  ALTER TABLE "site_settings" ALTER COLUMN "spec_url" SET DEFAULT 'https://github.com/AppRafter/apprafter/blob/master/spec.md';
  ALTER TABLE "site_settings" ADD COLUMN "brand_name" varchar DEFAULT 'AppRafter' NOT NULL;
  ALTER TABLE "site_settings" ADD COLUMN "brand_name_accent" varchar DEFAULT 'Rafter' NOT NULL;
  ALTER TABLE "landing_hero_locales" ADD COLUMN "license_footnote_html" varchar;
  ALTER TABLE "landing_hero_locales" ADD COLUMN "cue_caption" varchar;
  ALTER TABLE "tier_ladder_locales" ADD COLUMN "waitlist_button_label" varchar DEFAULT 'Join the waitlist' NOT NULL;
  ALTER TABLE "comparison_locales" ADD COLUMN "status_row_label" varchar DEFAULT 'Status' NOT NULL;
  ALTER TABLE "comparison_locales" ADD COLUMN "managed_waitlist_button_label" varchar DEFAULT 'Join waitlist' NOT NULL;
  ALTER TABLE "boring_tech_locales" ADD COLUMN "under_hood_label" varchar DEFAULT 'Under the hood' NOT NULL;
  ALTER TABLE "boring_tech_locales" ADD COLUMN "our_code_label" varchar DEFAULT 'What we wrote ourselves' NOT NULL;
  ALTER TABLE "waitlist_form_copy_locales" ADD COLUMN "email_validation_error" varchar DEFAULT 'Please enter a valid email address.' NOT NULL;
  ALTER TABLE "waitlist_form_copy_locales" ADD COLUMN "submit_error_message" varchar DEFAULT 'Submit failed. Try again in a minute.' NOT NULL;
  ALTER TABLE "waitlist_form_copy_locales" ADD COLUMN "network_error_message" varchar DEFAULT 'Network error — try again.' NOT NULL;
  ALTER TABLE "site_settings_locales" ADD CONSTRAINT "site_settings_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."site_settings"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "value_props_locales" ADD CONSTRAINT "value_props_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."value_props"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "legal_terms_locales" ADD CONSTRAINT "legal_terms_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."legal_terms"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "legal_privacy_locales" ADD CONSTRAINT "legal_privacy_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."legal_privacy"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "not_found_locales" ADD CONSTRAINT "not_found_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."not_found"("id") ON DELETE cascade ON UPDATE no action;
  CREATE UNIQUE INDEX "site_settings_locales_locale_parent_id_unique" ON "site_settings_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "value_props_locales_locale_parent_id_unique" ON "value_props_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "legal_terms_locales_locale_parent_id_unique" ON "legal_terms_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "legal_privacy_locales_locale_parent_id_unique" ON "legal_privacy_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "not_found_locales_locale_parent_id_unique" ON "not_found_locales" USING btree ("_locale","_parent_id");`)
}

export async function down({ db, payload, req }: MigrateDownArgs): Promise<void> {
  await db.execute(sql`
   ALTER TABLE "site_settings_locales" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "value_props_locales" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "legal_terms" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "legal_terms_locales" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "legal_privacy" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "legal_privacy_locales" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "not_found" DISABLE ROW LEVEL SECURITY;
  ALTER TABLE "not_found_locales" DISABLE ROW LEVEL SECURITY;
  DROP TABLE "site_settings_locales" CASCADE;
  DROP TABLE "value_props_locales" CASCADE;
  DROP TABLE "legal_terms" CASCADE;
  DROP TABLE "legal_terms_locales" CASCADE;
  DROP TABLE "legal_privacy" CASCADE;
  DROP TABLE "legal_privacy_locales" CASCADE;
  DROP TABLE "not_found" CASCADE;
  DROP TABLE "not_found_locales" CASCADE;
  ALTER TABLE "site_settings" ALTER COLUMN "spec_url" SET DEFAULT 'https://github.com/AppRafter/apprafter/blob/main/spec.md';
  ALTER TABLE "site_settings" DROP COLUMN "brand_name";
  ALTER TABLE "site_settings" DROP COLUMN "brand_name_accent";
  ALTER TABLE "landing_hero_locales" DROP COLUMN "license_footnote_html";
  ALTER TABLE "landing_hero_locales" DROP COLUMN "cue_caption";
  ALTER TABLE "tier_ladder_locales" DROP COLUMN "waitlist_button_label";
  ALTER TABLE "comparison_locales" DROP COLUMN "status_row_label";
  ALTER TABLE "comparison_locales" DROP COLUMN "managed_waitlist_button_label";
  ALTER TABLE "boring_tech_locales" DROP COLUMN "under_hood_label";
  ALTER TABLE "boring_tech_locales" DROP COLUMN "our_code_label";
  ALTER TABLE "waitlist_form_copy_locales" DROP COLUMN "email_validation_error";
  ALTER TABLE "waitlist_form_copy_locales" DROP COLUMN "submit_error_message";
  ALTER TABLE "waitlist_form_copy_locales" DROP COLUMN "network_error_message";`)
}
