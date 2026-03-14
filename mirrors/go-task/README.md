---
title: Task
description: A task runner and build tool simpler than GNU Make
keywords: task,taskfile,build,automation,make,go
---

# Task

Task is a task runner and build tool that aims to be simpler and easier to use than GNU Make. It uses a simple YAML schema (Taskfile) to define tasks, supports cross-platform execution, and requires no dependencies beyond a single binary.

## What's included

This package provides the Task command-line tool:

- **task** — run tasks defined in a Taskfile.yml

## Usage with OCX

```sh
# Install a specific version
ocx install go-task:3.40

# Run directly
ocx exec go-task:3.40 -- task --version

# Set as current
ocx install --select go-task:3.40
```

## Links

- [Task Documentation](https://taskfile.dev)
- [Task on GitHub](https://github.com/go-task/task)
- [Taskfile Schema](https://taskfile.dev/reference/schema/)
