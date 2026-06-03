// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import { sql } from '@payloadcms/db-postgres'
import type { MigrateDownArgs, MigrateUpArgs } from '@payloadcms/db-postgres'

export async function up({ db, payload, req }: MigrateUpArgs): Promise<void> {
  await db.execute(sql`
   CREATE TYPE "public"."_locales" AS ENUM('en');
  CREATE TYPE "public"."enum_value_props_blocks_icon_name" AS ENUM('grid', 'bars', 'lock');
  CREATE TYPE "public"."enum_tier_ladder_cards_status" AS ENUM('live', 'roadmap');
  CREATE TYPE "public"."enum_comparison_columns_self_status_kind" AS ENUM('live', 'waitlist', 'roadmap');
  CREATE TYPE "public"."enum_comparison_columns_managed_status_kind" AS ENUM('live', 'waitlist', 'roadmap');
  CREATE TYPE "public"."enum_comparison_columns_turnkey_status_kind" AS ENUM('live', 'waitlist', 'roadmap');
  CREATE TABLE "users_sessions" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"created_at" timestamp(3) with time zone,
  	"expires_at" timestamp(3) with time zone NOT NULL
  );
  
  CREATE TABLE "users" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"name" varchar,
  	"updated_at" timestamp(3) with time zone DEFAULT now() NOT NULL,
  	"created_at" timestamp(3) with time zone DEFAULT now() NOT NULL,
  	"email" varchar NOT NULL,
  	"reset_password_token" varchar,
  	"reset_password_expiration" timestamp(3) with time zone,
  	"salt" varchar,
  	"hash" varchar,
  	"login_attempts" numeric DEFAULT 0,
  	"lock_until" timestamp(3) with time zone
  );
  
  CREATE TABLE "waitlist_signups" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"email" varchar NOT NULL,
  	"use_case" varchar,
  	"wants_call" boolean DEFAULT false,
  	"source" varchar,
  	"call_email_sent_at" timestamp(3) with time zone,
  	"updated_at" timestamp(3) with time zone DEFAULT now() NOT NULL,
  	"created_at" timestamp(3) with time zone DEFAULT now() NOT NULL
  );
  
  CREATE TABLE "payload_kv" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"key" varchar NOT NULL,
  	"data" jsonb NOT NULL
  );
  
  CREATE TABLE "payload_locked_documents" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"global_slug" varchar,
  	"updated_at" timestamp(3) with time zone DEFAULT now() NOT NULL,
  	"created_at" timestamp(3) with time zone DEFAULT now() NOT NULL
  );
  
  CREATE TABLE "payload_locked_documents_rels" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"order" integer,
  	"parent_id" integer NOT NULL,
  	"path" varchar NOT NULL,
  	"users_id" integer,
  	"waitlist_signups_id" integer
  );
  
  CREATE TABLE "payload_preferences" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"key" varchar,
  	"value" jsonb,
  	"updated_at" timestamp(3) with time zone DEFAULT now() NOT NULL,
  	"created_at" timestamp(3) with time zone DEFAULT now() NOT NULL
  );
  
  CREATE TABLE "payload_preferences_rels" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"order" integer,
  	"parent_id" integer NOT NULL,
  	"path" varchar NOT NULL,
  	"users_id" integer
  );
  
  CREATE TABLE "payload_migrations" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"name" varchar,
  	"batch" numeric,
  	"updated_at" timestamp(3) with time zone DEFAULT now() NOT NULL,
  	"created_at" timestamp(3) with time zone DEFAULT now() NOT NULL
  );
  
  CREATE TABLE "site_settings" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"github_url" varchar DEFAULT 'https://github.com/AppRafter/apprafter' NOT NULL,
  	"spec_url" varchar DEFAULT 'https://github.com/AppRafter/apprafter/blob/main/spec.md' NOT NULL,
  	"docs_url" varchar,
  	"plausible_domain" varchar,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "landing_hero" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"cue_filename" varchar DEFAULT 'billing-api.cue' NOT NULL,
  	"cue_snippet" varchar NOT NULL,
  	"primary_c_t_a_href" varchar NOT NULL,
  	"tertiary_c_t_a_href" varchar NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "landing_hero_locales" (
  	"headline_html" varchar NOT NULL,
  	"subhead" varchar NOT NULL,
  	"status_badge" varchar NOT NULL,
  	"primary_c_t_a_label" varchar NOT NULL,
  	"secondary_c_t_a_label" varchar NOT NULL,
  	"tertiary_c_t_a_label" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "value_props_blocks" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"icon_name" "enum_value_props_blocks_icon_name" NOT NULL
  );
  
  CREATE TABLE "value_props_blocks_locales" (
  	"title" varchar NOT NULL,
  	"body_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "value_props" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "scaling_journey_footer_kickers" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL
  );
  
  CREATE TABLE "scaling_journey_footer_kickers_locales" (
  	"text" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "scaling_journey" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"left_stage_file_name" varchar DEFAULT 'Application.cue' NOT NULL,
  	"right_stage_file_name" varchar DEFAULT 'Application.cue' NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "scaling_journey_locales" (
  	"eyebrow" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"lede_html" varchar NOT NULL,
  	"left_stage_eyebrow" varchar NOT NULL,
  	"left_stage_file_meta" varchar NOT NULL,
  	"left_stage_caption" varchar NOT NULL,
  	"right_stage_eyebrow" varchar NOT NULL,
  	"right_stage_file_meta" varchar NOT NULL,
  	"right_stage_caption" varchar NOT NULL,
  	"arrow_label" varchar DEFAULT 'Tier upgrade' NOT NULL,
  	"caveat_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "tier_ladder_cards" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"status" "enum_tier_ladder_cards_status" NOT NULL
  );
  
  CREATE TABLE "tier_ladder_cards_locales" (
  	"num" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"price" varchar NOT NULL,
  	"desc" varchar NOT NULL,
  	"status_text" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "tier_ladder" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "tier_ladder_locales" (
  	"eyebrow" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"orthogonal_note_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "comparison_rows" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL
  );
  
  CREATE TABLE "comparison_rows_locales" (
  	"label" varchar NOT NULL,
  	"self_html" varchar NOT NULL,
  	"managed_html" varchar NOT NULL,
  	"turnkey_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "comparison" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"columns_self_status_kind" "enum_comparison_columns_self_status_kind" DEFAULT 'live' NOT NULL,
  	"columns_managed_status_kind" "enum_comparison_columns_managed_status_kind" DEFAULT 'waitlist' NOT NULL,
  	"columns_turnkey_status_kind" "enum_comparison_columns_turnkey_status_kind" DEFAULT 'roadmap' NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "comparison_locales" (
  	"eyebrow" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"columns_self_label" varchar DEFAULT 'Self-host' NOT NULL,
  	"columns_self_status_label" varchar DEFAULT 'Available now' NOT NULL,
  	"columns_managed_label" varchar DEFAULT 'Managed' NOT NULL,
  	"columns_managed_badge_suffix" varchar DEFAULT '(waitlist)',
  	"columns_managed_status_label" varchar DEFAULT 'Waitlist' NOT NULL,
  	"columns_turnkey_label" varchar DEFAULT 'Turnkey' NOT NULL,
  	"columns_turnkey_badge_suffix" varchar DEFAULT '(roadmap)',
  	"columns_turnkey_status_label" varchar DEFAULT 'Roadmap' NOT NULL,
  	"footnote_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "landing_transparency_blocks" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL
  );
  
  CREATE TABLE "landing_transparency_blocks_locales" (
  	"kicker" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"body_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "landing_transparency" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "boring_tech_under_hood" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"name" varchar NOT NULL
  );
  
  CREATE TABLE "boring_tech_under_hood_locales" (
  	"desc" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "boring_tech_our_code" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"name" varchar NOT NULL
  );
  
  CREATE TABLE "boring_tech_our_code_locales" (
  	"desc" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "boring_tech" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "boring_tech_locales" (
  	"eyebrow" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"lede" varchar NOT NULL,
  	"closing_html" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "advantages_blocks" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"featured" boolean DEFAULT false
  );
  
  CREATE TABLE "advantages_blocks_locales" (
  	"title" varchar NOT NULL,
  	"lead_html" varchar NOT NULL,
  	"detail" varchar NOT NULL,
  	"phase_tag" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "advantages" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "advantages_locales" (
  	"eyebrow" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"lede" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "roadmap_phases_items" (
  	"_order" integer NOT NULL,
  	"_parent_id" varchar NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL
  );
  
  CREATE TABLE "roadmap_phases_items_locales" (
  	"text" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "roadmap_phases" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"num" varchar NOT NULL
  );
  
  CREATE TABLE "roadmap_phases_locales" (
  	"title" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "roadmap" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "roadmap_locales" (
  	"eyebrow" varchar NOT NULL,
  	"title" varchar NOT NULL,
  	"lede" varchar NOT NULL,
  	"closing" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "bootstrap_strip" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "bootstrap_strip_locales" (
  	"body" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "footer_content_columns_links" (
  	"_order" integer NOT NULL,
  	"_parent_id" varchar NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"href" varchar NOT NULL,
  	"external" boolean DEFAULT false,
  	"soon" boolean DEFAULT false
  );
  
  CREATE TABLE "footer_content_columns_links_locales" (
  	"label" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "footer_content_columns" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL
  );
  
  CREATE TABLE "footer_content_columns_locales" (
  	"heading" varchar NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" varchar NOT NULL
  );
  
  CREATE TABLE "footer_content" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "footer_content_locales" (
  	"brand_desc" varchar NOT NULL,
  	"copyright" varchar DEFAULT '© {{year}} AppRafter · apprafter.dev' NOT NULL,
  	"license_note" varchar DEFAULT 'FSL-1.1-Apache-2.0 · auto-converts to Apache 2.0 after 2 years' NOT NULL,
  	"founder_note" varchar DEFAULT 'Bootstrap-funded. Built solo.' NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "waitlist_form_copy" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "waitlist_form_copy_locales" (
  	"form_intro" varchar NOT NULL,
  	"email_label" varchar DEFAULT 'Email' NOT NULL,
  	"use_case_label" varchar DEFAULT 'What''s your use case? (optional)' NOT NULL,
  	"call_label" varchar DEFAULT 'I''d like a short call to discuss my use case.' NOT NULL,
  	"submit_label" varchar DEFAULT 'Notify me' NOT NULL,
  	"success_message" varchar DEFAULT '→ We''ll be in touch.' NOT NULL,
  	"success_with_call" varchar DEFAULT '→ We''ll be in touch. You''ll get a separate email with a calendar link.' NOT NULL,
  	"storage_note" varchar DEFAULT 'Stored only for launch announcement.' NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "booking" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"discovery_call_url" varchar DEFAULT 'https://calendly.com/apprafter/discovery' NOT NULL,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  CREATE TABLE "booking_locales" (
  	"discovery_call_email_subject" varchar DEFAULT 'AppRafter — pick a slot for a discovery call' NOT NULL,
  	"discovery_call_email_body" varchar DEFAULT 'Thanks for signing up. Pick a 30-minute slot here:
  
  {{url}}
  
  We’ll dig into your use case — no pitch deck, no slides, just a conversation.
  
  — AppRafter' NOT NULL,
  	"id" serial PRIMARY KEY NOT NULL,
  	"_locale" "_locales" NOT NULL,
  	"_parent_id" integer NOT NULL
  );
  
  CREATE TABLE "publishing_edit_log" (
  	"_order" integer NOT NULL,
  	"_parent_id" integer NOT NULL,
  	"id" varchar PRIMARY KEY NOT NULL,
  	"at" timestamp(3) with time zone NOT NULL,
  	"global" varchar NOT NULL,
  	"editor" varchar
  );
  
  CREATE TABLE "publishing" (
  	"id" serial PRIMARY KEY NOT NULL,
  	"last_edit_at" timestamp(3) with time zone,
  	"last_edited_global" varchar,
  	"last_promoted_at" timestamp(3) with time zone,
  	"promote_to_prod" boolean DEFAULT false,
  	"updated_at" timestamp(3) with time zone,
  	"created_at" timestamp(3) with time zone
  );
  
  ALTER TABLE "users_sessions" ADD CONSTRAINT "users_sessions_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."users"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "payload_locked_documents_rels" ADD CONSTRAINT "payload_locked_documents_rels_parent_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."payload_locked_documents"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "payload_locked_documents_rels" ADD CONSTRAINT "payload_locked_documents_rels_users_fk" FOREIGN KEY ("users_id") REFERENCES "public"."users"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "payload_locked_documents_rels" ADD CONSTRAINT "payload_locked_documents_rels_waitlist_signups_fk" FOREIGN KEY ("waitlist_signups_id") REFERENCES "public"."waitlist_signups"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "payload_preferences_rels" ADD CONSTRAINT "payload_preferences_rels_parent_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."payload_preferences"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "payload_preferences_rels" ADD CONSTRAINT "payload_preferences_rels_users_fk" FOREIGN KEY ("users_id") REFERENCES "public"."users"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "landing_hero_locales" ADD CONSTRAINT "landing_hero_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."landing_hero"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "value_props_blocks" ADD CONSTRAINT "value_props_blocks_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."value_props"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "value_props_blocks_locales" ADD CONSTRAINT "value_props_blocks_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."value_props_blocks"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "scaling_journey_footer_kickers" ADD CONSTRAINT "scaling_journey_footer_kickers_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."scaling_journey"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "scaling_journey_footer_kickers_locales" ADD CONSTRAINT "scaling_journey_footer_kickers_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."scaling_journey_footer_kickers"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "scaling_journey_locales" ADD CONSTRAINT "scaling_journey_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."scaling_journey"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "tier_ladder_cards" ADD CONSTRAINT "tier_ladder_cards_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."tier_ladder"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "tier_ladder_cards_locales" ADD CONSTRAINT "tier_ladder_cards_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."tier_ladder_cards"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "tier_ladder_locales" ADD CONSTRAINT "tier_ladder_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."tier_ladder"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "comparison_rows" ADD CONSTRAINT "comparison_rows_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."comparison"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "comparison_rows_locales" ADD CONSTRAINT "comparison_rows_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."comparison_rows"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "comparison_locales" ADD CONSTRAINT "comparison_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."comparison"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "landing_transparency_blocks" ADD CONSTRAINT "landing_transparency_blocks_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."landing_transparency"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "landing_transparency_blocks_locales" ADD CONSTRAINT "landing_transparency_blocks_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."landing_transparency_blocks"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "boring_tech_under_hood" ADD CONSTRAINT "boring_tech_under_hood_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."boring_tech"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "boring_tech_under_hood_locales" ADD CONSTRAINT "boring_tech_under_hood_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."boring_tech_under_hood"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "boring_tech_our_code" ADD CONSTRAINT "boring_tech_our_code_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."boring_tech"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "boring_tech_our_code_locales" ADD CONSTRAINT "boring_tech_our_code_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."boring_tech_our_code"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "boring_tech_locales" ADD CONSTRAINT "boring_tech_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."boring_tech"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "advantages_blocks" ADD CONSTRAINT "advantages_blocks_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."advantages"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "advantages_blocks_locales" ADD CONSTRAINT "advantages_blocks_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."advantages_blocks"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "advantages_locales" ADD CONSTRAINT "advantages_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."advantages"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "roadmap_phases_items" ADD CONSTRAINT "roadmap_phases_items_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."roadmap_phases"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "roadmap_phases_items_locales" ADD CONSTRAINT "roadmap_phases_items_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."roadmap_phases_items"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "roadmap_phases" ADD CONSTRAINT "roadmap_phases_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."roadmap"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "roadmap_phases_locales" ADD CONSTRAINT "roadmap_phases_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."roadmap_phases"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "roadmap_locales" ADD CONSTRAINT "roadmap_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."roadmap"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "bootstrap_strip_locales" ADD CONSTRAINT "bootstrap_strip_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."bootstrap_strip"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "footer_content_columns_links" ADD CONSTRAINT "footer_content_columns_links_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."footer_content_columns"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "footer_content_columns_links_locales" ADD CONSTRAINT "footer_content_columns_links_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."footer_content_columns_links"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "footer_content_columns" ADD CONSTRAINT "footer_content_columns_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."footer_content"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "footer_content_columns_locales" ADD CONSTRAINT "footer_content_columns_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."footer_content_columns"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "footer_content_locales" ADD CONSTRAINT "footer_content_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."footer_content"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "waitlist_form_copy_locales" ADD CONSTRAINT "waitlist_form_copy_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."waitlist_form_copy"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "booking_locales" ADD CONSTRAINT "booking_locales_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."booking"("id") ON DELETE cascade ON UPDATE no action;
  ALTER TABLE "publishing_edit_log" ADD CONSTRAINT "publishing_edit_log_parent_id_fk" FOREIGN KEY ("_parent_id") REFERENCES "public"."publishing"("id") ON DELETE cascade ON UPDATE no action;
  CREATE INDEX "users_sessions_order_idx" ON "users_sessions" USING btree ("_order");
  CREATE INDEX "users_sessions_parent_id_idx" ON "users_sessions" USING btree ("_parent_id");
  CREATE INDEX "users_updated_at_idx" ON "users" USING btree ("updated_at");
  CREATE INDEX "users_created_at_idx" ON "users" USING btree ("created_at");
  CREATE UNIQUE INDEX "users_email_idx" ON "users" USING btree ("email");
  CREATE UNIQUE INDEX "waitlist_signups_email_idx" ON "waitlist_signups" USING btree ("email");
  CREATE INDEX "waitlist_signups_updated_at_idx" ON "waitlist_signups" USING btree ("updated_at");
  CREATE INDEX "waitlist_signups_created_at_idx" ON "waitlist_signups" USING btree ("created_at");
  CREATE UNIQUE INDEX "payload_kv_key_idx" ON "payload_kv" USING btree ("key");
  CREATE INDEX "payload_locked_documents_global_slug_idx" ON "payload_locked_documents" USING btree ("global_slug");
  CREATE INDEX "payload_locked_documents_updated_at_idx" ON "payload_locked_documents" USING btree ("updated_at");
  CREATE INDEX "payload_locked_documents_created_at_idx" ON "payload_locked_documents" USING btree ("created_at");
  CREATE INDEX "payload_locked_documents_rels_order_idx" ON "payload_locked_documents_rels" USING btree ("order");
  CREATE INDEX "payload_locked_documents_rels_parent_idx" ON "payload_locked_documents_rels" USING btree ("parent_id");
  CREATE INDEX "payload_locked_documents_rels_path_idx" ON "payload_locked_documents_rels" USING btree ("path");
  CREATE INDEX "payload_locked_documents_rels_users_id_idx" ON "payload_locked_documents_rels" USING btree ("users_id");
  CREATE INDEX "payload_locked_documents_rels_waitlist_signups_id_idx" ON "payload_locked_documents_rels" USING btree ("waitlist_signups_id");
  CREATE INDEX "payload_preferences_key_idx" ON "payload_preferences" USING btree ("key");
  CREATE INDEX "payload_preferences_updated_at_idx" ON "payload_preferences" USING btree ("updated_at");
  CREATE INDEX "payload_preferences_created_at_idx" ON "payload_preferences" USING btree ("created_at");
  CREATE INDEX "payload_preferences_rels_order_idx" ON "payload_preferences_rels" USING btree ("order");
  CREATE INDEX "payload_preferences_rels_parent_idx" ON "payload_preferences_rels" USING btree ("parent_id");
  CREATE INDEX "payload_preferences_rels_path_idx" ON "payload_preferences_rels" USING btree ("path");
  CREATE INDEX "payload_preferences_rels_users_id_idx" ON "payload_preferences_rels" USING btree ("users_id");
  CREATE INDEX "payload_migrations_updated_at_idx" ON "payload_migrations" USING btree ("updated_at");
  CREATE INDEX "payload_migrations_created_at_idx" ON "payload_migrations" USING btree ("created_at");
  CREATE UNIQUE INDEX "landing_hero_locales_locale_parent_id_unique" ON "landing_hero_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "value_props_blocks_order_idx" ON "value_props_blocks" USING btree ("_order");
  CREATE INDEX "value_props_blocks_parent_id_idx" ON "value_props_blocks" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "value_props_blocks_locales_locale_parent_id_unique" ON "value_props_blocks_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "scaling_journey_footer_kickers_order_idx" ON "scaling_journey_footer_kickers" USING btree ("_order");
  CREATE INDEX "scaling_journey_footer_kickers_parent_id_idx" ON "scaling_journey_footer_kickers" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "scaling_journey_footer_kickers_locales_locale_parent_id_uniq" ON "scaling_journey_footer_kickers_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "scaling_journey_locales_locale_parent_id_unique" ON "scaling_journey_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "tier_ladder_cards_order_idx" ON "tier_ladder_cards" USING btree ("_order");
  CREATE INDEX "tier_ladder_cards_parent_id_idx" ON "tier_ladder_cards" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "tier_ladder_cards_locales_locale_parent_id_unique" ON "tier_ladder_cards_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "tier_ladder_locales_locale_parent_id_unique" ON "tier_ladder_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "comparison_rows_order_idx" ON "comparison_rows" USING btree ("_order");
  CREATE INDEX "comparison_rows_parent_id_idx" ON "comparison_rows" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "comparison_rows_locales_locale_parent_id_unique" ON "comparison_rows_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "comparison_locales_locale_parent_id_unique" ON "comparison_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "landing_transparency_blocks_order_idx" ON "landing_transparency_blocks" USING btree ("_order");
  CREATE INDEX "landing_transparency_blocks_parent_id_idx" ON "landing_transparency_blocks" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "landing_transparency_blocks_locales_locale_parent_id_unique" ON "landing_transparency_blocks_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "boring_tech_under_hood_order_idx" ON "boring_tech_under_hood" USING btree ("_order");
  CREATE INDEX "boring_tech_under_hood_parent_id_idx" ON "boring_tech_under_hood" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "boring_tech_under_hood_locales_locale_parent_id_unique" ON "boring_tech_under_hood_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "boring_tech_our_code_order_idx" ON "boring_tech_our_code" USING btree ("_order");
  CREATE INDEX "boring_tech_our_code_parent_id_idx" ON "boring_tech_our_code" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "boring_tech_our_code_locales_locale_parent_id_unique" ON "boring_tech_our_code_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "boring_tech_locales_locale_parent_id_unique" ON "boring_tech_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "advantages_blocks_order_idx" ON "advantages_blocks" USING btree ("_order");
  CREATE INDEX "advantages_blocks_parent_id_idx" ON "advantages_blocks" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "advantages_blocks_locales_locale_parent_id_unique" ON "advantages_blocks_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "advantages_locales_locale_parent_id_unique" ON "advantages_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "roadmap_phases_items_order_idx" ON "roadmap_phases_items" USING btree ("_order");
  CREATE INDEX "roadmap_phases_items_parent_id_idx" ON "roadmap_phases_items" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "roadmap_phases_items_locales_locale_parent_id_unique" ON "roadmap_phases_items_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "roadmap_phases_order_idx" ON "roadmap_phases" USING btree ("_order");
  CREATE INDEX "roadmap_phases_parent_id_idx" ON "roadmap_phases" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "roadmap_phases_locales_locale_parent_id_unique" ON "roadmap_phases_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "roadmap_locales_locale_parent_id_unique" ON "roadmap_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "bootstrap_strip_locales_locale_parent_id_unique" ON "bootstrap_strip_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "footer_content_columns_links_order_idx" ON "footer_content_columns_links" USING btree ("_order");
  CREATE INDEX "footer_content_columns_links_parent_id_idx" ON "footer_content_columns_links" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "footer_content_columns_links_locales_locale_parent_id_unique" ON "footer_content_columns_links_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "footer_content_columns_order_idx" ON "footer_content_columns" USING btree ("_order");
  CREATE INDEX "footer_content_columns_parent_id_idx" ON "footer_content_columns" USING btree ("_parent_id");
  CREATE UNIQUE INDEX "footer_content_columns_locales_locale_parent_id_unique" ON "footer_content_columns_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "footer_content_locales_locale_parent_id_unique" ON "footer_content_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "waitlist_form_copy_locales_locale_parent_id_unique" ON "waitlist_form_copy_locales" USING btree ("_locale","_parent_id");
  CREATE UNIQUE INDEX "booking_locales_locale_parent_id_unique" ON "booking_locales" USING btree ("_locale","_parent_id");
  CREATE INDEX "publishing_edit_log_order_idx" ON "publishing_edit_log" USING btree ("_order");
  CREATE INDEX "publishing_edit_log_parent_id_idx" ON "publishing_edit_log" USING btree ("_parent_id");`)
}

