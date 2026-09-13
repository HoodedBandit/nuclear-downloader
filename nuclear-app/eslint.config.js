import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';

export default ts.config(
  {
    ignores: [
      'build/**',
      '.svelte-kit/**',
      'node_modules/**',
      'src/lib/bindings/**',
      'src-tauri/gen/**',
      'src-tauri/target*/**'
    ]
  },
  js.configs.recommended,
  ...ts.configs.recommended,
  ...svelte.configs.recommended,
  {
    languageOptions: {
      globals: {
        ...globals.browser,
        ...globals.node
      }
    }
  },
  {
    files: ['**/*.svelte'],
    languageOptions: {
      parserOptions: {
        parser: ts.parser
      }
    }
  },
  {
    files: ['e2e/**/*.mjs'],
    languageOptions: {
      globals: {
        ...globals.mocha,
        browser: 'readonly',
        expect: 'readonly',
        $: 'readonly',
        $$: 'readonly'
      }
    }
  },
  {
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }
      ]
    }
  },
  {
    // Test doubles exercise malformed wire data. Production inputs retain
    // generated types or use unknown followed by explicit narrowing.
    files: ['**/*.test.ts', 'e2e/**/*.mjs'],
    rules: { '@typescript-eslint/no-explicit-any': 'off' }
  }
);
