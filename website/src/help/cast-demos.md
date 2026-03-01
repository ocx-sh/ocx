# Cast Demos

Auto-generated terminal recordings from the [test/recordings/](https://github.com/ocx-sh/ocx/tree/main/test/recordings) suite. Each recording runs real commands against a real registry.

Regenerate with `task recordings`.

## Install

```sh
ocx install hello-world:1.0.0
```

<Terminal src="/casts/install.cast" title="Installing a package" collapsed />

## Exec

```sh
ocx exec hello-world:1.0.0 -- hello
```

<Terminal src="/casts/exec.cast" title="Running a package" collapsed />

## Env

```sh
ocx env hello-world:1.0.0
```

<Terminal src="/casts/env.cast" title="Package environment" collapsed />

## Index

```sh
ocx index catalog
ocx index list hello-world
```

<Terminal src="/casts/index.cast" title="Browsing the index" collapsed />