export async function down({ db, payload, req }: MigrateDownArgs): Promise<void> {
  await db.execute(sql`
   DROP TABLE "users_sessions" CASCADE;
  DROP TABLE "users" CASCADE;
  DROP TABLE "waitlist_signups" CASCADE;
  DROP TABLE "payload_kv" CASCADE;
  DROP TABLE "payload_locked_documents" CASCADE;
  DROP TABLE "payload_locked_documents_rels" CASCADE;
  DROP TABLE "payload_preferences" CASCADE;
  DROP TABLE "payload_preferences_rels" CASCADE;
  DROP TABLE "payload_migrations" CASCADE;
  DROP TABLE "site_settings" CASCADE;
  DROP TABLE "landing_hero" CASCADE;
  DROP TABLE "landing_hero_locales" CASCADE;
  DROP TABLE "value_props_blocks" CASCADE;
  DROP TABLE "value_props_blocks_locales" CASCADE;
  DROP TABLE "value_props" CASCADE;
  DROP TABLE "scaling_journey_footer_kickers" CASCADE;
  DROP TABLE "scaling_journey_footer_kickers_locales" CASCADE;
  DROP TABLE "scaling_journey" CASCADE;
  DROP TABLE "scaling_journey_locales" CASCADE;
  DROP TABLE "tier_ladder_cards" CASCADE;
  DROP TABLE "tier_ladder_cards_locales" CASCADE;
  DROP TABLE "tier_ladder" CASCADE;
  DROP TABLE "tier_ladder_locales" CASCADE;
  DROP TABLE "comparison_rows" CASCADE;
  DROP TABLE "comparison_rows_locales" CASCADE;
  DROP TABLE "comparison" CASCADE;
  DROP TABLE "comparison_locales" CASCADE;
  DROP TABLE "landing_transparency_blocks" CASCADE;
  DROP TABLE "landing_transparency_blocks_locales" CASCADE;
  DROP TABLE "landing_transparency" CASCADE;
  DROP TABLE "boring_tech_under_hood" CASCADE;
  DROP TABLE "boring_tech_under_hood_locales" CASCADE;
  DROP TABLE "boring_tech_our_code" CASCADE;
  DROP TABLE "boring_tech_our_code_locales" CASCADE;
  DROP TABLE "boring_tech" CASCADE;
  DROP TABLE "boring_tech_locales" CASCADE;
  DROP TABLE "advantages_blocks" CASCADE;
  DROP TABLE "advantages_blocks_locales" CASCADE;
  DROP TABLE "advantages" CASCADE;
  DROP TABLE "advantages_locales" CASCADE;
  DROP TABLE "roadmap_phases_items" CASCADE;
  DROP TABLE "roadmap_phases_items_locales" CASCADE;
  DROP TABLE "roadmap_phases" CASCADE;
  DROP TABLE "roadmap_phases_locales" CASCADE;
  DROP TABLE "roadmap" CASCADE;
  DROP TABLE "roadmap_locales" CASCADE;
  DROP TABLE "bootstrap_strip" CASCADE;
  DROP TABLE "bootstrap_strip_locales" CASCADE;
  DROP TABLE "footer_content_columns_links" CASCADE;
  DROP TABLE "footer_content_columns_links_locales" CASCADE;
  DROP TABLE "footer_content_columns" CASCADE;
  DROP TABLE "footer_content_columns_locales" CASCADE;
  DROP TABLE "footer_content" CASCADE;
  DROP TABLE "footer_content_locales" CASCADE;
  DROP TABLE "waitlist_form_copy" CASCADE;
  DROP TABLE "waitlist_form_copy_locales" CASCADE;
  DROP TABLE "booking" CASCADE;
  DROP TABLE "booking_locales" CASCADE;
  DROP TABLE "publishing_edit_log" CASCADE;
  DROP TABLE "publishing" CASCADE;
  DROP TYPE "public"."_locales";
  DROP TYPE "public"."enum_value_props_blocks_icon_name";
  DROP TYPE "public"."enum_tier_ladder_cards_status";
  DROP TYPE "public"."enum_comparison_columns_self_status_kind";
  DROP TYPE "public"."enum_comparison_columns_managed_status_kind";
  DROP TYPE "public"."enum_comparison_columns_turnkey_status_kind";`)
}
