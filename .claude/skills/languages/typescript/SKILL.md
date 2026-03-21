---
name: typescript
description: Write TypeScript code following best practices. Use when developing TypeScript/JavaScript applications. Covers design principles, type safety, patterns, and tooling.
---

# TypeScript Development

Universal design principles (SOLID, DRY, YAGNI) are defined in `.claude/rules/code-quality.md`. This section covers **TypeScript-specific applications**.

## Design Principles in TypeScript

### SOLID
- **SRP**: One class/module per concern. Prefer small, focused modules over large barrel files.
- **OCP**: Use discriminated unions and exhaustive `switch` with `never` default to ensure new variants are handled at compile time.
- **LSP**: Every interface implementation must honor the contract — no `throw new Error("not implemented")` in methods callers expect to work.
- **ISP**: Define narrow interfaces. Accept `readonly T[]` not `T[]` when not mutating. Use `Pick<T, K>` to narrow parameter types.
- **DIP**: Depend on interfaces/type aliases, inject implementations via constructor or factory functions.

### DRY
- **Generics** (`<T>`): zero-cost DRY — same algorithm over multiple types
- **Discriminated unions**: encode valid states as types, reducing runtime checks
- **Utility types** (`Pick`, `Omit`, `Partial`, `Required`): derive types from existing ones instead of duplicating
- **Don't DRY test code** — explicit assertions are clearer than clever test utilities

### YAGNI
- Don't create interfaces until a second implementation exists — start with the concrete type
- Don't add generic type parameters until the function is called with a second type
- Don't create wrapper classes around native types unless enforcing invariants (use branded types instead)
- Don't add `any` "for flexibility" — use `unknown` and narrow with type guards

## Anti-Patterns (TypeScript-Specific)

### Block
- **`any` type** — breaks type safety. Use `unknown` with type narrowing, or specific types.
- **Non-null assertion (`!`)** without justification — hides potential runtime errors. Use optional chaining or explicit checks.
- **`eval()` or `Function()` constructor** — injection risk. Always find a typed alternative.

### Warn
- **`as` type assertions** over type guards — assertions skip runtime checks. Prefer `is` type predicates or discriminated unions.
- **Index signatures** (`[key: string]: T`) where a `Record<K, T>` or explicit interface would prevent typos
- **Implicit `any` from untyped imports** — add `@types/*` or declare module types

## Project Setup

```bash
# Initialize with bun
bun init
bun add -D typescript @types/node

# TypeScript config
npx tsc --init
```

### tsconfig.json
```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "noImplicitReturns": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "outDir": "./dist"
  },
  "include": ["src/**/*"]
}
```

## Type Patterns

### Discriminated Unions
```typescript
type Result<T> =
  | { success: true; data: T }
  | { success: false; error: Error };

function handleResult(result: Result<User>) {
  if (result.success) {
    console.log(result.data); // User
  } else {
    console.error(result.error); // Error
  }
}
```

### Branded Types
```typescript
type UserId = string & { readonly brand: unique symbol };
type OrderId = string & { readonly brand: unique symbol };

function createUserId(id: string): UserId {
  return id as UserId;
}
```

### Utility Types
```typescript
// Make all properties optional
Partial<User>

// Make all properties required
Required<User>

// Pick specific properties
Pick<User, 'id' | 'email'>

// Omit specific properties
Omit<User, 'password'>

// Make properties readonly
Readonly<User>
```

## Error Handling

```typescript
// Result type pattern
type Result<T, E = Error> =
  | { ok: true; value: T }
  | { ok: false; error: E };

async function fetchUser(id: string): Promise<Result<User>> {
  try {
    const user = await db.users.findById(id);
    if (!user) {
      return { ok: false, error: new Error('User not found') };
    }
    return { ok: true, value: user };
  } catch (error) {
    return { ok: false, error: error as Error };
  }
}
```

## Testing with Vitest

```typescript
import { describe, test, expect, vi } from 'vitest';

describe('UserService', () => {
  test('creates user with valid email', async () => {
    const service = new UserService(mockRepo);
    const user = await service.create('test@example.com');
    expect(user.email).toBe('test@example.com');
  });

  test('throws on invalid email', async () => {
    const service = new UserService(mockRepo);
    await expect(service.create('invalid')).rejects.toThrow();
  });
});
```

## Tooling

```bash
# Biome (linting + formatting)
bun add -D @biomejs/biome
bun biome check --apply .

# Vitest (testing)
bun add -D vitest
bun vitest
```
