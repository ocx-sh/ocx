 - ~~env command~~
 - select command
 - ~~retry push, if manifest unknown~~ (fixed: manifest passed through from push_package, no re-fetch)
 - get urls from manifests not supported atm, breaking SHA?
 - symlink env/profile
 - link/unlink
 - layered storage for cached packages
 - more robust cascade, that is platform-aware (ie. migrating published platform and compute rolling releases relative to the same platform)
 - auto-index of unknown packages
 - target platform specific symlink structure
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

## Ideas

 - testing packages
