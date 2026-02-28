 - ~~env command~~
 - ~~select command~~
 - ~~retry push, if manifest unknown~~ (fixed: manifest passed through from push_package, no re-fetch)
 - ~~env command path is resovled corrent but metadata may be picked from a different install~~
 - link/unlink
 - symlink env/profile
 - layered storage for cached packages
 - more robust cascade, that is platform-aware (ie. migrating published platform and compute rolling releases relative to the same platform)
 - auto-index of unknown packages
 - target platform specific symlink structure
 - reconsider install lock and atomicity of installs. maybe temporary directory and rename on success, or symlink to a temporary directory and then rename the symlink on success?
 - refactor oci reference

## Long Term

 - gitlab steps for consuming ocx packages
 - bazel rule for consuming ocx packages
 - dependencies and lockfiles
 - multiple layer
 - mirror cli for github/gitlab releases, cargo crates, npm packages, etc.
 - sbom and signature
 - shims & lazy loading
 - infrastructure extensions

## Quality of Life

 - advanced log settings with layers and filters
 - progress bars for long operations
 - get urls from manifests not supported atm, breaking SHA?
 - mirrors config
 - homebrew style auto codesigning on macOS, incl. user-friendly error messages when it fails and docs on how to fix it

## Ideas

 - testing packages
