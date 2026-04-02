```markdown
# DuDuClaw Development Patterns

> Auto-generated skill from repository analysis

## Overview

This skill teaches you the core development patterns, coding conventions, and release workflows for the DuDuClaw project—a Rust-based system with a web dashboard. You'll learn how to contribute features, fix bugs, manage releases, and maintain code quality in a collaborative, convention-driven environment. The project uses conventional commits, modular Rust code, and a TypeScript/React frontend, with workflows for API, model inference, authentication, and dashboard internationalization.

## Coding Conventions

### File Naming

- **Rust:** Files use camelCase (e.g., `accountRotator.rs`, `claudeRunner.rs`).
- **Frontend:** Follows camelCase for files and PascalCase for React components (e.g., `AgentsPage.tsx`).

### Import Style

- **Rust:** Uses alias imports for clarity.
  ```rust
  use crate::handlers as gatewayHandlers;
  use duduclaw_core::types::{ApiResponse, User};
  ```
- **TypeScript:** Uses named imports and aliases.
  ```typescript
  import { getAgents as fetchAgents } from './lib/api';
  ```

### Export Style

- **Rust:** Named exports (public functions, structs, enums).
  ```rust
  pub fn handle_request() { ... }
  pub struct AccountRotator { ... }
  ```
- **TypeScript:** Named exports for functions and components.
  ```typescript
  export function AgentsPage() { ... }
  ```

### Commit Messages

- Follows [Conventional Commits](https://www.conventionalcommits.org/):
  - Prefixes: `feat`, `fix`, `chore`, `docs`
  - Example: `feat: add local model inference support`

## Workflows

### Release Version Bump and Homebrew Formula Update

**Trigger:** When releasing a new version of the software  
**Command:** `/release-bump`

1. Update the version in `Cargo.toml` and `Cargo.lock`.
2. Update the version in `HomebrewFormula/duduclaw.rb`.
3. Update version references in `README.md` (and other docs if needed).
4. Commit with a message indicating the version bump (e.g., `chore: bump version to 1.2.3`).

**Example:**
```bash
vim Cargo.toml
vim Cargo.lock
vim HomebrewFormula/duduclaw.rb
vim README.md
git add Cargo.toml Cargo.lock HomebrewFormula/duduclaw.rb README.md
git commit -m "chore: bump version to 1.2.3"
```

---

### Add or Update API Endpoint or Dashboard Feature

**Trigger:** When adding a new API endpoint or dashboard feature  
**Command:** `/add-api-endpoint`

1. Update or add backend handler (e.g., `src/handlers.rs`).
2. Update core types if needed (e.g., `src/types.rs`).
3. Update frontend API client (`web/src/lib/api.ts`).
4. Update or add relevant frontend page/component (`web/src/pages/` or `web/src/components/`).
5. Update tests or documentation if necessary.

**Example (Rust handler):**
```rust
// crates/duduclaw-gateway/src/handlers.rs
pub fn get_user_stats() -> ApiResponse { ... }
```
**Example (Frontend API):**
```typescript
// web/src/lib/api.ts
export async function getUserStats() { ... }
```

---

### Fix WebSocket Auth or Dashboard Race Condition

**Trigger:** When encountering dashboard bugs due to WebSocket auth or race conditions  
**Command:** `/fix-ws-auth`

1. Update `web/src/lib/ws-client.ts` to improve connection/auth handling.
2. Update affected dashboard pages (`web/src/pages/*.tsx`) to wait for proper auth state.
3. Update backend if handshake logic needs adjustment.
4. Test dashboard for race condition resolution.

**Example:**
```typescript
// web/src/lib/ws-client.ts
export function connectWithAuth(token: string) { ... }
```

---

### Add or Update Local Model Inference Support

**Trigger:** When adding new local model support or improving inference capabilities  
**Command:** `/add-local-model`

1. Add or update `crates/duduclaw-inference` (modules, registry, backends).
2. Update gateway runner/handlers for routing and config.
3. Update `agent.toml`/`config.toml` schema and types.
4. Update dashboard frontend for model selection/config.
5. Update or add onboarding flow for inference mode selection.

**Example (Rust):**
```rust
// crates/duduclaw-inference/src/registry.rs
pub fn register_model(name: &str, backend: Backend) { ... }
```

---

### Add or Update Account Rotation or Auth Method

**Trigger:** When supporting new authentication methods or improving account rotation  
**Command:** `/update-account-rotation`

1. Update or add `account_rotator.rs` in `duduclaw-agent`.
2. Update `claude_runner.rs` for rotation logic.
3. Update `handlers.rs` for account list/status endpoints.
4. Update dashboard frontend for account management.
5. Update docs for new auth/rotation logic.

**Example (Rust):**
```rust
// crates/duduclaw-agent/src/account_rotator.rs
pub struct AccountRotator { ... }
```

---

### Add or Update Dashboard i18n

**Trigger:** When adding UI features or new languages requiring translation  
**Command:** `/update-i18n`

1. Update or add `web/src/i18n/*.json` files.
2. Update i18n index or loader (`web/src/i18n/index.ts`).
3. Update affected UI components/pages to use new i18n keys.

**Example:**
```json
// web/src/i18n/en.json
{
  "dashboard.title": "Dashboard",
  "agents.add": "Add Agent"
}
```
```typescript
// web/src/i18n/index.ts
import en from './en.json';
export const messages = { en };
```

## Testing Patterns

- **Test File Pattern:** Files named `*.test.*` (e.g., `accountRotator.test.rs`, `api.test.ts`).
- **Framework:** Not explicitly detected; likely uses Rust's built-in test framework and Jest or similar for TypeScript.
- **Rust Example:**
  ```rust
  #[cfg(test)]
  mod tests {
      #[test]
      fn test_account_rotation() { ... }
  }
  ```
- **TypeScript Example:**
  ```typescript
  // api.test.ts
  test('fetches agents', async () => { ... });
  ```

## Commands

| Command                | Purpose                                                    |
|------------------------|------------------------------------------------------------|
| /release-bump          | Synchronize version numbers and update Homebrew formula    |
| /add-api-endpoint      | Add or update an API endpoint or dashboard feature         |
| /fix-ws-auth           | Fix WebSocket authentication or dashboard race conditions  |
| /add-local-model       | Add or update local model inference support                |
| /update-account-rotation | Add or update account rotation/authentication methods     |
| /update-i18n           | Add or update dashboard internationalization resources     |
```
