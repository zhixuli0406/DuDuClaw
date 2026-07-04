```markdown
# DuDuClaw Development Patterns

> Auto-generated skill from repository analysis

## Overview
This skill introduces the core development patterns and conventions used in the DuDuClaw Rust codebase. It covers file organization, code style, commit message standards, and testing practices. By following these guidelines, contributors can maintain consistency and quality across the project.

## Coding Conventions

### File Naming
- Use **snake_case** for all file and module names.
  - **Example:**  
    ```plaintext
    my_module.rs
    utils/helper_functions.rs
    ```

### Import Style
- Use **relative imports** within the crate.
  - **Example:**  
    ```rust
    mod utils;
    use crate::utils::helper_function;
    ```

### Export Style
- Use **named exports** to expose specific functions, structs, or modules.
  - **Example:**  
    ```rust
    pub fn do_something() { ... }
    pub struct MyStruct { ... }
    ```

### Commit Messages
- Follow the **conventional commit** format.
- Use the `fix` prefix for bug fixes.
- Keep commit messages descriptive (average length: ~109 characters).
  - **Example:**  
    ```
    fix: resolve panic when input is empty in process_data function
    ```

## Workflows

### Fixing a Bug
**Trigger:** When a bug or issue is identified in the codebase  
**Command:** `/fix-bug`

1. Create a new branch for your fix.
2. Locate the bug and update the code.
3. Write or update tests to cover the fix.
4. Commit your changes using the `fix:` prefix and a descriptive message.
5. Push your branch and create a pull request.

### Adding a New Module
**Trigger:** When introducing new functionality  
**Command:** `/add-module`

1. Create a new file using snake_case (e.g., `new_feature.rs`).
2. Implement the module logic.
3. Use relative imports to include dependencies.
4. Export necessary functions or structs using `pub`.
5. Write corresponding tests in a `*.test.*` file.
6. Commit with a descriptive message.

## Testing Patterns

- Test files follow the `*.test.*` naming pattern (e.g., `math.test.rs`).
- The specific test framework is not detected; use standard Rust testing conventions.
  - **Example:**  
    ```rust
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_add() {
            assert_eq!(add(2, 3), 5);
        }
    }
    ```
- Place tests in the same file or in a dedicated test file.

## Commands
| Command      | Purpose                                 |
|--------------|-----------------------------------------|
| /fix-bug     | Start the workflow for fixing a bug     |
| /add-module  | Start the workflow for adding a module  |
```
