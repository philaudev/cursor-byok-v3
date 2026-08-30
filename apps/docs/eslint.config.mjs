import { defineConfig, globalIgnores } from 'eslint/config';
import nextVitals from 'eslint-config-next/core-web-vitals';

const eslintConfig = defineConfig([
  ...nextVitals,
  globalIgnores([
    '.next/**',
    'out/**',
    'build/**',
    'next-env.d.ts',
    '.source/**',
    'public/product-demo/**',
    '.open-next/**',
    '.wrangler/**',
    'cloudflare-env.d.ts',
  ]),
]);

export default eslintConfig;