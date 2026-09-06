<!-- doc_type: how-to -->
<!-- doc_tier: first-steps -->

# Install the client

## Install

Before you install anything, read this section about the installation
background and the reasons the project ships the way it does. The client is
distributed as a single static binary for every supported platform, which
means there is no runtime to install first and no package manager to
configure. The build is reproducible from the tagged source tree, and the
release job publishes a signature beside every artifact so that a reader who
cares about supply chain provenance can verify what they downloaded before
running it. Readers who do not care about any of that can safely skip ahead,
although the page does not tell them where to skip to, which is exactly the
defect this rule catches.

!!! tip "One more aside"
    The callout also sits between the heading and the command.

```bash
grim install example
```
