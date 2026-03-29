---
layout: page
---

<RoadmapPage>

<RoadmapItem title="Composable Packages" icon="/licensed/icons/roadmap-composable.svg" accent="#6366f1">
<RoadmapDescription>

Multi-layer OCI artifacts that let publishers compose packages from reusable building blocks. Share stable internals across releases and apply infrastructure-specific patches without rebuilding from scratch.

</RoadmapDescription>
<RoadmapFeatures>
  <RoadmapFeature status="active" issue="20" pr="22">Multi-layer artifacts</RoadmapFeature>
  <RoadmapFeature status="active" pr="21">Infrastructure patches</RoadmapFeature>
  <RoadmapFeature status="planned">Shared internals</RoadmapFeature>
  <RoadmapFeature status="planned">Delta downloads</RoadmapFeature>
</RoadmapFeatures>
</RoadmapItem>

<RoadmapItem title="Dependencies" icon="/licensed/icons/roadmap-dependencies.svg" accent="#8b5cf6">
<RoadmapDescription>

Packages declare dependencies on other packages, enabling automatic resolution for composable toolchains. A Java application can require a specific JRE version, and OCX resolves the full graph.

</RoadmapDescription>
<RoadmapFeatures>
  <RoadmapFeature status="active" pr="13">Dependency resolution</RoadmapFeature>
  <RoadmapFeature status="planned">Dependency declarations</RoadmapFeature>
  <RoadmapFeature status="planned">Transitive constraints</RoadmapFeature>
  <RoadmapFeature status="planned">Composable toolchains</RoadmapFeature>
</RoadmapFeatures>
</RoadmapItem>

<RoadmapItem title="System Requirements" icon="/licensed/icons/roadmap-sysreq.svg" accent="#06b6d4">
<RoadmapDescription>

Packages declare required host capabilities like glibc version, musl compatibility, or specific CPU features. OCX validates the host before installation and selects the best matching variant.

</RoadmapDescription>
<RoadmapFeatures>
  <RoadmapFeature status="shipped" pr="14">Package variants</RoadmapFeature>
  <RoadmapFeature status="planned">Host capabilities</RoadmapFeature>
  <RoadmapFeature status="planned">Pre-install validation</RoadmapFeature>
  <RoadmapFeature status="planned">Variant fallbacks</RoadmapFeature>
</RoadmapFeatures>
</RoadmapItem>

<RoadmapItem title="Referrer API" icon="/licensed/icons/roadmap-referrer.svg" accent="#10b981">
<RoadmapDescription>

Attach SBOMs, signatures, and attestations to existing releases using the OCI Referrers API. No need to re-publish a package to add supply-chain metadata after the fact.

</RoadmapDescription>
<RoadmapFeatures>
  <RoadmapFeature status="planned" issue="24">OCI referrers</RoadmapFeature>
  <RoadmapFeature status="planned">SBOM attachment</RoadmapFeature>
  <RoadmapFeature status="planned">Post-publish attestations</RoadmapFeature>
  <RoadmapFeature status="planned">Signature discovery</RoadmapFeature>
</RoadmapFeatures>
</RoadmapItem>

<RoadmapItem title="Interoperability" icon="/licensed/icons/roadmap-interop.svg" accent="#f59e0b">
<RoadmapDescription>

First-class integrations with the tools and platforms where developers already work. From build systems to CI pipelines to local development environments, OCX meets you where you are.

</RoadmapDescription>
<RoadmapFeatures>
  <RoadmapFeature status="active" pr="12">Bazel module</RoadmapFeature>
  <RoadmapFeature status="planned">GitHub Actions</RoadmapFeature>
  <RoadmapFeature status="planned">DevContainer features</RoadmapFeature>
  <RoadmapFeature status="planned">Shims & lazy loading</RoadmapFeature>
  <RoadmapFeature status="planned" issue="25">Air-gap export/import</RoadmapFeature>
</RoadmapFeatures>
</RoadmapItem>

<RoadmapItem title="Hardening" icon="/licensed/icons/roadmap-hardening.svg" accent="#ef4444">
<RoadmapDescription>

Stabilizing the CLI interface and package metadata format for long-term reliability. Improved error messages, better documentation, and a commitment to backwards compatibility.

</RoadmapDescription>
<RoadmapFeatures>
  <RoadmapFeature status="planned">Stable CLI semver</RoadmapFeature>
  <RoadmapFeature status="planned">Schema validation</RoadmapFeature>
  <RoadmapFeature status="planned" issue="26">Idempotent PATH setup</RoadmapFeature>
  <RoadmapFeature status="planned" issue="23">Relocatable symlinks</RoadmapFeature>
  <RoadmapFeature status="planned">API documentation</RoadmapFeature>
</RoadmapFeatures>
</RoadmapItem>

</RoadmapPage>
