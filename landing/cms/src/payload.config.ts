// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { postgresAdapter } from '@payloadcms/db-postgres';
import { lexicalEditor } from '@payloadcms/richtext-lexical';
import { buildConfig } from 'payload';

import { Users } from './collections/Users';

const dirname = path.dirname(fileURLToPath(import.meta.url));

const serverURL = process.env.PAYLOAD_PUBLIC_SERVER_URL;

export default buildConfig({
  ...(serverURL ? { serverURL } : {}),
  secret: process.env.PAYLOAD_SECRET ?? '',
  db: postgresAdapter({
    pool: { connectionString: process.env.DATABASE_URI ?? '' },
  }),
  editor: lexicalEditor(),
  collections: [Users],
  globals: [],
  typescript: {
    outputFile: path.resolve(dirname, '../payload-types.ts'),
  },
  localization: {
    locales: ['en'],
    defaultLocale: 'en',
    fallback: true,
  },
  cors: ['http://localhost:4321', 'http://localhost:4322', 'https://apprafter.dev'],
  admin: {
    user: Users.slug,
    meta: {
      titleSuffix: '— AppRafter CMS',
    },
  },
});
