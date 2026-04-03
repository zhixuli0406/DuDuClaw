```markdown
# DuDuClaw Development Patterns

> Auto-generated skill from repository analysis

## Overview
This skill introduces the core development patterns and conventions used in the DuDuClaw Rust codebase. It covers file organization, code style, commit message formatting, and testing patterns. By following these guidelines, contributors can ensure consistency and maintainability throughout the project.

## Coding Conventions

### File Naming
- Use **camelCase** for file names.
  - Example: `myModule.rs`, `userProfile.rs`

### Import Style
- Use **relative imports** for referencing modules within the project.
  - Example:
    ```rust
    mod utils;
    use crate::utils::parseData;
    ```

### Export Style
- Use **named exports** to expose specific functions, structs, or enums.
  - Example:
    ```rust
    pub fn processInput(input: &str) -> Result<()> {
        // implementation
    }
    ```

### Commit Messages
- Follow **conventional commit** format.
- Use the `feat` prefix for new features.
- Keep commit messages concise (average ~42 characters).
  - Example:  
    ```
    feat: add user authentication module
    ```

## Workflows

### Feature Development
**Trigger:** When adding a new feature  
**Command:** `/feature-development`

1. Create a new branch for your feature.
2. Implement the feature using camelCase file naming and relative imports.
3. Export new functions or structs with named exports.
4. Write or update tests in a corresponding `*.test.*` file.
5. Commit changes using the `feat` prefix and a concise message.
6. Open a pull request for review.

### Testing
**Trigger:** When verifying code correctness  
**Command:** `/run-tests`

1. Identify or create test files matching the `*.test.*` pattern.
2. Add or update tests as needed.
3. Run tests using the project's preferred Rust testing command (e.g., `cargo test`).
4. Ensure all tests pass before merging changes.

## Testing Patterns

- Test files follow the `*.test.*` naming pattern (e.g., `userProfile.test.rs`).
- Testing framework is not explicitly defined; default Rust testing is assumed.
- Example test structure:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_process_input() {
          let result = processInput("test");
          assert!(result.is_ok());
      }
  }
  ```

## Commands
| Command               | Purpose                                  |
|-----------------------|------------------------------------------|
| /feature-development  | Start the feature development workflow   |
| /run-tests            | Run all tests in the codebase            |
```
